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
- [x] Settings: daemon-wide dialog over `config.json` (concurrency,
      segments, min segment size, shared speed cap with byte/suffix
      parsing, default download folder with native browser, yt-dlp path,
      hide-to-tray); RPC `GetConfig`/`SetConfig`, `ripdl config
      [key=value ...]`, concurrency applies live, the rest on next task
- [x] System tray + minimize-to-tray (Tauri tray icon: left click shows,
      menu "Show ripdl"/"Exit"; close minimizes to tray unless
      `hide_to_tray=false`; preference lives in config.json)

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
      queue record (fresh session re-verifies existing data); completed and
      paused torrents are re-attached too (see M3.4), so details survive a
      daemon restart

M3.2 - daemon + CLI:
- [x] Runner dispatch: torrent branch mirrors the HTTP branch (one runner per
      task, same queue, same progress/cancel semantics; shared session behind
      an async-mutex lazily created on first torrent)
- [x] `dest_file` = the torrent's root folder; `delete_files` removes the
      whole folder via `remove_dir_all` (per-task root, never the dest
      root); completed torrent size = recursive folder sum
- [x] RPC `TorrentDetails { id }` → `{ name, info_hash, size, files
      [{index,name,size,included}], peers [{addr,client,state,down,up}],
      trackers }`
- [x] CLI: `add` auto-detects torrent payloads (+ explicit `--torrent`)
- [x] Sequential download mode: `--sequential` flag → `TorrentConfig`
      streaming drain keeps one open `FileStream` per file (metadata order)
      so the piece picker follows the read position; persisted on the
      record and replayed on resume/boot
- [x] CLI `select <id> all|0,2,3` + RPC `UpdateTorrentSelection`;
      `only_files` persisted and replayed
- [x] Pre-add file selection (qB-style): RPC `TorrentPreview` resolves a
      payload's metadata + file list without queueing (rqbit `list_only`;
      instant for `.torrent`, swarm-wait for magnets), CLI `add --only
      0,2-4`; GUI Add dialog goes two-phase for torrent payloads
      ("Retrieving metadata…" → inline file checklist → add with the
      checked files); sequential checkbox shows only for torrent payloads
      (verified: librqbit honors add-time `only_files`; boundary pieces carry
      interleaved side data exactly as with any client)

M3.3 - GUI merge (HTTP + torrent in one grid):
- [x] Row model: kind-specific status (metadata phase when a magnet still
      has no bytes), torrent display name replaces the URL basename,
      Torrents count in the sidebar
- [x] Details dialog (double-click / context menu): summary line + tabs
      Files (per-file sizes + verity progress bars, want column),
      Peers (addr, client, state, counters; 2s refresh while open), Trackers
- [x] Toolbar: "Add Torrent" (native file picker, Ctrl+T); Add-URL dialog
      auto-detects magnet/.torrent and routes to the torrent engine
- [x] Sidebar "Torrents" (kind filter, not extension)
- [x] Files tab "Apply" checkbox editing -> `update_only_files` (want
      checkboxes park in pending state until Apply pushes
      `UpdateTorrentSelection`; whole-Want-cell click toggles; paused/
      completed tasks persist without a live-session push)

M3.4 - tests / verification:
- [x] Offline integration: in-proc seeder ↔ leecher over localhost with a
      generated .torrent - byte-identical payload, live progress, timeouts
- [x] Unit: payload detection heuristics, magnet dn decode/sanitization,
      metainfo name extraction, http fetch routed through resolve_payload
- [x] Live: real-world legal torrent (Sintel): 8 MiB/s swarm fill,
      mid-flight pause/resume at 61%, correct folder + counters completed,
      `rm --files` wipes the task-owned folder
- [x] Live GUI: Files-tab want checkboxes -> Apply (CLI `details` shows
      `included=no`, `only_files` persisted in queue.json, resume finishes
      with the excluded files still unwanted)
- [x] Pause/resume across daemon restart (fresh-session re-verify: existing
      files on disk previously failed the add with "error creating a new
      file"; `AddTorrentOptions.overwrite = true` makes the re-add
      hash-check and resume them)
