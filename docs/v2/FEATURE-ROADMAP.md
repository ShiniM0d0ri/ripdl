# Feature roadmap (v2 waves)

This file maps the feature matrix to release waves. It is the answer to
"what are we building, and in what order". `FEATURE-MATRIX.md` remains the
source of truth for **status**; this file is the source of truth for **target**.

Rule: before any feature gets code, it gets an ID, an owner module, a target
wave, and a test name in the matrix. A feature cannot be marked `shipped`
until that test passes.

This is a **capability roadmap**, not a bug list. Defects in `ISSUES.csv` are
evidence that a capability or invariant is missing; they are scheduled by
closing the invariant, not by patching one repro. Settings work
(`SETTINGS.md`) is part of every wave: a capability is not done until its
knobs are typed, scoped, validated, and generated into the frontends.

## v0.5 - Truth (truth layer, no new user features)

Goal: every number ripdl shows is correct, and every knob actually works.
This wave closes all S0/S1 issues and the S2 issues that mislead the user.

| Area | Feature IDs | Deliverable |
|---|---|---|
| Traffic | ENG-017, ENG-018, ENG-019, ENG-020, BUN-008, TOR-012, VID-004, ISS-060 | One global cap across HTTP, bundles, torrents, video; live changes; cancellation-safe limiter |
| Engine correctness | ENG-007..010, ENG-033, ENG-034, ENG-036, ISS-008, ISS-009 | Safe resume, gzip identity, no silent overwrite, config validation |
| Links | NET-008, NET-009, NET-015, NET-016, NET-017, NET-021, NET-022, NET-023 | One `LinkSet`, measured dt, durable metering, no ghost NICs |
| Refresh | REF-002, REF-003, REF-004, REF-005 | No version splicing, validated sidecar, hidden attribute |
| Bridge | BRG-003, BRG-004 | No thread leak, no blind retries |
| Protocol | RPC-007, RPC-008, RPC-011, NET-027, XTN-001..004 | v2 snapshot/delta, generated types, version handshake |
| GUI | GUI-015, GUI-016, GUI-027, GUI-028, GUI-029 | Render snapshots; no client-side rate math |
| Extension | XTN-001..004, B23 | Delta store, ping, pipe verification |
| Native spike | NUI-001..004 | egui prototype + AccessKit harness; go/no-go |

Exit: the v0.5 definition of done in `README.md`.

## v0.6 - Control (parity with the new Rust managers)

Goal: close the obvious power-user gaps that competitors already have.

| New/Existing ID | Feature | Wave | Notes |
|---|---|---|---|
| ENG-030 | Per-host connection limits | v0.6 | MDM has `HostLimits`; learn from 429/503 and Retry-After |
| ENG-026 | Proxy support (config + per-task) | v0.6 | HTTP/SOCKS; NDM-Next has per-task proxy |
| ENG-028 | Post-download checksum verify | v0.6 | Optional expected hash |
| ENG-025 | HTTP/2 support | v0.6 | Enable reqwest `http2`, test negotiated protocol |
| ENG-033/043 | Safe filename collision policy | v0.5/6 | `name (1)` or user prompt; S0 data loss |
| CFG-003 | Config validation at load | v0.5/6 | Reject/patch invalid hand-edited values |
| TOR-013 | Per-task torrent cap via vendor delta | v0.6 | Decision made: patch librqbit now |
| LAN-015 | Pairing-code brute-force throttle | v0.6 | Per-IP backoff + lockout |
| UPD-009 | Update-check cache/backoff | v0.6 | One GitHub GET per day, back off on failure |
| XTN-005..007 | Extension host hardening, plugin capabilities, diagnostics | v0.6 | See `EXTENSION-PLAN.md` |
| QOS-001 | Queue priorities and reordering | v0.6 | Drag order in GUI, persisted priority |
| QOS-002 | Bandwidth scheduling by time window | v0.6 | Off-peak caps; IDM/FDM parity |

## v0.7 - Reach (differentiators)

Goal: the features people switch for, after the foundation is trustworthy.

| New ID | Feature | Why |
|---|---|---|
| GRB-001 | Link grabber: paste a page → review → package | JD2's core workflow; `docs/product-research.md` ranks it first |
| GRB-002 | Recursive filtered site crawl | IDM's site grabber niche, reuse the same filter model |
| BUN-011 | Auto-extract packages when part sets complete | zip/rar/7z; part-set awareness |
| AUT-001 | Local automation API / MCP server | MDM and KGet both ship MCP; KGet also has a daemon + library |
| AUT-002 | CLI `--json` mode on the v2 snapshot | Stable automation surface without text parsing |
| OBS-003 | Copy diagnostics button | Product research: support-cost killer |
| OBS-004 | Metrics/health endpoint | Debuggability at 200 tasks |
| OSP-001 | Shell integration (right-click, "download with ripdl") | Table stakes on Windows |
| HOOK-001 | Post-download hooks (script/AV scan) | Cheap, visible power-user feature |
| BRW-012 | Firefox extension | Broader capture coverage |
| XTN-007 | Extension diagnostics UI | Supportability |
| TOR-014 | uTP metering/bonding | Only if measurements show it matters |

