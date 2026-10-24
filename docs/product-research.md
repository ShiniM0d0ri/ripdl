# What makes a good download manager (2026 survey)

Product research for ripdl. Sources are public docs/pages read on 2026-10-19
(links at the bottom). Nothing here is copied code; it is a feature/UX survey
plus a gap analysis against ripdl's current state.

## 1. The field

| Tool | Model | Strengths | Weaknesses people report |
|---|---|---|---|
| **IDM** (Windows, paid) | native, browser hook | dynamic segmentation + connection reuse, site grabber (recursive crawl with filters), scheduler, AV scan hook, video grabber, FTP/HTTPS/MMS, shell integration | paid + license nuisance, Windows-only, dated UI |
| **Free Download Manager** | native, multi-platform | free, torrents, traffic control, media preview, video from sites, categories | installer/offers halo, site pushes extras, heavier UI |
| **JDownloader 2** | Java, plugin corpus | the best *link-container* workflow (paste → LinkGrabber → packages), ~1800 site plugins, captcha handling, auto-extract, reconnect, history | Java runtime, heavy/slow UI, adware installer history (and a 2026 site compromise that served malware), per-site plugin sprawl, captcha-service dependency |
| **Xtreme Download Manager (XDM)** | Java, open source | multi-connection, HLS/DASH video grabber muxed to MP4, browser extension, proxies/auth, batch | maintenance gaps, bug reports about glitchiness on Windows |
| **Gopeed** | Go + Flutter, open source | multi-connection HTTP, torrents/magnets/ed2k, JS extensions, REST API/CLI, MCP, post-download scripts, auto-extract, cross-platform, no Electron | no scheduler/queues, no site grabber, no AV hook, no FTP, basic auth only |
| **AB Download Manager** | Kotlin/Compose, open source | fast segmentation, queues + scheduler, browser extensions, themes, multi-platform | younger ecosystem, fewer "capture" workflows |
| **Motrix / Persepolis / Varia** | aria2 frontends | aria2 engine: segments, Metalink, JSON-RPC, BT/DHT, SFTP/FTP, checksums | GUI is a skin; UX/maintenance varies |
| **Neat Download Manager** | macOS-first | IDM-like simplicity + media capture | macOS-centric |
| **DownThemAll** | browser extension | batch download of links/images from a page, filters | no app-level engine, sandbox limits |

Browser-native downloads are the real baseline now: resumable-ish, but
single-stream, no queue control, no retry policy, no packages, no extraction.

## 2. Table stakes (a DM without these is not considered)

1. **Resume that actually works** across app restarts and network failures,
   with integrity checks (ETag/If-Range, size verification).
