# S13 - Empty, loading and error states

Status: `spike partial` (empty state + offline settings fallback; loading and
error states open). Read `SHARED.md`, `S01-main.md`.

## Goal

Every screen says what is happening and what to do next. No blank grids, no
spinners with unknown meaning.

## IDM behavior

- Empty task list is a bare grid; errors live in Status/Last Try columns.
- This is a weakness: ripdl should be better, not 1:1 here.

## ripdl target

Main list:

- Empty -> "No downloads yet" + one CTA "Add a URL" + drop hint.
- Filter empty -> "Nothing in this category" + clear filter button.
- Daemon offline -> status pill "daemon offline" + retry hint; rows keep last
  known state but no live rate.

Dialogs:

- Validation -> inline error with the exact field.
- Duplicate -> inline choice, no modal storm.
- Refresh -> dry-run verdict strip before swap.

Labels:

- `connecting`, `verifying`, `metadata`, `stale/expired`, `seeding`,
  `queued/timed`, `0 B/s` all distinct.

## State model

```text
kind: empty | filtered-empty | offline | loading | ready
message: String
primary_action: Option<Action>
```

## Acceptance

- [ ] No screen shows a blank central area without explanation.
- [ ] No stale rate on offline/queued/paused.
- [ ] Screenshots for each listed state.

## Work IDs

FEATURES.csv: IDM-035, IDM-036. UI-REHAUL UXD-002, UXD-007.

## Do not

- Do not copy IDM's bare empty grid.
