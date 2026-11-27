# S12 - Speed display and graph

Status: `spec ready` (daemon truth landed). Read `SHARED.md`.

## Goal

Show honest bandwidth: no spikes, no stale values, no per-NIC mystery.

## IDM behavior

- Transfer rate per task; aggregate graph in progress window.
- No per-NIC visibility (ripdl addition).

## ripdl target

- Status strip: total rate from `ControllerSnapshot.total_bps`, average over
  60 s from sampled snapshots, graph.
- Row speed: `TaskInfo.speed_bps`, rendered from daemon.
- Per-NIC chips: `traffic_links` from `GetTrafficSnapshot` / `Reply::Config`.
- Paused/queued rows show no live rate.
- 0 B/s renders as `0 B/s`, not `-` (which means unknown).

## State model

```text
snapshot: ControllerSnapshot       // authoritative
history: Vec<(Instant, f64)>       // display history only
window: 60s
sample_cadence: 1s or 2s           // fixed, not on render
```

## Interactions

- Graph samples on a timer, not on every UI render.
- Hover a point -> exact value and time (optional).
- Per-NIC chip click -> Settings > Connection.

## Acceptance

- [ ] No client-side rate math anywhere.
- [ ] No sample above plausible cap after warm-up (INV TR-011).
- [ ] Per-link sum equals aggregate (INV LN-001).
- [ ] Paused row contributes 0 to total (INV UI-002).
- [ ] Screenshots: idle, 1 task, 8 tasks, links chips.

## Work IDs

FEATURES.csv: IDM-033, IDM-034. Invariants: TR-*, LN-*, UI-001..006.

## Do not

- Do not recompute speed from `downloaded` deltas in any frontend.
