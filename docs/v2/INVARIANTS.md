# v2 invariants

Each invariant is a test that must fail on a broken implementation. Tolerances
are deliberately loose enough for CI timing jitter and tight enough to catch a
real leak.

Legend: **C** = confirmed defect today, **S** = suspected/untested today.

## Traffic

| ID | Invariant | Test | Today |
|---|---|---|---|
| TR-001 | The global cap is global: two HTTP tasks under one 1 MiB/s cap move at most 1 MiB/s together after a 2 s warm-up. | `global_cap_two_tasks` | C |
| TR-002 | A bundle's parts share the task cap; seven parts do not get seven caps. | `bundle_parts_share_task_cap` | C |
| TR-003 | A torrent's download traffic counts against the same global cap. | `torrent_respects_global_cap` | C |
| TR-004 | A video task's yt-dlp traffic counts against the same global cap (`--limit-rate` receives the remaining budget). | `video_respects_global_cap` | C |
| TR-005 | A limit change applies to running tasks within 2 s; no pause/resume is required. | `limit_change_applies_live` | C |
| TR-006 | A paused task reports no live rate, and its bytes do not count toward the live total. | `paused_task_has_no_live_rate` | C |
| TR-007 | A retry never counts the same byte twice: after a rollback and re-download, the rate window sees at most the bytes actually re-read. | `retry_does_not_double_count` | C |
| TR-008 | A resume never produces a rate spike: the first sample after adoption starts a fresh window. | `resume_has_no_adoption_spike` | S |
| TR-009 | A progress sample from an older run is dropped; it cannot re-insert a removed task or reset a new window. | `stale_progress_sample_is_ignored` | S |
| TR-010 | Pause during a 10 KB/s limit returns within 300 ms. | `limiter_cancel_latency` | C |
| TR-011 | A token bucket never exceeds its configured burst (default 250 ms of rate) after warm-up. | `limiter_burst_bound` | C |
| TR-012 | A limit of 0 in config means unlimited; a negative or NaN value is rejected. | `limit_zero_and_invalid` | S |

## Links

| ID | Invariant | Test | Today |
|---|---|---|---|
| LN-001 | Per-link rates sum to the task aggregate within 2% for HTTP and torrent traffic. | `link_rates_sum_to_task` | C |
| LN-002 | Per-link rates use measured `dt`; a delayed sampler tick does not inflate the rate. | `link_rate_measured_dt` | C |
| LN-003 | A link that leaves discovery disappears from the snapshot and its counters are dropped. | `departed_link_is_pruned` | C |
| LN-004 | An idle link's displayed rate decays to zero within 15 s. | `idle_link_decays` | S |
| LN-005 | A link with no connections does not keep a stale nonzero rate forever. | `idle_link_no_stale_rate` | S |
| LN-006 | A quarantined link gets a real chance to recover: the explore window lasts long enough for at least one dial, or the link is not re-quarantined from stale rates. | `quarantined_link_can_recover` | C |
| LN-007 | Weights use per-connection capacity, not aggregate share; more connections do not automatically mean more weight. | `weights_follow_capacity` | C |
| LN-008 | A vanished link's stale rate cannot set the weight of a live link. | `stale_rate_does_not_weight` | C |
| LN-009 | When bonding is off, the daemon performs no external reachability probes. | `no_probes_when_bonding_off` | C |
| LN-010 | A working uplink is not marked dead because a hardcoded probe target is blocked. | `probe_targets_are_configurable_or_fallback` | C |
| LN-011 | Virtual/tunnel adapters (including common VPN names) are never bonded. | `vpn_adapters_are_excluded` | S |
| LN-012 | CLI `--bond` uses the same link set, reachability, and weights as the daemon. | `cli_bond_matches_daemon` | C |
| LN-013 | Torrent `bind_totals` counters are bounded: a long-running session with peer churn does not grow without limit. | `bind_meter_is_pruned` | C |
| LN-014 | uTP traffic is either metered/bonded or explicitly documented as excluded; it never silently bypasses the per-link rate. | `utp_is_metered_or_excluded` | S |

## Engine