## Standing track: UXD (UI rehaul + onboarding)

This track runs across waves; it is not a single release. Detailed screen and
component inventory is in `UI-REHAUL.md`; the screenshot/debug loop is in
`UI-QA.md`.

| ID | Capability | Wave | Notes |
|---|---|---|---|
| UXD-001 | First-run onboarding (3 skippable screens) | v0.5 | Folder, optional browser, optional network |
| UXD-002 | Honest empty states | v0.5 | One CTA: paste a link or drop a .torrent |
| UXD-003 | Generated Settings form from `GetSchema` | v0.5 | Kills orphan fields and drift |
| UXD-004 | Default column set + optional advanced columns | v0.5 | Anti-clutter |
| UXD-005 | Component/token system with state fixtures | v0.6 | Shared by Tauri UI and native spike |
| UXD-006 | Full screen rehaul (all UXD-001..015 in `UI-REHAUL.md`) | v0.6 | Screenshot-gated |
| UXD-007 | Screenshot QA harness and sidecar artifacts | v0.6 | `UI-QA.md`, CI on Windows |
| UXD-008 | Themes, density, localization-ready layout | v0.7 | After behavior is correct |
| UXD-009 | Native parity with the same screen inventory | v0.6/7 | Only if the spike wins |

Rule: no UXD screen moves to `shipped` until every state has a passing
screenshot with tree assertions, per `UI-QA.md`.

## Work packages (grouped fixes and enhancements)

`ISSUES.csv` has a `work_package` column. This is how defects and enhancements
are grouped: one root cause, one design change, many issues closed together.
No issue is fixed alone; it is closed by its package's invariant tests.

| WP | Name | Root cause | Issue IDs | Wave | Exit |
|---|---|---|---|---|---|
| WP-01 | Traffic truth | Limiter is owned per `Engine`; no live policy or shared meter; torrent/video/bundle bypass it | 001-007, 052, 060 | v0.5 | TR-001..012; two tasks + bundle + torrent + video under one cap |
| WP-02 | Byte & resume integrity | Sidecar and `Content-Length` trusted; destination truncated; gzip decoding | 008-013, 038 | v0.5 | EN-001..012; gzip hash match, no overwrite, corrupt sidecar recovers |
| WP-03 | Link & NIC truth | Two link systems (`Bond` + `BindMeter`), hardcoded `dt`, read-time metering, no pruning | 018-029, 034-037, 057, 058, 065 | v0.5 | LN-001..014; per-link sum = aggregate; no ghost NICs; no probes when off |
| WP-04 | Refresh integrity | Refresh checks identity but not version or segment bounds | 066-068 | v0.5 | EN-013..015; no version splicing, validated sidecar, hidden attribute |
| WP-05 | Config & settings governance | Flat `Option` bag, validation only in `SetConfig`, no schema or bounds | 015, 030, 051, 053, 071-073 | v0.5/6 | CFG-001..012 + `SETTINGS.md`; every knob typed, scoped, validated, generated |
| WP-06 | Engine correctness & capability | Planner/client config gaps; retry policy contradicts its comment | 014, 016, 017, 047-050, 059, 064 | v0.6 | EN-008..012 + new tests; planner bounds, HTTP/2, proxy, checksum, per-host limits |
| WP-07 | Queue & process lifecycle | No run generation; progress map not pruned; unbounded reconcile/walk | 039-041, 056, 062 | v0.6 | RP-003 + lifecycle tests; no stale samples, bounded maps and budgets |
| WP-08 | Protocol & frontend contract | Frontends compute rates; no version/delta; no generated types | 031-033, 042, 043, 054, 055, 061, 063 + XTN | v0.5/6 | RP-001..006, UI-001..006; one snapshot, generated types, no client math |
| WP-09 | Bridge, LAN & update hardening | Resource leaks, no throttles, no update cache | 044-046, 069, 070 | v0.6 | RP-005/006 + LAN tests; no leaked threads, code throttle, cached update check |

WP-01..04 are the v0.5 truth layer. WP-05..09 finish in v0.6; they are grouped
here so a fix never lands as a lone patch. The UXD standing track runs across
all waves.

## Explicitly not planned

- Debrid account integrations, captcha services, hoster plugin marketplaces:
  scope explosions with third-party dependencies.
- In-app DASH/HLS muxing: keep the yt-dlp sidecar; it is the honest route.
- Slint: licensing needs a deliberate decision before entering MIT/Apache.
- GPUI: young ecosystem; revisit after the native spike has numbers.
- A full UI rewrite before the truth layer is green.

## How a feature graduates

```
idea -> matrix row (status: missing, owner, target wave, test)
     -> invariant (if it changes a numeric rule)
     -> code behind capability/version gate
     -> test passes, status: partial
     -> conformance + e2e green, status: shipped
     -> docs/product-research.md updated to match
```

No feature merges directly from "idea" to "shipped".
