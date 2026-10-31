# Plan: living bond — NIC-aware downloads, restart-free

Follows the approved thesis (one bond brain shared by HTTP + torrents, no
restart ever, UI truth in the status bar). This version folds in the three
production findings from the first real dual-NIC run on v0.4.7.

## G. New production findings -> concrete work items

### G1. Lag spike whenever a NEW part starts (then normal) - CODE CLEAN, kernel-side suspect
Observed live 2026-10-27: each part start stalls the desktop for a few
seconds (operator: "so bad even mouse stuck", 2026-10-31, v0.4.8). The
400 ms dial stagger did not remove it. Audited end to end 2026-10-31
(research/lag/): probe/plan, single prealloc `set_len` before spawn,
stagger, PICK_WAIT (200 ms poll), gate, per-attempt file handle reused
across flushes, 2 s sidecar cadence, 15 s queue persist, GUI poll
(500 ms single-flight, structural-key render) - all bounded. Looped
stress through a private daemon: 4 GB pulled (up to ~1 GB/s), daemon
burned 1.52 s CPU TOTAL, Defender 0 CPU at ~900 MB/s of fresh-file
writes, per-core DPC <= 6%. No user-mode spin exists to fix. Prime
suspect: the USB RNDIS tether driver - DPC/interrupt storm at the ~5x
throughput bonding now drives through it (Wi-Fi-only era was ~12 MB/s,
under the pain threshold); loopback cannot reproduce (no NIC in path).
Confirm with: does bond-OFF (Wi-Fi-only) still lag? If yes -> watcher
repro (research/lag/watch-real.ps1, per-core DPC max) with the operator
starting one download. Mitigations if confirmed: OS-side (driver
update, USB mode, power saving) and a ripdl-side per-link speed cap
(backlog "per-task quota polish") to keep the tether under the
stutter threshold.

### G2. Bundle/HTTP progress bar does not move until a part completes
Row % must derive from summed durable bytes / summed total of every part
with a known total, updated every tick - not from part-completion events.
Also: parts always had `total` after v0.4.7's probe; verify the GUI bundle
row actually uses per-part sums (both `downloaded` and bar width), and that
the daemon aggregate does not report `total: null` when所有 parts know it.

### G3. (re-diagnosed with the operator) static weld, not fallback
Truth on the machine: Wi-Fi ISP caps at 100 Mbps; the tether ("Ethernet 2")
does 1-500 Mbps per cell conditions, currently 300+ Mbps. Nothing failed:
the segments indexed onto the tether finished "as soon as assigned", then
Wi-Fi (12 MB/s+) ground through its half alone - static partitions cannot
give the fast link more work. Fix: work-stealing by cell splitting (C13)
replaces land-grab partitioning; fallback accounting stays as an
instrumentation item so a genuinely dead link is still observable.

### G4. (added) work-stealing - LANDED 2026-10-27, test `freed_worker_steals_from_the_largest_straggler`
A finished segment worker does not retire while any cell still has
remaining bytes: it splits the slowest-remaining cell's tail into a new
cell and claims it (cell = sidecar-iq piece, offsets durable). Every new
cell is picked by the weighted picker - fast links keep stealing work,
the board empties at max-flow. Sidecar format already stores an arbitrary
cells array, so runtime splits survive pause/resume.

## A. One bond brain (`ripdl_core::bond`, new) - LANDED 2026-10-27, 12/12 unit tests

1. `BondState` engine-global `Arc<RwLock>`: links `{ip, name,
   default_route, weight, rate EMA, consecutive_fails, quarantine, epoch}`.
2. Shared picker: smooth weighted RR lifted out of the vendored delta;
   preferences only (1..10, self-measured), no speed-based bans - only dead
   dials skip (2 consecutive), re-probed each 60 s window; unit-tested 12/12.
3. Unified metering: the daemon's 5 s sampler counts HTTP + torrent bytes
   per link (deltas -> EMA -> weights); no probe traffic.
