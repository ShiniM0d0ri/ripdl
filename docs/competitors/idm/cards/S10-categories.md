# S10 - Categories and filters

Status: `spec ready` (categories exist). Read `SHARED.md`, `S01-main.md`.

## Goal

Auto-file downloads into meaningful folders and let the user filter by them.

## IDM behavior

- File-type folders: Compressed, Documents, Music, Programs, Video.
- Workflow folders: Unfinished, Finished, Grabber projects, Queues.
- Categories sidebar can be collapsed.

## ripdl target

- Built-in: All, Downloading, Completed, Torrents, Bundles, Video, Documents,
  Music, Programs, Compressed, Queues.
- Per-category policy: folder, speed cap, default segments (from SETTINGS).
- Header filter combines with selected category.

## State model

```text
categories: Vec<CategoryInfo>   // id, name, icon, count, policy
selected: CategoryId
filter_text: String
```

## Interactions

- Assign category at add time (S02 advanced) or change in details.
- Category policy affects new tasks only (show badge).
- Counts update from snapshot, not local counters.

## Acceptance

- [ ] Category counts equal snapshot counts.
- [ ] Policy feeds default folder/caps for new tasks.
- [ ] Screenshot with expanded/collapsed sidebar and two categories.

## Work IDs

FEATURES.csv: IDM-003, IDM-004, IDM-031, RIP-039, RIP-040.
Roadmap: category policies.

## Do not

- Do not store counts in the frontend; derive from snapshot once per change.
