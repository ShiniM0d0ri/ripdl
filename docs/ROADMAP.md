# ripdl roadmap

Guiding rules:

1. **Engine first, frontends thin.** Anything reusable lives in `ripdl-core` or
   the daemon; GUI/CLI/extension are shells.
2. **MVP = the 20% of IDM features 80% of people use.** No grabber, no site
   crawler, no plugin system until the core is boring and solid.
3. **Zero telemetry, zero ads, forever.** Non-negotiable.

## P0 - core engine + CLI (current)

- [x] Workspace: `ripdl-core` lib + `ripdl` CLI
- [x] URL probe (HEAD, ranged-GET fallback): size, accept-ranges, etag, filename
- [x] Multi-segment planner (min segment size, max segments)
- [x] Segment fetcher writing at absolute file offsets
- [x] Sidecar resume state (`<file>.ripdl.json`), etag-guarded
- [x] Cancellation tokens (cancels propagate to all segments)
- [x] In-memory queue + auto-categorization
- [x] Atomic sidecar writes (temp + rename)
- [x] Retry/backoff per segment (bounded; transport errors only)
- [x] Integration tests against a local test HTTP server
      (`crates/ripdl-core/tests/engine_integration.rs`: offline mock server
      with drop-injection, no-range mode, etag switching; covers exact-byte
      correctness, retry recovery, single-stream fallback, pause→resume from
      sidecar, etag-change fresh restart)
- [x] Buffered async segment writes (256 KiB blocks through the blocking
      pool; slow disks no longer stall the network pipeline)
- [x] Request headers pass-through (`--header`/`--referer`/`--cookie`/`--user-agent`;
      engine default_headers so probe + segments carry them; user UA wins)
- [x] Content-Disposition filename (RFC 5987 decode + basename sanitization)
- [x] Daemon idempotency: same URL + dest shares the live task (no file
      trample)

## P1 - daemon + persistence

- [x] `ripdl-daemon`: long-running process owning the queue (TCP localhost RPC v1)
- [x] Local RPC (newline-delimited JSON over TCP): add, ls, pause, resume, rm, stop
- [x] Queue persistence (`%LOCALAPPDATA%\ripdl\queue.json`, atomic writes)
- [x] Named pipe / unix socket transport (`\\.\pipe\ripdl-rpc` on Windows,
      `<data_dir>/rpc.sock` elsewhere; clients try it first and fall back to
      TCP loopback — no fixed-port conflicts, daemon keeps both listeners)
- [x] Daemon config file (`<data_dir>/config.json`, all fields optional):
      `max_concurrent`, `max_segments`, `min_segment_size`,
      `max_speed_bytes`, `default_download_dir`; unreadable file = warn +
      built-in defaults
- [x] Speed limiting (shared token-bucket across all segments; CLI
      `download --max-speed 500k|2.5m|1g`)
- [x] Scheduler (`add --after 1h30m` / `--at 22:30`; runner holds Queued
      until the wall clock honors it; cancels cleanly while waiting; start
      times persist across daemon restarts)

## P2 - GUI (Tauri)

- [x] Task list: progress, speed, per-task pause/resume/cancel (`ripdl-gui`,
      IDM-style: toolbar, category sidebar, grid with inline progress bars,
      context menu, status bar; auto-spawns the daemon on first poll)
- [x] Add-URL dialog (`add_task` command + URL/category form)
- [ ] Settings: category folders, segment count, concurrency
- [ ] System tray + minimize-to-tray

## P3 - torrent support

- [ ] Integrate `librqbit` (Apache-2.0) as a `Transport` behind the same task API
- [ ] .torrent + magnet support, sequential download option
- [ ] Unified queue view (HTTP and torrent tasks side by side)

## P4 - browser integration (the killer feature)

- [x] REA analysis of FDM / IDM browser extensions: how URL capture +
      native messaging actually work, and what telemetry they send
- [x] Our own WebExtension + Rust native messaging host
- [x] Click-to-capture: intercepted links flow into the daemon queue
- [x] Hunt mode: video/stream tasks via the `yt-dlp` sidecar (`add
      --video`; `ytdlp_path` in config.json; progress parsed from the
      tool's lines; pause/resume = kill/continue)

## P5 - ship it

- [ ] Packaging: winget / scoop / msi, portable zip
- [ ] Website + README showdown (screenshots, honest feature table)
- [ ] i18n
