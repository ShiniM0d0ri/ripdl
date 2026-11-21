# v2 working index

Status: **planning complete, no product code written**. Branch: `next`.
`main` is the release line: no hotfixes there.

**This file is the only v2 doc to load at session start.** The rest are
reference docs, opened on demand. `AGENTS.md` points here.

## Goal

One owner per cross-cutting rule, one source of truth for every number a user
sees. Strangler refactor, not a rewrite. Architecture-first, not bug-driven:
a defect closes when its invariant and contract test exist (`QUALITY.md`).

## Current state

| Item | Value |
|---|---|
| Branch | `next` (clean) |
| Commit | see `git log -1 --oneline` (updated each PR) |
| Product code | M1 Traffic domain landed (`traffic.rs`), not yet wired into daemon |
| Trackers | `ISSUES.csv` (73), `FEATURE-MATRIX.md`, `INVARIANTS.md` |
| Next milestone | M2 - Link domain (`LinkSet`) |

M1 exit met: 12 traffic tests pass; global cap, bundle share, torrent/video
coverage, live limits, cancellation, burst, retry rollback, resume guard, and
stale run ids are proven at domain level. WP-01 issues remain open until M3/M4
wire the controller into the daemon and frontends.

## Next action (M2)

Work package **WP-03 Link & NIC truth** (grouping in `FEATURE-ROADMAP.md`).
Implement the **Link domain** as a standalone module, not a bond patch:

1. `LinkSet` - one link set, one meter, one rate estimator, one weight model.
2. Measured `dt` from the sampler loop; durable-byte feeding from both engines.
3. Prune departed links and per-connection counters; no probes when off.
4. Contract tests from `INVARIANTS.md` (LN-001..014).

Exit: per-link rates sum to the aggregate; no ghost links; CLI and daemon can
share the same link snapshot.

## Decisions made

- Global cap covers **all traffic** (HTTP, bundles, torrents, video).
- Torrent per-task caps: **vendor librqbit delta now**.
- Native spike: **egui**, no WebView2; CDP replaced by `EGUI_INSPECTION` + AccessKit.
- Overwrite data-loss fix stays on `next`, not `main`.
- Extension is a first-class v2 frontend: delta snapshots, generated types,
  version handshake, no client-side rate math.

## Document index (read on demand)

| File | Purpose | Open when |
|---|---|---|
| `ARCHITECTURE.md` | Layers, ports, events, ownership | designing a module or wire shape |
| `SETTINGS.md` | Typed/scoped settings schema and apply policy | adding or changing a knob |
| `QUALITY.md` | Method, layering rules, error model, CI gates, review checklist | before a PR or architectural choice |
| `SCALABILITY.md` | Load targets, protocol sizing, generated types | touching RPC, frontends, or perf |
| `EXTENSION-PLAN.md` | Extension v2 workstreams and version skew | working on `ext/` or the host |
| `UI-REHAUL.md` | Onboarding, screen inventory, component rules | any UI change |
| `UI-QA.md` | Screenshot/debug loop per screen and state | before calling any screen done |
| `FEATURE-ROADMAP.md` | v0.5 / v0.6 / v0.7 capability waves | deciding what to build next |
| `INVARIANTS.md` | Numeric rules + failing test names | writing or reviewing a test |
| `ISSUES.csv` | Evidence register (severity, root cause, fix) | triaging a defect |
| `FEATURE-MATRIX.md` | Status + evidence per feature | marking work `shipped` |
| `ADR-0001-strangler-v2.md` | Why strangler, not rewrite | questioning direction |

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
- Archive finished milestone checklists into `docs/v2/archive/`; keep this file
  under 100 lines.
- If a section here grows, it belongs in one of the reference docs above.
