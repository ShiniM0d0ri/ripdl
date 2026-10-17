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
- [ ] Atomic sidecar writes
- [ ] Retry/backoff per segment; retry on flaky connections
- [ ] Integration tests against a local test HTTP server

## P1 - daemon + persistence

- [ ] `ripdl daemon`: long-running process owning the queue
- [ ] Local RPC (JSON over named pipe / unix socket): add, status, pause,
      resume, remove, set-speed
- [ ] Queue persistence (JSON sidecar first; SQLite only if it earns it)
- [ ] Speed limiting (token-bucket)
- [ ] Scheduler (start/stop at times; off-peak windows)

## P2 - GUI (Tauri)

- [ ] Task list: progress, speed, per-task pause/resume/cancel
- [ ] Add-URL dialog + paste/clipboard monitor
- [ ] Settings: category folders, segment count, concurrency
- [ ] System tray + minimize-to-tray

## P3 - torrent support

- [ ] Integrate `librqbit` (Apache-2.0) as a `Transport` behind the same task API
- [ ] .torrent + magnet support, sequential download option
- [ ] Unified queue view (HTTP and torrent tasks side by side)

## P4 - browser integration (the killer feature)

- [ ] REA analysis of FDM / IDM browser extensions: how URL capture +
      native messaging actually work, and what telemetry they send
- [ ] Our own WebExtension + Rust native messaging host
- [ ] Click-to-capture: intercepted links flow into the daemon queue
- [ ] Optional video-grab: wrap `yt-dlp` as a sidecar (do NOT rebuild it)

## P5 - ship it

- [ ] Packaging: winget / scoop / msi, portable zip
- [ ] Website + README showdown (screenshots, honest feature table)
- [ ] i18n
