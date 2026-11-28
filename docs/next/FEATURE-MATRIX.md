# ripdl feature matrix

Status is evidence-based, not aspirational.

| Status | Meaning |
|---|---|
| `shipped` | Code path exists and a test proves the user-visible behavior |
| `partial` | Works, but a documented sub-case is missing or weak |
| `broken` | A known defect makes the feature unreliable or unsafe |
| `missing` | Not implemented |
| `untested` | Code exists, but no test proves the behavior |
| `spike` | Prototype only, not part of the product |

Every row should name the owning module and the proof. A row without proof is
not `shipped`.

## Link domain (next)

| ID | Feature | Status | Evidence | Proof / gap |
|---|---|---|---|---|
| LNK-001 | One `LinkSet` with per-link meters, rates, and snapshots | shipped | `links.rs::LinkSet` | `link_rates_sum_to_task`, `cli_bond_matches_daemon` |
| LNK-002 | Measured `dt` sampler, durable + uTP byte feeding | shipped | `links.rs::sample`, `ConnectionGuard` | `link_rate_measured_dt`, `utp_is_metered_or_excluded` |
| LNK-003 | Departed-link pruning and bounded connection counters | shipped | `links.rs::set_links`, `connection_counters` | `departed_link_is_pruned`, `bind_meter_is_pruned` |
| LNK-004 | Capacity-based weights with full-window quarantine recovery | shipped | `links.rs::sample`, `note_fail` | `weights_follow_capacity`, `quarantined_link_can_recover`, `stale_rate_does_not_weight` |
| LNK-005 | Probe port, configurable targets, off when bonding disabled | shipped | `links.rs::Probe`, `probe_if_enabled` | `no_probes_when_bonding_off`, `probe_targets_are_configurable_or_fallback` |
| LNK-006 | VPN/virtual adapter exclusion policy | shipped | `links.rs::is_eligible_link` | `vpn_adapters_are_excluded` |
| LNK-007 | Wire the `LinkSet` into daemon/engine/torrent/CLI/GUI (close WP-03 issues) | shipped | `daemon/main.rs`, `engine/mod.rs`, `links.rs` | Decoupled 1 s sampler, 6 s link rate window, IPv6 NIC aliases, HTTP `ConnectionGuard`; `daemon_sampler_updates_link_rates_across_5s_and_ipv6_aliases` |

## Traffic domain (next)

| ID | Feature | Status | Evidence | Proof / gap |
|---|---|---|---|---|
| TRA-001 | Monotonic meter and measured rate window with rollback/reset | shipped | `traffic.rs::Meter`, `RateWindow` | `retry_does_not_double_count`, `resume_has_no_adoption_spike` |
| TRA-002 | Hierarchical token bucket (global -> task -> host), live rates, cancellation, bounded burst | shipped | `traffic.rs::Limiter` | `global_cap_two_tasks`, `limiter_cancel_latency`, `limiter_burst_bound`, `limit_zero_and_invalid` |
| TRA-003 | Task and part traffic handles with run generations and live limits | shipped | `traffic.rs::TaskTraffic`, `PartTraffic` | `bundle_parts_share_task_cap`, `paused_task_has_no_live_rate`, `stale_progress_sample_is_ignored` |
| TRA-004 | `TrafficController` and `Traffic` port with snapshots | shipped | `traffic.rs::TrafficController` | `torrent_respects_global_cap`, `video_respects_global_cap`, `limit_change_applies_live` |
| TRA-005 | Wire the controller into engine/daemon/frontends (close WP-01 issues) | shipped | `daemon/Cargo.toml` (`default = ["unified-traffic"]`), `daemon/main.rs`, `spike/native-ui` | Unified cap covers HTTP, bundle, torrent & video by default; `unified_traffic_is_default_enabled` + `traffic.rs` tests |

## HTTP engine

