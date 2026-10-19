# AGENTS.md — ripdl

Standing instructions for AI agents working in this repository. Keep this file
current when build commands, architecture, or conventions change.

## What this is

`ripdl` — a free, ad-free, multi-segment download manager in Rust. An
open-source alternative to IDM (overpriced) and FDM (ad-supported). Zero
telemetry, zero ads, forever — non-negotiable, code it into no feature.

## Layout

- `crates/ripdl-core` — pure library: HTTP engine (probe/plan, multi-segment
  ranges, sidecar resume, retry/backoff, cancellation), RPC protocol types.
- `crates/ripdl-cli` — thin CLI over core + daemon RPC (`download`, `info`,
  `add`, `ls`, `pause`, `resume`, `rm`, `stop`).
- `crates/ripdl-daemon` — queue owner: persistent queue at
  `%LOCALAPPDATA%\ripdl\queue.json`, one runner per task, EMA speed tracking.
- `docs/ROADMAP.md` — phase plan; keep checkboxes current for every
  completed/added item.
- `docs/rea-findings.md` — REA research notebook (P4 browser-capture work).

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

- `.github/workflows/ci.yml`: on every push/PR, `cargo check` + offline
  `cargo test` on windows-latest and ubuntu-latest.
- `.github/workflows/release.yml`: on tag `v*` (plus `workflow_dispatch`
  dry run - packages everything, no release). Version source = workspace
  Cargo.toml (tag must match). Artifacts: `ripdl-vX.Y.Z-x86_64-windows-`
  `{portable.zip,setup.exe}` (Inno script `packaging/windows/ripdl.iss`,
  Sources relative to repo root), `ripdl-vX.Y.Z-x86_64-linux.tar.gz`,
  deb+rpm via nfpm (`packaging/linux/nfpm.yaml`, installed with go),
  `ripdl-browser-extension-vX.Y.Z.zip` + `SHA256SUMS.txt` in the release
  job. Cut releases by bumping the workspace version, then:
  `git tag -a vX.Y.Z; git push origin vX.Y.Z`.
  Proven against v0.2.0 (2026-10-18): release published with all 7 assets.
  Gotchas folded in: CLI bin is `ripdl` (packaging must not invent names),
  `crates/ripdl-gui/icons/icon.png` is required (tauri-build embeds it on
  non-Windows), nfpm placeholders are `sed`-resolved in the workflow
  (don't rely on its env expansion), nfpm needs `--packager deb/rpm` when
  `--target` is a directory, and Inno wants
  `ArchitecturesInstallIn64BitMode` (bare `Architectures` is invalid).

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
watched the "Me at the zoo" task end-to-end (`ripdl add … --video` →
completed webm in Downloads), and the popup chain proven over CDP in
headless Edge (branded Chrome ≥137 ignores `--load-extension`; a repurposed
driver lives at `research/ext-e2e-cdp.js`): `ripdl_send` add and
`ripdl_hunt_query` answered from the real popup-page context with the task
completing on disk; `rm --files` now drains the runner before the path
sweep so the pause path can no longer resurrect a sidecar after deletion
(fix in `cmd_remove`, daemon). Media-URL offer rows and activeTab-granted
page reads remain user-confirmable only in a real browser session.
50 tests pass offline (`cargo test --workspace`), including an in-proc
seeder↔leecher torrent round-trip.
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
`version` literal pinned every build to 0.3.2).
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
Backlog: selected-links capture polish, DNR fallback referer (may be
unneeded: offer-first), named-pipe handoff for the bridge (bridge itself
still speaks TCP), per-task quota polish, global daemon speed cap,
magnet paused-until-pick flow (currently the add dialog resolves metadata
before queueing; a fully paused meta-first flow would allow add-then-pick).
