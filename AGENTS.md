# AGENTS.md — ripdl

Standing instructions for AI agents working in this repository. Keep this file
current when build commands, architecture, or conventions change.

## What this is

`ripdl` — a free, ad-free, multi-segment download manager in Rust. An
open-source alternative to IDM and FDM. Zero
telemetry, zero ads, forever — non-negotiable, code it into no feature.

## Layout

- `crates/ripdl-core` — pure library: HTTP engine (probe/plan, multi-segment
  ranges, sidecar resume, retry/backoff, cancellation), RPC protocol types,
  torrent session (`torrent`), LAN drop box (`lan`: receiver, sender,
  UDP discovery).
- `crates/ripdl-cli` — thin CLI over core + daemon RPC (`download`, `info`,
  `add`, `ls`, `pause`, `resume`, `rm`, `lan`, `stop`).
- `crates/ripdl-daemon` — queue owner: persistent queue at
  `%LOCALAPPDATA%\ripdl\queue.json`, one runner per task, EMA speed tracking.
- `docs/ROADMAP.md` — phase plan; keep checkboxes current for every
  completed/added item.
- `docs/rea-findings.md` — REA research notebook (P4 browser-capture work).
- Brand assets: `docs/logo.svg` is the source of truth for the mark (deep-teal
  tile, white download arrow, three mint speed dashes);
  `packaging/icon/generate.ps1` redraws it with GDI+ into
  `crates/ripdl-gui/icons/icon.png` (512) + `icon.ico` (16-256: DIB frames,
  plus one PNG frame at 256) and `docs/logo-512.png`. Edit the SVG and the
  script together, then re-run it. Gotcha: tauri-build caches the Windows
  resource, so a plain `cargo build` keeps the *old* exe icon after
  regenerating `icon.ico` — run `cargo clean -p ripdl-gui` first.

## Commands

```sh
cargo check --workspace --all-targets
cargo test -p ripdl-core
cargo build --workspace
```

Integration tests are fully offline: `crates/ripdl-core/tests/` contains a
hand-rolled mock HTTP server (drop-injection, no-range mode, etag switching).
Never hit real networks in tests.

Smoke-test URL (manual verification only):
`https://speed.cloudflare.com/__down?bytes=N` — no range support, exercises
the single-stream path.

Extension checks are plain node scripts. `plugins` and `handoff` also run in
CI; `duplicate-model` is local only:

```sh
node ext/test/plugins.test.mjs
node ext/test/handoff.test.mjs
node ext/test/duplicate-model.test.mjs
```