| ID | Feature | Status | Evidence | Proof / gap |
|---|---|---|---|---|
| ENG-001 | HEAD probe with ranged-GET fallback | shipped | `engine/planner.rs` | `bare_head_still_discovers_ranges` |
| ENG-002 | Multi-segment ranged download | shipped | `engine/mod.rs` | `multi_segment_exact_bytes` |
| ENG-003 | Dynamic work stealing from the largest straggler | shipped | `engine/mod.rs` | `freed_worker_steals_from_the_largest_straggler` |
| ENG-004 | No-range single-stream fallback | shipped | `engine/mod.rs` | `no_ranges_falls_back_to_single_stream` |
| ENG-005 | Crash-safe resume sidecar | shipped | `engine/mod.rs` | `pause_resumes_from_sidecar` |
| ENG-006 | ETag/If-Match guard on resume | shipped | `engine/mod.rs` | `stale_etag_forces_fresh_restart` |
| ENG-007 | Resume validates the server's total | shipped | `engine/mod.rs::validate_resume_state` | `resume_total_mismatch_starts_fresh` |
| ENG-008 | Resume validates the on-disk file length | shipped | `engine/mod.rs::validate_resume_state` | `resume_file_length_validated` |
| ENG-009 | Corrupt sidecar recovery | shipped | `engine/mod.rs::prepare_destination` | `corrupt_and_malformed_sidecar_recover_cleanly` |
| ENG-010 | Malformed sidecar safety | shipped | `engine/mod.rs::validate_resume_state` | `corrupt_and_malformed_sidecar_recover_cleanly` |
| ENG-011 | Retry with exponential backoff | shipped | `engine/retry.rs` | `rate_limited_ranges_back_off_and_finish` |
| ENG-012 | Host-pressure gate (429/5xx halves connections) | shipped | `engine/gate.rs` | `fills_up_to_the_cap_then_waits`, `shrink_serializes_then_grow_restores` |
| ENG-013 | Stream watchdog for a silently dead connection | shipped | `engine/mod.rs` | `stalled_connection_recovers_via_watchdog` |
| ENG-014 | Sparse preallocation on Windows | shipped | `engine/mod.rs` | `sparse_preallocation_allocates_only_written_bytes` |
| ENG-015 | Pipelined buffered writes (network/disk overlap) | untested | `engine/mod.rs` | No direct test of the ping-pong buffer path |
| ENG-016 | Per-task speed limiter (all segments share) | shipped | `engine/limiter.rs` | `speed_limit_paces_download`, `speed_limit_applies_to_single_stream` |
| ENG-017 | Global speed limiter across tasks | shipped | `daemon/Cargo.toml`, `traffic.rs` | `global_cap_two_tasks`, `unified_traffic_is_default_enabled` |
| ENG-018 | Live limit reconfiguration | shipped | `daemon/main.rs::cmd_set_config`, `traffic.rs` | `limit_change_applies_live`, `traffic_limit_change_applies_live` |
| ENG-019 | Limiter cancellation latency | shipped | `engine/limiter.rs::acquire_with_cancel` | `limiter_cancel_latency` |
| ENG-020 | Limiter burst bound | shipped | `engine/limiter.rs` | `limiter_burst_bound` |
| ENG-021 | Content-Encoding identity for downloads | shipped | `engine/mod.rs::http_client` | `gzip_identity_and_hash` |
| ENG-022 | Content-Disposition filename parsing | shipped | `engine/planner.rs` | `carries_headers_and_uses_cd_filename` |
| ENG-023 | Headers, cookies, referer, User-Agent pass-through | shipped | `engine/mod.rs` | Integration tests |
| ENG-024 | Redirect following | untested | `reqwest` default | No test for a redirect chain or loop |
| ENG-025 | HTTP/2 support | missing | `Cargo.toml` | `reqwest` is built with `default-features = false` and no `http2` feature |
| ENG-026 | Proxy support | missing | - | No config key or client builder hook |
| ENG-027 | FTP/SFTP support | missing | - | - |
| ENG-028 | Checksum verification after download | missing | - | - |
| ENG-029 | Metalink support | missing | - | - |
| ENG-030 | Per-host connection limits | missing | - | - |
| ENG-031 | Adaptive segment count | partial | `engine/mod.rs` | Fixed `max_segments` plus stealing; no measurement-driven cap |
| ENG-032 | IPv6 source binding for HTTP bonding | missing | `daemon/main.rs::feed_http_bond` | HTTP bond links are IPv4-only; torrents bind v6 |
| ENG-033 | Safe filename collision handling | shipped | `engine/mod.rs::prepare_destination` | `no_silent_overwrite` |
| ENG-034 | `max_segments = 1` respected | shipped | `engine/mod.rs::plan_fresh` | `max_segments_one` |
| ENG-035 | `max_segments` upper bound | shipped | `engine/mod.rs::plan_fresh` | `max_segments_bound` |
| ENG-036 | `min_segment_size = 0` rejected | shipped | `engine/mod.rs::plan_fresh` | `min_segment_zero_rejected` |
| ENG-037 | Config validation before planning | shipped | `rpc.rs::DaemonSettings::load` | `config_load_validation` |
| ENG-038 | Duplicate URL idempotency | partial | `daemon/main.rs::duplicate_task` | Same URL+dest is shared; same filename from a different URL is safely suffixed |
| ENG-039 | Direct CLI download safety | shipped | `engine/mod.rs::prepare_destination` | `no_silent_overwrite` |
| ENG-040 | Direct CLI bond sampling | broken | `engine/mod.rs::Dialer::new` | A standalone engine builds a `Bond` but never calls `sample`; weights stay 1 |
| ENG-041 | Browser-like TLS fingerprint | missing | - | `reqwest`/rustls defaults only |
| ENG-042 | POST/form/body downloads | missing | - | - |
| ENG-043 | Download filename collision suffix | shipped | `engine/mod.rs::prepare_destination` | `no_silent_overwrite` |

## Refresh and bridge

