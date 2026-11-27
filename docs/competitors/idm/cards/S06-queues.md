# S06 - Queues

Status: `spec ready` (single queue today). Read `SHARED.md`, `S01-main.md`.

## Goal

Group tasks into named queues that can be started/stopped and capped together.
IDM has this; ripdl currently has one global queue.

## IDM behavior

- Sidebar `Queues` category.
- Start Queue / Stop Queue split toolbar buttons.
- Reorder tasks inside a queue; per-queue concurrency and speed policy.

## ripdl target (first slice)

- `Queues` sidebar category listing built-in queue + user queues.
- Per-task `Queue` field in Add advanced (default `default`).
- Start/Stop queue actions map to daemon pause/resume of that queue's tasks.
- Per-queue cap stored as a Category/Queue policy.

## State model

```text
queues: Vec<QueueInfo>      // id, name, running, active, cap
selected_queue: Option<QueueId>
order: Vec<TaskId>
```

## Interactions

- Drag task onto queue in sidebar (later).
- Toolbar Start/Stop acts on selected queue; falls back to selection.
- Reorder persists `position`.

## Acceptance

- [ ] Queue state visible in sidebar.
- [ ] Start/Stop only changes tasks in that queue.
- [ ] Cap from policy feeds daemon semaphore.
- [ ] Screenshot with two queues and mixed states.

## Work IDs

FEATURES.csv: IDM-021, IDM-006.
Roadmap: QOS-001, QOS-002.

## Do not

- Do not implement queue priorities before task `position` is persisted.
