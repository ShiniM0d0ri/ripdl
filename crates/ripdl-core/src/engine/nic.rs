//! NIC bonding discovery.
//!
//! Throughput only scales past the OS-default route when the machine has
//! several *simultaneously up* internet paths (Ethernet + Wi-Fi, Wi-Fi +
//! USB tethering, an LTE dongle, ...). Every such interface egresses with
//! its own address, so binding download connections to one address per
//! interface makes the bandwidth add up — and per-IP server throttles no
//! longer click in for the whole machine (each path carries its own
//! public IP).

use std::net::Ipv4Addr;

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
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct UplinkNic {
    pub name: String,
    pub addr: Ipv4Addr,
    pub is_default_route: bool,
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
        let nic = UplinkNic {
            name: iface
                .friendly_name
                .clone()
                .unwrap_or_else(|| iface.name.clone()),
            addr: ip,
            is_default_route: iface.default,
        };
        if !out.contains(&nic) {
            out.push(nic);
        }
    }
    out
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
}