| ID | Feature | Status | Evidence | Proof / gap |
|---|---|---|---|---|
| REF-001 | Retarget a paused HTTP task to a fresh URL | shipped | `refresh.rs` | `retarget_keeps_progress_and_swaps_identity` |
| REF-002 | Reject a new address with a different ETag | shipped | `refresh.rs` | `refresh_etag_mismatch_rejected` |
| REF-003 | Validate sidecar bounds on refresh | shipped | `refresh.rs` | `refresh_validates_sidecar_bounds` |
| REF-004 | Windows hidden attribute after refresh | shipped | `refresh.rs` | `refresh_sidecar_stays_hidden` |
| REF-005 | Corrupt refresh sidecar recovery | broken | `refresh.rs::load` | Parse error propagates and blocks the refresh |
| BRG-001 | Native-messaging host framing | shipped | `bridge.rs` | `handoff.test.mjs`, e2e B phase |
| BRG-002 | Daemon spawn rate limit | shipped | `bridge.rs::spawn_daemon_bg` | 3 s global cooldown |
| BRG-003 | RPC timeout does not leak threads | shipped | `bridge.rs::talk_async` | `bridge_timeout_no_thread_leak` |
| BRG-004 | Retry only transient errors | shipped | `bridge.rs::forward_once` | `bridge_no_retry_on_auth_failure` |
| BRG-005 | 1 MiB reply cap | shipped | `bridge.rs` | `write_frame` refuses oversized replies |

## Networking and bonding

| ID | Feature | Status | Evidence | Proof / gap |
|---|---|---|---|---|
| NET-001 | NIC discovery with gateway/default-route metadata | shipped | `engine/nic.rs` | `discovery_returns_sane_entries` |
| NET-002 | Virtual/tunnel adapter blocklist | shipped | `engine/nic.rs` | `virtual_adapter_blocklist` |
| NET-003 | Source-bound reachability probe | shipped | `engine/nic.rs` | `tcp_probe_sees_open_and_closed_ports` |
| NET-004 | Two-strike uplink eviction and revive | shipped | `daemon/main.rs::refresh_uplinks` | Daemon logic; no direct test |
| NET-005 | HTTP smooth weighted round-robin | shipped | `bond.rs` | `pick_respects_smooth_weights` |
| NET-006 | HTTP per-connection capacity weights | shipped | `bond.rs` | `weights_follow_per_connection_capacity_not_share` |
| NET-007 | HTTP on-the-fly rebalance/migration | shipped | `bond.rs`, `engine/mod.rs` | `rebalance_moves_a_connection...`; live migration is untested |
| NET-008 | HTTP per-link aggregate rate | broken | `daemon/main.rs::feed_http_bond` | Hardcoded `dt = 5 s`; a delayed tick inflates the rate |
| NET-009 | HTTP per-link durable-byte metering | broken | `engine/mod.rs` | `bond.record` runs at read time, before rollback; retries over-count |
| NET-010 | HTTP per-link rate decay | partial | `bond.rs::sample` | EMA decays only while sampling; a stopped sampler leaves the last value |
| NET-011 | HTTP link pruning | partial | `bond.rs::set_links` | Departed links vanish, but unsampled delta is dropped |
| NET-012 | HTTP link history follows IP | partial | `bond.rs::set_links` | An IP reassigned to another adapter inherits stale history |
| NET-013 | Torrent source binding | shipped | `torrent.rs`, `vendor/librqbit` | Torrent integration test |
| NET-014 | Torrent weighted peer distribution | shipped | `daemon/main.rs::apply_bind_weights` | Unit tests for `weights_from_rates`; no live swarm test |
| NET-015 | Torrent per-link rate display | broken | `daemon/main.rs` | `bind_rates` entries are never pruned; ghost IPs stay forever |
| NET-016 | One link authority for HTTP and torrent | missing | `bond.rs`, `vendor/librqbit` | Two separate meters and weight models |
| NET-017 | Per-NIC display includes torrent traffic | broken | `ripdl-gui/ui/app.js` | Chips read `bond_links` (HTTP only) |
| NET-018 | CLI `--bond` uses the daemon's link truth | broken | `engine/mod.rs` | No `sample`, no reachability probe, no shared set |
| NET-019 | Torrent bind meter pruning | broken | `vendor/librqbit/src/stream_connect.rs` | `counters` map grows per peer connection, never pruned |
| NET-020 | uTP metering/bonding | missing | `vendor/librqbit` | Only TCP reads are wrapped in `MeteredRead` |
| NET-021 | Quarantine recovery | broken | `bond.rs`, `daemon/main.rs` | Explore window is one 5 s tick per 60 s; a link with no dial in that tick is re-quarantined |
| NET-022 | Aggregate vs capacity consistency | broken | `daemon/main.rs` | HTTP weights use capacity; torrent weights use aggregate rate |
| NET-023 | No probes when bonding is off | broken | `daemon/main.rs::watch_bonding` | `refresh_uplinks` runs every 5 s regardless of `bond_interfaces` |
| NET-024 | Probe targets are robust | partial | `engine/nic.rs` | Hardcoded 1.1.1.1:443 and 8.8.8.8:53; blocked ports mark a working link dead |
| NET-025 | VPN adapter coverage | partial | `engine/nic.rs` | Blocklist misses many VPN names (NordLynx, Proton, Mullvad, WARP, ...) |
| NET-026 | Uplink scan does not block the watcher | partial | `daemon/main.rs` | `scan_uplinks` can take 1.2 s before the 5 s sample |
| NET-027 | Bonding state has one RPC shape | missing | `rpc.rs` | `bond_links` and `torrent_rates` are separate arrays |

