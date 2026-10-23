//! NIC bonding discovery.
//!
//! Throughput only scales past the OS-default route when the machine has
//! several *simultaneously up* internet paths (Ethernet + Wi-Fi, Wi-Fi +
//! USB tethering, an LTE dongle, ...). Every such interface egresses with
//! its own address, so binding download connections to one address per
//! interface makes the bandwidth add up — and per-IP server throttles no
//! longer click in for the whole machine (each path carries its own
//! public IP).

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

/// Substrings (lowercase) that identify virtual/tunnel adapters which
/// must never be treated as an independent uplink. A tunnel multiplexes
/// an interface we already have (WireGuard/OpenVPN/Tailscale ride the
/// same physical NIC), and hypervisor/virtualization bridges add
/// capacity of zero (Hyper-V/WSL/Docker/VMware/VirtualBox) — binding to
/// them would just stack segments on the same WAN or point them at a
/// dead-end bridge.
const BLOCKED_NAMES: &[&str] = &[
    "hyper-v",
    "vethernet",
    "wsl",
    "docker",
    "vmware",
    "virtualbox",
    "virtual box",
    "wireguard",
    "tailscale",
    "zerotier",
    "openvpn",
    "hamachi",
    "tap-",
    "tap adapter",
    "tunnel adapter",
    "vpn ",
    " vpn",
    "wi-fi direct virtual",
    "virtual adapter",
    "loopback pseudo",
];

/// True when any of the interface's names matches a blocker.
fn virtual_adapter(name: &str, friendly: Option<&str>, description: Option<&str>) -> bool {
    for hay in [Some(name), friendly, description] {
        let Some(hay) = hay else { continue };
        let hay = hay.to_ascii_lowercase();
        if BLOCKED_NAMES.iter().any(|b| hay.contains(b)) {
            return true;
        }
    }
    false
}

/// One locally bindable uplink address. `name` is display-only
/// (Windows friendly name like "Wi-Fi", Unix interface name otherwise);
/// `addr` is what segments should bind to; `is_default_route` marks the
/// interface the OS routing table already prefers.
///
/// `v6` is the same interface's global IPv6 address when it has one. Most of
/// the swarm here is IPv6, and without a v6 bind address those connections
/// take the OS route: unmeasurable, unweighted, and free to pile onto the
/// wrong uplink.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct UplinkNic {
    pub name: String,
    pub addr: Ipv4Addr,
    pub v6: Option<std::net::Ipv6Addr>,
    pub is_default_route: bool,
}

/// True for an address a peer outside this network can actually reach.
fn is_global_v6(addr: &std::net::Ipv6Addr) -> bool {
    !(addr.is_unspecified()
        || addr.is_loopback()
        || addr.is_multicast()
        // link-local fe80::/10
        || (addr.segments()[0] & 0xffc0) == 0xfe80
        // unique-local fc00::/7
        || (addr.segments()[0] & 0xfe00) == 0xfc00)
}

/// Enumerate uplink interfaces at call time (links come and go, so this
/// is never cached) and return one bindable address per interface.
///
/// Included: interfaces that are up, carry an IPv4 address, and own a
/// default gateway (proof of an independent internet path — a Wi-Fi
/// client bridge, phone tethering, an Ethernet NIC, an LTE dongle).
/// Excluded: loopback, TUN-style VPN tunnels (usually multiplexing the
/// same uplink), IPv6-only uplinks (source-bind plumbing for IPv6 scope
/// IDs is not there yet).
pub fn bonded_nics() -> Vec<UplinkNic> {
    let mut out: Vec<UplinkNic> = Vec::new();
    for iface in netdev::get_interfaces() {
        if virtual_adapter(
            &iface.name,
            iface.friendly_name.as_deref(),
            iface.description.as_deref(),
        ) {
            tracing::debug!(
                name = %iface.name,
                friendly = ?iface.friendly_name,
                "bonding: skipping virtual/tunnel adapter"
            );
            continue;
        }
        if !iface.is_up() || iface.is_loopback() || iface.is_tun() {
            continue;
        }
        if iface.gateway.is_none() {
            continue;
        }
        let Some(&ip) = iface.ipv4_addrs().first() else {
            continue;
        };
        // Global unicast only: link-local cannot reach peers and a ULA is
        // not routable outside the site, so binding to either would silently
        // kill the connection instead of using the path.
        let v6 = iface.ipv6_addrs().into_iter().find(is_global_v6);
        let nic = UplinkNic {
            name: iface
                .friendly_name
                .clone()
                .unwrap_or_else(|| iface.name.clone()),
            addr: ip,
            v6,
            is_default_route: iface.default,
        };
        if !out.contains(&nic) {
            out.push(nic);
        }
    }
    out
}

