# v2 architecture

This document defines ownership. If two modules can answer the same question,
the design is wrong.

## Principles

1. **One owner per concern.** Traffic, links, queue state, and protocol
   snapshots each have exactly one owner.
2. **Frontends render, never derive.** A frontend may format a number; it may
   not compute a rate, a total, or an ETA from raw counters.
3. **Durable-byte accounting.** A byte counts for progress and rate only when it
   is on disk, or when a rollback compensates it. Read-time counting is not
   allowed to leak into user-visible rates.
4. **Measured time.** Every rate uses a measured `dt`. No hardcoded tick
   duration, no implicit 5 s window.
5. **Monotonic-safe.** A counter that goes backwards resets the window; it never
   becomes a negative delta or a spike.
6. **Live policy.** Limits, concurrency, and link policy are handles, not
   constructor arguments copied at task start.
7. **Versioned protocol.** New fields are additive; old clients keep working;
   breaking changes get a version gate.
8. **Proof per invariant.** Every rule in `INVARIANTS.md` has a test name.

## Module map

```
ripdl-core
  traffic/            NEW  Meter, RateWindow, Limiter, TrafficController
  links/              NEW  LinkSet, discovery, reachability, weights
  engine/             consumer of traffic + links
  torrent.rs          consumer of traffic + links
  hunt.rs             consumer of traffic (yt-dlp --limit-rate)
  refresh.rs          unchanged
  bundle.rs           unchanged
  lan.rs              unchanged (optional traffic hook later)
  update.rs           unchanged (optional traffic hook later)
  rpc.rs              protocol v1 + v2 snapshots
  task.rs             persisted task/segment state
  names.rs            unchanged

ripdl-daemon
  queue/runtime       owner of task lifecycle and concurrency
  TrafficController   owner of global/task/host limits
  LinkSet             owner of NIC state and per-link rates
  config              validated policy, live-apply dispatch
  progress            renders TrafficController snapshots into TaskInfo

ripdl-cli             renders RPC snapshots
ripdl-gui             renders RPC snapshots
ext/                  captures and hands off; never computes rates
```

## Traffic ownership

`TrafficController` is created once by the daemon and shared with every task:

```
TrafficController
  global: Limiter                 // one cap for the whole daemon
  tasks:  Map<TaskId, TaskTraffic>
  hosts:  Map<HostKey, HostPolicy>
  links:  Arc<LinkSet>            // per-link meters live here

TaskTraffic
  meter:  Meter                   // durable bytes
  rate:   RateWindow              // task speed
  limit:  Option<u64>             // per-task cap
  parts:  Vec<PartTraffic>        // bundles share the task cap
```

Rules:

- HTTP segments, bundle parts, torrent peer reads, and yt-dlp progress all
  report to the same `TaskTraffic`.
- The global limiter is acquired **before** the read that consumes the bytes,
  or the read-ahead is bounded so the burst is explicit.
- A limit change updates the live limiter; no task restart.
- A task removal drops its `TaskTraffic`; a late reporter is ignored by
  generation id.

## Link ownership

`LinkSet` replaces `bond.rs`, `engine/nic.rs`, and torrent `BindMeter` as the
single source of per-link truth:

```
LinkSet
  links: Vec<Link>
  policy: BondPolicy { enabled, excludes }
  probe:  ReachabilityProbe

Link
  info:      LinkInfo { ip, name, default_route }
  meter:     Meter                 // durable bytes, both engines
  rate:      RateWindow            // aggregate display
  capacity:  RateWindow            // per-connection weight input
  in_use:    AtomicU32
  fails:     AtomicU32
  weight:    AtomicU32
```

Rules:

- HTTP `record()` and torrent `MeteredRead` feed the same `Link::meter`.
- The sampler uses measured `dt` from the last sample.
- A link that leaves discovery is removed from the set and its counters are
  dropped.
- Idle links decay to zero; they do not keep a stale rate.
- Reachability probes run only when bonding is enabled, or when the user asks
  for a diagnostics refresh.
- Weights use per-connection capacity, not aggregate share, so the weighting
  cannot reward itself for the connections it handed out.

## Queue ownership

The daemon remains the only owner of task lifecycle:

- one runner per task;
- `max_concurrent` is a semaphore over runners (bundle parts take slots, not
  the bundle row);
- pause/resume/remove semantics stay as they are;
- progress samples carry a `run_id`/generation; a sample from an older run is
  dropped.

## Protocol v2

The wire protocol stays newline-delimited JSON for v1 compatibility. v2 adds:

```
Request { id, protocol: 2, cmd }
Reply   { id, protocol: 2, reply }

Command::GetSnapshot { since: Option<u64> }
Command::Subscribe   { topics: [Tasks, Links, Traffic, Config] }
Command::SetLimits   { global_bps, tasks: Map<TaskId, Option<u64>> }
Reply::Snapshot      { seq, tasks: Vec<TransferSnapshot>, links: Vec<LinkSnapshot>, traffic: TrafficSnapshot }
```

`TransferSnapshot` is the only rate source:

```
TransferSnapshot {
  id, kind, state,
  downloaded, total, percent,
  down_bps, up_bps, eta_secs,
  limit_bps, throttled,
  links: Vec<LinkRate>,      // per-link share for this task
  error, stale_reason, ...
}
```

Frontends may keep a display history (for a graph), but they may not derive the
current rate from `downloaded` deltas.

## Migration seams

1. `EngineConfig.max_speed` becomes `Arc<Traffic>`; the old field is removed
   after all callers move.
2. `EngineConfig.bond_state` becomes `Arc<LinkSet>`; `bond.rs` is deleted after
   the torrent session uses the same set.
3. `TaskInfo.speed_bps` becomes a computed field from `TrafficController`; the
   daemon's `ProgressTrack` is deleted.
4. `Reply::Config.bond_links` and `torrent_rates` become one `links` array.
5. The GUI's `liveSpeed`/`speedText` become formatters over the snapshot.

## Failure rules

- A missing link is not a failed task; the OS route remains the fallback.
- A limit of zero means unlimited, normalized at the config boundary.
- A config value outside its documented range is rejected at load, not at use.
- A sidecar that fails to parse is moved aside and the download starts fresh;
  it never blocks the task forever.
- A progress sample for an unknown or removed task is dropped, not re-inserted.