## Queue and daemon

| ID | Feature | Status | Evidence | Proof / gap |
|---|---|---|---|---|
| QUE-001 | Persistent queue at `%LOCALAPPDATA%\\ripdl\\queue.json` | shipped | `daemon/main.rs` | Daemon tests |
| QUE-002 | One runner per task | shipped | `daemon/main.rs::spawn_runner` | Daemon tests |
| QUE-003 | Queue-wide concurrency cap | shipped | `daemon/main.rs::slots` | Daemon tests |
| QUE-004 | Live concurrency resize | shipped | `daemon/main.rs::SlotGate` | `live_concurrency_shrink_blocks_queued_tasks` |
| QUE-005 | `--after` / `--at` scheduling | shipped | `daemon/main.rs::start_delay` | `start_delay_windows` |
| QUE-006 | Pause / resume | shipped | `daemon/main.rs` | Daemon and integration tests |
| QUE-007 | Remove task and optionally delete files | shipped | `daemon/main.rs::cmd_remove` | Daemon tests |
| QUE-008 | Duplicate URL idempotency | partial | `daemon/main.rs::duplicate_task` | Same URL+dest is shared; same filename from a different URL is safely suffixed |
| QUE-009 | Stale-link refresh flow | shipped | `refresh.rs`, `daemon/main.rs` | `refreshed_address_resumes_from_sidecar` |
| QUE-010 | HTTP task details | shipped | `daemon/main.rs::cmd_details` | `Reply::TaskDetails` |
| QUE-011 | Bundle aggregate progress | shipped | `daemon/main.rs::bundle_totals` | Bundle tests |
| QUE-012 | Boot restore of torrents | shipped | `daemon/main.rs` | Daemon tests |
| QUE-013 | Progress map cleanup | shipped | `daemon/main.rs::on_progress` | `unknown_progress_dropped` |
| QUE-014 | Run generation on progress samples | shipped | `daemon/main.rs::on_progress_run`, `traffic.rs` | `stale_progress_sample_with_old_run_id_is_ignored`, `stale_progress_sample_is_ignored` |
| QUE-015 | Queue file validation | partial | `daemon/main.rs::new` | Corrupt queue is backed up; individual records are not validated |
| QUE-016 | Dead legacy `Queue` module | missing | `ripdl-core/src/queue.rs` | Exported but unused; contains a stale `TODO(P1)` |
| QUE-017 | Task list pagination | missing | `rpc.rs` | `List` returns every task on every 2 Hz poll |
| QUE-018 | Task priority/order | missing | `daemon/main.rs` | FIFO only |
| QUE-019 | On-disk missing-file detection and UI marker | shipped | `daemon/main.rs`, `rpc.rs`, `crates/ripdl-gui` | `TaskInfo.file_missing` + `TaskDetailsSnapshot.file_missing` (`list_and_details_detect_externally_deleted_files`, `missing_file_completed_task_reports_missing_state_and_blocks_open`) |

## Torrent

| ID | Feature | Status | Evidence | Proof / gap |
|---|---|---|---|---|
| TOR-001 | Magnet, `.torrent`, and `.torrent` URL payloads | shipped | `torrent.rs` | In-proc seeder/leecher test |
| TOR-002 | One shared torrent session | shipped | `daemon/main.rs::ensure_torrent_session` | Daemon tests |
| TOR-003 | Pre-add file selection | shipped | `torrent.rs::preview` | Integration test |
| TOR-004 | Sequential download | shipped | `torrent.rs::sequential_drain` | Integration test |
| TOR-005 | Seeding after completion | shipped | `daemon/main.rs::start_seeding` | Daemon tests |
| TOR-006 | Seed ratio/time limits | shipped | `daemon/main.rs` | Daemon tests |
| TOR-007 | Fastresume bitfield cache | shipped | `torrent.rs`, vendor delta | Live verification noted in AGENTS |
| TOR-008 | DHT routing-table persistence | shipped | `torrent.rs` | Live verification noted in AGENTS |
| TOR-009 | Stable listen port + UPnP | shipped | `torrent.rs` | Live verification noted in AGENTS |
| TOR-010 | NIC bonding for peer TCP | shipped | `torrent.rs`, vendor | Torrent integration test |
| TOR-011 | Per-link torrent rate | partial | `daemon/main.rs` | Aggregate only, no per-connection capacity |
| TOR-012 | Global speed limit | partial | `torrent.rs::set_download_rate_limit`, daemon `unified-traffic` | Session-wide librqbit cap is live and applied on save |
| TOR-013 | Per-task speed limit | shipped | `torrent.rs::TorrentConfig::download_bps` | Set through `AddTorrentOptions.ratelimits` at add time; no vendor delta needed |
| TOR-014 | uTP | missing | `torrent.rs` | TCP-only listener/connect defaults |
| TOR-015 | Download speed display | partial | `daemon/main.rs::on_progress` | 4 s byte window; tool rate is ignored |
| TOR-016 | Adoption/resume spike guard | partial | `daemon/main.rs::adoption_jump` | Relies on the tool rate being near zero while stalled |
| TOR-017 | Files/peers/trackers details | shipped | `torrent.rs::details` | Daemon tests |
| TOR-018 | Pause by info-hash | shipped | `daemon/main.rs::pause_torrent_in_session` | Daemon tests |
| TOR-019 | Seeding stop/start from the row | shipped | `daemon/main.rs::cmd_pause` / `cmd_resume` | Daemon tests |
| TOR-020 | Bind meter pruning | broken | `vendor/librqbit` | Unbounded per-connection counters |

