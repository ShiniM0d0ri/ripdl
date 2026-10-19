# Vendored librqbit fork

Apache-2.0, upstream <https://github.com/ikatson/rqbit> (crates.io `librqbit`
9.0.1). Wired in through `[patch.crates-io]` in the workspace root and a
workspace member, so CI builds and tests this copy even without a network.

## Why a fork

`bind_device_name` (upstream's interface binding) is one device per session
and returns `BindDeviceNotSupported` on Windows. NIC bonding needs a *different
local source address per connection*, round-robin — because on Windows the OS
picks the interface by source address (RFC 1122 strong-host model), which is
the same mechanism `ripdl_core::engine::nic` / reqwest `local_address` use for
the HTTP engine.

## Delta vs upstream 9.0.1

All in `src/stream_connect.rs` + `src/session.rs`:

1. `SessionOptions::bind_ips: Option<Vec<IpAddr>>` — source addresses for
   outgoing peer connections.
2. `StreamConnector` rotates over `bind_ips` per outgoing TCP connection
   (`next_pick`, family-matched; no match → OS default route; a bind failure
   (e.g. the interface vanished) logs and falls back to the default route
   instead of failing the peer).
3. Unit tests for the rotation helper (`stream_connect::tests`).
4. `SessionOptions::fastresume_folder: Option<PathBuf>` — when `persistence`
   is unset, the session builds `JsonSessionPersistenceStore` over that folder
   and uses it as the *bitfield factory only* (`Ok((None, store))` in
   `persistence_factory`). rqbit then loads/stores `<info-hash>.bitv` piece
   maps, so a re-added torrent runs `validate_fastresume` (a sample of pieces
   plus one per file) instead of `initial_check` over every byte. The session
   half stays `None`, so no torrent list, trackers or pause state are
   restored: ripdl's `queue.json` remains the single source of truth and
   nothing is silently re-added or resumed.

Nothing else was touched; `ConnectionOptions`, uTP, DHT and trackers behave
exactly as upstream. Notably, uTP (once TCP loses the 1s race) and DHT stay
OS-routed — bonding redistributes outgoing TCP peer load, which is the
dominant traffic direction when downloading.

## Updating

Copy the new registry tarball over this directory (keep this README), reapply
the delta above, then run `cargo test --workspace`. The rotation tests and the
loopback bonded seeder↔leecher round-trip in `crates/ripdl-core/src/torrent.rs`
must both stay green (it also asserts the fastresume bitfield was written).
