use std::{
    io::{IoSliceMut},
    net::{IpAddr, SocketAddr},
    pin::Pin,
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    task::{Poll, Context as TaskContext},
    time::Duration,
};
use std::collections::HashMap;

use tokio::io::{AsyncRead, ReadBuf};

use anyhow::{Context, bail};
use librqbit_utp::{BindDevice, UtpSocketUdp};
use serde::Serialize;
use tracing::debug;

use crate::{
    Error, PeerConnectionOptions, Result,
    type_aliases::{BoxAsyncReadVectored, BoxAsyncWrite},
    vectored_traits::AsyncReadVectoredIntoCompat,
};

#[derive(Debug, Clone, Copy, Serialize)]
pub enum ConnectionKind {
    #[serde(rename = "tcp")]
    Tcp,
    #[serde(rename = "utp")]
    Utp,
    #[serde(rename = "socks")]
    Socks,
}

impl std::fmt::Display for ConnectionKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectionKind::Tcp => f.write_str("tcp"),
            ConnectionKind::Utp => f.write_str("uTP"),
            ConnectionKind::Socks => f.write_str("socks"),
        }
    }
}

pub struct ConnectionOptions {
    // socks5://[username:password@]host:port
    // If set, all outgoing connections will go through the proxy over TCP.
    pub proxy_url: Option<String>,
    // TCP outgoing connections are enabled by default
    pub enable_tcp: bool,
    pub peer_opts: Option<PeerConnectionOptions>,
}