## Video (hunt)

| ID | Feature | Status | Evidence | Proof / gap |
|---|---|---|---|---|
| VID-001 | yt-dlp sidecar | shipped | `hunt.rs` | Unit tests + live P4 verification |
| VID-002 | Percent-based progress | shipped | `hunt.rs::parse_progress` | `parses_plain_percent` and friends |
| VID-003 | Pause = kill sidecar, resume = re-invoke | shipped | `hunt.rs` | Live verification |
| VID-004 | Global speed limit | shipped | `hunt.rs::build_args` | `limit_rate_is_passed_to_ytdlp`; daemon passes the global budget |
| VID-005 | Speed display | missing | `hunt.rs` | Only percent is emitted |
| VID-006 | Refresh address | missing | `daemon/main.rs` | Refresh is HTTP-only |
| VID-007 | Configurable yt-dlp path | shipped | `rpc.rs`, `hunt.rs` | CLI/GUI config |
| VID-008 | Native HLS/DASH | missing | - | Sidecar only |

## Bundles

| ID | Feature | Status | Evidence | Proof / gap |
|---|---|---|---|---|
| BUN-001 | Link grouping into one bundle | shipped | `bundle.rs` | Bundle tests |
| BUN-002 | Preview before add | shipped | `daemon/main.rs::cmd_preview_bundle` | Daemon tests |
| BUN-003 | Add a bundle as one task | shipped | `daemon/main.rs::cmd_add_bundle` | Daemon tests |
| BUN-004 | Append parts to an existing bundle | shipped | `daemon/main.rs::cmd_add_bundle_parts` | Daemon tests |
| BUN-005 | Per-part headers | shipped | `daemon/main.rs` | Bundle tests |
| BUN-006 | Aggregate progress and percent | shipped | `daemon/main.rs::on_part_progress` | Bundle tests |
| BUN-007 | Aggregate speed | partial | `daemon/main.rs` | Derived from the aggregate byte counter; no per-part rate |
| BUN-008 | Shared task speed limit | broken | `daemon/main.rs::run_bundle` | Each part creates its own engine/limiter |
| BUN-009 | Per-part resume | shipped | `daemon/main.rs` | Bundle tests |
| BUN-010 | Delete bundle files | shipped | `daemon/main.rs::delete_artifacts` | Daemon tests |

## Browser integration

| ID | Feature | Status | Evidence | Proof / gap |
|---|---|---|---|---|
| BRW-001 | Native-messaging host | shipped | `bridge.rs`, `ext/` | `handoff.test.mjs`, e2e B phase |
| BRW-002 | `register-host` self-repair | shipped | `cli/main.rs`, installer | Live verification |
| BRW-003 | Download capture | shipped | `ext/background.js` | e2e B phase |
| BRW-004 | Selected-links capture | shipped | `ext/capture.js` | e2e B phase |
| BRW-005 | Copy chip for prose/bundles/textarea | shipped | `ext/capture.js` | CSP-safe CSSOM light/dark chip with Lucide SVG icons & fixed primary hover state; e2e B phase |
| BRW-006 | Duplicate window (again/resume/keep/stale) | shipped | `ext/duplicate.html`, `ext/duplicate-model.js` | `duplicate-model.test.mjs` (`stale` bucket test), e2e B9/B10/B12 |
| BRW-007 | Options page | shipped | `ext/options.html`, `ext/options.js` | e2e B13, `plugins.test.mjs`, redacted diagnostics copy |
| BRW-008 | Hoster plugin registry | shipped | `ext/plugins.js` | `plugins.test.mjs` |
| BRW-009 | Media offer rows | partial | `ext/` | User-confirmable only in a real browser session |
| BRW-010 | DNR referer fallback | partial | `ext/` | May be unneeded with offer-first |
| BRW-011 | Named-pipe bridge | missing | `bridge.rs` | Bridge still speaks TCP |
| BRW-012 | Firefox build | missing | `ext/manifest.json` | Chromium MV3 only |
| BRW-013 | `activeTab` page reads | partial | `ext/` | User-granted only |

## GUI