4. `Reply::Config.bond_links` (replace/extend `torrent_rates`); serde
   defaults keep old frontends working.

## B. Live NIC truth - restart becomes impossible

5. IP/interface change subscription (NotifyIpInterfaceChange on Windows,
   netlink on Linux); 5 s scan + reachability handshake as fallback
   (existing `scan_uplinks`, 2-fail drop, 1-success re-adopt).
6. NIC events bump the epoch instantly: HTTP client pool is a lazy
   per-addr cache (arrivals on demand, departures weight-0 + removed);
   stale binds fail fast and the attempt re-picks. Torrent `set_bind_ips`
   updated the same instant, dead addresses removed.
7. Every restart path dies: bonding toggle (already live), plug/unplug,
   IP renewal, daemon-before-DHCP. The Settings "Restart background
   service" button stays only for general maintenance.

## C. HTTP engine - segments follow the best link
### (re-derived after parallel research - IDM/aria2 dynamics, JSQ-heterogeneity
### theory, Windows network/I-O engineering; findings drive every choice)

8. Warm-connection workers: a worker keeps its link + pooled HTTP client
   while healthy - the next chunk's Range request rides the SAME TCP/TLS
   connection (cwnd retained, no handshake, no slow-start restart; Windows
   does not decay idle cwnd). Link re-picked only after a failed dial or a
   closed pool connection. Research evidence: aria2's default piece selector
   exists because "establishing a connection is an expensive operation";
   IDM advertises "full reuse of connections ... without additional connect".
9. Ramp-in: concurrent NEW handshakes capped (small, ~2), extra workers join
   over the first seconds (IDM ramps exactly this way) - kills the G1
   part-start burst instead of 8 simultaneous TLS connects.
10. Steal semantic (IDM/XDM-proven): a freed worker takes the LARGEST
    remaining cell (tail-split when one monopolist remains); its claim is
    sized by its own measured rate x 2 s (bounded 256 KiB..64 MiB, never
    below min_segment). Fairness needs no thresholds: everyone stays busy
    until the board empties (largest-first is LPT-greedy).
11. Maximal spread: 8 cells whenever ranges are proven; an idle link gains
    work only through stealing (no arcana about who "should" hold what).
12. Stalled/stranded segment: aria2-style lowest-speed check - cancel +
    rerun from the durable `done` offset on a healthy link; <=1
    migration/segment/10 s; respects the per-IP gate.
13. Legacy upgrade: resuming an UNBOUNDED sidecar re-ranges from the
    durable offset once a fresh probe proves ranges (no delete/re-add).
14. Windows I-O softening (research): never touch socket buffer options
    (kills autotuning), spawn_blocking disk writes, write batches >= 1 MiB,
    retry renames when AV holds the .part, optional one-click Defender
    folder-exclusion helper in Settings (biggest cheap win on Windows).

## C9b. Disk-toll budget (every download, watching disk AND cpu)

The disk pays three ways: write-ENQUEUE events (AV minifilter intercepts
each, so cost scales with event count, not bytes; NVMe seek is a non-issue
with write-back caching), file-extend/metadata churn, and sidecar/queue
state churn. Rules, in priority order:
- One write batch per segment per flush: raise the 256 KiB segment buffer
  to 1 MiB (INTERRUPTION cost is bounded: unflushed-at-kill re-downloads
  are only that batch). Batch = fewer Defender inspections, fewer syscalls,
  no seek penalty on NVMe.
- Sidecar state carves: a task must persist its `.ripdl.json` at most
  ~2s cadence in steady state (it is an atomic rename + rescan), always
  immediately on pause/complete (every state transition flushes first).
  Progress byte counters stay in-memory atomics between flushes.
- No fsync in the hot path: OS write-back cache owns crash durability;
  the durable/rolling-back offset accounting already tolerates losing the
  tail batch.
