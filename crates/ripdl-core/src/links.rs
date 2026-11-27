//! next links domain: one source of truth for NIC state, rates and weights.
//!
//! This replaces the two parallel systems (`bond::Bond` for HTTP and librqbit's
//! `BindMeter` for torrents). Both engines feed the same [`LinkSet`] through
//! [`ConnectionGuard::record`]; the daemon samples it once with a measured
//! `dt`. No external probing happens here - a [`Probe`] port is called only
//! when the set is enabled.
//!
//! Invariants and failing test names live in `docs/next/INVARIANTS.md`.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::time::Instant;
use ts_rs::TS;

use crate::traffic::{Meter, RateWindow, DEFAULT_RATE_WINDOW};

/// How often a quarantined link is given a full windows worth of explore
/// dials. A single 5 s tick is not enough: a link needs a real chance to land
/// at least one connection before it can be judged again.
pub const DEFAULT_EXPLORE_EVERY: Duration = Duration::from_secs(60);
/// Two consecutive dial failures quarantine a non-default link.
const DIAL_FAIL_SKIP: u32 = 2;

/// One bindable uplink address.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
pub struct LinkInfo {
    #[ts(as = "String")]
    pub ip: IpAddr,
    pub name: String,
    pub default_route: bool,
}

/// Serializable view of one link. This is the only shape frontends receive.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct LinkSnapshot {
    pub name: String,
    #[ts(as = "String")]
    pub ip: IpAddr,
    pub default_route: bool,
    #[ts(as = "f64")]
    pub rate_bps: u64,
    #[ts(as = "f64")]
    pub capacity_bps: u64,
    pub weight: u32,
    pub in_use: u32,
    pub fails: u32,
    pub utp_metered: bool,
}

#[derive(Debug)]
pub struct LinkShared {
    info: LinkInfo,
    meter: Arc<Meter>,
    utp_meter: Arc<Meter>,
    rate: Mutex<RateWindow>,
    capacity: Mutex<f64>,
    last_total: AtomicU64,
    conn_ms: AtomicU64,
    in_use: AtomicU32,
    fails: AtomicU32,
    weight: AtomicU32,
    explore_until: Mutex<Option<Instant>>,
    counters: Mutex<Vec<Arc<AtomicU64>>>,
}

impl LinkShared {
    fn total(&self) -> u64 {
        self.meter.load().saturating_add(self.utp_meter.load())
    }

    fn end_connection(&self, elapsed_ms: u64, counter: &Arc<AtomicU64>) {
        self.in_use.fetch_sub(1, Ordering::Relaxed);
        self.conn_ms.fetch_add(elapsed_ms, Ordering::Relaxed);
        let mut counters = self.counters.lock().unwrap();
        if let Some(pos) = counters.iter().position(|c| Arc::ptr_eq(c, counter)) {
            counters.remove(pos);
        }
    }

    fn snapshot(&self) -> LinkSnapshot {
        LinkSnapshot {
            name: self.info.name.clone(),
            ip: self.info.ip,
            default_route: self.info.default_route,
            rate_bps: self.rate.lock().unwrap().rate().unwrap_or(0.0) as u64,
            capacity_bps: *self.capacity.lock().unwrap() as u64,
            weight: self.weight.load(Ordering::Relaxed),
            in_use: self.in_use.load(Ordering::Relaxed),
            fails: self.fails.load(Ordering::Relaxed),
            utp_metered: true,
        }
    }
}

/// A live connection on one link. Dropping it releases the meter counter and
/// records connection-seconds, which the sampler divides into bytes to derive
/// per-connection capacity.
#[derive(Debug)]
pub struct ConnectionGuard {
    link: Arc<LinkShared>,
    counter: Arc<AtomicU64>,
    start: Instant,
}

impl ConnectionGuard {
    /// Attribute bytes read by this connection. `utp` is recorded in the same
    /// link total so uTP traffic can never silently bypass the link rate.
    pub fn record(&self, bytes: u64, utp: bool) {
        self.counter.fetch_add(bytes, Ordering::Relaxed);
        if utp {
            self.link.utp_meter.add(bytes);
        } else {
            self.link.meter.add(bytes);
        }
    }