/// Public endpoints used only to prove that an uplink can carry traffic.
/// IP literals on purpose: the check must not depend on DNS, which may be
/// served by the very path under test. Two operators so one blocked edge
/// does not disable bonding for everybody.
const PROBE_TARGETS_V4: &[(Ipv4Addr, u16)] = &[
    (Ipv4Addr::new(1, 1, 1, 1), 443),
    (Ipv4Addr::new(8, 8, 8, 8), 53),
];

/// Same idea over IPv6: some tether/CGNAT paths drop IPv4 long before v6.
const PROBE_TARGETS_V6: &[(std::net::Ipv6Addr, u16)] = &[
    (std::net::Ipv6Addr::new(0x2606, 0x4700, 0x4700, 0, 0, 0, 0, 0x1111), 443),
    (std::net::Ipv6Addr::new(0x2001, 0x4860, 0x4860, 0, 0, 0, 0, 0x8888), 53),
];

/// How long one uplink gets to complete a TCP handshake.
const UPLINK_PROBE_TIMEOUT: Duration = Duration::from_millis(1200);

/// One source-bound TCP handshake. `false` covers "no route", "network
/// unreachable", "refused" and "timed out" alike: from the caller's point
/// of view the uplink cannot carry a connection either way.
async fn tcp_probe(src: IpAddr, dst: SocketAddr, timeout: Duration) -> bool {
    let sock = match src {
        IpAddr::V4(_) => tokio::net::TcpSocket::new_v4(),
        IpAddr::V6(_) => tokio::net::TcpSocket::new_v6(),
    };
    let Ok(sock) = sock else {
        return false;
    };
    if sock.bind(SocketAddr::new(src, 0)).is_err() {
        return false;
    }
    matches!(
        tokio::time::timeout(timeout, sock.connect(dst)).await,
        Ok(Ok(_))
    )
}

/// Try every target of one address family; true as soon as one handshake
/// completes. Probes race, so a dead path costs one timeout, not two.
async fn probe_family(src: IpAddr, targets: &[SocketAddr]) -> bool {
    use futures::StreamExt;

    let mut probes = futures::stream::FuturesUnordered::new();
    for dst in targets {
        probes.push(tcp_probe(src, *dst, UPLINK_PROBE_TIMEOUT));
    }
    while let Some(ok) = probes.next().await {
        if ok {
            return true;
        }
    }
    false
}

/// Does this uplink have a path that actually works right now? Either family
/// answers for the whole interface: a tether whose IPv4 CGNAT is broken but
/// whose v6 works is still a usable uplink.
///
/// Static discovery is not enough. An unplugged USB tether still has its
/// adapter `Up`, still owns its DHCP address *and* DHCP gateway, so it
/// passes every table check while every packet it sends is dropped: binding
/// peer connections to it turned 86% of dial attempts into 6 s timeouts and
/// left 11 live peers where the same swarm then held 45.
async fn uplink_carries_traffic(nic: &UplinkNic) -> bool {
    let v4: Vec<SocketAddr> = PROBE_TARGETS_V4
        .iter()
        .map(|(ip, port)| SocketAddr::from((*ip, *port)))
        .collect();
    if probe_family(IpAddr::V4(nic.addr), &v4).await {
        return true;
    }
    let Some(v6_src) = nic.v6 else {
        return false;
    };
    let targets: Vec<SocketAddr> = PROBE_TARGETS_V6
        .iter()
        .map(|(ip, port)| SocketAddr::from((*ip, *port)))
        .collect();
    probe_family(IpAddr::V6(v6_src), &targets).await
}

/// Discovery plus a live reachability check, split so frontends can explain
/// what was skipped instead of silently ignoring a configured interface.
#[derive(Debug, Clone, Default)]
pub struct UplinkScan {
    /// Uplinks that just completed a handshake: usable for bonding.
    pub live: Vec<UplinkNic>,
    /// Interfaces that look like uplinks but have no working path right now.
    pub dead: Vec<UplinkNic>,
}

