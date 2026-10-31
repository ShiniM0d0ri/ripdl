# Plan: link refresh as a system

Status: phase 1 landed (2026-10-31). Builds on the v0.3.12 `RefreshUrl`
(manual address swap, name+size validation, sidecar preserved). This plan
turns "refresh" from a single command into a link-lifecycle system.

Phase-1 implementation notes (deviations worth remembering):

- There is no separate `url_state` field: the new `TaskState::Stale` carries
  the "address dead, bytes fine" fact directly; `TaskRecord.stale_reason`
  carries why (`auth` today; `size` on the changed-file path).
- The engine reports terminal auth answers (401/403/404/410) as the
  structured `Error::HttpStatus(code)` (planner probe + segment tail), and
  mid-flight auth answers ride the ordinary retry budget before turning
  terminal - so a CDN's one-off 403 never reaches the daemon's classifier.
- The daemon (`park_stale_or_fail`) parks `stale(auth)` only for HTTP tasks
  with bytes (record or live sample), after one confirming probe; a probe
  that answers with a *different size* parks `stale(size)` instead of
  letting an etag mismatch restart the download over the kept bytes.
- A pre-existing flake fell out of the same files and is fixed: the
  engine's sidecar reporter could land a write after the successful run
  removed the sidecar (reporter writes now serialize against the success
  tail via `sidecar_stop`).
- Tests: `expired_link_reports_auth_status_and_keeps_sidecar` (core) and
  `dead_link_parks_task_stale_and_refresh_recovers` (daemon, full flow:
  progress -> pause -> 403 -> stale(auth) -> refresh -> resume -> SHA-style
  byte check).

Ground rules: engine first (classification + validation in
`ripdl-core`/`ripdl-daemon`), frontends thin. No scraping of parent pages, no
login flows, no telemetry. URLs carry tokens — they live in the local queue
only, never in tracked docs or logs.

## 1. Link lifecycle states

Add to `TaskRecord` / `Progress` (serde defaults, old records read as `ok`):

- `url_state: none | ok | stale`
- `stale_reason: none | auth | size | name | login`
- `parent_page: Option<String>` (referring page, extension-captured)

`stale` means: the bytes on disk are fine, the address is not. It is not
`failed` — no retry storm, no runner slot, survives daemon restart as `stale`.

## 2. Detection (daemon-side classification)

| Signal | Class | Result |
|---|---|---|
| 401/403/410/404 on a *fresh attempt start* | auth | `stale(auth)` |
| probe size != sidecar size | size | `stale(size)` |
| probe filename != record filename | name | `stale(name)` |
| 302 ends on a different host with `text/html` | login | `stale(login)` |
| host unreachable / DNS dead | host | `failed` (unchanged) |
| mid-flight 429/5xx | transient | unchanged retry path |

Rules:

- Classify only on fresh attempt start. Mid-flight auth-looking statuses stay
  in the normal retry path; only after retries exhaust and a re-probe also
  answers auth-class does the task become `stale`.
- `stale` tasks are auto-paused (not failed), never auto-retried, skipped by
  boot-restore scheduling.
- `failed` keeps its current meaning (transport/host death).

## 3. Validation decision tree (`ripdl-core::refresh` → `RefreshPlan`)

- `Adopt` — name + size match, ranges advertised. Swap `url`+drop old `etag`
  (new source), keep sidecar byte-for-byte.
- `AdoptNoRange` — name + size match, no ranges. Swap; task degrades to
  single-stream; warn that scattered partial bytes cannot be reused and the
  stream restarts from zero.
- `Redirected` — probe follows redirects; adopt the original URL (engine
  follows redirects anyway) and record the chain for diagnosis.
- `NameMismatch` — reject. `--force` adopts and renames the record.
- `SizeMismatch` — reject, report both sizes. `--force` discards the sidecar
  and restarts from zero.
- Same size + different ETag — adopt with a "content may differ" warning.
- New URL == old URL, or new probe also auth-fails — no swap, report why.

No silent byte loss on any path. `--force` is the only sidecar-discard route.

## 4. Capture paths (who can trigger a refresh)

1. **GUI dialog** (exists) — context menu → "Refresh address...", pre-filled
   old URL. Upgrade: inline dry-run preview *before* applying (see §6).
2. **CLI** (exists) — `ripdl refresh <id> <url> [--resume]`.
3. **Extension auto-match** (new) — user copies a fresh link; the popup checks
   paused/failed/stale HTTP tasks for a filename match (daemon-side, candidates
   capped at 2, confirmed by a cheap size probe). Match → popup offers
   **"Refresh existing task (keeps X)"** next to the normal add button.
4. **Auto-detect on resume** (new) — resume a task whose link has died: the
   probe classifies it `stale` instead of `failed`; the GUI row turns amber
   ("link expired — refresh address"), CLI `ls` prints `stale(auth)`.
5. **Parent-page hint** (stretch) — the extension records the page that
   produced the link; on `stale`, the refresh dialog offers "open <parent>"
   so re-grabbing is two clicks.

## 5. Task-kind scope

- HTTP tasks: full support.
- Video (yt-dlp) tasks: out of scope for now — yt-dlp re-resolves on its own;
  hide the refresh entry. (Later: refresh = re-invoke metadata probe.)
- Torrents: no URL to rot; hide the entry.
- Completed tasks: hide the entry (nothing to keep).

## 6. QoL details

- **Dry-run preview**: `RefreshUrl` gains `probe_only` — the GUI dialog probes
  the pasted URL and shows the verdict + "keeps 5.4 GiB / 50 GiB" *before*
  applying. Nothing changes on disk until the user confirms.
