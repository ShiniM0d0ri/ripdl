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

## Readiness check (2026-11-21)

Branch `next` is clean; the Phase 0 documents are committed. Nothing has been
changed in the engine or daemon yet.

### Verified from source, not assumed

- `EngineConfig.max_speed` is copied into a new `Limiter` in `Engine::new`, and
  the daemon builds one engine per task and per bundle part. The "total" limit
  is genuinely per engine.
- The torrent session leaves `SessionOptions::ratelimits` at its default, and
  `hunt.rs` never passes `--limit-rate`. Both bypass the setting.
- `DaemonSettings::apply` has no `extension_dir` branch.
- `plan_fresh` forces two segments when `max_segments = 1` and divides by
  `min_segment_size` without a zero check.
- `download_named` opens a new destination with `truncate(true)` with no
  free-name check. A different URL with the same filename overwrites the file.
- `feed_http_bond` passes a hardcoded 5 s `dt` to `Bond::sample`, while
  `refresh_uplinks` can take up to 1.2 s before the sampler runs.
- `bind_rates` and the librqbit `BindMeter` counter map are both append-only.
- The daemon calls `refresh_uplinks` every 5 s regardless of `bond_interfaces`,
  so external reachability probes happen with bonding off.
- `Cargo.lock` shows no `h2` dependency for `reqwest 0.12.28`, confirming the
  HTTP client is HTTP/1.1-only.

### Corrected during analysis

- The gzip issue was initially filed S0. reqwest 0.12 only adds
  `Accept-Encoding: gzip` when the request **has no `Range` header**, so ranged
  segment GETs are incidentally protected. The remaining risk is HEAD probes
  and non-compliant servers. It is now **S2** in `ISSUES.csv`. The fix is still
  `no_gzip()` plus `Accept-Encoding: identity`; we just should not claim a
  universal corruption bug.

### Decisions needed before M1

1. Does the global cap include torrents and video from day one, or do we ship an
   HTTP-only v2 cap first? The plan assumes all traffic.
2. Per-task torrent limits: librqbit exposes session-wide `LimitsConfig`; a
   per-task cap would need another vendor delta. Accept session-wide for v2?
3. Native toolkit for the spike: egui is recommended because eframe has an
   `inspection` TCP port that exposes the AccessKit tree, injects input, and
   captures screenshots - the closest analogue to CDP we found.
4. How long do v1 RPC adapters remain? The plan says until CLI, GUI, and
   extension all migrate.
5. Is the `name (1)` overwrite fix an S0 hotfix on `main`, or does it land in
   v2 M4? Data loss argues for a `main` hotfix.
6. Is LAN brute-force throttling in v2 scope, or a separate security pass?

### Risks

- Two rate systems live at once during `traffic-v2`; the flag must be
  bulletproof and the old path must remain the default until parity.
- The native UI needs a new test harness; CDP checks do not transfer.
- Config validation can change behavior for users with hand-edited files;
  every rejection needs a warning and a documented fallback.
- The event/snapshot protocol can bloat at 2 Hz with 200 tasks; measure before
  committing to a wire format.

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
- `SCALABILITY.md`: load targets, protocol sizing, generated types, CI gates.
- `EXTENSION-PLAN.md`: the extension as a first-class v2 frontend.
- `FEATURE-ROADMAP.md`: which feature IDs land in v0.5, v0.6, and v0.7.

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

### M3 - daemon integration + protocol v2

Wire `TrafficController` and `LinkSet` behind `traffic-v2`, and define the
wire format at the same time so frontends do not build against a shape that
cannot scale:

- HTTP tasks, bundle parts, torrents, and video share the global cap;
- live limit changes apply within one tick;
- `GetSnapshot { since }` with full/delta snapshots, sized for the extension's
  1 MiB reply cap;
- one `TransferSnapshot` with task, link, limit, and throttle state;
- `protocol_version` in the handshake; v1 command adapters for old clients;
- generated `protocol.d.ts` for GUI and extension, checked in CI.

Exit: two HTTP tasks + one bundle + one torrent + one video under one cap;
pause latency < 300 ms at 10 KB/s; a 200-task snapshot stays under 1 MiB.

### M4 - frontends

Split by surface so one frontend cannot block another:

- **M4a CLI**: render the snapshot; add `--json` for automation; keep old
  commands working through v1 adapters where needed.
- **M4b Tauri GUI**: remove `liveSpeed`/`speedText` math; render snapshots;
  keep only graph history in the frontend.
- **M4c Extension**: execute `EXTENSION-PLAN.md` E1-E4 - pipe verification,
  version handshake, ping, delta store, generated types, rehydration.
- **M4d Native spike** (parallel, timeboxed): egui prototype + AccessKit
  inspection harness; no WebView2.

Exit: CLI, GUI, and extension show the same numbers from the same snapshot;
old extension still works; the native harness can assert a row without pixels.

### M5 - flip and delete

- `traffic-v2` on by default;
- delete the old rate/limit/bond paths;
- release `v0.5.0` ("truth") with no new features.

Exit: conformance suite, existing tests, node tests, and the manual e2e gate
all pass.

### M6 - feature waves

Follow `FEATURE-ROADMAP.md`:

- v0.6 "control": per-host limits, proxy, checksum, HTTP/2, safe filenames,
  config validation, per-task torrent cap, LAN throttle, update cache,
  extension hardening, queue priorities, bandwidth scheduling.
- v0.7 "reach": grabber/packages, auto-extract, automation/MCP, diagnostics,
  Firefox, shell integration, post-download hooks.
- native UI adoption only if the M4d measurements win; otherwise keep Tauri.

Each feature graduates idea -> matrix row -> invariant -> code -> test ->
shipped. No shortcut.

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