- Extend the file once to its full planned size before segment writes
  begin (SetFileLength-style) when total is known: avoids per-write
  grow/metadata events and keeps allocation contiguous on NTFS.
- One hash pass on completion is accepted (reads sequential); nothing
  re-downloads to verify.
- Verify by measurement, not vibes: before/after physical-disk queue
  depth, write IOPS, and Defender CPU during a live 2 GB run.

## D. Scenario matrix (each = an offline test)

| #  | Scenario                                  | Mechanism |
|----|-------------------------------------------|-----------|
| 1  | links healthy, different speeds           | weights |
| 2  | tether loses data, link stays up          | reachability scan -> quarantine -> migrate |
| 3  | adapter unplug                            | NIC event -> pool drop -> instant repick |
| 4  | adapter appears mid-task                  | epoch bump -> next attempts pick it |
| 5  | Wi-Fi roams / IP renews                   | stale bind fails fast -> repick |
| 6  | single NIC                                | plain mode |
| 7  | no NIC at all                             | segments parked-deferred, auto-resume |
| 8  | daemon starts before DHCP                 | plain -> bonds on arrival |
| 9  | bonding toggled live                      | pool switch, no restart |
| 10 | restart mid-download                      | sidecar offsets, EMA converges fast |
| 11 | host 429 pressure                         | gate per-IP unchanged, rebalancer yields |
| 12 | part-start burst (G1)                     | staggered spawn + warm connect |
| 13 | fast link finishes early, slow link grinds | cell-split work-stealing (G4) |
| 14 | bound link fails mid-flight               | fail-count skip, fallback bytes uncredited |

## E. UI (status bar + settings), verify by screenshot through the harness

14. Status bar: per-uplink chips `name rate spark state`
    (Active teal / Bonded-idle grey / Quarantined amber / Off-bond),
    word + tooltip not color-only; tooltip: IP, now/avg, in-use count
    (HTTP segs + torrent conns), last NIC event; click -> Settings >
    Network, row highlighted.
15. Settings NIC cards: state dot, live rate, in-use badge, per-NIC
    include/exclude (`bond_exclude`); one status sentence; CLI parity.

## F. Delivery slices

- Slice 1 - LANDED 2026-10-27 (commit 1910e2b): `bond` module +
  engine Dialer (per-attempt smooth-RR pick, warm per-address client
  pools, dial note_ok/note_fail + per-chunk byte metering, shared
  daemon authority via `EngineConfig.bond_state`) + G1 ramp-in
  stagger (400 ms between first dials) + rate-adaptive write batches
  (250 ms target, 256 KiB..4 MiB) + completed-cell skip at resume
  (degenerate out-of-order Range when serial retries finished cells
  before the pause). 16/16 engine integration + 120/120 lib tests.
  G3 fallback accounting becomes skip-list behavior (2 dead dials;
  note_ok restores instantly) - quiesced as instrumentation.
- Slice 2 - LANDED 2026-10-27 (commit c0ec4f6): watcher feeds the HTTP
  bond (`set_links` from proved uplinks + `sample` every 5 s, beside
  the torrent path); `Reply::Config.bond_links`; `ripdl config`
  prints `http uplinks:` and the GUI appends `downloads: ...` to the
  bonding line every engine picks from this one instance.
  G2 LANDED: bundle runner probes every never-run part's total
  upfront (concurrently; `Attempted` marker keeps it off already-run
  parts so a resumed failed part is not double-probed) - the bar
  unblocks after one probe wave. Legacy UNBOUNDED sidecar re-range
  LANDED (carried prefix -> first cell, ranged re-plan; test covers
  mid-cell resume + multi-cell + byte-exact).
