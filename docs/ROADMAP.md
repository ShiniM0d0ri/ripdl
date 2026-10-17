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
- [x] Add-URL dialog (`add_task` command + URL/category form, per-task
      "Save to" location with native folder browser, hunt toggle)
- [x] Location + file management: per-row destination display, open
      file/folder, remove-with-files (`Remove { delete_files }`, sidecar-aware,
      `rm --files` in the CLI); GUI rows re-render only on real changes and
      daemon auto-spawn is rate-limited
- [ ] Settings: category folders, segment count, concurrency
- [ ] System tray + minimize-to-tray

## P3 - torrent support (via librqbit, Apache-2.0 — never a GPL client)

Plan (single UI, kind-routed backend, exact same queue/pause/resume semantics):

M3.1 - core engine (`ripdl-core::torrent`):
- [x] Dep: `librqbit = { version = "9", default-features = false,
      features = ["default-tls"] }` (own the session inside the daemon
      process; no rqbit HTTP dashboard)
- [x] Payload detection: `magnet:` URI | local `.torrent` path | `.torrent`
      URL (fetched by us via reqwest so the display name is known up front;
      bencode sniff `d<digit>:`, `looks_like_torrent_payload`)
- [x] `TorrentSession`: one `librqbit::Session` (DHT on, explicit ephemeral
      ports + no rqbit-side persistence - our queue records own state),
      per-task add with `AddTorrentOptions` (`output_folder`, `only_files`)
- [x] Per-task-owned artifact root: `<dest>/<display name>` via the
      torrent name (metainfo or magnet `dn=`), always set as the absolute
      `output_folder` (rqbit takes folder-or-subfolder, never both; without
      it files spill straight into the dest root)
- [x] Progress sampling: per-torrent stats → the same `Progress` stream
      HTTP uses (downloaded/total/speed; phase kept internal for now)
- [x] Pause = rqbit pause through the cancel token (piece state survives
      on disk), resume = re-ensure + unpause; completion = `finished`
- [x] `TaskKind::Torrent`; boot-resume re-adds unfinished torrents from the
      queue record (fresh session re-verifies existing data); completed
      torrents stay records-only (no silent re-seeding)

M3.2 - daemon + CLI:
- [x] Runner dispatch: torrent branch mirrors the HTTP branch (one runner per
      task, same queue, same progress/cancel semantics; shared session behind
      an async-mutex lazily created on first torrent)
- [x] `dest_file` = the torrent's root folder; `delete_files` removes the
      whole folder via `remove_dir_all` (per-task root, never the dest
      root); completed torrent size = recursive folder sum
- [ ] RPC `TorrentDetails { id }` → `{ name, info_hash, size, files
      [{index,name,size,wanted}], peers [{addr,client,flags,down,up,progress}],
      trackers }`
- [x] CLI: `add` auto-detects torrent payloads (+ explicit `--torrent`)

M3.3 - GUI merge (HTTP + torrent in one grid):
- [ ] Row model: kind-specific status labels (metadata/verifying/seeding),
      torrent name replaces URL basename; per-file selection drives overall %?
      (report wanted-bytes progress)
- [ ] Details dialog (double-click / context menu): summary line + tabs
      Files (checkbox list, Apply → `update_only_files`), Peers (addr, client,
      flags, down/up, progress; 2s refresh), Trackers
- [ ] Toolbar: "Add Torrent" (native file picker for .torrent);
      Add-URL dialog auto-detects magnet/.torrent and routes to hunt-style
      second phase for metadata fetch
- [ ] Sidebar adds "Torrents" (kind filter, not extension)

M3.4 - tests / verification:
- [x] Offline integration: in-proc seeder ↔ leecher over localhost with a
      generated .torrent - byte-identical payload, live progress, timeouts
- [x] Unit: payload detection heuristics, magnet dn decode/sanitization,
      metainfo name extraction, http fetch routed through resolve_payload
- [x] Live: real-world legal torrent (Sintel): 8 MiB/s swarm fill,
      mid-flight pause/resume at 61%, correct folder + counters completed,
      `rm --files` wipes the task-owned folder
- [ ] Pause/resume across daemon restart (boot-resume re-verify path)
- [ ] Clean-room check: behavior parity notes only, no copying from
      qBittorrent/FDM code or assets; findings → docs/rea-findings.md

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