| ID | Feature | Status | Evidence | Proof / gap |
|---|---|---|---|---|
| GUI-001 | Task list with live rows | shipped | `crates/ripdl-gui/src/main.rs` | e2e C phase |
| GUI-002 | Resizable/sortable columns | shipped | `crates/ripdl-gui/src/main.rs` | e2e C phase |
| GUI-003 | Multi-selection and bulk actions | shipped | `crates/ripdl-gui/src/main.rs` | e2e C phase |
| GUI-004 | Context menu | shipped | `crates/ripdl-gui/src/main.rs` | e2e C phase |
| GUI-005 | HTTP details dialog | shipped | `crates/ripdl-gui/src/views/details_dialog.rs` | e2e C phase |
| GUI-006 | Torrent details (files/peers/trackers) | shipped | `crates/ripdl-gui/src/views/details_dialog.rs` | e2e C phase |
| GUI-007 | Add URL dialog | shipped | `crates/ripdl-gui/src/views/add_dialog.rs` | e2e C phase |
| GUI-008 | Add torrent dialog | shipped | `crates/ripdl-gui/src/views/add_dialog.rs` | e2e C phase |
| GUI-009 | Add bundle dialog | shipped | `crates/ripdl-gui/src/views/add_dialog.rs` | e2e C phase |
| GUI-010 | Settings dialog (seven tabs) | shipped | `crates/ripdl-gui/src/views/settings_dialog.rs` | General, Speed & Limits, Bonding, Torrents, Browser, LAN Drop Box, Updates |
| GUI-011 | Tray icon and close-to-tray | shipped | `crates/ripdl-gui/src/tray.rs` | `tray-icon` + `hide_to_tray` + `INSTALLING` guard |
| GUI-012 | Single-instance hand-off | shipped | `crates/ripdl-gui/src/single_instance.rs` | e2e C1 (`show` over named pipe / unix socket) |
| GUI-013 | Drag-and-drop `.torrent` | shipped | `crates/ripdl-gui/src/main.rs` | `handle_shortcuts_and_drop` |
| GUI-014 | Keyboard shortcuts | shipped | `crates/ripdl-gui/src/main.rs` | Ctrl+N/T/B/F/A, Enter, Delete, Escape |
| GUI-015 | 60 s speed graph | shipped | `crates/ripdl-gui/src/rpc_worker.rs` | Fixed 1 s cadence sampling + sparkline |
| GUI-016 | Per-NIC status chips | partial | `crates/ripdl-gui/src/main.rs` | Live links displayed in status bar |
| GUI-017 | Update banner and in-app install | shipped | `crates/ripdl-gui/src/main.rs` | Dismissible banner + `INSTALLING` update guard |
| GUI-018 | About dialog | shipped | `crates/ripdl-gui/src/main.rs` | Help > About ripdl |
| GUI-019 | Send & Receive dialog | shipped | `crates/ripdl-gui/src/views/lan_dialog.rs` | API checks; not in the gate |
| GUI-020 | Open file/folder | shipped | `crates/ripdl-daemon/src/main.rs` | e2e C phase |
| GUI-021 | Per-task Save to | shipped | `crates/ripdl-gui/src/views/add_dialog.rs` | Native folder picker |
| GUI-022 | Categories and filters | shipped | `crates/ripdl-gui/src/main.rs` | Sidebar filters & live badge counts |
| GUI-023 | Search | shipped | `crates/ripdl-gui/src/main.rs` | Instant search filter (`Ctrl+F`) |
| GUI-024 | Window size persistence | shipped | `crates/ripdl-gui/src/main.rs` | `gui-geometry.json` + `gui-theme.txt` |
| GUI-025 | Accessibility (Narrator/NVDA) | partial | `crates/ripdl-gui` (`eframe` AccessKit) | Native UIA tree exported via AccessKit |
| GUI-026 | Native UI | shipped | `crates/ripdl-gui` | Promoted native `eframe`/`egui` shell replacing Tauri/WebView2; tray, single-instance, Lucide SVG vectors, Light/Dark palettes |
| GUI-027 | Total speed is the daemon total | shipped | `crates/ripdl-gui/src/rpc_worker.rs` | Uses `GetTrafficSnapshot` when available |
| GUI-028 | Paused/queued rows report no rate | shipped | `daemon/main.rs::task_info_of` | `queued_row_no_rate`, `paused_task_has_no_live_rate` |
| GUI-029 | No client-side rate math | partial | `crates/ripdl-gui/src/rpc_worker.rs` | Graph prefers `GetTrafficSnapshot` totals |
| UXD-001 | First-run onboarding (3 skippable screens) | shipped | `crates/ripdl-gui/src/views/onboarding_dialog.rs`, `ext/welcome.html` | `onboarding_never_hijacks_existing_v0_4_users`, `v0_4_10_config_deserializes_cleanly_without_new_fields`, e2e B13b |
| UXD-002 | Honest empty states with one CTA | missing | `UI-REHAUL.md` | Screenshot gate pending |
| UXD-003 | Settings form generated from `GetSchema` | missing | `SETTINGS.md` | Needs schema codegen first |
| UXD-004 | Default column set + optional advanced columns | partial | `ui/app.js` | Advanced columns exist; defaults need review |
| UXD-005 | Shared component/token system with state fixtures | missing | `UI-REHAUL.md` | Fixture harness pending |
| UXD-006 | Full screen rehaul | missing | `UI-REHAUL.md` | Screen inventory UXD-001..015 |
| UXD-007 | Screenshot QA loop with tree assertions | missing | `UI-QA.md` | Iterate per screen/state until green |
| UXD-008 | Themes/density/localization-ready layout | missing | `UI-REHAUL.md` | v0.7 |
| UXD-009 | Native UI parity with the same screens | missing | `UI-SPIKE.md` | Only if the spike wins |