- **State-preserving resume**: refresh remembers whether the task was running
  when it went stale and resumes automatically after a successful swap
  (`--no-resume` / dialog checkbox to override). Replace the current
  opt-in `--resume` with this default.
- **Headers carried over**: refresh keeps the task's saved headers, cookies
  and referer; the dialog exposes them for editing (hosts often need the
  same cookie jar with the new token).
- **URL history ring**: keep the last 3 addresses per task in the record for
  diagnosis (shown in TaskDetails).
- **Row affordance**: amber state chip "expired"; clicking it (or pressing the
  row button) opens the refresh dialog pre-filled. Context menu entry stays.
- **CLI**: `ripdl refresh <id>` with no URL prints the task's refresh state
  and last error (guidance, not a swap).
- **Running-task refresh**: existing behavior kept (pause runner → wait for
  the paused sidecar → probe → swap → resume); GUI shows "pausing…" briefly.
- **Selection**: batch refresh is a backlog item, not phase work — stale
  matches are almost always single-task.
- **Notifications**: backlog — a desktop toast "link expired, refresh?" when a
  running task goes stale while the GUI is hidden.

## 7. Protocol & storage deltas

- `TaskRecord`/`Progress`: `url_state`, `stale_reason`, `parent_page`,
  `url_history: Vec<String>` (all optional, serde defaults).
- `Command::RefreshUrl`: add `probe_only: Option<bool>`.
- `Reply::Refreshed`: add `plan` outcome + `warning: Option<String>`.
- New `Command::StaleCandidates { name }` → up to 2 matching tasks (extension).
- `Reply::Config`-style backward compatibility: old daemons omit new fields,
  old GUIs ignore them (same pattern as v0.3.10).

## 8. Testing (offline only)

Extend the hand-rolled mock server in `crates/ripdl-core/tests/`:

- token-expiry variant (403 after N requests) → classify `stale(auth)`,
  refresh keeps sidecar bytes, resume completes, SHA-256 matches.
- size-mismatch variant → `stale(size)`, `--force` restarts, old bytes not
  silently reused.
- no-range variant → `AdoptNoRange` warning path.
- redirect-to-login variant → `stale(login)`.
- E2E (extension phase): popup offers "Refresh existing" for a matched stale
  task; both buttons work.

## 9. Rollout order

1. Core: status-class layer, `stale` state + reasons, boot-restore and
   scheduler behavior, CLI display. (No UI yet.)
2. GUI: amber row state, dry-run preview in the refresh dialog, auto-resume
   after swap, headers editing.
3. Extension: stale-candidate matching + one-tap "Refresh existing task".
4. Backlog: parent-page hint, notifications, batch refresh, no-range byte
   salvage, video-task refresh.

## 10. Implementation notes (as landed)

### Phase 1 (commit `58d231a`)

- `TaskState::Stale` in core; `park_stale_or_fail` in the daemon (HTTP only,
  bytes-held check incl. the live progress sample, one confirming
  `Engine::plan` probe, `StaleCause::Auth`/`Changed` → `stale_reason`
  `"auth"`/`"size"`). Mid-flight auth answers ride the normal retry budget
  first (`Error::HttpStatus` from planner + segment tails).
- CLI: `stale(auth)` suffix in `ls`; GUI: minimal safe mapping (pause icon,
  sort rank, resume/toggle includes stale).
- Deviations from the plan text: no `url_state` field (the `TaskState` IS the
  state); `stale_reason` is a short string, not an enum over RPC.

### Phase 2

- `Command::RefreshUrl.probe_only: bool` (serde default): the same probe +
  same-file check run read-only — no pause, no swap. A task with no sidecar
  yet answers `kept = downloaded` (usually 0 → "ready to download").
- `Reply::Refreshed.warning: Option<String>` and `ripdl_core::refresh::
  RefreshCheck { warning }`: `check_compatible` now adopts-with-warning when
  the server's etag differs from the sidecar's ("different file version -
  kept bytes may not match") instead of just succeeding silently. The other
  gates (size, name, ranges) stay hard rejections with the same messages.
- Auto-resume: the daemon records `was_running` (live runner or
  Running/Queued) before pausing for the swap and resumes after it
  (`resume || was_running`). CLI `--resume` still forces it; a task that was
  parked stays parked. Bundle parts: same rule against the bundle runner.
- GUI (`app.js`/`index.html`/`style.css`):
  - The state label for `stale` is **expired** (amber, same family as
    `timed`), globe icon, tooltip explains "refresh to keep the bytes"
    (worded by `stale_reason`). The row's play button becomes a globe
    "Refresh address" button; `toggleIntent` returns null for stale so bulk
    pause/resume skips it (resuming a stale row would only re-confirm the
    failure).
  - The refresh dialog now validates before it commits: 400 ms after open
    (diagnosing the old link) and 500 ms after every edit, it runs
    `probe_only` and shows an inline verdict strip - green "Same file -
    keeps X of Y", amber for the etag warning, red for the precise
    rejection. The swap button is disabled until the check passes, so a bad
    paste never reaches the daemon as a real swap. Palette reuses the
    existing tokens (`#e1f1e8`/`--green`, `#f8eedb`/`#8a6116`,
    `#fae9e6`/`--red`) - no new colors.
  - Tests: `refresh_probe_only_previews_without_touching_the_task` and
    `refresh_auto_resumes_a_running_task` (daemon; the paced single-file
    server moved into `spawn_refresh_server`, shared by all three refresh
    tests, with a `/small.bin` wrong-size endpoint); core unit tests for the
    clean/warn adopt split.
- Deferred: headers/cookies editing in the dialog (rarely the blocker; the
  extension capture supplies cookies) and `AdoptNoRange` salvage - both stay
  in the backlog list below.
