# v2 scalability

Scalability here has three meanings. All three are design constraints, not
future optimizations.

1. **Architecture** - adding a feature must not require editing five modules.
2. **Load** - the daemon and every frontend stay responsive at the targets
   below.
3. **Process** - the feature matrix, invariants, and CI make drift visible.

## Load targets

| Dimension | Target | Why |
|---|---|---|
| HTTP tasks | 200 queued/running | Power users keep a backlog |
| Bundle parts | 100 in one bundle | Release sets |
| Torrents | 50 managed/seeding | A few active, many seeding |
| Browser offer links | 100 (already capped in `background.js`) | Copy a release page |
| Uplinks/links | 8 | Realistic NIC count |
| UI poll | 2 Hz for GUI, 1 Hz for extension | Existing GUI cadence |
| Snapshot payload | < 1 MiB at 200 tasks | Chrome native-messaging host cap |
| RPC reply latency | < 50 ms for a delta snapshot | Same local pipe/TCP |
| Pause latency | < 300 ms at 10 KB/s limiter | Current limiter loops can block |

<!-- More targets to be added as the conformance suite measures them. -->

## Architecture rules

- `ripdl-core` owns mechanics; `ripdl-daemon` owns policy; frontends own
  rendering. A frontend never computes a rate, total, ETA, or link share.
- One owner per concern: `TrafficController` for bytes/limits, `LinkSet` for
  NIC state, the daemon queue for task lifecycle, RPC for snapshots.
- New behavior enters through a capability field or a versioned command, not
  by overloading an existing message with a surprise meaning.
- A feature that needs a new cross-cutting rule must add an invariant and a
  test in the same PR.
- No module reaches into another module's internals to "just read a number".
  Use a snapshot handle.

## Protocol sizing

v1 `List` returns the full task array on every poll. At 200 tasks with URLs,
paths, and detail fields that is large enough to make the native host's 1 MiB
reply cap a real boundary. v2 fixes this by construction:

```
GetSnapshot { since: Option<u64> }
Snapshot {
  seq: u64,
  full: bool,
  tasks: Vec<TransferSnapshot>,     // changed or full set
  removed: Vec<TaskId>,
  links: Vec<LinkSnapshot>,         // changed or full set
  traffic: TrafficSnapshot,
  config_version: u64,
}
```

- `since = None` returns a full snapshot (used on frontend startup and after a
  service-worker restart).
- `since = seq` returns only entities changed since that sequence, plus
  removals.
- If a delta would exceed a size budget, the daemon returns a full snapshot
  with a `full: true` marker rather than a truncated one.
- The daemon keeps a bounded ring of recent snapshots so a slightly stale
  client can re-sync without a full rebuild.

A small `Ping` command replaces the extension's current `list`-as-liveness
probe, so a host probe cannot ship 200 tasks across the native-messaging pipe.

## Generated protocol types

Hand-written TypeScript types are the main source of frontend drift. Add a
codegen step:

```
xtask codegen-protocol
  -> ext/src/protocol.d.ts
  -> crates/ripdl-gui/ui/protocol.d.ts
```

- Source of truth: `ripdl-core::rpc` types.
- CI runs codegen and fails if the checked-in files differ.
- Both the Tauri UI and the extension consume the generated file.
- A protocol field rename is then a compiler error in both frontends, not a
  runtime `undefined`.

## Scaling inside the daemon

| Concern | Current risk | v2 rule |
|---|---|---|
| Traffic accounting | per-engine limiters | one `TrafficController`, O(1) per chunk |
| Rate windows | duplicated in daemon/bond/torrent | one `RateWindow` implementation |
| Link sampler | hardcoded 5 s, two systems | one `LinkSet`, measured dt, O(links) |
| Progress map | late reporters re-insert removed tasks | run-id + tombstone; drop stale samples |
| Proposal map | TTL prune exists | keep; add a hard cap on live proposals |
| Torrent reconcile | walks every paused/completed torrent every 15 s | separate budgeted task, not the bond watcher |
| Bind meter | one counter per peer, never pruned | prune closed connections |
| Task list | full array at 2 Hz | snapshot/delta, change-driven render |
| Boot restore | re-adds all torrents at once | bounded concurrency and a progress state |
| Config | validated only in `SetConfig` | validate at load and on change |

## Frontend scaling

- GUI: render on snapshot change, not on a timer that forces a full DOM diff.
  Keep only display history (graph points, sparklines) in the frontend.
- Extension: keep a local task store in the MV3 service worker; rehydrate with
  `GetSnapshot { since: None }` after a worker restart. Never hand-maintain a
  task list by polling `list`.
- CLI: a `--json` mode reads the same snapshot, which also gives us a stable
  automation surface without a separate parser.
- Native spike: render `TransferSnapshot`; the AccessKit/inspection harness
  asserts on the same data the GUI sees.

## CI gates to add

- `snapshot_200_tasks_under_1mib` - serialization size test.
- `delta_snapshot_only_changed` - correctness test for `since`.
- `generated_types_current` - codegen drift check.
- `traffic_oversubscription` - 200 tasks do not exceed the global cap.
- `progress_map_bounded` - add/remove churn does not grow the map.
- `bind_meter_bounded` - peer churn does not grow the counter map.
- `bridge_reply_cap` - a 200-task reply is either under 1 MiB or cleanly
  paginated, never a corrupt frame.

## Feature growth

Every new capability gets an ID in `FEATURE-MATRIX.md` before code:

```
<AREA>-<NNN> | Feature | Status: missing | Owner module | Target wave | Test
```

A feature cannot be marked `shipped` until its invariant test exists. This is
how we avoid repeating the "docs say global limit, code says per task" drift.
