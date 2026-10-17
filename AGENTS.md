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
- **No telemetry of any kind.**
- Commits: use `git -c user.name="ripdl" -c user.email="ripdl@localhost" commit …`.
  One commit per coherent unit, terse imperative subject.

## CI / release

- `.github/workflows/ci.yml`: on every push/PR, `cargo check` + offline
  `cargo test` on windows-latest and ubuntu-latest.
- `.github/workflows/release.yml`: on tag `v*` → release builds for
  `x86_64-pc-windows-msvc` (zip: ripdl.exe, ripdl-daemon.exe, `ext/`) and
  `x86_64-unknown-linux-gnu` (tar.gz), attached to an auto-generated GitHub
  Release. Cut releases by pushing an annotated tag:
  `git tag -a vX.Y.Z; git push origin vX.Y.Z`.

## Resume-sidecar format

`<file>.ripdl.json` (`TaskStateFile` with per-segment `done` offsets).
Sentinel `u64::MAX` = UNBOUNDED (no range support → single stream; retries
restart the stream from zero). Segment writes are buffered (256 KiB) and
flushed via the blocking pool; `cells[index].done` advances only for durable
bytes.

## Current status

Check `docs/ROADMAP.md` checkboxes for ground truth. As of 2026-10-17:
P0/P1 core complete: multi-segment download, resume, retry, daemon + RPC,
persistent queue, buffered writes, shared speed limiter, header/auth/cookie
pass-through, Content-Disposition naming, duplicate-URL idempotency,
scheduler (`--after`/`--at` with delayed runner start), named pipe transport
(pipe-first client + dual listeners; TCP kept as fallback; unix socket on
non-Windows), daemon config file (`config.json`: max_concurrent,
max_segments, min_segment_size, max_speed_bytes, default_download_dir),
hunt mode (video/stream tasks via the `yt-dlp` sidecar: `add --video`,
`ytdlp_path` config key, percent-based progress, pause = kill sidecar,
resume = re-invoke which continues `.part` files).
GUI: `ripdl-gui` (Tauri 2 shell, `crates/ripdl-gui` + static `ui/`):
IDM-style task manager over daemon RPC (`list_tasks`/`add_task`/pause/
resume/remove commands, static frontend embedded at build time, no bundler;
auto-spawns the daemon like the bridge). P4 extension stays live; hunt
offers from the popup are the remaining P4 thread.
50 tests pass offline (`cargo test --workspace`), including an in-proc
seeder↔leecher torrent round-trip.
In progress: P2 GUI polish — basic file management landed (per-task "Save to"
with native browse, per-row location display, open file/folder via Explorer
`/select`, `remove_task(delete_files)` with sidecar-aware disk cleanup,
`rm --files` CLI flag, GUI render-on-change, spawn rate limit in the bridge);
remaining P2: settings panel (daemon config) + tray.
P3 merged-T3.1 landed (see ROADMAP M3.1/M3.2): `ripdl-core::torrent` over
librqbit 9 — magnet/.torrent/.torrent-URL payloads, one shared daemon session
(ephemeral ports, no rqbit persistence), task-owned `<dest>/<name>` root
folder, same Progress stream + pause/resume/cancel as HTTP, CLI auto-detect
(`--torrent`), GUI add auto-routes torrents; M3.3 Details RPC/peers/files is
the remaining P3 thread. Daemon counters now sum torrent folder trees.
Backlog: selected-links capture polish, DNR fallback referer (may be
unneeded: offer-first), named-pipe handoff for the bridge (bridge itself
still speaks TCP), per-task quota polish,
`rm` sidecar cleanup, global daemon speed cap.