End-to-end gate (manual, not in CI): one isolated pass of the CLI, a private
daemon, the extension in headless Edge, and the GUI (single-instance hand-off,
Settings, Add URL). The GUI phase needs the interactive desktop, so run it
through the helper, one pass at a time:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File ext\test\e2e\run-in-session.ps1 -HarnessArgs "--no-build"
```

- `--no-build` reuses `target\e2e\debug\*.exe`. Without it the harness first runs
  `cargo build -p ripdl-cli -p ripdl-daemon -p ripdl-gui --target-dir target/e2e`;
  rebuild before trusting a `--no-build` pass after Rust edits.
- Full build: run the same helper without `-HarnessArgs`. An empty `""` is dropped
  on the PowerShell 5.1 command line and the parameter binding fails.
- `--only=cli,ext,gui` limits the phases. Exit 0 = all checks pass, 1 = some
  failed, 2 = harness exception.
- Scenarios (101 checks; last full pass 2026-10-26, 101/101):
  - Setup (1): the private daemon stays inside the run folders.
  - CLI, A0-A12 (32): the daemon answers on its own pipe; ranged and no-range
    downloads match SHA-256; a wrong token gets no task list; pause, address
    refresh and resume keep the bytes; a duplicate add returns the existing id;
    `rm` removes the file and the task; bad config values are refused; a bundle
    preview becomes one task of three parts; `info` and a direct download add no
    task; LAN drop box: the page hides the pairing code, a wrong code gets 403
    before the body, the right code delivers with a matching SHA-256.
  - Extension, B0-B17 (61): headless Edge with the real bridge; the copy chip for
    prose, bundles, a textarea and one link; a popup add through the native
    host; the duplicate window for finished, paused and captured files (Download
    again, Resume, browser keeps its copy); the options page; a daemon stop and
    restart still lands a fallback byte for byte; SHA-256 of every fixture; box
    and prose links never fetched; Referer rules; no console errors.
  - GUI, C0-C3 (5): first window up; a second launch hands off and exits; Settings
    and Add URL open. Closing checks (2): the installed daemon, its queue and
    Downloads are untouched.
- Output: `target\e2e\runs\<RUN>\` (`report.json`, logs, `shots\`, `downloads\`).
- Isolation: a private daemon (own pipe, TCP fallback, data folder), its own GUI
  pipe and WebView2 profile, a dead proxy, and the update check off. The
  installed daemon, its queue, and the user's Downloads must not change. The LAN
  check briefly opens the private drop box on all interfaces (the one network
  exception).

## Environment gotchas

- **PowerShell**: `cargo ... 2>&1 | Select-Object ...` wraps stderr as
  NativeCommandError and mates exit code 1 even on success. Judge by output
  lines (`Finished`, `test result: ok`), not the exit code.
- Builds: Rust 1.98 MSVC target. Workspace root is `D:\ripdl`.
- Session daemon: named pipe `\\.\pipe\ripdl-rpc` (Windows, primary) with TCP
  5593 loopback fallback (`ripdl_core::rpc::DEFAULT_PORT`, used by bridge);
  unix socket `<data_dir>/rpc.sock` on non-Windows. Client transport helpers
  live in `ripdl_core.transport`. Daemon policy file:
  `%LOCALAPPDATA%\ripdl\config.json` (all-optional fields).
- **E2E GUI**: the agent shell is elevated (session 0), where WebView2 cannot
  open windows (`0x80070578`), and elevation ignores
  `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS`. Run GUI checks only through
  `run-in-session.ps1`, which starts the harness in the desktop session via a
  one-off scheduled task and waits for `done.flag`. The extension phase is
  headless Edge and needs no desktop.
- Send & Receive is outside the gate: opening it runs LAN discovery, so the
  drop box is covered by API checks instead.
- Do not run `research/gui-*.js` probes: they spawn `target\debug\ripdl-gui.exe`
  without isolation and reach the installed daemon.
- Keep PowerShell scripts ASCII-only (Windows PowerShell 5.1 reads BOM-less
  files as ANSI). Wait on a per-call event id and check `done.flag`; a fixed
  `Wait-Event` source id can return a stale event from an earlier run.

## Conventions

- **Engine first, frontends thin.** New capabilities land in `ripdl-core`
  (pure lib) and are exposed through the daemon; CLI/GUI are shells.
- **Clean-room REA rules** (for all reverse-engineering work on incumbents):
  understand behavior, never copy decompiled code or assets, never strip
  license checks. Record findings in `docs/rea-findings.md`; stash downloaded
  artifacts in `research/` (gitignored).
- **License**: MIT OR Apache-2.0. No GPL dependencies in first-party code —
  torrent support must go through `librqbit` (Apache-2.0), not GPL clients.
  A minimal in-tree fork lives at `vendor/librqbit` (upstream 9.0.1 +
  `SessionOptions::bind_ips` for torrent NIC bonding), wired via
  `[patch.crates-io]`; keep the delta small and reapply it on upstream bumps
  (see `vendor/librqbit/README.md`).
- **No telemetry of any kind.**
- Commits: use `git -c user.name="ripdl" -c user.email="ripdl@localhost" commit …`.
  One commit per coherent unit, terse imperative subject.

## CI / release

- `.github/workflows/ci.yml`: on pushes to `main` and PRs, `cargo check` +
  offline `cargo test` + the `plugins` and `handoff` node tests, on
  windows-latest and ubuntu-latest. The e2e gate is not in CI.
- `.github/workflows/release.yml`: on tag `v*` (plus `workflow_dispatch`
  dry run - packages everything, no release). Version source = workspace
  Cargo.toml (tag must match). Artifacts: `ripdl-vX.Y.Z-x86_64-windows-`
  `{portable.zip,setup.exe}` (Inno script `packaging/windows/ripdl.iss`,
  Sources relative to repo root), `ripdl-vX.Y.Z-x86_64-linux.tar.gz`,
  deb+rpm via nfpm (`packaging/linux/nfpm.yaml`, pinned prebuilt tarball -
  no Go toolchain; `go install` flaked on sum.golang.org),
  `ripdl-browser-extension-vX.Y.Z.zip` + `SHA256SUMS.txt` in the release
  job. Cut releases by bumping the workspace version *and* `ext/manifest.json`
  (the extension zip ships that manifest; Chrome only reads it for Web Store
  uploads, but keep it matching), then:
  `git tag -a vX.Y.Z; git push origin vX.Y.Z`.
  Release notes are hand-written: `.github/release-notes/<version>.md`, a
  handful of user-facing bullets (no commit log, no auto-generated changelog -
  the workflow concatenates that file with the fixed `.github/release-notes/
  assets.md` blurb and uses it as the release body). Keep them short and
  concrete: what changed for the user, not what moved in the tree.
  Proven against v0.2.0 (2026-10-18): release published with all 7 assets.
  Gotchas folded in: CLI bin is `ripdl` (packaging must not invent names),
  `crates/ripdl-gui/icons/icon.png` is required (tauri-build embeds it on
  non-Windows), nfpm placeholders are `sed`-resolved in the workflow
  (don't rely on its env expansion), nfpm needs `--packager deb/rpm` when
  `--target` is a directory, and Inno wants
  `ArchitecturesInstallIn64BitMode` (bare `Architectures` is invalid).
  Installer close-out: use `CloseApplications=force` (the Tauri GUI treats
  "close" as minimize-to-tray, so a plain close leaves the wizard stuck on
  "unable to close all applications"), and kill `ripdl-gui.exe` *before*
  `ripdl-daemon.exe` - the GUI's poll auto-spawns a daemon, and a respawned
  windowless daemon is exactly what Restart Manager cannot close.

## Resume-sidecar format

`<file>.ripdl.json` (`TaskStateFile` with per-segment `done` offsets).
Sentinel `u64::MAX` = UNBOUNDED (no range support → single stream; retries
restart the stream from zero). Segment writes are buffered (256 KiB) and
flushed via the blocking pool; `cells[index].done` advances only for durable
bytes.

## Current status

Check `docs/ROADMAP.md` checkboxes for ground truth. As of 2026-10-18:
P0/P1 core complete: multi-segment download, resume, retry, daemon + RPC,
persistent queue, buffered writes, shared speed limiter, header/auth/cookie
pass-through, Content-Disposition naming, duplicate-URL idempotency,
scheduler (`--after`/`--at` with delayed runner start), named pipe transport
(pipe-first client + dual listeners; TCP kept as fallback; unix socket on
non-Windows), daemon config file (`config.json`: max_concurrent,
max_segments, min_segment_size, max_speed_bytes, default_download_dir,
lan_enabled/lan_port/lan_dir/lan_open/lan_code (same-network drop box),
bond_interfaces — NIC bonding, segments bind per uplink interface via
`ripdl_core::engine::nic` + `Bond::Auto`/`Addresses`; per-task `--bond`),
hunt mode (video/stream tasks via the `yt-dlp` sidecar: `add --video`,
`ytdlp_path` config key, percent-based progress, pause = kill sidecar,
resume = re-invoke which continues `.part` files).
GUI: `ripdl-gui` (Tauri 2 shell, `crates/ripdl-gui` + static `ui/`):
IDM-style task manager over daemon RPC (`list_tasks`/`add_task`/pause/
resume/remove commands, static frontend embedded at build time, no bundler;
auto-spawns the daemon like the bridge). Torrent rows show display names +
Torrents sidebar filter; "Add Torrent" picker; details dialog (double-click)
with Files/Peers/Trackers tabs refreshed every 2s. P4 complete and E2E
verified (2026-10-19): yt-dlp standalone binary via config `ytdlp_path`
watched a test clip end-to-end (`ripdl add … --video` →
completed webm in Downloads), and the popup chain proven over CDP in
headless Edge (branded Chrome ≥137 ignores `--load-extension`; a repurposed
driver lives at `research/ext-e2e-cdp.js`): `ripdl_send` add and
`ripdl_hunt_query` answered from the real popup-page context with the task
completing on disk; `rm --files` now drains the runner before the path
sweep so the pause path can no longer resurrect a sidecar after deletion
(fix in `cmd_remove`, daemon). Media-URL offer rows and activeTab-granted
page reads remain user-confirmable only in a real browser session.
115 tests pass offline (`cargo test --workspace`), including an in-proc
seeder↔leecher torrent round-trip.
v0.3.9: HTTP resilience pass (verified live): a per-IP quota host (OVH
answers 429 to ranged GETs after ~37 MiB) used to fail the task mid-flight; it
now completes. Engine: transient statuses (408/429/5xx) and transport faults
retry, a refused probe is retried instead of killing a fresh task, a
host-pressure failure gets a longer leash (10 attempts, 2→10 s jittered) and
halves a shared in-flight cap (`engine/gate.rs`) so eight parallel ranges
degrade to one connection and ramp back up, and parked-but-not-durable bytes
are rolled back on every failed attempt (they used to double-count, which read
as a 1.5 Gbps spike and could end in SizeMismatch). Display: engine ticks use
`MissedTickBehavior::Delay` (no catch-up burst) and the daemon derives speed
from a 2 s sliding window instead of a per-tick EMA, so a 1 GB Hetzner pull
shows a clean ramp to ~22 MiB/s with no spike and no zero-gaps. Three live
runs of the same 100 MB URL (single-segment, 8-segment, quota-degraded) hash
identically (SHA256).
v0.3.10: torrent bonding is visible and self-applying. The shared rqbit session
fixes `bind_ips` when it is created, so a saved `bond_interfaces` toggle used
to look like a no-op (the status-line hint was easy to miss). `Reply::Config`
now carries `torrent_session` + `torrent_bind` (the addresses the live session
actually bound), so a new `torrent bonding:` line in `ripdl config` and a line
under the Settings toggle say "active on <uplinks>", "enabled, but the running
session is not bonded - restart the daemon", or "off, but the session is still
bonded - restart". Saving a changed toggle asks to restart the daemon on the
spot (the existing snapshot-and-resume path) and the state line turns
green/amber. Older daemons just omit the new fields (serde defaults).
v0.3.11: installer unblocked. Inno force-closes the app
(`CloseApplications=force`) instead of stopping at "Setup was unable to
automatically close all applications", and `[Code]` sweeps `ripdl-gui.exe`
first, then the browser host, then the daemon, twice (a respawn during the
first sweep is caught by the second). The GUI stops auto-spawning daemons and
stops hiding to the tray once an in-app install has launched (`INSTALLING`),
so the 0.3.9 -> 0.3.10 stall - minimize-to-tray swallowed the close request
while a poll-respawned windowless daemon could not be closed - cannot repeat.
In progress: P2 GUI polish — basic file management landed (per-task "Save to"
with native browse, per-row location display, open file/folder via Explorer
`/select`, `remove_task(delete_files)` with sidecar-aware disk cleanup,
`rm --files` CLI flag, GUI render-on-change, spawn rate limit in the bridge);
settings panel + tray landed: `GetConfig`/`SetConfig` RPC (core
`DaemonSettings`, full-replace, `config.json` persisted, concurrency
resizes the runner semaphore live), GUI Settings dialog (toolbar gear),
tray icon with close-to-tray (`hide_to_tray` preference in config.json),
single-instance guard (a second GUI launch asks the running window to come
forward and exits; `\\.\pipe\ripdl-gui` on Windows, `<data_dir>/gui.sock`
elsewhere).
Remaining P2: nothing major.
Update mechanism landed: `ripdl-core::update` (GitHub Releases probe of
`api.github.com`, v-compare, tests incl. mock server), RPC `CheckUpdate` →
`Reply::Update`, CLI `ripdl update`, GUI banner + Help menu + per-launch
auto-check (setting `update_check`, default on). Plus an explicit install
path: `UpdateAsset` (windows-setup.exe picker), SHA256SUMS.txt verification,
streamed 256 MiB-capped download into `<data_dir>/update` (atomic `.part`
swap, reuse-by-size of a previously verified installer, week-pruning at
boot), RPC `UpdateInstall` (AtomicBool busy-guard, `silent` variant), GUI
banner "Download & Install" button + `update_auto_install` opt-in setting
(daemon does the unattended `/VERYSILENT` run at boot), CLI
`ripdl update --install`; sha2 dep. Windows setup upgraded in place
(`AppId` fixed forever, [Code] kills daemon/GUI before file copy,
`[InstallDelete]` refreshes `ext/`, uninstaller removes ripdl's per-user
native-messaging keys). v0.3.4 fixes the updater's asset URL: installs fetch
`browser_download_url` (the API `url` returns JSON metadata, so every real
install failed the SHA256SUMS gate; asset downloads now share one
`download_url()` helper). Same release: GUI chrome pass (icon-only toolbar +
SVG state/row icons from one inline sprite, 60 s total-speed strip with
now/avg readout, install failures shown inline), and the exe version is
inherited from the workspace Cargo.toml (the stale `tauri.conf.json`
`version` literal pinned every build to 0.3.2). Installer launch elevates via
`ShellExecuteW("runas")` when the daemon is not admin (Inno's manifest is
`requireAdministrator`, so `CreateProcess` used to fail with 740); the
unattended `/VERYSILENT` path refuses to run without elevation so it can
never raise a UAC prompt at boot.
v0.3.5 fixes the installer's last blocker: `taskkill ... /T` on
`ripdl-daemon.exe` killed Setup itself, because Setup is a child of the daemon
when the update is triggered from the app (ShellExecute). Symptom: the daemon
died, the GUI survived, nothing installed. Kills are name-only now, the daemon
stops its own video runners (yt-dlp sidecars) before handing over (a running
video task comes back Paused after an upgrade), and `[Run]` uses
`runasoriginaluser` so the relaunched GUI is not left elevated.
v0.3.6 completes the elevation story: the updater launches Setup with the
shell's *default verb* (never `"runas"`), so Setup itself elevates via UAC and
still knows the original non-elevated user - `runasoriginaluser` can only drop
privileges when Setup was started that way. Verified 0.3.2 -> 0.3.5 and
0.3.5 -> 0.3.6 end to end from the app's own banner (binaries hash-match each
release, queue preserved, v0.3.6 relaunched the GUI *unelevated*: token check
0). Same release: real About dialog (Help > About ripdl) with the running
version from the new `app_version` command, licence, and source/releases/issues
links.
v0.3.7: the in-app path installs with `/SILENT` (UAC stays the single
confirmation - no wizard pages) and `[Run]` no longer skips silent installs, so
the app relaunches by itself. A check that reports "current" now also hides a
stale banner: swapping the daemon during an update used to leave the old banner
offering the version that had just been installed. Also the "torrent vs HTTP"
pass: a dedicated icon-only kind column (magnet / download / film, tooltips),
category-coloured file-type glyphs in the name cell, sidebar category icons,
an icon context menu whose Pause/Resume entry follows the row state, a kind
hint in row tooltips, and volatile cells keyed by class (the kind column had
silently shifted the index-based updater).
v0.3.8: persistent torrent seeding + post-restart visibility (boot restore of
completed/paused torrents, 2 s upload/peer watcher, lifetime uploaded, row
seeding state + up rate, stop/start from the row, ratio/time limits in
Settings; verified live against the real queue). Also the clean-room
qBittorrent options inventory (275 controls / 9 pages). On main after the tag:
single-file metainfo payloads now land at `<dest>/<name>` (qBittorrent layout)
instead of `<dest>/<name>/<name>`; the nested root is still reused when it
already exists so resume/seeding never re-downloads. Magnet single-file
torrents keep the nested root (the layout must be chosen before rqbit fetches
metadata; revisit only if it can be done without a second metadata fetch).
Also on main: bonding transparency — the Settings dialog and `ripdl config`
now list the uplinks bonding would actually use (`name (addr[, default
route])`) and flag a single-uplink machine, and Settings grew a "Restart
daemon" button because torrent bonding is fixed when the shared session is
created (HTTP tasks read the setting per download); the button snapshots
Running tasks and replays `Resume` after the fresh daemon answers, so active
transfers keep downloading instead of the stop's cancelled runners leaving
them Paused. Note rqbit's listener defaults to TCP-only
(`ListenerMode::TcpOnly`, no uTP socket), so uTP is not a tuning lever.
Torrent resume/recheck is now a distinct `verifying` state: librqbit's
`Initializing` phase reports *checked* bytes (a 29 GB torrent reads at disk
speed), so `Progress.verifying` is set, the daemon skips its derived-rate
EMA (which used to show ~500 MB/s and pollute the graph), and GUI/CLI label
the row `verifying` with no speed until the check finishes. The daemon now
also prefers the tool's own rate when a sample carries one (`Progress.
speed_bps`, torrents only) over a byte delta, so adopting existing data on
resume can't read as a multi-GB/s burst. Torrent fastresume landed too:
vendored delta 4 (`SessionOptions::fastresume_folder`) points rqbit's
bitfield factory at `<data_dir>/ripdl/torrent-state` without enabling its
session store, so `<info-hash>.bitv` skips the full on-disk pass on
restart (60 s -> <1 s for the 29 GB task) while `queue.json` stays the only
task-state owner; boot prunes orphaned bitfields and `rm` deletes the
task's own. Shutdown (and boot restore, for a hard kill) now folds live
byte counts into the records so a paused torrent no longer shows `0 B`.
P3 merged-T3.1 landed (see ROADMAP M3.1/M3.2): `ripdl-core::torrent` over
librqbit 9 — magnet/.torrent/.torrent-URL payloads, one shared daemon session
(ephemeral ports, no rqbit persistence), task-owned `<dest>/<name>` root
folder, same Progress stream + pause/resume/cancel as HTTP, CLI auto-detect
(`--torrent`), GUI add auto-routes torrents; M3.3 GUI merge landed (Add
Torrent picker, torrent rows/details dialog with Files/Peers/Trackers);
per-file selection + sequential landed end-to-end (M3.3 Files-tab want
checkboxes -> Apply -> `UpdateTorrentSelection`, `--sequential` streaming
drain in `ripdl-core::torrent`, both persisted and replayed; resume of
existing on-disk files fixed via `AddTorrentOptions.overwrite = true`);
remaining P3: none beyond backlog items below. Torrent NIC bonding landed
(2026-10-19): with `bond_interfaces` on, the shared rqbit session binds every
outgoing peer connection to a rotating local uplink address (vendored
`SessionOptions::bind_ips`); listener/DHT/uTP stay OS-routed; session-wide —
created once, so a setting change needs a daemon restart (per-task `--bond`
stays HTTP-only). Pre-add file selection landed (2026-10-19, qB-style):
`TorrentSession::preview` via rqbit `list_only` (RPC `TorrentPreview`),
`Add.only_files` end-to-end (CLI `add --only 0,2-4`), GUI Add dialog
two-phase for torrent payloads (metadata wait → inline file checklist →
add checked files); librqbit honors add-time `only_files`, but selecting a
file whose first piece overlaps unselected files drags their bytes of that
piece along (same as any client — do not "fix").
Seeding + post-download visibility landed (2026-10-19): daemon boot re-adds
completed/paused torrents into the shared session (previously it started empty,
so Files/Peers/Trackers failed with "torrent session not running" after every
restart). Finished torrents keep seeding; a per-task watcher (no download
semaphore slot) samples uploaded/up-rate/peers every 2 s, persists lifetime
uploaded (rqbit counters are per-session), and stops at `seed_ratio` (default
1.0; 0 = unlimited) / `seed_time_min` (0 = never) or on pause. Config keys:
`seed_enabled`/`seed_ratio`/`seed_time_min` (GUI Settings + CLI `config`);
row/context toggle starts/stops seeding (pause keeps the torrent inspectable).
GUI shows a teal "seeding" state with ↑ rate; details summary adds
uploaded/ratio/peers; qBittorrent clean-room options inventory lives at
`docs/qbittorrent-options.md` (275 controls across 9 pages).
v0.3.12: HTTP task parity with IDM. Two things landed, one protocol addition
each (`Reply::TaskDetails`, `Command::RefreshUrl`/`Reply::Refreshed`):
(1) **Refresh download address** - `ripdl-core::refresh` validates a new URL
against the paused sidecar (same file name + size, byte ranges advertised,
segments bounded) and re-points `url`+`etag` without touching offsets; the
daemon (`cmd_refresh_url`) first pauses a live runner and waits for its
paused sidecar to land, then probes, swaps and optionally resumes. GUI:
"Refresh address..." in the context menu and the new HTTP details dialog,
with a dialog that pre-fills the old URL and a "kept X of Y" readout; CLI
`ripdl refresh <id> <url> [--resume]`. Verified offline against a
loopback token-expiry server: 40% paused -> expired link -> refresh kept
10.5 MB, resumed mid-segment, SHA256 matched (core integration test
`refreshed_address_resumes_from_sidecar` plus a live daemon run; the
"different file" guard reports both sizes).
(2) **Details for normal downloads** - `Details` now dispatches by kind:
torrents keep files/peers/trackers, HTTP/video tasks answer
`Reply::TaskDetails` (address, output file, ETag, last error, per-segment
durable bytes) read from the same sidecar resume uses. Double-click (or the
context entry, now labelled per kind) opens the new dialog, refreshed every
second while open, with Open file/folder. Partial HTTP tasks now record
`dest_file` from `Progress.dest_file` on the first engine tick, so partial
paths show in the row and `rm --files` no longer needs the URL scan there.
v0.3.13: browser wiring self-heals. The native-messaging host was a manual
step (`ripdl register-host`) and nothing ever repaired it, so a registration
pointing at a dev build (this machine: `target\debug\ripdl.exe`, still
0.3.11 while the app was 0.3.12) survived every upgrade and the browser kept
launching that binary for "send to ripdl". Setup now runs
`{app}\ripdl.exe register-host` (`[Run]`, `runhidden nowait
runasoriginaluser`) on every install/upgrade, so the HKCU keys and
`%LOCALAPPDATA%\ripdl\org.ripdl.host.json` always name the installed exe:
installed users no longer need the CLI step, and a hijacked registration is
corrected by the next update. `register-host` also refuses to point the
browser at a `target\debug|release` build while an installed ripdl is
registered (`--force` overrides; dev builds warn that the next install
replaces them). Verified live: `--force` from the debug 0.3.13 binary
re-created the hijack, the app updated through its own banner, and Setup's
[Run] put the manifest back on `C:\Program Files\ripdl\ripdl.exe` - a framed
`list` on stdin/stdout then answered from the installed host against the
installed daemon.
v0.3.14: the tray icon stopped costing what a browser tab costs (GUI side
only, no protocol change). (1) Rows are titled with the file the engine
writes (`dest_file` basename) instead of the URL tail, so a GitHub release
asset reads `ripdl-v0.3.3-x86_64-windows-setup.exe` instead of
`12e29558-517b-4c69-a649-8415f4e2f08f`, and the file-type glyph/category/icon
follow the real extension. (2) Hiding to the tray suspends the WebView2
(`crates/ripdl-gui/src/webview_mem.rs`: `SetIsVisible(false)` then
`SetMemoryUsageTargetLevel(Low)` and `TrySuspend`; every reveal path - tray
click/menu, second-instance forward - goes through `reveal()`, which resumes
before showing), and the webview is software-composited
(`additionalBrowserArgs = "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection
--disable-gpu"`; setting it replaces wry's defaults, so they are repeated).
Approximate A/B (2026-10-26, dev builds, 2 runs per side, same fixtures, GUI and
WebView2 combined; baseline = HEAD without these two changes, not v0.3.13; local
bench, not committed): hidden 30-90 s working set 53-60 MB vs 341-369 MB; hidden
private commit ~106 MB vs ~178 MB; visible private commit ~110 MB vs ~171 MB, most
of it the GPU process (~78 MB before, ~15 MB now); hidden CPU 0.0 % vs 2.5 %; after
reveal working set ~155 MB vs ~330 MB. Visible working set barely moves (~340 vs
~361 MB). Report working set next to private commit: working set double-counts
shared pages. Two traps worth remembering: a plain hide frees nothing (WebView2
keeps everything resident) and Tauri only flips the *window's* visible flag, never
the webview's, so `SetIsVisible(false)` is mandatory or `TrySuspend` fails with
ERROR_INVALID_STATE; resident memory also takes 30-90 s to be trimmed after
suspension, so measure late. Resume paints the last frame and the pending poll
fires at once (verified hide -> 50 s -> reveal).
On main after the tag: the single-instance guard no longer fails open when an
instance owns the pipe but cannot be poked - a GUI relaunched by an elevated
installer runs elevated, and a normal-integrity second launch cannot write to
its pipe (UIPI), which used to start a second window on the same daemon (seen
as three windows after a `-Verb RunAs` install). It now treats the "show" hint
as best effort and exits; the tray icon still restores the first window.
v0.3.15: torrent state truth + list ergonomics. (1) The real defect behind
"seeds even when paused": rqbit latches the *add-time* start flag while its
initial hash check runs, so a paused add came up live when the check ended,
and the stale intention flag made a later unpause bail with "torrent is
already live" - seen live as a task stuck at `failed` while the swarm kept
downloading a 6.6 GB ISO. Boot restore now adds torrents with
`AddTorrentOptions.paused` (never Live), the engine decides pause/unpause on
`TorrentStatsState` instead of the flag, a runner/watcher that finds the
torrent parked wakes it (bounded, cancellation still wins), and every error
exit pauses the torrent so a failed row can never keep uploading. `cmd_pause`
also pauses by info-hash immediately (no wait for a runner) and `set_state`
folds the last progress sample into the record, so a paused torrent shows its
real bytes/size after a restart. Playback of the fix: restore a paused record
-> details `finished [paused]`, no peers; `resume` -> seeding, no error.
(2) Session start is warm like qBittorrent instead of cold: a stable listen
port 4240-4243 (falls back to random), UPnP port forwarding, DHT routing-table
persistence at `<data_dir>/torrent-state/dht-nodes.json` (reuses its saved
port; task state still lives only in queue.json), and a 6 s peer-connect
timeout (rqbit defaults to 10 s, which a second/tethered uplink burns). This
targets the slow ramp: qBittorrent restarts with a warm DHT table, known
peers, a stable mapped port and uTP, while ripdl used to re-bootstrap DHT on
a random port with no incoming peers and no UPnP.
(3) Task list: resizable columns (drag header edge, double-click resets,
widths in localStorage, proportional first-run widths that never overflow),
click-to-sort every data column (second click flips, third clears; persisted),
row selection via Up/Down with the file + state in the status bar,
Enter opens details, Delete removes, remembered category filter, remembered
window size, `.torrent` drag-and-drop onto the window (opens the add dialog
with the file checklist), and Copy address in the HTTP details dialog. This
needed the first Tauri capabilities file (`crates/ripdl-gui/capabilities/
default.json`: `core:default` + event listen + window size getters/setter);
without it `setSize`/`onResized`/`onDragDropEvent`/`listen` are denied.
Backlog: selected-links capture polish, DNR fallback referer (may be
unneeded: offer-first), named-pipe handoff for the bridge (bridge itself
still speaks TCP), per-task quota polish, global daemon speed cap,
magnet paused-until-pick flow (currently the add dialog resolves metadata
before queueing; a fully paused meta-first flow would allow add-then-pick).
v0.3.16: same-network file transfer, no new dependencies. `ripdl-core::lan`
is a hand-rolled HTTP/1.1 drop box (the daemon binds 0.0.0.0:8463 only when
`lan_enabled`) with a self-contained upload page for phones
(`GET /`, `POST /api/send`, `GET /api/files`, `GET /files/<name>`,
`GET /api/check`), a sender that streams a local file with an exact
Content-Length, and a UDP discovery beacon on 8464. Security is a
per-device 6-char code (ambiguous characters skipped, persisted as
`lan_code`, rotatable) required for upload and listing unless the user
turns on "no code" (`lan_open`) for a trusted network; names are
sanitized to one path component inside the inbox; uploads land in a
token-named `.part-<8hex>-<name>` (visible as `partial` rows while they
arrive) and are renamed to a unique `name (1)` after the last byte, so two
concurrent same-name sends cannot corrupt each other. The sender pre-checks
the code (`/api/check`) so a wrong code fails before a multi-GB body starts.
RPC: `LanInfo`/`LanPeers`/`LanSend`/`LanSends`/`LanSendForget`/`LanNewCode`;
outgoing copies are progress-polled volatile entries, not queue tasks.
`apply_lan()` rebuilds the listener live on any settings change (port,
folder, on/off, code) with a short retry because a just-dropped listener
releases its port a tick later. Frontends: `ripdl lan
info|peers|send|sends|new-code|open` (send prints live progress) and the
GUI Send & Receive dialog (toolbar paper plane / File menu): address +
copy, code, new code, no-code toggle, inbox folder + open, received files
with partial badges, discovered devices to fill the address, file picker,
per-copy progress. Verified live: 5 MiB and 40 MiB copies hash-match,
wrong code answers 403 before the body, the page never leaks the code,
toggle off/on rebinds cleanly, discovery finds the local receiver.
v0.4.0: the same-network release, and everything since v0.3.15 in one tag.
Carries the LAN drop box (`ripdl-core::lan`, daemon `lan_*` settings + `Lan*`
RPC, CLI `ripdl lan …`, GUI Send & Receive) plus the hardening that landed
after the v0.3.16 tag: an upload without `Content-Length` answers 411,
Windows device names are prefixed, names sanitize on both separators
(`a\b` == `a/b`), the final name is claimed atomically so two concurrent
same-name uploads both survive (`name` + `name (1)`), in-flight uploads show
as partial rows, and `parallel_same_name_uploads_stay_intact` covers it.
Also in: the new brand (motion-arrow mark in `docs/logo.svg`, icons
regenerated by `packaging/icon/generate.ps1`, teal About accent), a
first-timer README with real screenshots and a bonding diagram, and a
Windows-CI gate for the LSD-only torrent loopback test (the runner blocks
that multicast; the ubuntu job still runs it). No RPC changes beyond
v0.3.16, so 0.3.16 frontends work against a 0.4.0 daemon.
On main after v0.4.0: **bonding follows the machine** (vendor delta 5).
`bind_ips` used to be frozen when the shared torrent session was created, so a
session built at boot before Wi-Fi finished DHCP - the normal case - stayed on
one uplink forever; settings even said "restart the daemon". `SessionOptions::
bind_ips` is now an `Arc<RwLock<Vec<IpAddr>>>` read per outgoing connection
(`Session::set_bind_ips`), `ipv4_only` is a shared `Arc<AtomicBool>`
(`set_ipv4_only`), ripdl_core exposes both, and the daemon re-reads
`bonded_nics()` every 5 s (`watch_bonding`/`sync_torrent_binding`) plus on
every `SetConfig`. Measured on a Wi-Fi + USB-tether laptop: 30 of 36 peer
connections were on the default route's **IPv6** address (IPv6 peers never hit
the bind list, which is IPv4-only), 2 on the tether. Forcing IPv4 fixed the
spread (5 Wi-Fi / 6 tether) but **cut the peer count** (36 -> 11) - the swarm
is v6-heavy here, so that policy was reverted.
What actually decides throughput is path *quality*: measured with curl against
the same host, Wi-Fi delivered 7 MB/s while the tethered phone delivered
0.24-2.1 MB/s (it degrades with mobile conditions). A fixed 50/50 split parks
half the swarm on the dead path - the "bonding is slower than one connection"
report. Vendor delta 6 meters the bytes each bound connection actually reads
(`BindMeter`, per-connection `AtomicU64`, no probe traffic) and picks new
connections with smooth weighted round-robin: weight = measured rate relative
to the fastest link (1..10), a link under 5% of the best is quarantined (0),
and every 60 s the weights reset to 1 for one tick so a recovered link can
prove itself. The daemon samples totals every 5 s, EMA-smooths the rates, and
publishes them as `Reply::Config.torrent_rates`; `ripdl config` prints
`torrent uplinks: Wi-Fi 7.0 MB/s · Ethernet 2 idle` and Settings appends the
same to the bonding line. Weights only steer *new* peer connections - peers
already connected to a dying uplink stay until they disconnect (candidate
follow-up: disconnect peers on a quarantined address). The Settings toggle
applies live (no restart prompt); bound addresses now cover global IPv6 as well
as IPv4, and the manual "Restart daemon" button stays.
An uplink must also *prove* it carries traffic: a tether that loses its data
connection keeps a link-up adapter, an address and a DHCP gateway while
dropping every packet (measured: 86% of peer dials timed out, 11 live peers vs
45 with bonding off). `engine/nic.rs::scan_uplinks` runs a source-bound TCP
handshake per address (1.1.1.1:443, 8.8.8.8:53, 1.2 s, parallel), the daemon
drops a link after two consecutive failures and re-adopts it on the first
success, and `Reply::Config.torrent_unreachable` explains a skipped uplink in
`ripdl config`/Settings.
Torrent speed display: rows derive bytes/elapsed over a 4 s window
(`TORRENT_RATE_WINDOW`) instead of trusting librqbit's estimate - measured
0-600 KiB/s from the tool across 30 s while `downloaded` was frozen, so the
number lied in the other direction too. A resume makes librqbit report 0 and
then adopt everything already on disk in one tick; `adoption_jump` (pure,
unit-tested, plus a 200 MiB/s plausibility cap) drops that baseline instead of
showing 943 MiB/s. HTTP keeps its 2 s window and tool-rate preference.
`RUST_LOG=ripdl_daemon=debug` logs one `progress sample` line per tick
(downloaded, shown rate, tool rate) - that is how the above was measured.
The Settings toolbar button is a real cog (`#i-gear`); the unused `#i-sliders`
sprite entry is gone.
GUI row/context toggle: one `toggleIntent()` (running/queued/verifying/metadata
= pause, paused/failed/timed = resume, completed torrent = seed stop/start)
feeds the row button, the context entry and Tasks > Resume/Stop all - a paused
row used to have no button at all. The context menu resolves
`closest("#ctx [data-act]")`; reading `ev.target.dataset.act` made every
icon/label click a no-op (only the blank strip of an entry worked). Parked rows
report no rate: the daemon answers `speed_bps` only for running/queued, and
frontends treat it as live (it used to keep the last sample, so a paused row
printed 8 MiB/s and inflated the total-speed strip).
GUI selection + row states, round 2: a running task with no progress sample yet
reports `probing` (the daemon infers it, no engine plumbing) and both frontends
label it "connecting" instead of `running` + 0 B for the ~6 s probe/plan phase.
Ctrl/Shift-click, Shift+arrows and Ctrl+A build a multi-selection (`selectedIds`
+ a primary `selectedId` for details/Enter) that drives bulk pause/resume/remove;
the menu counts only the rows the click would change ("Pause 3 tasks"), hides
per-row entries, and removing an unfinished task asks first (completed ones stay
one click so the flow is not slowed down). Headers are generated from `COLS`, so
View > Peers column (off by default, torrents only, width taken from the name
column) toggles a column without the header drifting; saved widths are shrunk to
fit a narrower window (`fitWidths`) instead of growing a scrollbar. The row menu
also gained "Copy address", and "Open file" only shows when a file exists.