- Slice 2 remainder - work-stealing (G4) LANDED 2026-10-27: a freed
  worker splits the largest remaining cell and claims its tail
  (`chunks_for` sizes the claim by its link's own measured rate when
  one exists - about 2 s of throughput, 256 KiB..64 MiB; rate-less
  workers take the plain half, IDM's split-in-two) instead of
  retiring; a resumed worker that finds its own cell already durable
  steals too. Live donors are cut-safe by protocol: the carve happens
  under the board lock, the donor's join credits only up to the cell's
  (possibly shrunk) current span and gives the surplus back to the
  headline counter, then stops streaming - the claim worker refetches
  the tail, every byte is counted exactly once, and the resumed-slow
  part is carried by whichever link is free. Test:
  `freed_worker_steals_from_the_largest_straggler` (robbery = a ranged
  GET judged *inside* a planned cell) plus clamped-donor coverage in
  the pause/resume and etag suites. Still open from this slice:
  stalled-segment cancel + rerun from durable done (aria2
  `lowest-speed-limit`-style, deferred - the steal itself already
  rescues a stalled uplink's cell once any other worker frees).
- Slice 3 - LANDED 2026-10-27 (e2e C4, screenshots c4-gui-uplinks /
  c4-gui-settings-network): status-bar uplink chips `name rate spark
  state` on a 2 s sampler (`bond_links` GUI command over
  `Reply::Config.bond_links`; hidden while bonding is off). State is a
  word, never color-only: active (teal) / idle (grey) / flaky (amber, a
  dial failed and a re-probe is due) / dead (red, quarantined).
  Tooltip: name+IP+default route, now/avg from the 80 s window,
  connections + dial fails, weight. Click opens Settings > Network
  with the bond row flashed. e2e gate grew the C4 checks (3): chips
  appear after the private daemon's bonding toggle turns on late in the
  run (early phases' loopback engine flows untouched), state word +
  tooltip shape, chip click -> Network panel.
- Slice 4 - LANDED 2026-10-27 (e2e C4, screenshot c4-gui-settings-network):
  `bond_exclude` end to end + NIC settings cards. Core: config key
  `bond_exclude` (case-insensitive exact adapter names; empty clears),
  accessor `bond_excludes()`. Daemon: `bond_allowed()` filters both the
  HTTP bond's link set (`feed_http_bond`) and the torrent bind list
  (`uplink_addrs`); unit test for the match rules. CLI:
  `ripdl config bond_exclude=A,B` (empty value clears) and the
  `uplinks:` line marks `(excluded)`. GUI: Settings > Network renders one
  card per detected uplink - checkbox (uncheck = excluded, row dims +
  strike-through) and a live badge (rate - connections - state word)
  refreshed every 2 s while the Network tab shows; Save writes the
  draft. Bonus fix: the Settings save now merges edited fields over the
  snapshot taken at open, so the full-replace protocol can no longer
  reset keys the dialog never showed (lan_*, extension_dir, the pairing
  code) - previously every GUI save silently wiped the LAN drop box
  config. e2e C4 grew the card-save check: uncheck first card, save,
  CLI read-back shows bond_exclude AND default_download_dir still
  pointing at the run folder (14/14 GUI checks pass).
- Slice 5 (live) - VERIFIED 2026-10-27, `research/bond-live/watch.mjs`
  (private daemon, 1 s RPC sampling, log in `research/bond-live/log-*.jsonl`):
  Hetzner 100 MB over Ethernet 2 (tether, default route) + Wi-Fi
  completed in 33 s. Ramp 0 -> 0.4 -> 30.4 MiB/s (no startup-spike
  artifact; first bytes ~6 s in - the pick-wait covers the uplink scan).
  Both links metered and steered: peaks Ethernet 2 5.0 MiB/s w6,
  Wi-Fi 8.6 MiB/s w10 - the faster link earned the top weight. Two
  defects the live run exposed, both fixed:
  - the bond credit (`in_use`) only bracketed the transport *send*, so
    a link streamed for minutes while its chip read `x0`/idle - credit
    now lives in a Drop guard from dial to stream end;
  - tasks starting inside the uplink-scan window stranded every first
    dial on the OS route for the whole (minutes-long) cell - `Dialer::
    attempt` now waits up to 3 s for a published link set.
  Known follow-up: the tail (last ~2%) drained as a single slow stream
  for ~18 s; work-stealing only helps when other workers free up first.
  Then tag + release notes.

- Churn (live, manual) - 2026-10-27, `research/bond-live/churn.mjs`
  (cues the user to unplug/replug the tether mid-download; adapter
  toggling from the agent shell is denied - the shell runs a filtered
  UAC token, Administrators deny-only). Two phases:
  - Pre-watchdog runs froze permanently at a partial byte count
    (46-65 MiB) with a live TCP connection whose source address died
    with the adapter: alive-but-silent, `await read` parked the worker
    forever (3 reproductions). Fixed by the stream-idle watchdog
    (`stream_idle`, 30 s, `Error::StreamStalled` retryable + host-
    pressure; offline regression `stalled_connection_recovers_via_watchdog`
    wedges 8 mock connections mid-body with the socket left open).
    Note: loopback Wi-Fi-bound dials always fail, so local-server churn
    is single-link; the honest churn pull is a real URL. Also: the local
    "throttled" mock does not actually throttle (res.write without
    backpressure drains into Node's buffer), so local-run rate numbers
    are buffer bursts, not link truth - do not calibrate on them.
  - With the watchdog (run 06b9d6, 100 MB proof.ovh pull, cable cue):
    eviction on unplug at t=42, handoff to Wi-Fi kept progressing
    (83.6 -> 88.6 MB), replug re-adopted in ~25 s (t=67). The run still
    did not finish by the cap, but the two suspected defects were partly
    confounded: proof.ovh.net is an OVH per-IP-quota host (429s to
    ranged GETs after ~37 MiB, the v0.3.9 degradation path), which both
    slowed the tail and polluted the per-link fail counters.
  - Fixed in `9c1c97e` - both defects, root-caused by the retest:
    1. pick starvation / wrong-crown: weights derived from each link's
       AGGREGATE metered rate, which is proportional to the connection
       share the weights themselves steered there (rich-get-richer; the
       Hetzner run crowned Wi-Fi w10 while the user's ground truth says
       the tether is fastest). Weights now follow a per-connection
       capacity EMA (metered bytes / connection-seconds accumulated by
       the dial guard): a fast link with one connection still measures
       fast, a wedged connection craters its link's capacity, and claim
       sizing uses the same per-conn figure. `rate_bps` stays the
       aggregate the chip displays; `capacity_bps` is new on
       `LinkStatus` (older frontends ignore it).
    2. StreamStalled no longer counts as host pressure - a stalled path
       must not take the long leash and halve the in-flight gate exactly
       when recovery needs to fan out over the surviving links. It now
       strikes the link's fail streak instead (note_fail on stall).
  - Retest (run f9e212, full Ubuntu 26.04 live-server ISO, 2783 MB,
    releases.ubuntu.com via Fastly, both uplinks, no cap): completed in
    ~100 s, average ~28 MiB/s, peak total 66.1 MiB/s. Ethernet 2
    (tether) measured 13.4-37.0 MiB/s at w10 with 2-4 connections;
    Wi-Fi 7.2-9.8 MiB/s at w1-w3 with 4-6 - the weighting now ranks the
    links the way the user's ground truth says they perform, and the
    split followed it (early: both x4 with Eth2 w10/w4; late: Eth2 x2
    carrying 2-3x Wi-Fi's rate). 18 bond unit tests + 24 engine
    integration tests green offline.
  Offline edge tests added (`engine_integration.rs`): credit drains to
  zero after cancel, no-range single stream meters through the bond,
  IPv6 loopback bind (fixed follow-up from the earlier gap list - a
  real non-loopback v6 uplink remains live-only), speed cap paces bonded
  traffic, two tasks share one bonded engine.

Debug daemon for live testing: private pipe + data dir, CDP-attached GUI,
screenshots through run-in-session harness only (elevated shell cannot
open WebView2 windows directly).
