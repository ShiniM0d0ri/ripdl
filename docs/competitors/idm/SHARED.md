# Shared rules for all IDM-map cards

Read once; each card assumes these rules.

## Clean-room

- Understand behavior, never copy IDM code, strings verbatim, icons, or images.
- Research artifacts stay in `research/idm-portable/` and are gitignored.
- Record findings as behavior and intent only.

## Product rules (non-negotiable)

- No telemetry, no ads, no registration/trial/upgrade nags. About dialog only.
- The daemon is the only source of speed, totals, ETA, and link state.
  Frontends render `GetTrafficSnapshot` / `TaskInfo`; no client-side math.
- Settings come from one typed schema (`docs/next/SETTINGS.md`), generated into
  frontends by `xtask`.
- Use the generated `protocol.d.ts` from `cargo run -p xtask -- protocol`.

## Status legend for cards

- `spec ready` — behavior mapped, no code assumed.
- `spike partial` — native spike has a shell, not final.
- `done` — implemented and screenshot-QA passed.

## Screen QA loop (every card)

1. Build the screen with stable AccessKit names on every control.
2. Drive it from the inspection harness (`spike/native-ui/qa/`).
3. Capture a screenshot for each state the card lists.
4. Assert the tree (names/roles/one primary action) and check overflow.
5. Iterate until `UI-QA.md` passes; store curated screenshots outside git.

## Priorities

P0 = must for v0.5 truth layer; P1 = native UI parity; P2 = differentiators;
P3 = later. Upgrade/registration work is excluded entirely.

## Cross-cutting IDs

- Global cap / live limits: WP-01, TRA-*.
- One link truth: WP-03, LNK-*.
- Settings schema: WP-05, CFG-*.
- Protocol snapshot: WP-08, RP-*, CLI-018.
- UI rehaul/onboarding: UXD-*, UI-QA.md.