impl Default for ConnectionOptions {
    fn default() -> Self {
        Self {
            enable_tcp: true,
            proxy_url: None,
            peer_opts: None,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct SocksProxyConfig {
    pub host: String,
    pub port: u16,
    pub username_password: Option<(String, String)>,
}

#[derive(Default, Debug, Clone)]
pub(crate) struct StreamConnectorArgs {
    pub enable_tcp: bool,
    pub socks_proxy_config: Option<SocksProxyConfig>,
    pub utp_socket: Option<Arc<UtpSocketUdp>>,
    pub bind_device: Option<BindDevice>,
    /// Source addresses for outgoing TCP connections, used round-robin (one
    /// pick per connection). Addresses not matching the destination family
    /// are skipped; with no match the connection falls back to the OS route.
    /// Shared with the session so uplinks can be hot-swapped (`set_bind_ips`)
    /// without rebuilding the session; an empty list means "OS route".
    pub bind_ips: Option<Arc<RwLock<Vec<IpAddr>>>>,
    /// Byte accounting + weights per bound uplink (bonding only).
    pub bind_meter: Option<Arc<BindMeter>>,
    pub ipv4_only: Arc<AtomicBool>,
}

/// Per-uplink accounting for bonded connections.
///
/// Bonding used to split peer connections 50/50 no matter how fast each uplink
/// was, which halves throughput when one link is much slower (Wi-Fi 75 Mbps +
/// a tethered phone at 2 Mbps: half the peers sit on a path that delivers
/// almost nothing). This meters the bytes each bound connection actually
/// reads, so the daemon can weight (or quarantine) an uplink from real traffic
/// without probe downloads.
#[derive(Debug, Default)]
pub(crate) struct BindMeter {
    /// One counter per live connection, grouped by the source address it was
    /// bound to. The read path only touches its own `AtomicU64`, so hot reads
    /// never contend on a shared map.
    counters: Mutex<HashMap<IpAddr, Vec<Arc<AtomicU64>>>>,
    /// Relative share of new connections per uplink (0 = never pick it).
    /// Missing = 1 (plain round-robin, the old behaviour).
    weights: Mutex<HashMap<IpAddr, u32>>,
    /// Smooth weighted-round-robin credit per uplink.
    credit: Mutex<HashMap<IpAddr, i64>>,
}

impl BindMeter {
    /// Register one connection's counter for `ip`.
    pub(crate) fn register(&self, ip: IpAddr) -> Arc<AtomicU64> {
        let counter = Arc::new(AtomicU64::new(0));
        self.counters
            .lock()
            .unwrap()
            .entry(ip)
            .or_default()
            .push(counter.clone());
        counter
    }

    /// Monotonic bytes read per uplink (sum over that uplink's connections).
    pub(crate) fn totals(&self) -> Vec<(IpAddr, u64)> {
        let counters = self.counters.lock().unwrap();
        counters
            .iter()
            .map(|(ip, v)| (*ip, v.iter().map(|c| c.load(Ordering::Relaxed)).sum()))
            .collect()
    }

    pub(crate) fn set_weights(&self, weights: &[(IpAddr, u32)]) {
        let mut guard = self.weights.lock().unwrap();
        guard.clear();
        for (ip, w) in weights {
            guard.insert(*ip, *w);
        }
    }

    fn weight(&self, ip: IpAddr) -> u32 {
        self.weights.lock().unwrap().get(&ip).copied().unwrap_or(1)
    }

    /// Smooth weighted round-robin over the family-matching candidates:
    /// with weights 3:1 every four picks contain three of the first. Entries
    /// with weight 0 are skipped entirely (quarantined uplink).
    fn pick(&self, candidates: &[IpAddr]) -> Option<IpAddr> {
        let mut credit = self.credit.lock().unwrap();
        let mut total = 0i64;
        let mut best: Option<(IpAddr, i64)> = None;
        for ip in candidates {
            let weight = self.weight(*ip) as i64;
            if weight <= 0 {
                credit.insert(*ip, 0);
                continue;
            }
            total += weight;
            let entry = credit.entry(*ip).or_insert(0);
            *entry += weight;
            if best.map(|(_, b)| *entry > b).unwrap_or(true) {
                best = Some((*ip, *entry));
            }
        }
        let (pick, _) = best?;
        if let Some(c) = credit.get_mut(&pick) {
            *c -= total;
        }
        Some(pick)
    }
}

/// Read half of a bonded peer connection that attributes its bytes to the
/// uplink the connection was bound to (see `BindMeter`).
struct MeteredRead {
    inner: tokio::net::tcp::OwnedReadHalf,
    counter: Arc<AtomicU64>,
}

impl AsyncRead for MeteredRead {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        let before = buf.filled().len();
        match Pin::new(&mut this.inner).poll_read(cx, buf) {
            Poll::Ready(Ok(())) => {
                let n = buf.filled().len().saturating_sub(before);
                if n > 0 {
                    this.counter.fetch_add(n as u64, Ordering::Relaxed);
                }
                Poll::Ready(Ok(()))
            }
            other => other,
        }
    }
}

impl crate::vectored_traits::AsyncReadVectored for MeteredRead {
    fn poll_read_vectored(
        self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &mut [IoSliceMut<'_>],
    ) -> Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        let res = Pin::new(&mut this.inner).poll_read_vectored(cx, buf);
        if let Poll::Ready(Ok(n)) = &res {
            if *n > 0 {
                this.counter.fetch_add(*n as u64, Ordering::Relaxed);
            }
        }
        res
    }
}

/// Round-robin pick helper (no meter configured): the entry of `ips` at
/// `counter % matching.len()` among those matching the destination family.
#[allow(dead_code)]
fn next_pick(counter: &AtomicUsize, ips: &[IpAddr], want_v4: bool) -> Option<IpAddr> {
    let matching: Vec<&IpAddr> = ips.iter().filter(|ip| ip.is_ipv4() == want_v4).collect();
    if matching.is_empty() {
        return None;
    }
    let n = counter.fetch_add(1, Ordering::Relaxed);
    Some(matching[n % matching.len()].to_owned())
}

impl SocksProxyConfig {
    pub fn parse(url: &str) -> anyhow::Result<Self> {
        let url = ::url::Url::parse(url).context("invalid proxy URL")?;
        if url.scheme() != "socks5" {
            anyhow::bail!("proxy URL should have socks5 scheme");
        }
        let host = url.host_str().context("missing host")?;
        let port = url.port().context("missing port")?;
        let up = url
            .password()
            .map(|p| (url.username().to_owned(), p.to_owned()));
        Ok(Self {
            host: host.to_owned(),
            port,
            username_password: up,
        })
    }

    async fn connect(
        &self,
        addr: SocketAddr,
    ) -> tokio_socks::Result<(
        impl tokio::io::AsyncRead + Unpin + 'static,
        impl tokio::io::AsyncWrite + Unpin + 'static,
    )> {
        let proxy_addr = (self.host.as_str(), self.port);

        let stream = if let Some((username, password)) = self.username_password.as_ref() {
            tokio_socks::tcp::Socks5Stream::connect_with_password(
                proxy_addr,
                addr,
                username.as_str(),
                password.as_str(),
            )
            .await?
        } else {
            tokio_socks::tcp::Socks5Stream::connect(proxy_addr, addr).await?
        };

        Ok(tokio::io::split(stream))
    }
}

gen_stats!(SingleStatAtomic SingleStatSnapshot, [
    attempts u64,
    successes u64,
    errors u64
], []);
gen_stats!(PerFamilyAtomic PerFamilySnapshot, [], [
    v4 SingleStatAtomic SingleStatSnapshot,
    v6 SingleStatAtomic SingleStatSnapshot
]);
gen_stats!(ConnectStatsAtomic ConnectStatsSnapshot, [], [
    socks PerFamilyAtomic PerFamilySnapshot,
    tcp PerFamilyAtomic PerFamilySnapshot,
    utp PerFamilyAtomic PerFamilySnapshot
]);

#[derive(Debug)]
pub(crate) struct StreamConnector {
    proxy_config: Option<SocksProxyConfig>,
    enable_tcp: bool,
    bind_device: Option<BindDevice>,
    bind_ips: Option<Arc<RwLock<Vec<IpAddr>>>>,
    bind_meter: Option<Arc<BindMeter>>,
    /// Round-robin position for `bind_ips`.
    bind_rr: AtomicUsize,
    utp_socket: Option<Arc<librqbit_utp::UtpSocketUdp>>,
    stats: ConnectStatsAtomic,
    ipv4_only: Arc<AtomicBool>,
}

impl StreamConnector {
    pub async fn new(config: StreamConnectorArgs) -> anyhow::Result<Self> {
        #[allow(clippy::single_match)]
        match (
            config.socks_proxy_config.is_some(),
            config.enable_tcp,
            config.utp_socket.is_some(),
        ) {
            (false, false, false) => {
                bail!("no way to connect to peers, enable TCP, uTP or socks proxy")
            }
            _ => {
                // TODO: maybe validate other combinations. For now there's no way to disable TCP
            }
        }

        Ok(Self {
            proxy_config: config.socks_proxy_config,
            enable_tcp: config.enable_tcp,
            utp_socket: config.utp_socket,
            bind_device: config.bind_device,
            bind_ips: config.bind_ips,
            bind_meter: config.bind_meter,
            bind_rr: AtomicUsize::new(0),
            stats: Default::default(),
            ipv4_only: config.ipv4_only,
        })
    }

    fn get_stat(&self, kind: ConnectionKind, is_v6: bool) -> &SingleStatAtomic {
        let stat = match kind {
            ConnectionKind::Tcp => &self.stats.tcp,
            ConnectionKind::Utp => &self.stats.utp,
            ConnectionKind::Socks => &self.stats.socks,
        };
        if is_v6 { &stat.v6 } else { &stat.v4 }
    }

    async fn with_stat<R, E>(
        &self,
        kind: ConnectionKind,
        is_v6: bool,
        fut: impl Future<Output = std::result::Result<R, E>>,
    ) -> std::result::Result<R, E> {
        let stat = self.get_stat(kind, is_v6);
        stat.attempts(1);
        fut.await
            .inspect(|_| stat.successes(1))
            .inspect_err(|_| stat.errors(1))
    }

    /// Source IP to bind this outgoing connection to, per weighted (or plain
    /// round-robin when no meter is installed) choice over the bonded
    /// interface addresses, family-matched.
    fn pick_bind_ip(&self, addr: &SocketAddr) -> Option<IpAddr> {
        let ips = self.bind_ips.as_ref()?.read().unwrap();
        // Empty = bonding turned off after creation: take the OS route.
        if ips.is_empty() {
            return None;
        }
        match self.bind_meter.as_ref() {
            Some(meter) => {
                let matching: Vec<IpAddr> = ips
                    .iter()
                    .copied()
                    .filter(|ip| ip.is_ipv4() == addr.is_ipv4())
                    .collect();
                meter.pick(&matching)
            }
            None => next_pick(&self.bind_rr, &ips[..], addr.is_ipv4()),
        }
    }

    /// Read live so a session can stop using IPv6 without a rebuild (see
    /// `Session::set_ipv4_only`).
    fn v6_disabled(&self) -> bool {
        self.ipv4_only.load(Ordering::Relaxed)
    }

    async fn tcp_connect(
        &self,
        addr: SocketAddr,
    ) -> librqbit_dualstack_sockets::Result<(tokio::net::TcpStream, Option<IpAddr>)> {
        use librqbit_dualstack_sockets::Error;

        self.with_stat(
            ConnectionKind::Tcp,
            addr.is_ipv6(),
            async {
                let sock = if addr.is_ipv6() {
                    tokio::net::TcpSocket::new_v6().map_err(Error::SocketNew)?
                } else {
                    tokio::net::TcpSocket::new_v4().map_err(Error::SocketNew)?
                };
                let sref = socket2::SockRef::from(&sock);

                if let Some(bd) = self.bind_device.as_ref() {
                    bd.bind_sref(&sref, addr.is_ipv6())?;
                }
                let mut bound = None;
                if let Some(ip) = self.pick_bind_ip(&addr) {
                    // Bind the source address: the OS picks the interface
                    // through this address (RFC 1122 weak/strong-host models
                    // both route it to the owning NIC). Windows has no
                    // SO_BINDTODEVICE, so this is the mechanism bonding relies
                    // on there; on Linux/macOS it is equally valid whenever a
                    // source IP is known. If the interface went away between
                    // discovery and now, fall back to the default route.
                    if sref
                        .bind(&std::net::SocketAddr::new(ip, 0).into())
                        .is_err()
                    {
                        debug!(?addr, ?ip, "failed to bind local address, using default route");
                    } else {
                        bound = Some(ip);
                    }
                }

                let stream = sock.connect(addr).await.map_err(Error::Connect)?;
                Ok((stream, bound))
            },
        )
        .await
    }

    pub fn stats(&self) -> &ConnectStatsAtomic {
        &self.stats
    }

    pub async fn connect(
        &self,
        addr: SocketAddr,
    ) -> Result<(ConnectionKind, BoxAsyncReadVectored, BoxAsyncWrite)> {
        if addr.port() == 0 {
            return Err(Error::Anyhow(anyhow::anyhow!(
                "invalid peer address (port 0): {}",
                addr
            )));
        }

        if self.v6_disabled() && addr.is_ipv6() {
            return Err(Error::Anyhow(anyhow::anyhow!(
                "ipv6 disabled, skipping connection to {}",
                addr
            )));
        }

        if let Some(proxy) = self.proxy_config.as_ref() {
            let (r, w) = self
                .with_stat(ConnectionKind::Socks, addr.is_ipv6(), proxy.connect(addr))
                .await?;
            debug!(?addr, "connected through SOCKS5");
            return Ok((
                ConnectionKind::Socks,
                Box::new(r.into_vectored_compat()),
                Box::new(w),
            ));
        }

        // Try to connect over TCP first. If in 1 second we haven't connected, try uTP also (if configured).
        // Whoever connects first wins.
        let tcp_connect = async {
            if !self.enable_tcp {
                return Ok(None);
            }
            let conn = self.tcp_connect(addr).await?;
            debug!(?addr, "connected over TCP");
            Ok::<_, librqbit_dualstack_sockets::Error>(Some(conn))
        };

        let tcp_failed_notify = tokio::sync::Notify::new();

        let utp_connect = async {
            let sock = match self.utp_socket.as_ref() {
                Some(sock) => sock,
                None => return Ok(None),
            };

            // Give TCP priority as it's more mature and simpler.
            if self.enable_tcp {
                // wait until either 1 second has passed or TCP failed.
                tokio::select! {
                    _ = tcp_failed_notify.notified() => {},
                    _ = tokio::time::sleep(Duration::from_secs(1)) => {}
                }
            }

            let conn = self
                .with_stat(ConnectionKind::Utp, addr.is_ipv6(), sock.connect(addr))
                .await?;

            debug!(?addr, "connected over uTP");
            Ok(Some(conn))
        };

        tokio::pin!(tcp_connect);
        tokio::pin!(utp_connect);

        let mut tcp_err: Option<Option<librqbit_dualstack_sockets::Error>> = None;
        let mut utp_err: Option<Option<librqbit_utp::Error>> = None;

        // wait until all fail, or one succeeds.
        loop {
            if let (Some(tcp), Some(utp)) = (tcp_err.as_mut(), utp_err.as_mut()) {
                match (tcp.take(), utp.take()) {
                    (Some(tcp), Some(utp)) => return Err(Error::Connect { tcp, utp }),
                    (Some(tcp), None) => return Err(Error::TcpConnect(tcp)),
                    (None, Some(utp)) => return Err(Error::UtpConnect(utp)),
                    (None, None) => return Err(Error::ConnectDisabled),
                }
            }
            tokio::select! {
                tcp_res = &mut tcp_connect, if tcp_err.is_none() => {
                    match tcp_res {
                        Ok(Some(stream)) => {
                            let (stream, bound) = stream;
                            let (r, w) = stream.into_split();
                            let r: BoxAsyncReadVectored = match (bound, self.bind_meter.as_ref()) {
                                (Some(ip), Some(meter)) => Box::new(MeteredRead {
                                    inner: r,
                                    counter: meter.register(ip),
                                }),
                                _ => Box::new(r),
                            };
                            return Ok((ConnectionKind::Tcp, r, Box::new(w)));
                        },
                        Ok(None) => {
                            tcp_err = Some(None);
                            tcp_failed_notify.notify_waiters();
                        }
                        Err(e) => {
                            tcp_err = Some(Some(e));
                            tcp_failed_notify.notify_waiters();
                        }
                    }
                },
                utp_res = &mut utp_connect, if utp_err.is_none() => {
                    match utp_res {
                        Ok(Some(stream)) => {
                            let (r, w) = stream.split();
                            return Ok((ConnectionKind::Utp, Box::new(r), Box::new(w)));
                        },
                        Ok(None) => {
                            utp_err = Some(None);
                        }
                        Err(e) => {
                            utp_err = Some(Some(e));
                        }
                    }
                },
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ips(v4: &[&str]) -> Vec<IpAddr> {
        v4.iter().map(|s| s.parse().unwrap()).collect()
    }

    #[test]
    fn round_robin_cycles_family_matched() {
        let c = AtomicUsize::new(0);
        let list = ips(&["192.168.1.8", "10.0.0.5", "10.0.0.9"]);
        let a = next_pick(&c, &list, true).unwrap();
        let b = next_pick(&c, &list, true).unwrap();
        let d = next_pick(&c, &list, true).unwrap();
        let e = next_pick(&c, &list, true).unwrap();
        assert_eq!(
            (a, b, d, e),
            (list[0], list[1], list[2], list[0]),
            "cycles through v4 candidates in order"
        );
    }

    #[test]
    fn skips_wrong_family() {
        let c = AtomicUsize::new(0);
        let list = ips(&["192.168.1.8", "fe80::1"]);
        // v6 destination: only the v6 candidate matches.
        assert_eq!(next_pick(&c, &list, false).unwrap().to_string(), "fe80::1");
        // v4 destination: only the v4 candidate matches.
        assert_eq!(
            next_pick(&c, &list, true).unwrap().to_string(),
            "192.168.1.8"
        );
    }

    #[test]
    fn no_family_match_is_none() {
        let c = AtomicUsize::new(0);
        let list = ips(&["192.168.1.8", "10.0.0.5"]);
        assert!(next_pick(&c, &list, false).is_none());
    }

    // The session hands the connector a shared list so uplinks can be swapped
    // while torrents run (Wi-Fi finishing DHCP after the session was built).
    #[tokio::test]
    async fn connector_reads_the_live_bind_list() {
        let shared = Arc::new(RwLock::new(ips(&["192.168.1.8"])));
        let connector = StreamConnector::new(StreamConnectorArgs {
            enable_tcp: true,
            bind_ips: Some(shared.clone()),
            ..Default::default()
        })
        .await
        .expect("connector");

        let dest: SocketAddr = "203.0.113.5:6881".parse().unwrap();
        assert_eq!(
            connector.pick_bind_ip(&dest).unwrap().to_string(),
            "192.168.1.8"
        );

        // A tether is plugged in: the next connection rotates over both.
        *shared.write().unwrap() = ips(&["192.168.1.8", "10.118.70.127"]);
        let picks: Vec<String> = (0..2)
            .map(|_| connector.pick_bind_ip(&dest).unwrap().to_string())
            .collect();
        assert!(
            picks.contains(&"10.118.70.127".to_string()),
            "new uplink is used without rebuilding the session: {picks:?}"
        );

        // Bonding off: back to the OS route.
        shared.write().unwrap().clear();
        assert!(connector.pick_bind_ip(&dest).is_none());
    }

    // Bonding used to split peers 50/50 even when one uplink was 40x slower.
    #[test]
    fn weights_steer_new_connections_to_the_fast_uplink() {
        let meter = BindMeter::default();
        let wifi: IpAddr = "192.168.1.8".parse().unwrap();
        let tether: IpAddr = "10.118.70.127".parse().unwrap();
        let candidates = [wifi, tether];

        // Measured 7 MB/s vs 2 MB/s -> roughly 3:1, not 1:1.
        meter.set_weights(&[(wifi, 3), (tether, 1)]);
        let picks: Vec<IpAddr> = (0..40).map(|_| meter.pick(&candidates).unwrap()).collect();
        let wifi_n = picks.iter().filter(|p| **p == wifi).count();
        assert!((28..=32).contains(&wifi_n), "3:1 split, got {wifi_n}/40");

        // A quarantined uplink gets no new connections at all.
        meter.set_weights(&[(wifi, 1), (tether, 0)]);
        let picks: Vec<IpAddr> = (0..20).map(|_| meter.pick(&candidates).unwrap()).collect();
        assert!(picks.iter().all(|p| *p == wifi), "quarantined: {picks:?}");
    }

    #[test]
    fn meter_totals_add_up_per_uplink() {
        let meter = BindMeter::default();
        let a: IpAddr = "10.0.0.1".parse().unwrap();
        let b: IpAddr = "10.0.0.2".parse().unwrap();
        let c1 = meter.register(a);
        let c2 = meter.register(a);
        let c3 = meter.register(b);
        c1.fetch_add(100, Ordering::Relaxed);
        c2.fetch_add(50, Ordering::Relaxed);
        c3.fetch_add(7, Ordering::Relaxed);
        let mut totals = meter.totals();
        totals.sort_by_key(|(ip, _)| ip.to_string());
        assert_eq!(totals, vec![(a, 150), (b, 7)]);
    }
}
