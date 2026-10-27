# Plan: living bond — NIC-aware downloads, restart-free

Follows the approved thesis (one bond brain shared by HTTP + torrents, no
restart ever, UI truth in the status bar). This version folds in the three
production findings from the first real dual-NIC run on v0.4.7.

## G. New production findings -> concrete work items

### G1. Lag spike whenever a NEW part starts (then normal)
Observed live 2026-10-27: each part start stalls the desktop for a few
seconds. Suspects, to be proven by the 1 s spike watcher (armed):
burst of 8 TLS connects at once, sparse-file grow + Defender WriteFile
interception x8, runner semaphore stampede, GUI polling burst on state
flip. Fix direction (choose by evidence):
- stagger segment spawns (~150-300 ms apart) and warm the first byte range
  before spawning the rest;
- open/extend the dest file once before spawning (already single fd - verify);
- GUI: batch task-list refreshes around start/stop events instead of a burst.

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

### G4. (added) work-stealing
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

- Slice 1: bond module + live pool + pick-per-attempt + G3
  instrumented fallback accounting.
- Slice 2: IP-change subscription + rebalancer + legacy re-range
  + G1 stagger + G2 progress fix.
- Slice 3: status-bar NIC chips (GUI, screenshot-verified).
- Slice 4: NIC settings cards + exclude list + docs/ROADMAP.

Debug daemon for live testing: private pipe + data dir, CDP-attached GUI,
screenshots through run-in-session harness only (elevated shell cannot
open WebView2 windows directly).
