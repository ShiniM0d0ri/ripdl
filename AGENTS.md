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
max_segments, min_segment_size, max_speed_bytes, default_download_dir).
34 tests pass offline (`cargo test --workspace`).
In progress: P4 browser integration — REA research done; bridge +
`register-host` + MV3 extension landed and capture verified live twice
(temp Chrome profile -> daemon -> file). Extension now has: SW-race matching
(700ms + content-length + initiator), correct suggest/cancel discipline,
popup with capture toggle and OPT-IN cookie sharing (optional permission,
off = permission revoked entirely), selection/all-links capture via
on-demand scripting, 256 KiB seize floor. Tests: 34 pass offline.
Backlog: selected-links capture polish, DNR fallback referer (may be
unneeded: offer-first), named-pipe handoff for the bridge (bridge itself
still speaks TCP), video/stream hunt mode, per-task quota polish,
`rm` sidecar cleanup, global daemon speed cap.
