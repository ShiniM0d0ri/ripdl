# S04 - Scheduler

Status: `spike shell` (queue list + schedule fields static; daemon queue and
scheduling API open). Read `SHARED.md`, `S02-add-url.md`, `S03-settings.md`.

## Goal

Start downloads at a chosen time; stop at a chosen time. No new engine work -
daemon already supports `--after` / `--at`.

## IDM behavior

- Weekly time grid / start-stop list.
- Per-task start time and global "start at / stop at".
- Tasks remain queued until the wall clock arrives.

## ripdl target

- Global schedule block in Settings > General (start at, stop at, weekdays).
- Per-task `Start at` in Add advanced and in task details.
- Queued rows show `timed` state with the start time (already in TaskInfo).

## State model

```text
start_at: Option<i64>       // unix seconds
stop_at: Option<i64>
weekdays: Vec<u8>           // 0=Sunday (later)
validation_error: Option<String>
```

## Interactions

- Pick date/time or relative delay.
- Clear -> immediate start.
- Changing a running task's start time has no effect (show hint).

## Acceptance

- [ ] `start_at` round-trips through RPC and queue.json.
- [ ] GUI shows `timed` with local time before start.
- [ ] Scheduler row appears only when set.
- [ ] Screenshot of empty + configured states.

## Work IDs

FEATURES.csv: IDM-014, IDM-018.
Daemon: `start_delay`, QUE-005. Roadmap: QOS-002.

## Do not

- Do not implement per-weekday recurrence before named queues exist.
