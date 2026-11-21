# ripdl v2 - strangler plan

Status: **planning**. Branch: `next`. `main` stays the release line.

This is not a greenfield rewrite. v2 keeps the working engine, resume format,
torrent session, browser host, installer, and LAN drop box, and replaces the
parts that currently have more than one owner: traffic accounting, speed
limits, link/NIC state, progress snapshots, and the UI's rate math.

The goal is simple: **one source of truth for every number a user sees**, and
one owner for every cross-cutting rule. New features wait until that is true.

## Why a strangler and not a rewrite

The repository already has the right shape:

```
ripdl-core      engine, torrent, LAN, RPC, update, refresh, bundle
ripdl-daemon    queue, runtime, config, sessions, policy
ripdl-cli       thin client
ripdl-gui       Tauri shell + static UI
ext/            browser extension + native host
```

The defects are concentrated in cross-cutting concerns:

- the HTTP limiter is created per `Engine`, so the "total" limit is per task;
- torrent and yt-dlp traffic ignore the limit entirely;
- HTTP bonding and torrent bonding use two different meters, rate estimators,
  weight algorithms, and display paths;
- the daemon, engine, CLI, and GUI each have a different idea of "speed";
- config is a flat optional struct with no validation at load and no live-apply
  contract;
- tests prove single-task behavior, not global behavior.

A rewrite would recreate those seams and throw away the hard parts that already
work: crash-safe sidecars, sparse preallocation, torrent fastresume, browser
capture, signed updates, and the LAN receiver.

## What stays

These are frozen unless a v2 test exposes a correctness bug:

- `ripdl-core::engine` resume sidecar format, sparse preallocation, buffered
  writes, retry/watchdog plumbing, Content-Disposition naming;
- `ripdl-core::torrent` session lifecycle, fastresume, file selection,
  sequential mode, seeding watcher;
- `ripdl-core::refresh`, `bundle`, `lan`, `update`, `names`, `bridge`;
- `ripdl-daemon` queue persistence, task records, pause/resume/remove
  semantics, scheduler, stale-task parking;
- the extension, native-messaging host, installer, and updater;
- the public RPC commands (v1 adapters stay until every frontend moves).

## What is replaced

- `engine::limiter` -> `traffic::Limiter` with a global -> task -> host
  hierarchy and live reconfiguration.
- `bond` + `engine::nic` + torrent `BindMeter` -> one `links::LinkSet` with one
  meter, one rate estimator, and one weight model.
- daemon `ProgressTrack`, `bind_sample`, `bind_rates`, `bind_weights`,
  `bond_links` -> `TrafficController` and `LinkSet` snapshots.
- `TaskInfo` rate fields -> one `TransferSnapshot` that frontends render.
- GUI rate math (`liveSpeed`, `speedText`, graph sampling) -> render the
  daemon snapshot; the UI keeps only display history.
- `SetConfig` best-effort live apply -> explicit `TrafficController::set_limits`
  and `LinkSet::set_policy` calls.

## Issue register

`ISSUES.csv` is the single defect list. Severity:

- **S0** - data loss, corruption, or a security hole; stop and fix.
- **S1** - a core promise is broken (resume, global limit, safe add).
- **S2** - a headline feature is unreliable or misleading (per-NIC rates,
  quarantine, live config).
- **S3** - a real but bounded defect or a scalability/robustness gap.
- **S4** - polish, dead code, documentation drift.

Confidence is `confirmed` (read from code or reproduced) or `suspected` (needs a
repro before it can be scheduled as a fix).

## Branch model

```
main                  release line; critical fixes only
next                  v2 integration; protected
v2/traffic-core       new traffic module + tests
v2/link-core          new link module + tests
v2/daemon-integration daemon wiring behind `traffic-v2`
v2/rpc-snapshot       protocol v2 + v1 adapters
v2/frontends          CLI/GUI/extension render the snapshot
spike/native-ui       throwaway native UI prototype (never merged)
```

Rules:

1. `main` remains shippable. No v2 merge lands on `main` until the definition of
   done is green.