/// Enumerate uplinks and keep only the ones that can reach the internet.
///
/// A single discovered uplink short-circuits: there is nothing to bond, and
/// an offline machine must not lose its only path to a probe. Probes run in
/// parallel, so the wall time is one timeout at most.
pub async fn scan_uplinks() -> UplinkScan {
    let nics = bonded_nics();
    if nics.len() < 2 {
        return UplinkScan {
            live: nics,
            dead: Vec::new(),
        };
    }
    let checks = nics.iter().map(uplink_carries_traffic);
    let results = futures::future::join_all(checks).await;
    let mut scan = UplinkScan::default();
    for (nic, ok) in nics.into_iter().zip(results) {
        if ok {
            scan.live.push(nic);
        } else {
            tracing::info!(
                name = %nic.name,
                addr = %nic.addr,
                "bonding: uplink has no working path, skipping"
            );
            scan.dead.push(nic);
        }
    }
    scan
}

/// Just the usable uplinks (`scan_uplinks().live`).
pub async fn live_bonded_nics() -> Vec<UplinkNic> {
    scan_uplinks().await.live
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pure smoke test: discovery must not panic and must return sane
    /// entries. A CI runner has (at least) one default-route interface,
    /// but a VM with none is not an error, so nothing is asserted about
    /// the count — only about the well-formedness of whatever it finds.
    #[test]
    fn discovery_returns_sane_entries() {
        for nic in bonded_nics() {
            assert!(!nic.addr.is_unspecified(), "unspecified address: {nic:?}");
            assert!(!nic.name.is_empty(), "nameless nic: {nic:?}");
        }
    }

    /// Real-world adapter names that must be excluded vs kept. Windows
    /// names in particular come from GetAdaptersAddresses and vary by
    /// virtualization host.
    #[test]
    fn virtual_adapter_blocklist() {
        // Blocked: tunnels riding an existing uplink...
        assert!(virtual_adapter("wg0", Some("WireGuard"), None));
        assert!(virtual_adapter(
            "{4D2...}",
            Some("TAP-Windows Adapter V9"),
            Some("TAP Provider, VPN tunnel")
        ));
        assert!(virtual_adapter("Null0", Some("Tailscale Tunnel"), None));
        // ...and no-bandwidth virtualization bridges.
        assert!(virtual_adapter(
            "{9FA...}",
            Some("vEthernet (WSL (Hyper-V firewall))"),
            Some("Hyper-V Virtual Ethernet Adapter")
        ));
        assert!(virtual_adapter(
            "docker0",
            None,
            Some("Docker Virtual Ethernet")
        ));
        assert!(virtual_adapter(
            "",
            Some("VirtualBox Host-Only Ethernet Adapter"),
            None
        ));
        assert!(virtual_adapter(
            "lo0",
            Some("Microsoft Wi-Fi Direct Virtual Adapter"),
            None
        ));
        assert!(virtual_adapter("", Some("TAP Adapter"), None));

        // Kept: real uplinks with their own gateways.
        assert!(!virtual_adapter(
            "Ethernet 2",
            Some("Remote NDIS based Internet Sharing Device"),
            Some("Phone tethering")
        ));
        assert!(!virtual_adapter(
            "Wi-Fi",
            Some("Wi-Fi"),
            Some("Intel(R) Wi-Fi 6 AX201 160MHz")
        ));
        assert!(!virtual_adapter(
            "Ethernet 4",
            Some("Realtek PCIe GbE Family Controller"),
            None
        ));
        assert!(!virtual_adapter("wwan0", Some("Mobile broadband"), None));
        // Substring traps: must not over-block.
        assert!(!virtual_adapter(
            "eth10",
            Some("Rivet Killer Wi-Fi 6E"),
            None
        ));
    }

    /// The reachability check itself, proven against loopback so the suite
    /// stays offline: an open listener is "a working path", a closed port
    /// is not.
    #[tokio::test]
    async fn tcp_probe_sees_open_and_closed_ports() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let open = listener.local_addr().unwrap();
        let accept = tokio::spawn(async move {
            let _ = listener.accept().await;
        });
        assert!(
            tcp_probe(
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                open,
                Duration::from_millis(500)
            )
            .await,
            "a listening port must count as reachable"
        );
        let _ = accept.await;

        // Bind then drop: the address is certainly free of a listener.
        let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let closed = probe.local_addr().unwrap();
        drop(probe);
        assert!(
            !tcp_probe(
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                closed,
                Duration::from_millis(500)
            )
            .await,
            "a closed port must not count as a working path"
        );
    }
}