## CLI

| ID | Feature | Status | Evidence | Proof / gap |
|---|---|---|---|---|
| CLI-001 | `download` (direct engine) | shipped | `cli/main.rs` | Manual + tests |
| CLI-002 | `info` (probe only) | shipped | `cli/main.rs` | Manual |
| CLI-003 | `add` | shipped | `cli/main.rs` | e2e A phase |
| CLI-004 | `bundle` | shipped | `cli/main.rs` | e2e A phase |
| CLI-005 | `ls` | shipped | `cli/main.rs` | e2e A phase |
| CLI-006 | `pause` / `resume` / `rm` | shipped | `cli/main.rs` | e2e A phase |
| CLI-007 | `details` | shipped | `cli/main.rs` | e2e A phase |
| CLI-008 | `refresh` | shipped | `cli/main.rs` | e2e A phase |
| CLI-009 | `add-parts` / `select` | shipped | `cli/main.rs` | Manual |
| CLI-010 | `config` | partial | `cli/main.rs` | `extension_dir` is parsed but `DaemonSettings::apply` ignores it |
| CLI-011 | `stop` | shipped | `cli/main.rs` | Manual |
| CLI-012 | `update` | shipped | `cli/main.rs` | Manual |
| CLI-013 | `lan` | shipped | `cli/main.rs` | e2e A phase |
| CLI-014 | `register-host` | shipped | `cli/main.rs` | Live verification |
| CLI-015 | `plugin` | shipped | `cli/main.rs` | `plugins.test.mjs` |
| CLI-016 | Direct `--bond` parity | broken | `cli/main.rs` | Builds a bond authority but never samples it |
| CLI-017 | Shell completion | missing | - | - |
| CLI-018 | JSON output mode | shipped | `cli/main.rs::Cmd::Traffic` | `ripdl traffic --json` prints the `GetTrafficSnapshot` payload |

## Config

| ID | Feature | Status | Evidence | Proof / gap |
|---|---|---|---|---|
| CFG-001 | `config.json` persistence | shipped | `rpc.rs` | Round-trip tests |
| CFG-002 | UTF-8 BOM tolerance | shipped | `rpc.rs::load` | Code path; no test |
| CFG-003 | Validation on load | broken | `rpc.rs::load` | Only `SetConfig` validates |
| CFG-004 | Live concurrency apply | partial | `daemon/main.rs` | Shrink races queued runners |
| CFG-005 | Live speed-limit apply | missing | `daemon/main.rs` | Explicitly next-task only |
| CFG-006 | `extension_dir` apply | broken | `rpc.rs::apply` | Field is missing from `apply` |
| CFG-007 | Defaults for absent fields | shipped | `rpc.rs` | Accessor tests |
| CFG-008 | Unknown fields ignored | shipped | serde | - |
| CFG-009 | Config schema version | missing | - | No migration path |
| CFG-010 | Per-key documented range | partial | `docs` | Scattered; not a single table |

## RPC and protocol

| ID | Feature | Status | Evidence | Proof / gap |
|---|---|---|---|---|
| RPC-001 | Named pipe on Windows | shipped | `transport.rs` | e2e A phase |
| RPC-002 | TCP loopback fallback | shipped | `transport.rs` | Bridge tests |
| RPC-003 | Unix socket on non-Windows | shipped | `transport.rs` | - |
| RPC-004 | Token authentication | shipped | `transport.rs` | e2e A phase |
| RPC-005 | Newline-delimited JSON | shipped | `rpc.rs` | Tests |
| RPC-006 | Typed error codes | partial | `rpc.rs` | Errors are display strings |
| RPC-007 | Protocol version negotiation | missing | `rpc.rs` | No version field |
| RPC-008 | Subscriptions/events | missing | `rpc.rs` | 2 Hz polling only |
| RPC-009 | Max message size | missing | `daemon/main.rs` | A local client can send an unbounded line |
| RPC-010 | Request idempotency | partial | `daemon/main.rs` | Add is idempotent; other commands are not |
| RPC-011 | Old-client compatibility | partial | `rpc.rs` | Additive serde defaults only |

## Update and packaging

| ID | Feature | Status | Evidence | Proof / gap |
|---|---|---|---|---|
| UPD-001 | GitHub release probe | shipped | `update.rs` | Mock-server tests |
| UPD-002 | Version comparison | shipped | `update.rs` | Unit tests |
| UPD-003 | `SHA256SUMS.txt` verification | shipped | `update.rs` | Unit tests |
| UPD-004 | Windows setup asset picker | shipped | `update.rs` | Unit tests |
| UPD-005 | Auto-install at boot | shipped | `daemon/main.rs` | Live verification |
| UPD-006 | Elevation handling | shipped | `daemon/main.rs` | Live verification |
| UPD-007 | Speed limit for update download | missing | `update.rs` | Uses the same uncapped client |
| UPD-008 | Cryptographic signature | missing | `update.rs` | SHA-256 only |
| UPD-009 | Check-result cache / backoff | missing | `update.rs` | Every GUI launch performs a fresh GitHub GET |
| PKG-001 | Windows Inno setup | shipped | `packaging/windows` | Release v0.2.0 |
| PKG-002 | Portable zip | shipped | `.github/workflows` | Release v0.2.0 |
| PKG-003 | Linux tar/deb/rpm | shipped | `packaging/linux` | Release v0.2.0 |
| PKG-004 | Extension zip | shipped | `.github/workflows` | Release v0.2.0 |
| PKG-005 | CI check + tests | shipped | `.github/workflows/ci.yml` | CI |
| PKG-006 | Manual e2e gate | shipped | `ext/test/e2e` | 101/101 last pass |