- [ ] Clean-room check: behavior parity notes only, no copying from
      qBittorrent/FDM code or assets; findings → docs/rea-findings.md

M3.5 - seeding lifecycle + post-download visibility (2026-10-19):
- [x] Torrents restored into the shared session at daemon boot (completed
      and paused; `only_files` replayed), so Files/Peers/Trackers keep
      working after a restart - previously the session started empty and
      Details errored with "torrent session not running"
- [x] Persistent seeding: a finished torrent stays in the swarm; a daemon
      watcher (no download semaphore slot) reports uploaded bytes, up rate
      and connected peers into the task record once per 2 s
- [x] Seeding limits: `seed_enabled` (default on), `seed_ratio` (default
      1.0, 0 = unlimited), `seed_time_min` (default 0 = unlimited); limits
      pause the swarm socket but keep the torrent inspectable
- [x] Row: `seeding` state (teal up icon) with upload rate in the speed
      cell; kind tooltip shows uploaded/ratio/peers; row + context-menu
      toggle stops (pause) or restarts (resume) seeding
- [x] Details dialog: summary shows uploaded/ratio/peers/up rate; all tabs
      live after completion and after daemon restarts
- [x] Lifetime uploaded persisted (rqbit's counter is per-session), so the
      ratio survives restarts; stale live rates cleared at boot
- [x] Settings dialog: seeding toggle, ratio and time limit; applying the
      toggle stops/starts watchers live (no restart)
- [x] Offline test: create torrent -> finished on-disk layout -> boot
      restore -> finished details -> pause/resume seeding
- [x] Clean-room qBittorrent options inventory (275 controls / 9 pages):
      docs/qbittorrent-options.md + docs/rea-findings.md

## P4 - browser integration (the killer feature)

- [x] REA analysis of FDM / IDM browser extensions: how URL capture +
      native messaging actually work, and what telemetry they send
- [x] Our own WebExtension + Rust native messaging host
- [x] Click-to-capture: intercepted links flow into the daemon queue
- [x] Hunt mode: video/stream tasks via the `yt-dlp` sidecar (`add
      --video`; `ytdlp_path` in config.json; progress parsed from the
      tool's lines; pause/resume = kill/continue)
- [x] Popup hunt offers: the extension popup lists direct media files
      seen in the tab (webRequest content-type/ext sniffing, no content
      scripts, no new standing permissions) and offers "download page
      as video" for known stream sites (activeTab, granted only while
      the popup is open)

## P5 - ship it

- [x] Packaging: GitHub Release pipeline (portable zip + Inno setup.exe,
      tar.gz + deb/rpm, extension zip + SHA256SUMS) - proven v0.2.0. The
      Windows setup upgrades in place (fixed `AppId`, closes a running
      daemon/GUI, refreshes `ext/`) and the uninstaller removes the
      per-user native-messaging keys; "Updating"/"Uninstalling" documented
      in the README.
- [x] Update mechanism: core `update` module (GitHub Releases probe, v-semver
      compare) + RPC `CheckUpdate` + `Reply::Update`; GUI auto-check once per
      launch (setting `update_check`, default on) with a release-page banner,
      Help menu item, CLI `ripdl update`. Information only - never
      auto-downloads; one api.github.com GET, no telemetry.
- [x] Auto-update install: verified download path (`UpdateAsset` from release
      JSON, SHA256SUMS.txt check, streamed to `<data_dir>/update` with size cap,
      .part-atomic swap, re-use of a previously verified installer), RPC
      `UpdateInstall` (busy-guarded), GUI banner "Download & Install" +
      `update_auto_install` opt-in setting (daemon-side unattended
      `/VERYSILENT` run at boot; installer replaces ripdl in place), CLI
      `ripdl update --install`. Old installers pruned after a week.
      v0.3.4 fixes the asset URL: downloads must use `browser_download_url`
      (the API `url` is JSON metadata, so every real install failed the
      SHA256SUMS gate); the GUI exe version now inherits the workspace
      version instead of a stale `tauri.conf.json` literal; the installer is
      launched through `ShellExecuteW("runas")` so a non-elevated daemon gets
      the UAC prompt instead of error 740 (silent/unattended runs skip when
      not already elevated). v0.3.5 fixes the installer's own `taskkill /T`:
      Setup is a child of the daemon when the update is triggered from the
      app, so the tree kill terminated Setup itself mid-install (name-only
      kills now; the daemon stops its video runners before handing over, and
      `[Run]` uses `runasoriginaluser`). v0.3.6 launches Setup with the shell's
      default verb so Setup elevates itself and the relaunched GUI is not left
      elevated, and adds a real About dialog (Help > About ripdl: running
      version via the new `app_version` command, licence, links). v0.3.7
      installs with `/SILENT` (UAC only, no wizard, app relaunches itself)
      and hides a stale update banner when a check reports "current".
- [ ] Packaging: winget / scoop / Homebrew, AUR
- [ ] Website + README showdown (screenshots, honest feature table)
- [ ] i18n

## P6 - speed & reach

- [x] NIC bonding: combine bandwidth of every uplink interface (Wi-Fi +
      Ethernet + tethering) by binding segments per interface
      (`bond_interfaces` in config.json / `ripdl download --bond` / GUI
      settings; per-IP server caps bypassed, VPN tunnels excluded)
- [ ] Segment visibility in the GUI (active connections + ranges per task)

## P7 - core control (engine power features)

- [ ] Per-task engine controls in GUI + CLI: max connections, per-task speed
      cap, header/referer/UA overrides (config plumbing mostly exists)
- [ ] Proxy support: HTTP + SOCKS5, `config.json`, GUI settings, CLI
      `--proxy`; applies to probe + segments + torrent session
- [ ] Task priority + queue ordering: `--priority`, GUI move up/down,
      scheduler respects priority
- [ ] Per-host concurrency limit (`max_per_host`, default off) so one host
      cannot starve the queue
- [ ] Global daemon speed cap exposed in GUI (engine + config already have it)
- [ ] Mid-download segment map: active connections, byte ranges, stalls;
      re-divide a stalled segment from busy neighbours
- [ ] Post-download checksum: sha256 verify when a hash is known
      (sidecar/manifest/`--sha256`), warn on mismatch

## P8 - task UX + OS integration

- [x] Single app instance: a second launch focuses the running window and
      exits (named-pipe/unix-socket guard in the GUI, no duplicate daemons)
- [x] GUI chrome: icon-only toolbar + SVG state/row icons (one inline sprite,
      tooltips for labels), sidebar category icons, 60 s total-speed strip with
      now/avg readout (canvas, DPR-scaled), inline installer-failure reason in
      the banner, About dialog under Help (version, licence, links, update
      check)
- [x] Torrent vs HTTP at a glance: dedicated icon-only kind column (magnet /
      download / film with tooltips), category-coloured file-type glyph in the
      name cell, kind hint in the row tooltip, "Torrent details..." hidden for
      non-torrent rows, icon context menu (toggle retitled Pause/Resume per
      row); volatile cells now addressed by class so columns can't shift them
- [x] Seeding UX: teal "seeding" row state with upload rate, Files/Peers/
      Trackers stay available after completion and restarts, seed start/stop
      from the row + context menu, ratio/time limits in Settings
- [ ] Categories: per-category destination, queue slot, speed policy;
      auto-categorize on add (extension heuristic already exists)
- [ ] Search, filter (status/kind/host), sort columns; multi-select batch
      pause/resume/remove
- [ ] History: completed/removed tasks with redownload
- [ ] Completion notifications (tray balloon) + drag-and-drop URLs /
      `.torrent` files onto the window
- [ ] Windows Explorer context menu ("Download with ripdl") + `ripdl://`
      protocol handler
- [ ] Diagnostics: every failed task exposes a reason + "copy diagnostics"
      (URL, headers without secrets, last error)

## Packaging / reach (backlog)

- [ ] winget / scoop / Homebrew, AUR
- [ ] Website + README showcase (screenshots, honest feature table)
- [ ] i18n
