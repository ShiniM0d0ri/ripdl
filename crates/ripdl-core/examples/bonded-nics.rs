//! Debug helper: print every interface that NIC bonding would use, one
//! line per eligible uplink. Run directly from the repo:
//! `cargo run -p ripdl-core --example bonded-nics`
use ripdl_core::engine::bonded_nics;

fn main() {
    let nics = bonded_nics();
    println!("eligible uplinks for NIC bonding: {}", nics.len());
    for (i, nic) in nics.iter().enumerate() {
        println!(
            "  {}. {:<24} {:<16} default_route={}",
            i + 1,
            nic.name,
            nic.addr,
            nic.is_default_route
        );
    }
    let extras = nics.iter().filter(|n| !n.is_default_route).count();
    if !nics.is_empty() && extras == 0 {
        println!("  -> only the OS-default interface found; bonding stays off (OS routing).");
    } else if extras > 0 {
        println!(
            "  -> bonding will use the default interface (unbound) + {extras} pinned interface(s)."
        );
    }
}