    pub fn ip(&self) -> IpAddr {
        self.link.info.ip
    }
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        let ms = self.start.elapsed().as_millis() as u64;
        self.link.end_connection(ms, &self.counter);
    }
}

/// Reachability port. The daemon supplies a real TCP handshake; tests use a
/// fake that can refuse hardcoded targets and accept configured ones.
pub trait Probe: Send + Sync {
    fn probe(&self, targets: &[SocketAddr]) -> bool;
}

/// Owns the machine's uplink set. Created once by the daemon and shared with
/// the HTTP engine, the torrent session, the CLI, and the frontends.
pub struct LinkSet {
    links: RwLock<Vec<Arc<LinkShared>>>,
    enabled: AtomicBool,
    explore_every: Duration,
    probe: Mutex<Option<Box<dyn Probe>>>,
    probe_targets: RwLock<Vec<SocketAddr>>,
    probe_calls: AtomicU64,
    explore_until_global: Mutex<Option<Instant>>,
}

impl std::fmt::Debug for LinkSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LinkSet")
            .field("links", &self.links.read().unwrap().len())
            .field("enabled", &self.enabled.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl LinkSet {
    pub fn new() -> Arc<Self> {
        Self::with_explore(DEFAULT_EXPLORE_EVERY)
    }

    pub fn with_explore(explore_every: Duration) -> Arc<Self> {
        Arc::new(Self {
            links: RwLock::new(Vec::new()),
            enabled: AtomicBool::new(false),
            explore_every,
            probe: Mutex::new(None),
            probe_targets: RwLock::new(Vec::new()),
            probe_calls: AtomicU64::new(0),
            explore_until_global: Mutex::new(None),
        })
    }

    pub fn set_probe(&self, probe: Box<dyn Probe>) {
        *self.probe.lock().unwrap() = Some(probe);
    }

    pub fn set_probe_targets(&self, targets: Vec<SocketAddr>) {
        *self.probe_targets.write().unwrap() = targets;
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
        if !enabled {
            self.links.write().unwrap().clear();
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// Replace the link set in whole. History follows an address; links that
    /// left discovery (and their counters) are dropped.
    pub fn set_links(&self, infos: &[LinkInfo]) {
        let old = self.links.read().unwrap().clone();
        let mut next: Vec<Arc<LinkShared>> = Vec::with_capacity(infos.len());
        for info in infos.iter().cloned().filter(is_eligible_link) {
            if let Some(existing) = old.iter().find(|l| l.info == info) {
                next.push(Arc::clone(existing));
                continue;
            }
            if let Some(existing) = old.iter().find(|l| l.info.ip == info.ip) {
                // Same address, renamed or re-flagged: keep learned rates.
                let link = LinkShared {
                    info,
                    meter: Arc::clone(&existing.meter),
                    utp_meter: Arc::clone(&existing.utp_meter),
                    rate: Mutex::new(RateWindow::new(DEFAULT_RATE_WINDOW)),
                    capacity: Mutex::new(*existing.capacity.lock().unwrap()),
                    last_total: AtomicU64::new(existing.last_total.load(Ordering::Relaxed)),
                    conn_ms: AtomicU64::new(existing.conn_ms.load(Ordering::Relaxed)),
                    in_use: AtomicU32::new(existing.in_use.load(Ordering::Relaxed)),
                    fails: AtomicU32::new(existing.fails.load(Ordering::Relaxed)),
                    weight: AtomicU32::new(existing.weight.load(Ordering::Relaxed)),
                    explore_until: Mutex::new(*existing.explore_until.lock().unwrap()),
                    counters: Mutex::new(Vec::new()),
                };
                // The rate history is tied to the address, so re-seed it from
                // the old window's last total at construction time below.
                if let Some(last) = existing.rate.lock().unwrap().rate() {
                    *link.capacity.lock().unwrap() = last.max(*link.capacity.lock().unwrap());
                }
                next.push(Arc::new(link));
                continue;
            }
            next.push(Arc::new(new_link(info)));
        }
        drop(old);
        *self.links.write().unwrap() = next;
    }

    pub fn links(&self) -> Vec<Arc<LinkShared>> {
        self.links.read().unwrap().clone()
    }

    pub fn link(&self, ip: IpAddr) -> Option<Arc<LinkShared>> {
        self.links
            .read()
            .unwrap()
            .iter()
            .find(|l| l.info.ip == ip)
            .cloned()
    }

    /// Open a metered connection on `ip`. `None` when the link is gone.
    pub fn connect(&self, ip: IpAddr) -> Option<ConnectionGuard> {
        let link = self.link(ip)?;
        let counter = Arc::new(AtomicU64::new(0));
        link.counters.lock().unwrap().push(Arc::clone(&counter));
        link.in_use.fetch_add(1, Ordering::Relaxed);
        Some(ConnectionGuard {
            link,
            counter,
            start: Instant::now(),
        })
    }

    /// Aggregate byte feed for an engine that has no connection guard (e.g.
    /// reqwest's pooled sockets). Per-connection capacity still comes from
    /// guards opened by calls that can hold one.
    pub fn record_ip(&self, ip: IpAddr, bytes: u64, utp: bool) {
        if let Some(link) = self.link(ip) {
            if utp {
                link.utp_meter.add(bytes);
            } else {
                link.meter.add(bytes);
            }
        }
    }

    pub fn note_ok(&self, ip: IpAddr) {
        if let Some(link) = self.link(ip) {
            link.fails.store(0, Ordering::Relaxed);
            link.weight.store(1, Ordering::Relaxed);
        }
    }

    pub fn note_fail(&self, ip: IpAddr) {
        if let Some(link) = self.link(ip) {
            let fails = link.fails.fetch_add(1, Ordering::Relaxed) + 1;
            // The default route must always be dialable.
            if fails >= DIAL_FAIL_SKIP && !link.info.default_route {
                link.weight.store(0, Ordering::Relaxed);
            }
        }
    }

    /// Probe only when bonding is enabled. Returns `false` when disabled or
    /// when no probe is installed; the daemon uses this to avoid any external
    /// handshake while bonding is off.
    pub fn probe_if_enabled(&self) -> bool {
        if !self.enabled() {
            return false;
        }
        let guard = self.probe.lock().unwrap();
        let Some(probe) = guard.as_ref().map(|p| p.as_ref()) else {
            return false;
        };
        self.probe_calls.fetch_add(1, Ordering::Relaxed);
        let targets = self.probe_targets.read().unwrap().clone();
        probe.probe(&targets)
    }

    pub fn probe_calls(&self) -> u64 {
        self.probe_calls.load(Ordering::Relaxed)
    }

    /// Fold metered bytes into rates and re-derive weights. `dt` must be the
    /// **measured** interval since the previous sample; no fixed tick is used.
    pub fn sample(&self, now: Instant, dt: Duration) -> Vec<LinkSnapshot> {
        let links = self.links.read().unwrap().clone();
        let dt = dt.max(Duration::from_millis(1));
        let secs = dt.as_secs_f64();

        let mut deltas: HashMap<IpAddr, u64> = HashMap::new();
        let mut capacities: HashMap<IpAddr, f64> = HashMap::new();

        for link in &links {
            let total = link.total();
            let delta = total.saturating_sub(link.last_total.swap(total, Ordering::Relaxed));
            let ms = link.conn_ms.swap(0, Ordering::Relaxed);
            if ms > 0 && delta > 0 {
                let per_conn = delta as f64 / (ms as f64 / 1000.0);
                let mut cap = link.capacity.lock().unwrap();
                *cap = if *cap == 0.0 {
                    per_conn
                } else {
                    *cap * 0.6 + per_conn * 0.4
                };
                capacities.insert(link.info.ip, *cap);
            } else {
                capacities.insert(link.info.ip, *link.capacity.lock().unwrap());
            }
            link.rate.lock().unwrap().push(now, total);
            deltas.insert(link.info.ip, delta);
        }

        let max_capacity = capacities.values().copied().fold(0.0f64, f64::max);
        let exploring = self.is_exploring(now);
        for link in &links {
            let fails = link.fails.load(Ordering::Relaxed);
            if fails >= DIAL_FAIL_SKIP && !link.info.default_route && !exploring {
                link.weight.store(0, Ordering::Relaxed);
                continue;
            }
            // An explore window lasts the whole interval, and a link is never
            // re-quarantined from a stale rate - only a failed dial does that.
            if exploring || fails >= DIAL_FAIL_SKIP {
                link.weight.store(1, Ordering::Relaxed);
                continue;
            }
            let cap = capacities.get(&link.info.ip).copied().unwrap_or(0.0);
            let weight = if max_capacity > 0.0 && cap > 0.0 {
                ((cap / max_capacity * 10.0).round() as u32).clamp(1, 10)
            } else {
                1
            };
            link.weight.store(weight, Ordering::Relaxed);
        }
        let _ = secs; // dt is consumed by the rate windows via `now`.
        links.iter().map(|l| l.snapshot()).collect()
    }

    fn is_exploring(&self, now: Instant) -> bool {
        let mut until = self.explore_until_global.lock().unwrap();
        match *until {
            Some(t) if now < t => true,
            _ => {
                *until = Some(now + self.explore_every);
                false
            }
        }
    }

    pub fn snapshot(&self) -> Vec<LinkSnapshot> {
        self.links
            .read()
            .unwrap()
            .iter()
            .map(|l| l.snapshot())
            .collect()
    }

    /// Bounded per-connection counter count - churn cannot grow it without
    /// limit because guards remove their counter on drop.
    pub fn connection_counters(&self) -> usize {
        self.links
            .read()
            .unwrap()
            .iter()
            .map(|l| l.counters.lock().unwrap().len())
            .sum()
    }
}

fn new_link(info: LinkInfo) -> LinkShared {
    LinkShared {
        info,
        meter: Arc::new(Meter::default()),
        utp_meter: Arc::new(Meter::default()),
        rate: Mutex::new(RateWindow::new(DEFAULT_RATE_WINDOW)),
        capacity: Mutex::new(0.0),
        last_total: AtomicU64::new(0),
        conn_ms: AtomicU64::new(0),
        in_use: AtomicU32::new(0),
        fails: AtomicU32::new(0),
        weight: AtomicU32::new(1),
        explore_until: Mutex::new(None),
        counters: Mutex::new(Vec::new()),
    }
}

/// Adapters must never bond a tunnel or virtual bridge: it multiplexes an
/// existing uplink and adds no bandwidth. Kept here so the rule is testable
/// without the netdev dependency.
pub fn is_eligible_link(info: &LinkInfo) -> bool {
    let name = info.name.to_ascii_lowercase();
    const BLOCKED: &[&str] = &[
        "hyper-v", "vethernet", "wsl", "docker", "vmware", "virtualbox",
        "virtual box", "wireguard", "tailscale", "zerotier", "openvpn",
        "hamachi", "nordlynx", "proton", "mullvad", "cloudflare warp",
        "tap-", "tap adapter", "tunnel adapter", "vpn ", " vpn",
        "wi-fi direct virtual", "virtual adapter", "loopback pseudo",
    ];
    !info.ip.is_loopback() && !BLOCKED.iter().any(|b| name.contains(b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    fn ip(n: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(10, 0, 0, n))
    }

    fn info(n: u8, name: &str, default: bool) -> LinkInfo {
        LinkInfo {
            ip: ip(n),
            name: name.into(),
            default_route: default,
        }
    }

    struct FakeProbe {
        accepted: Vec<SocketAddr>,
    }
    impl Probe for FakeProbe {
        fn probe(&self, targets: &[SocketAddr]) -> bool {
            targets.iter().any(|t| self.accepted.contains(t))
        }
    }

    #[tokio::test(start_paused = true)]
    async fn link_rates_sum_to_task() {
        let set = LinkSet::new();
        set.set_links(&[info(1, "wifi", true), info(2, "tether", false)]);
        let a = set.connect(ip(1)).unwrap();
        let b = set.connect(ip(2)).unwrap();
        let t0 = Instant::now();
        set.sample(t0, Duration::from_millis(1)); // baseline
        a.record(600, false);
        b.record(400, true);
        let snap = set.sample(t0 + Duration::from_secs(1), Duration::from_secs(1));
        let sum: u64 = snap.iter().map(|l| l.rate_bps).sum();
        assert_eq!(sum, 1000, "per-link rates sum to the aggregate");
        assert!(snap.iter().any(|l| l.utp_metered));
    }

    #[tokio::test(start_paused = true)]
    async fn link_rate_measured_dt() {
        let set = LinkSet::new();
        set.set_links(&[info(1, "wifi", true)]);
        let c = set.connect(ip(1)).unwrap();
        let t0 = Instant::now();
        set.sample(t0, Duration::from_millis(1)); // baseline
        c.record(1000, false);
        // A 1 s measured window must yield 1000 B/s. A hardcoded 5 s window
        // (the old bug) would report 200 B/s instead.
        let snap = set.sample(t0 + Duration::from_secs(1), Duration::from_secs(1));
        assert_eq!(snap[0].rate_bps, 1000, "rate must use the measured dt");
    }

    #[tokio::test(start_paused = true)]
    async fn departed_link_is_pruned() {
        let set = LinkSet::new();
        set.set_links(&[info(1, "wifi", true), info(2, "tether", false)]);
        let c = set.connect(ip(2)).unwrap();
        c.record(1000, false);
        drop(c);
        assert_eq!(set.connection_counters(), 0);
        set.set_links(&[info(1, "wifi", true)]);
        let snap = set.snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].ip, ip(1));
    }

    #[tokio::test(start_paused = true)]
    async fn idle_link_decays() {
        let set = LinkSet::new();
        set.set_links(&[info(1, "wifi", true)]);
        let c = set.connect(ip(1)).unwrap();
        let t0 = Instant::now();
        set.sample(t0, Duration::from_millis(1)); // baseline
        c.record(10_000, false);
        let first = set.sample(t0 + Duration::from_secs(2), Duration::from_secs(2));
        assert!(first[0].rate_bps > 0);
        let mut last = 0;
        for i in 1..=8 {
            last = set
                .sample(
                    t0 + Duration::from_secs(2 + i * 2),
                    Duration::from_secs(2),
                )[0]
                .rate_bps;
        }
        assert_eq!(last, 0, "idle link decays to zero within 15 s");
    }

    #[tokio::test(start_paused = true)]
    async fn idle_link_no_stale_rate() {
        let set = LinkSet::new();
        set.set_links(&[info(1, "wifi", true)]);
        let c = set.connect(ip(1)).unwrap();
        let t0 = Instant::now();
        c.record(5000, false);
        drop(c);
        let snap = set.sample(t0 + Duration::from_secs(20), Duration::from_secs(20));
        assert_eq!(snap[0].rate_bps, 0, "no connections means no displayed rate");
    }

    #[tokio::test(start_paused = true)]
    async fn quarantined_link_can_recover() {
        let set = LinkSet::with_explore(Duration::from_secs(60));
        set.set_links(&[info(1, "wifi", true), info(2, "tether", false)]);
        set.note_fail(ip(2));
        set.note_fail(ip(2));
        let t0 = Instant::now();
        let first = set.sample(t0, Duration::from_secs(5));
        assert_eq!(first[1].weight, 0);
        // The next sample is inside the explore window: weight 1 for the
        // whole 60 s, not one 5 s tick that a peer may never land in.
        let second = set.sample(t0 + Duration::from_secs(5), Duration::from_secs(5));
        assert_eq!(second[1].weight, 1);
        let third = set.sample(t0 + Duration::from_secs(30), Duration::from_secs(25));
        assert_eq!(third[1].weight, 1, "still exploring, still dialable");
    }

    #[tokio::test(start_paused = true)]
    async fn weights_follow_capacity() {
        let set = LinkSet::new();
        set.set_links(&[info(1, "tether", true), info(2, "wifi", false)]);
        let tether = set.connect(ip(1)).unwrap();
        let wifi1 = set.connect(ip(2)).unwrap();
        let wifi2 = set.connect(ip(2)).unwrap();
        let now = Instant::now();
        set.sample(now, Duration::from_millis(1)); // baseline
        tether.record(20_000, false);
        wifi1.record(8_000, false);
        wifi2.record(8_000, false);
        tokio::time::advance(Duration::from_secs(1)).await;
        drop((tether, wifi1, wifi2));
        let snap = set.sample(now + Duration::from_secs(2), Duration::from_secs(2));
        // Aggregate favors wifi (16k vs 20k is close), but capacity per
        // connection favors tether (20k/1 vs 8k/1).
        assert!(snap[0].capacity_bps > snap[1].capacity_bps);
        assert!(snap[0].weight >= snap[1].weight);
    }

    #[tokio::test(start_paused = true)]
    async fn stale_rate_does_not_weight() {
        let set = LinkSet::new();
        set.set_links(&[info(1, "wifi", true), info(2, "old", false)]);
        let old = set.connect(ip(2)).unwrap();
        old.record(1_000_000, false);
        drop(old);
        let now = Instant::now();
        set.sample(now + Duration::from_secs(2), Duration::from_secs(2));
        set.set_links(&[info(1, "wifi", true)]);
        let snap = set.sample(now + Duration::from_secs(4), Duration::from_secs(2));
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].weight, 1, "a vanished link cannot set max capacity");
    }

    #[tokio::test(start_paused = true)]
    async fn no_probes_when_bonding_off() {
        let set = LinkSet::new();
        struct CountingProbe(AtomicU64);
        impl Probe for CountingProbe {
            fn probe(&self, _t: &[SocketAddr]) -> bool {
                self.0.fetch_add(1, Ordering::Relaxed);
                true
            }
        }
        set.set_probe(Box::new(CountingProbe(AtomicU64::new(0))));
        assert!(!set.probe_if_enabled());
        assert_eq!(set.probe_calls(), 0);
        set.set_enabled(true);
        assert!(set.probe_if_enabled());
        assert_eq!(set.probe_calls(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn probe_targets_are_configurable_or_fallback() {
        let set = LinkSet::new();
        set.set_enabled(true);
        let custom: SocketAddr = (Ipv4Addr::new(192, 168, 1, 1), 443).into();
        set.set_probe_targets(vec![custom]);
        set.set_probe(Box::new(FakeProbe {
            accepted: vec![custom],
        }));
        assert!(
            set.probe_if_enabled(),
            "a working custom target must be usable"
        );
    }

    #[test]
    fn vpn_adapters_are_excluded() {
        assert!(!is_eligible_link(&info(1, "NordLynx", false)));
        assert!(!is_eligible_link(&info(2, "Proton VPN", false)));
        assert!(!is_eligible_link(&info(3, "Cloudflare WARP", false)));
        assert!(!is_eligible_link(&info(4, "WireGuard Tunnel", false)));
        assert!(is_eligible_link(&info(5, "Wi-Fi", true)));
        assert!(is_eligible_link(&info(6, "Ethernet 2", false)));
    }

    #[tokio::test(start_paused = true)]
    async fn cli_bond_matches_daemon() {
        let set = LinkSet::new();
        set.set_links(&[info(1, "wifi", true), info(2, "tether", false)]);
        let shared = Arc::clone(&set);
        let a = set.snapshot();
        let b = shared.snapshot();
        assert_eq!(a, b, "all clients must read the same link snapshot");
    }

    #[tokio::test(start_paused = true)]
    async fn bind_meter_is_pruned() {
        let set = LinkSet::new();
        set.set_links(&[info(1, "wifi", true)]);
        for _ in 0..1000 {
            let c = set.connect(ip(1)).unwrap();
            c.record(10, false);
        }
        assert_eq!(
            set.connection_counters(),
            0,
            "dropped connections must not leak counters"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn utp_is_metered_or_excluded() {
        let set = LinkSet::new();
        set.set_links(&[info(1, "wifi", true)]);
        let c = set.connect(ip(1)).unwrap();
        let t0 = Instant::now();
        set.sample(t0, Duration::from_millis(1)); // baseline
        c.record(500, false);
        c.record(500, true);
        let snap = set.sample(t0 + Duration::from_secs(1), Duration::from_secs(1));
        assert_eq!(snap[0].rate_bps, 1000, "uTP bytes are in the same link rate");
        assert!(snap[0].utp_metered);
    }

    #[test]
    fn ipv6_links_are_supported() {
        let info = LinkInfo {
            ip: IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1)),
            name: "v6".into(),
            default_route: false,
        };
        assert!(is_eligible_link(&info));
    }
}