## LAN drop box

| ID | Feature | Status | Evidence | Proof / gap |
|---|---|---|---|---|
| LAN-001 | HTTP receiver | shipped | `lan.rs` | LAN tests |
| LAN-002 | Phone upload page | shipped | `lan.rs` | LAN tests |
| LAN-003 | Pairing code | shipped | `lan.rs` | `wrong_code_is_rejected...` |
| LAN-004 | Open/no-code mode | shipped | `lan.rs` | LAN tests |
| LAN-005 | Upload with exact Content-Length | shipped | `lan.rs` | LAN tests |
| LAN-006 | File listing | shipped | `lan.rs` | LAN tests |
| LAN-007 | Partial upload rows | shipped | `lan.rs` | LAN tests |
| LAN-008 | UDP discovery | shipped | `lan.rs` | e2e A phase |
| LAN-009 | Sender with pre-check | shipped | `lan.rs` | LAN tests |
| LAN-010 | Sends list/forget | shipped | `daemon/main.rs` | Manual |
| LAN-011 | New code | shipped | `daemon/main.rs` | Manual |
| LAN-012 | Same-name atomic claim | shipped | `lan.rs` | `parallel_same_name_uploads_stay_intact` |
| LAN-013 | Windows device-name prefix | shipped | `lan.rs` | LAN tests |
| LAN-014 | Missing Content-Length -> 411 | shipped | `lan.rs` | LAN tests |
| LAN-015 | Brute-force throttle | shipped | `lan.rs::check_auth` | `lan_code_throttle_blocks_brute_force` |
| LAN-016 | Code not in query/history | partial | `lan.rs` | Supports `X-Ripdl-Code` header + `?code=` query fallback |
| LAN-017 | Speed limit for sends | missing | `lan.rs` | Uncapped stream |
| LAN-018 | Upload size/disk quota | missing | `lan.rs` | No per-upload cap |

## Security, privacy, observability

| ID | Feature | Status | Evidence | Proof / gap |
|---|---|---|---|---|
| SEC-001 | No telemetry | shipped | workspace | Code review |
| SEC-002 | Update checksum gate | shipped | `update.rs` | Unit tests |
| SEC-003 | RPC token | shipped | `transport.rs` | e2e A phase |
| SEC-004 | Constant-time LAN code compare | shipped | `lan.rs` | Code path |
| SEC-005 | LAN brute-force resistance | shipped | `lan.rs::check_auth` | `lan_code_throttle_blocks_brute_force` |
| SEC-006 | Path sanitization | shipped | `names.rs` | Unit tests |
| SEC-007 | Header/secret redaction in diagnostics | partial | `docs` | No diagnostics bundle yet |
| OBS-001 | Structured logs | shipped | `tracing` | - |
| OBS-002 | Debug progress sample | shipped | `daemon/main.rs` | Used for live measurements |
| OBS-003 | Copy diagnostics button | partial | `ext/options.html`, `ext/background.js` | Redacted extension/host diagnostics JSON copy landed; desktop UI pending |
| OBS-004 | Metrics/health endpoint | missing | - | - |

## Native UI spike

| ID | Feature | Status | Evidence | Proof / gap |
|---|---|---|---|---|
| NUI-001 | Task list + status bar in a native toolkit | shipped | `crates/ripdl-gui` | Virtualized zero-clip partitioned table, Lucide v1.82.0 SVG path tessellator, WCAG focus rings, area sparkline & quick speed cap (`all_lucide_icons_parse_cleanly_within_bounds`, `table_columns_never_overflow_and_stay_non_overlapping`) |
| NUI-002 | Add URL + Settings | shipped | `crates/ripdl-gui/src/views` | Vector card modals (Add/Torrent/Bundle/Schedule, Onboarding, Details, Refresh, LAN, Duplicate prompt, 7-tab Options, About) over `Order::Background` scrim (`Order::Middle` modals stay full brightness) |
| NUI-003 | Tray + single instance | shipped | `crates/ripdl-gui/src/{tray,single_instance}.rs` | Tray icon + close-to-tray (`hide_to_tray`) + `INSTALLING` update guard + single-instance pipe/socket hand-off |
| NUI-004 | Memory/startup/accessibility report | shipped | `crates/ripdl-gui`, `ext/test/e2e` | AccessKit native UIA integration + Light/Dark screenshots & native E2E C0..C2 |
