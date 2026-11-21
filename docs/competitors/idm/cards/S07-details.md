# S07 - Task details and progress

Status: `spec ready` (Tauri dialog exists). Read `SHARED.md`, `S01-main.md`.

## Goal

Double-click a row -> one dialog that answers "what is happening with this
download?" without a second source of truth.

## IDM behavior

- Progress window with per-connection segment bars, speed graph, ETA, size,
  source, time left, last try.
- Torrents are not a first-class concept; ripdl must add them.

## ripdl target

HTTP/video/bundle details:

- Address (copyable), output file, ETag, last error/stale reason.
- Per-segment durable bytes strip (from `TaskDetailsSnapshot.segments`).
- Per-link share strip (from unified `LinkSnapshot`).
- Open file / Open folder.
- Refresh address button (RIP-047) for unfinished tasks.

Torrent details:

- Files tab with want checkboxes + Apply -> `UpdateTorrentSelection`.
- Peers tab, Trackers tab, lifetime uploaded, ratio, up rate, seed state.
- Sequential toggle, stop/start seeding.

## State model

```text
task_id: TaskId
tab: summary | files | peers | trackers
details: TaskDetailsSnapshot | TorrentDetailsSnapshot
refresh_open: bool
poll_every: 2s while open
```

## Interactions

- Double-click row -> open; Escape closes.
- Fields refresh from RPC, never from cached row values.
- Refresh address opens S-like dialog with dry-run verdict and kept X of Y.

## Acceptance

- [ ] All numbers equal daemon snapshot.
- [ ] Copy address does not log secrets.
- [ ] Torrent file selection round-trips.
- [ ] Screenshots: HTTP running, HTTP stale, torrent seeding, refresh dialog.

## Work IDs

FEATURES.csv: IDM-022..024, RIP-047.
GUI: GUI-005/006, UXD-012.

## Do not

- Do not derive speed from downloaded bytes here.