2. **Segmented HTTP** with sane defaults and connection reuse; dynamic
   re-segmentation when a segment stalls (IDM's differentiator).
3. **Queue control**: pause/resume/cancel, retry with backoff, concurrency
   cap, speed limit, scheduler (`--after`/`--at` equivalents).
4. **Browser integration** that is reliable: capture the real URL + referer +
   cookies, and fall back cleanly to the browser when the app is off.
5. **A clear list UI**: status, speed, ETA, size, destination, per-row actions,
   search/filter, categories.
6. **No ads, no telemetry, no bundled offers** — this is now a *feature*
   (JD2's installer reputation follows it around; FDM's offers do too).

## 3. Differentiators (what makes people switch)

- **Link containers / grabber**: paste or copy a list → one package → one
  folder. This is why people still run JD2 despite the UI: it is the only tool
  that eats "release page → 9 filehost links → one folder".
- **Package semantics**: aggregate progress/speed per package, per-package
  destination, per-package extraction.
- **Auto-extract with part-set awareness** (know a set is incomplete, wait,
  then extract; password lists).
- **Video/stream capture**: HLS/DASH or yt-dlp-like extraction, one click from
  the page.
- **Scheduling for off-peak**, shutdown-when-done, queues per category.
- **Power surface**: CLI, RPC/API, post-download hooks/scripts, custom
  headers/proxy/auth.
- **A trustworthy update channel** (signed artifacts, checksums) — the 2026
  JD2 site compromise is a direct lesson.

## 4. What users complain about (the exploitable gaps)

- JD2: Java + memory + slow start; adware-installer reputation; the official
  installer is a trust hazard after the 2026 compromise; captcha solving is
  effectively paid/third-party.
- FDM: installer extras/offers, notifications, "free but nudge-y".
- IDM: pay + license blocks; Windows-only; UI aging.
- Gopeed/AB: missing scheduler/grabber/extraction depth, younger.
- XDM/Motrix: maintenance stalls; engine is great, UX is a wrapper.
- Everyone: browser capture breaks on MV3 edge cases, 0-byte races, POST
  bodies, referer rewriting.

## 5. ripdl today (grounded in docs/ROADMAP.md)

Have: segmented HTTP with sidecar resume, retry/backoff, cancellation,
daemon + queue persistence, header/cookie/referer pass-through,
Content-Disposition naming, per-task + global speed limits, scheduler
(`--after`/`--at`), torrents (librqbit: magnets, per-file selection,
sequential), hunt mode (yt-dlp), GUI (task list, details, files/peers/trackers,
tray, settings, update check + verified install), CLI, MV3 extension capture.

Missing vs the field: grabber/packages, archive extraction, categories,
per-host limits, proxy config, FTP/SFTP, Metalink, video grabber in-app
(beyond yt-dlp sidecar), site/recursive grabber, shell integration, AV hook,
post-download hooks, history, queue priorities, bandwidth scheduling.

## 6. Recommended order (impact × effort)

1. **Grabber + packages + extraction** (P5, planned) — the JD2-shaped
   differentiator; nobody else combines this with a fast native engine and no
   ads/telemetry.
2. **Hoster resolution + session reuse + browser handoff** (P6, planned) —
  supports multi-part file-host workflows through normal browser handoff.
3. **Categories + per-category folder/queue/speed policy** (small, expected;
   also gives the scheduler a queue dimension).
4. **Proxy + per-host concurrency + priority ordering** (IDM/Gopeed parity;
   small engine work, high expectation).
5. **Post-download hooks (script) + AV scan hook + shell integration**
   (cheap, visible, "power user" feel).
6. **Video grabber in-app** (HLS/DASH via yt-dlp remains the honest route;
   in-app DASH muxing is a big project — keep the sidecar).
7. **Metalink / checksum verify / FTP** (completeness, low urgency).
8. **Site grabber** (recursive filtered crawl; IDM's niche — only after
   packages exist; reuse the same filter model as the grabber).

## 7. UX principles for the grabber-first app

- **No modal storms.** One grab → one reviewable package dialog; everything
  else inline (rename, destination, per-link toggles).
- **Instant feedback**: grabber row appears immediately, parsing/badge state
  visible; "n links found, m duplicates dropped".
- **Explicit conversion**: staging → queue is a visible action ("Start"), with
  bulk start/pause; nothing downloads silently by default.
- **Honest states**: `waiting for parts`, `needs browser`, `verifying`,
  `extracting` — never a spinner that means "stuck".
- **Per-row truth**: exact bytes done/total, real ETA (EMA), destination,
  source/resolved URL copyable.
- **Keyboard-first**: add from clipboard, focus search, start/pause selected,
  delete with confirm.
- **Density + themes**: dark/light, compact rows, no wasted chrome.
- **Debuggable**: every failed task has a reason + a "copy diagnostics" button
  (URL, headers sans secrets, last error). This is a support-cost killer.

## 8. What ripdl can own (positioning)

- **No ads, no telemetry, no bundle, forever** — with a *verifiable* supply
  chain: signed installers, SHA256SUMS, in-app verified updates.
- **Engine-first + offline-tested**: deterministic tests, no third-party
  network in CI.
- **One protocol everywhere**: CLI, GUI, extension, RPC all speak the same
  daemon protocol (MCP-style automation later).
- **Release-workflow native**: paste → package → folder → extract is a
  first-class path, not a plugin ecosystem.
- **Honest about limits**: no anti-bot bypass; browser handoff when a host
  insists on a human.

## 9. Benchmarks to hold ourselves to

- cold start < 1 s (GUI first paint), idle RSS < 100 MB, per-task overhead
  small with 50 tasks;
- resume byte-exact after kill -9 at random points (tested);
- 4–8 connections by default with adaptive segment count, not 32;
- no download stalls without a visible state change within 5 s;
- extractor: set-completeness detection under 50 ms for a 50-part folder.

## 10. Sources

- TechRadar, "Best free download manager of 2026" (Ninja, FDM, JDownloader
  picks + what to look for): techradar.com/best/free-download-manager
- Gopeed README + feature list, GopeedLab/gopeed (protocols, JS extensions,
  MCP, REST/CLI, auto-extract).
- Gopeed discussion #1162, "Gopeed vs IDM capability matrix" (dynamic
  segmentation, scheduler, site grabber, AV scan, advanced auth, FTP, shell
  integration, video capture) — a useful checklist of expectations.
- AB Download Manager README/site (segmentation, queues + scheduler, themes,
  browser extensions, multiplatform).
- XDM README (multi-connection, HLS/DASH mux to MP4, proxies/auth, batch).
- aria2 manual/README (HTTP/FTP/SFTP/BT/Metalink, JSON-RPC/XML-RPC, segments).
- IDM Grabber documentation (recursive site crawl with page/file filters,
  project templates, saved projects, scheduling).
- Community reports: JD2 adware-installer complaints; Malwarebytes (2026-05)
  on the JDownloader site being compromised to serve malware; an unverified
  report describes handing a copied link list to JD2/IDM when a file host
  presents a Cloudflare challenge (not a guaranteed workflow).
