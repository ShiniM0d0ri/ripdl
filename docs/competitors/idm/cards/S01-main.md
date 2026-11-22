# S01 - Main window and task table

Status: `spike implemented` (menu/icon toolbar with hover labels/table/status/
actions, light Segoe/Fluent design tokens and search; sorting, resizing and
multi-select open). Read `SHARED.md` first.

Screenshot: `research/idm-portable/shots/idm-main.png`

## Goal

A calm task manager: sidebar + toolbar + one dense table + status strip. The
user should see every download's state, size, rate, ETA and last problem without
opening dialogs.

## IDM behavior observed

- Menu bar: Tasks / File / Downloads / View / Help / Registration.
- Toolbar: Add URL, Resume, Stop, Stop All, Delete, Delete Completed, Options,
  Scheduler, Start Queue, Stop Queue, Tell a Friend.
- Sidebar "Categories": All Downloads (Compressed, Documents, Music, Programs,
  Video), Unfinished, Finished, Grabber projects, Queues.
- Table columns: File Name (search icon), Size, Status, Time left, Transfer
  rate, Last Try, Description.
- Bottom horizontal scroll strip for wide tables.

## ripdl target

- Left nav: All, Downloading, Completed, **Torrents**, **Bundles**, Categories.
- Toolbar: Add URL, Add torrent, Add bundle, Pause/Resume, Remove, Refresh
  address, Open file/folder, Settings, Send & Receive.
- Table: Name (file-type glyph), Kind, Size, Progress, Speed, ETA, Status,
  Last Try. Columns resizable/sortable, persisted.
- Status strip: total speed, 60 s graph, per-link chips, daemon state.

## State model

```text
category: all | running | completed | torrent | bundle | category:<id>
selected_ids: Set<TaskId>
primary_id: Option<TaskId>
filter_text: String
snapshot: TransferSnapshot (daemon-owned)
```

## Interactions

- Click nav row -> change category, keep selection if still visible.
- Click row -> primary selection; Ctrl/Shift click -> multi-selection.
- Double-click -> S07 details.
- Toolbar acts on selection; disabled when nothing selected.
- Filter text filters visible rows, does not change task state.

## Acceptance

- [ ] One primary action visible in toolbar.
- [ ] 200 rows scroll without layout shift.
- [ ] Every number comes from the snapshot; no derived rates.
- [ ] Last Try shows `stale_reason` / last error for unfinished rows.
- [ ] Screenshot captured for empty, 1-task, 200-task, multi-select states.

## Work IDs

FEATURES.csv: IDM-001..006, RIP-039, RIP-040, RIP-044.
Invariants: UI-001, UI-002, UI-006.

## Do not

- Do not add IDM's Registration menu or Tell a Friend.
- Do not compute totals or ETA in the frontend.
