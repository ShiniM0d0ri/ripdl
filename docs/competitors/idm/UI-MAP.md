# IDM screen-by-screen map

Observed screenshot: `research/idm-portable/shots/idm-main.png`.
Dialog screenshots were attempted; the main window is reliable, and menu/dialog
behavior is cross-checked against IDM's own help pages where needed.

Clean-room rule: this describes **layout, labels, and intent**. Do not copy
IDM's art, strings verbatim, or behavior that depends on its proprietary code.

## S01 — Main window (observed)

- Title: product name + version.
- Menu bar: `Tasks`, `File`, `Downloads`, `View`, `Help`, `Registration`.
- Toolbar (left to right):
  - Add URL
  - Resume
  - Stop
  - Stop All
  - Delete
  - Delete Completed
  - Options
  - Scheduler
  - Start Queue / Stop Queue (dropdown split buttons)
  - Tell a Friend
- Left sidebar, `Categories` with a close `x`:
  - All Downloads (expanded) → Compressed, Documents, Music, Programs, Video
  - Unfinished
  - Finished
  - Grabber projects
  - Queues
- Main table columns:
  `File Name` (with a small search/filter icon), `Size`, `Status`, `Time left`,
  `Transfer rate`, `Last Try...`, `Description`.
- Bottom: horizontal scroll strip for the wide table.
- Empty state: a plain grid, no onboarding text inside the table.

### ripdl mapping

- Keep this as the primary shell layout: sidebar + task table + toolbar.
- Category icons/folders map to our `categories` feature; add **Torrents** and
  **Bundles** as first-class categories beside `Video`, not hidden in a menu.
- Toolbar maps to our existing actions; add **Refresh address** as a prominent
  toolbar/icon beside Resume for unfinished HTTP rows.
- The `Last Try...` column maps to our `stale_reason`/last error — expose it as
  a column, not buried in details.
- Our speed graph belongs in the status bar, not in the table.

## S02 — Add URL dialog

- IDM: one URL field, optional "Save to" folder, headers/auth/cookies through
  an Options sub-dialog, then `Download`.
- ripdl: mirror the simple first screen (`URL`, `Save to`, primary `Download`).
  Put category, video/torrent mode, sequential, and headers behind one
  `Advanced` expander so the default stays uncluttered.

## S03 — Options / Settings

IDM groups (observable from its help and menu structure):

- General: startup, default folder, browser integration, language.
- Connection: speed limit, connections per download, proxy, user agent,
  timeout/retry.
- Download: file naming, "confirm download", download complete action.
- Scheduler: start/stop times.
- Site Grabber / Monitoring: file extensions to monitor, sites list.
- Video: capture settings.
- Advanced: queue priorities, logging.

### ripdl mapping

We already planned generated settings from one typed schema (SETTINGS.md).
Group order should follow IDM's familiar sequence, with our additions inline:

- Connection: global/task/host caps, per-NIC bonding, proxy.
- Content: filename collision policy, checksum, sidecar cadence.
- Torrents: seeding ratio/time, listen port, DHT, fastresume.
- Bundles: auto-extract, part-set policy.
- Browser: extension status, referer policy, capture rules.
- Updates: check/auto-install, verification.
- Diagnostics: log level, copy diagnostics, no telemetry (fixed).

## S04 — Scheduler

- IDM: a weekly grid/time list with "Download at" / "Stop at" and tasks.
- ripdl: we already have `--after` / `--at`. UI needs a calendar/time row in
  Settings and a per-task "Start at" field in Add advanced.

## S05 — Site Grabber

- IDM: paste URL → filter page → list of files → select → download as package.
- ripdl: bundle flow already covers the "list to package" half. Add a
  preview/filter step before queueing; new feature IDs GRB-001/002.

## S06 — Queue management

- IDM: named queues, start/stop queue, reorder, limits per queue.
- ripdl: one global queue; add named queues and per-queue caps later (QOS-001).

## S07 — Task details / progress pane

- IDM: double-click opens a progress window with per-connection segments,
  speed graph, ETA, and file info.
- ripdl: HTTP/torrent details dialog exists. Add a per-segment/link visual and
  keep speed/ETA authoritative from the daemon.

## S08 — Browser integration popup

See `ONBOARDING.md`. ripdl path: R1..R6.

## S09 — Tray / mini panel

- IDM: tray icon, mini progress window, context menu, suspend/resume.
- ripdl: tray + close-to-tray shipped; native spike must keep it.

## S10 — Categories and filters

- IDM: folder categories with automatic file-type routing.
- ripdl: categories exist; add per-category folder/speed policy later.

## S11 — Right-click menus

- IDM: per-row context actions; toolbar follows selection.
- ripdl: context menu exists; native UI must implement the same pattern.

## S12 — Speed display

- IDM: transfer rate per task and aggregate graph in the progress window.
- ripdl: status-bar strip + graph from daemon snapshots; no client math.

## S13 — Error/empty states

- IDM: grid stays empty; errors show in Status / Last Try columns.
- ripdl: honest empty state with one CTA; errors in a visible column and in
  details, never only a toast.

## S14 — Onboarding/first run

- IDM: no in-app wizard; browser integration popup only.
- ripdl: our 3-screen first-run flow is more than IDM but still skippable;
  keep it short and replayable from Help.

## S15 — Registration/purchase

- IDM: Registration menu, trial nag, purchase link.
- ripdl: **never**. About dialog with version/licence/source/releases only.
  No trial, no ads, no telemetry, no nags — non-negotiable.