| ID | Invariant | Test | Today |
|---|---|---|---|
| EN-001 | A gzip-encoded response is downloaded as the exact bytes the server names: `Content-Length`, durable bytes, and SHA-256 agree. | `gzip_identity_and_hash` | S |
| EN-002 | Range requests are never combined with transparent content decoding, and the client does not rely on a third-party library quirk to achieve that. | `range_with_identity_encoding` | S |
| EN-003 | An existing file is never truncated by a new download with the same name; a free name is chosen or the user is asked. | `no_silent_overwrite` | C |
| EN-004 | Resume validates the probe's total against the sidecar's total; a mismatch starts fresh. | `resume_total_mismatch_starts_fresh` | C |
| EN-005 | Resume validates the on-disk file length against the sidecar offsets; a truncated file does not become a sparse hole. | `resume_file_length_validated` | C |
| EN-006 | A corrupt sidecar is moved aside and the download starts fresh; it never blocks forever. | `corrupt_sidecar_recovers` | C |
| EN-007 | A malformed sidecar cannot panic or index out of bounds. | `malformed_sidecar_is_safe` | C |
| EN-008 | `max_segments = 1` produces exactly one segment. | `max_segments_one` | C |
| EN-009 | `max_segments` has a hard upper bound (e.g. 64) even if config says 100000. | `max_segments_bound` | C |
| EN-010 | `min_segment_size = 0` is rejected at config load; the planner never divides by zero. | `min_segment_zero_rejected` | C |
| EN-011 | A short segment is retried, not counted as complete. | `short_segment_retries` | S |
| EN-012 | A stream stall is retried with a short leash, not the host-pressure long leash, unless the policy explicitly says otherwise. | `stalled_stream_policy_matches_comment` | C |

## Config and protocol

| ID | Invariant | Test | Today |
|---|---|---|---|
| CF-001 | Every `DaemonSettings` field can be set by `ripdl config` and survives a load/save round-trip. | `config_round_trip_all_fields` | C |
| CF-002 | Hand-edited `config.json` is validated at load; invalid values fall back to defaults with a warning. | `config_load_validation` | C |
| CF-003 | Live-apply rules are explicit: concurrency, limits, bonding, LAN, seeding, and update settings each say whether they apply now or next task. | `config_live_apply_matrix` | C |
| RP-001 | The daemon and clients negotiate a protocol version; an old client still works. | `protocol_v1_compat` | C |
| RP-002 | A request larger than the configured maximum is rejected without allocating it. | `rpc_max_message_size` | C |
| RP-003 | A progress sample for an unknown task id is dropped. | `unknown_progress_dropped` | C |
| RP-004 | A rate is never `NaN`, negative, or infinite on the wire. | `rate_is_finite` | S |

## UI

| ID | Invariant | Test | Today |
|---|---|---|---|
| UI-001 | The status-bar total equals the sum of the daemon's live task rates, not a client-side delta sum. | `total_matches_daemon` | C |
| UI-002 | A queued or paused row shows no live rate and does not inflate the total. | `queued_row_no_rate` | C |
| UI-003 | The speed graph samples on a fixed cadence, not on every render. | `graph_fixed_cadence` | C |
| UI-004 | The per-NIC chips show the same link set and rates as `ripdl config`, including torrent traffic. | `chips_match_config` | C |
| UI-005 | A 0 B/s rate renders as `0 B/s`, not as `-` (which means unknown). | `zero_rate_renders` | S |
| UI-006 | The Settings "Total speed limit" label is true: it caps all traffic, not one task. | `settings_limit_is_global` | C |

## Phase 0 follow-up

| ID | Invariant | Test | Today |
|---|---|---|---|
| EN-013 | Refresh never adopts kept bytes when the ETag changed; a warning is not sufficient. | `refresh_etag_mismatch_rejected` | C |
| EN-014 | Refresh validates every segment's bounds and `kept_bytes <= total` before retargeting. | `refresh_validates_sidecar_bounds` | C |
| EN-015 | A refresh sidecar keeps the Windows hidden attribute. | `refresh_sidecar_stays_hidden` | C |
| RP-005 | A bridge RPC timeout does not leave a thread blocked on the dead stream. | `bridge_timeout_no_thread_leak` | C |
| RP-006 | The bridge retries only connect/timeout failures, not auth or protocol errors. | `bridge_no_retry_on_auth_failure` | C |
| CFG-011 | `max_concurrent` from a hand-edited config is clamped to a documented maximum. | `max_concurrent_bound` | C |
| CFG-012 | Non-finite `seed_ratio` and short LAN codes are rejected at load. | `config_load_validation` | S |
