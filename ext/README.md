# ripdl WebExtension

MV3, no content scripts, no telemetry, no cookie harvesting.

## Install (development)

1. Build and expose the native host:

   ```
   cargo build
   ripdl register-host --ext-id <EXTENSION-ID>
   ```

   `<EXTENSION-ID>` comes from chrome://extensions -> Developer mode ->
   **Load unpacked** -> this directory -> the ID shown on the card. Then
   click **Reload** on the extension card once (nativeMessaging manifests
   are read at extension startup; a reload connects it).

2. What `register-host` does: writes
   `%LOCALAPPDATA%\ripdl\org.ripdl.host.json` pointing at the ripdl binary
   and sets the per-user HKCU keys for Chrome and Edge. No admin window.

3. Start the daemon (`ripdl-daemon`) or let the host spawn it automatically
   on first capture (it looks for `ripdl-daemon` next to the ripdl binary).

## How capture works

- Context menu on a link/page -> the URL is queued to the daemon.
- Right-click over a text selection -> every link inside the selection is
  queued; an empty-area menu offers "all page links" (capped at 100). The
  collector is injected at click time via `chrome.scripting` - there are NO
  resident content scripts.
- While the daemon answers, browser-initiated downloads are intercepted in
  `downloads.onDeterminingFilename` and re-queued fast (with their Referer
  and browser User-Agent so protected links still resolve). If the daemon
  disappears, captures stop interfering immediately: the browser downloads
  normally.
- The popup controls two switches: intercept on/off, and cookie sharing.
  Cookie sharing is an optional permission - turning it on makes the
  BROWSER ask you; turning it off revokes the permission entirely. Cookies
  travel only to the local daemon, only attached to the downloads you fire.
- The daemon owns all policy. Nothing is hardcoded here by design.

## Hunt offers (video popup)

- Open the popup on a video/stream page (YouTube, Bilibili, Twitch...) and
  a "Download page as video" button appears - it queues a *video* task the
  daemon runs through the `yt-dlp` sidecar.
- Direct media the page actually streamed (containers by content-type or
  file extension, observed in the same webRequest listeners capture uses)
  shows up as one-click "Download media file" rows (top 5, newest last).
- Detection is permission-minimal: no content scripts, no resident page
  access. The page URL is read once via `activeTab`, which Chrome grants
  only while the popup is open. Media sightings live in the worker's
  memory for 15 minutes and vanish with it.

## Deliberately not present (yet)

- Referer rewrite of fallback reissues during daemon death: not needed by
  design. ripdl offers the task to the daemon BEFORE disturbing the browser
  download (offer-first), so the browser item is never canceled for a task
  the daemon didn't commit to. If the daemon dies after accepting, the task
  is resumable from the daemon side.
