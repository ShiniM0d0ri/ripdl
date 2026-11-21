# S11 - Context menus and selection

Status: `spec ready`. Read `SHARED.md`, `S01-main.md`.

## Goal

Fast, contextual actions on rows without hunting through toolbars.

## IDM behavior

- Right-click row opens action menu.
- Toolbar actions follow selection.
- Menus differ by task state.

## ripdl target

Per-row context entries:

- Running/queued/verifying/metadata -> Pause.
- Paused/failed/timed -> Resume.
- Completed torrent -> Stop seeding / Start seeding.
- Any -> Details, Copy address, Open file, Open folder, Save to, Remove.
- Stale -> Refresh address (primary).

Multi-selection:

- Pause N tasks / Resume N tasks / Remove N tasks.
- Remove unfinished asks once; completed removes directly.
- Menu counts only rows the action would change ("Pause 3 tasks").

## State model

```text
selected_ids: Set<TaskId>
primary_id: Option<TaskId>
menu: Option<TaskId>
```

## Interactions

- Right-click sets primary selection if row was not selected.
- `closest("[data-act]")` style resolution; icons and labels both clickable.
- Escape closes without changing state.

## Acceptance

- [ ] One `toggleIntent` drives row button, context entry, bulk actions.
- [ ] No action shown for invalid state.
- [ ] Keyboard: Shift+F10 opens menu.
- [ ] Screenshots: single row, multi-select, stale row.

## Work IDs

FEATURES.csv: IDM-032. GUI-004, UXD-009.

## Do not

- Do not add a "Delete forever" style action without confirmation.
