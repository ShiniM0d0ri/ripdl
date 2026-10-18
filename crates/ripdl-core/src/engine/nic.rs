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
}
