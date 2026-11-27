# next working index

Status: **planning complete, next-layer code landing**. Branch: `next`.
`main` is the release line: no hotfixes there.

Naming: we never shipped a "v1", so nothing here is called v2. This is the
**next** architecture for the current v0.4.x product; the feature flag is
`unified-traffic`, not `traffic-v2`.

**This file is the only planning doc to load at session start.** The rest are
reference docs, opened on demand. `AGENTS.md` points here.

## Goal

One owner per cross-cutting rule, one source of truth for every number a user
sees. Strangler refactor, not a rewrite. Architecture-first, not bug-driven:
a defect closes when its invariant and contract test exist (`QUALITY.md`).

## Current state

| Item | Value |
|---|---|
| Branch | `main` (`v0.5.0` release prep) |
| Commit | see `git log -1 --oneline` (updated each PR) |
| Product code | M1/M2/M3 complete (`unified-traffic` enabled by default); native `eframe`/`egui` desktop shell promoted to `crates/ripdl-gui` (replacing Tauri/WebView2) with tray icon, single-instance hand-off, 3-step first-run onboarding wizard, Light/Dark themes, and Lucide vectors; extension `welcome.html` + `ripdl register-host --open-welcome` onboarding landed |
| Trackers | `ISSUES.csv`, `FEATURE-MATRIX.md`, `INVARIANTS.md` |
| Next milestone | Ship `v0.5.0` and continue v0.6 protocol/schema & capture enhancements |

M1 and M2 exits met: 12 traffic tests + 14 link tests pass. Global cap, bundle
share, torrent/video coverage, live limits, cancellation, burst, retry
rollback, resume guard, stale run ids; one `LinkSet` with measured `dt`,
durable/per-connection capacity weights, pruning, bounded counters, explicit
uTP metering, and no probes when disabled. WP-01/WP-03 issues remain open until
M3/M4 wire the domains into the daemon and frontends.

## Next action (M3)

Wire the two domains into the daemon behind a `unified-traffic` feature flag:

1. `Daemon` owns one `TrafficController` and one `LinkSet`.
2. HTTP tasks, bundle parts, torrents, and video acquire from the same global
   budget; live limit changes call `set_global_rate` / `set_task_rate`.
3. The `LinkSet` replaces `bond_links` / `torrent_rates` / `bind_rates` in
   `Reply::Config` with one snapshot; probing runs only when bonding is on.
4. Old rate/limit/bond paths stay the default until parity tests pass.

Landed so far: daemon owns `TrafficController` + `LinkSet` behind the
`unified-traffic` Cargo feature; `Command::GetTrafficSnapshot` answers one snapshot;
`SetConfig` applies live global-limit and bonding changes; every task/part is
registered with a run generation, `on_progress` feeds bytes into its
`TaskTraffic`, and HTTP/bundle engines acquire budget from the shared handle
instead of a private limiter. Old paths remain default; CLI keeps a v1 arm.

Landed next: session-wide torrent download cap via librqbit `LimitsConfig`
(live on `SetConfig`), **per-task torrent cap at add time** through
`AddTorrentOptions.ratelimits` (no vendor delta was needed), yt-dlp
`--limit-rate` from the global budget, and HTTP per-link metering into the one
`LinkSet` with a measured sampler `dt`.

M3 complete: torrent `bind_totals` deltas now feed the same `LinkSet`, and
`Reply::Config.traffic_links` carries the one link snapshot (additive, so old
frontends ignore it). CLI prints `unified links:`. Test
`config_reply_carries_unified_links` proves it.

Exit met: HTTP + bundle + torrent + video all draw from the daemon-owned budget;
live changes apply within one tick; one link snapshot feeds config and CLI.

## Next action (M4)

Render that snapshot in every frontend, with no client-side rate math:

- CLI: `ripdl traffic [--json]` landed. It calls `GetTrafficSnapshot`, prints a
  human table by default and the snapshot as JSON with `--json`.
- Generated types: `tools/xtask` exports `protocol.d.ts` into the extension and
  Tauri UI from the Rust types; CI checks them with `--check`.
- **Native UI first**: M3 is complete, so the snapshot contract is stable and
  the Tauri GUI has not been adapted yet. Adapting Tauri to the new snapshot
  would be throwaway work if native wins. Build the egui UI directly against
  `GetTrafficSnapshot`, with AccessKit inspection and screenshot QA per screen.
- Tauri GUI: leave it on the v1-compatible path as the comparison baseline.
  Only adapt it if the native spike fails its go/no-go.
- Extension: independent of the desktop shell; proceed with delta store,
  `Ping` instead of `list`, generated `protocol.d.ts`, version handshake
  (see `EXTENSION-PLAN.md`).

## Decisions made

- Global cap covers **all traffic** (HTTP, bundles, torrents, video).
- Torrent per-task caps: librqbit already supports `LimitsConfig` in
  `AddTorrentOptions`, so **no vendor delta is needed**. Session cap is live;
  a per-task cap is set when the torrent is added.
- Native spike: **egui**, no WebView2; CDP replaced by `EGUI_INSPECTION` + AccessKit.
- Overwrite data-loss fix stays on `next`, not `main`.
- Extension is a first-class next frontend: delta snapshots, generated types,
  version handshake, no client-side rate math.

## Document index (read on demand)

| File | Purpose | Open when |
|---|---|---|
| `ARCHITECTURE.md` | Layers, ports, events, ownership | designing a module or wire shape |
| `SETTINGS.md` | Typed/scoped settings schema and apply policy | adding or changing a knob |
| `QUALITY.md` | Method, layering rules, error model, CI gates, review checklist | before a PR or architectural choice |
| `SCALABILITY.md` | Load targets, protocol sizing, generated types | touching RPC, frontends, or perf |
| `EXTENSION-PLAN.md` | Extension next workstreams and version skew | working on `ext/` or the host |
| `UI-REHAUL.md` | Onboarding, screen inventory, component rules | any UI change |
| `UI-QA.md` | Screenshot/debug loop per screen and state | before calling any screen done |
| `FEATURE-ROADMAP.md` | v0.5 / v0.6 / v0.7 capability waves | deciding what to build next |
| `INVARIANTS.md` | Numeric rules + failing test names | writing or reviewing a test |
| `ISSUES.csv` | Evidence register (severity, root cause, fix) | triaging a defect |
| `FEATURE-MATRIX.md` | Status + evidence per feature | marking work `shipped` |
| `ADR-0001-strangler-next.md` | Why strangler, not rewrite | questioning direction |

## Trackers are the source of truth

- `ISSUES.csv`: 73 rows. Severity S0-S4, confidence, root cause, regression
  test. A bug is closed only when its invariant test passes - never by patching
  one repro.
- `FEATURE-MATRIX.md`: every feature has an id, status, evidence, and proof.
  `missing -> partial -> shipped`; no shortcut.
- `INVARIANTS.md`: every rule names a test that must fail on broken code.

## Keeping this lean

- Do **not** add another planning doc. Put new detail in the file listed above
  that already owns that topic, or in an ADR for an architectural decision.
- Every PR that changes behavior updates this index, the affected tracker, and
  the invariant/test in the **same commit**.
- Archive finished milestone checklists into `docs/next/archive/`; keep this file
  under 100 lines.
- If a section here grows, it belongs in one of the reference docs above.