2. One subsystem per PR. Each PR includes a test that fails on the old code.
3. The old path stays behind a feature flag (`traffic-v2`) until parity.
4. No new user-facing features during the truth-layer work.
5. Docs are updated in the same PR as the code they describe.

## Milestones

### M0 - Inventory and contracts (this branch)

- `FEATURE-MATRIX.md`: every feature, status, evidence, and proof.
- `ISSUES.csv`: every known or suspected defect with severity and root cause.
- `INVARIANTS.md`: the numeric rules the system must never break.
- `ARCHITECTURE.md`: module ownership and data flow.
- `UI-SPIKE.md`: native UI scope, toolkit, and go/no-go metrics.

Exit: every feature marked `shipped` has a code path and a test; every headline
bug has a failing-test sketch.

### M1 - `traffic` core

Standalone module, no daemon changes:

- `Meter` (monotonic durable bytes per task and link);
- `RateWindow` (measured `dt`, gap reset, monotonic guard);
- `Limiter` (global -> task -> host token buckets, cancellation-aware);
- `TrafficController` (policy, live updates, snapshots).

Exit: unit/property tests for cap, burst, cancellation, and retry accounting.

### M2 - `links` core

Standalone module:

- one discovery + reachability pass;
- one per-link meter fed by HTTP and torrent;
- one weight model (capacity, not aggregate share);
- prune links and counters when an uplink disappears;
- no external probes when bonding is off.

Exit: per-link rates sum to the aggregate; no ghost links; CLI and daemon agree.

### M3 - daemon integration

Wire `TrafficController` and `LinkSet` behind `traffic-v2`:

- HTTP tasks, bundle parts, torrents, and video share the global cap;
- live limit changes apply within one tick;
- `TaskInfo` rate fields come from the controller;
- old path remains default until parity.

Exit: two HTTP tasks + one bundle + one torrent + one video under one cap;
pause latency < 300 ms at 10 KB/s.

### M4 - protocol v2

- `protocol_version` on the handshake;
- `GetSnapshot` / `Subscribe` (or a bounded event stream);
- one `TransferSnapshot` with task, link, limit, and throttle state;
- v1 command adapters for old clients.

Exit: old CLI/extension still work; new CLI/GUI render the same numbers.

### M5 - flip and delete

- `traffic-v2` on by default;
- delete the old rate/limit/bond paths;
- release `v0.5.0` ("truth") with no new features.

Exit: conformance suite, existing tests, node tests, and the manual e2e gate
all pass.

### M6 - native UI decision

Runs in parallel from M1, timeboxed:

- `spike/native-ui` with the task list, status bar, Add URL, Settings;
- read-only snapshot first, then pause/resume/remove;
- measure startup, RSS, CPU, binary size, DPI, keyboard, Narrator;
- keep Tauri unless the numbers clearly win.

Exit: `docs/v2/UI-SPIKE.md` updated with measurements and a go/no-go.

## Definition of done

A release candidate is v2-ready only when all of these hold:

- **Global cap**: two HTTP tasks, one bundle, one torrent, and one video stay
  under one configured cap; changing the cap applies to running tasks.
- **Honest speed**: no sample above 1.5x the measured cap after warm-up; no
  spike on retry/resume; a paused/queued row reports no live rate.
- **One link truth**: per-link rates sum to the task aggregate within 2%; a
  vanished adapter leaves no row; CLI, daemon, and GUI agree.
- **Byte correctness**: gzip responses hash-match; resume after kill -9 is
  byte-exact; a changed server file never resumes into corruption.
- **Config safety**: hand-edited `config.json` cannot panic or exhaust
  resources; every key has a documented range and a live-apply rule.
- **Protocol compatibility**: an older CLI/extension still works against the
  new daemon.
- **Proof**: every invariant has a test; the manual e2e gate passes.

## Non-goals for v2

- new protocols (FTP/SFTP/Metalink), proxy UI, site grabber, archive
  extraction, or a native UI committed before the spike;
- changing the resume sidecar format without a migration;
- replacing librqbit or the extension architecture.

Those are feature work. They wait for the truth layer.
