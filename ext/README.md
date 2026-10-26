# ripdl WebExtension

MV3, no telemetry, no cookie harvesting. One resident content script, and it
only listens for `copy`.

## Install (development)

1. Build and expose the native host:

   ```
   cargo build
   ripdl register-host
   ```

   `register-host` refuses to point the browser at a `target\debug` /
   `target\release` build when an *installed* ripdl is already registered
   (that is how a stale dev binary ends up serving real downloads); pass
   `--force` when you really want the dev build.

   **Installed ripdl needs no manual step**: Setup runs `ripdl register-host`
   for the installed binary on every install/upgrade, which also repairs a
   registration left pointing at a dev build.

   The extension pins its own ID (see below), so `register-host` needs no
   ID by default: it allows the pinned one everywhere. Extra dev/build IDs
   (e.g. an older loaded copy) can be added with `--ext-id <ID>`, repeatable
   — and previously allowed IDs are never dropped on re-register.
   Then click **Reload** on the extension card in chrome://extensions once
   (nativeMessaging manifests are read at extension startup).

2. Browsers: `register-host` sets the per-user HKCU keys for **Chrome,
   Edge and Brave**. What it writes:
   `%LOCALAPPDATA%\ripdl\org.ripdl.host.json` pointing at the ripdl binary.
   No admin window.

3. Start the GUI. It copies the bundled extension to `<data_dir>/extension`
  and updates that app-managed folder when a newer bundled version is
  available.
4. In Settings > **Browser extension**, click **Open extension settings**.
  Enable **Developer mode**, choose **Load unpacked**, and select the folder
  shown there. The browser's final confirmation stays manual.
5. Start the daemon (`ripdl-daemon`) or let the native host spawn it
  automatically on first capture.

After an app update, the browser keeps its previously loaded extension until
you click **Reload** on the extension card in chrome://extensions. Settings
reports the loaded version and whether a newer copy is waiting. User scripts in
`hosts/local/` are preserved when the app-managed folder updates.

### Pinned extension ID

`manifest.json` embeds an RSA public key (`"key"`), which overrides
Chromium's default unpacked-ID derivation from the load path. Without it,
loading the zip from a different folder silently changes the ID and the
native host rejects it ("daemon not detected"). With the pin, every unpacked
load of *any* ripdl extension build has id
`ripdl-core::bridge::PINNED_EXT_ID` (`cimcjenbnacepkgebkmpkggjoahdmggc`).

## How capture works

- Context menu on a link -> the link goes through the same hoster plugins as the chip, then is queued with the page as Referer.
- Right-click over a text selection -> the links inside the selection are
  grouped the way the copy chip groups them, then queued: parts of one
  multi-part set become ONE bundle task (one folder), single links stay
  ordinary downloads. The collector is injected at click time via
  `chrome.scripting`.
- **Copy capture** (`capture.js`): when you copy a selection that contains
  links, a small offer chip appears near it - "Send 3 links to ripdl", the
  groups the daemon would make ("x - 3 of 3 parts"), and Send / Edit /
  Dismiss. Nothing is queued until you click Send. The script reads the
  selection only in response to a copy you made, never touches the
  clipboard, and the chip lives in a shadow root so page CSS cannot touch
  it. An anchor's visible text is used as the part filename only when it
  actually names a file (a page full of "Download" anchors must not rename
  every part "Download"). An anchor with no file-looking label is offered only when the copied text itself has an http(s) URL, or when a plugin or known file type claims its URL.
- Grouping is the daemon's job (`preview_bundle`), so the chip, the GUI
  dialog and the CLI all agree. A daemon that predates bundles is detected
  through its `config` reply (`bundles` capability flag) and the offer
  degrades to one ordinary download per link - the chip says so.
- While the daemon answers, browser-initiated downloads are intercepted in
  `downloads.onDeterminingFilename` (with their Referer and browser
  User-Agent so protected links still resolve). The daemon probes the link
  first, and the browser's copy is cancelled only once the daemon has queued
  it. If the daemon refuses or does not answer within 3 seconds, the browser
  keeps the download. If the daemon disappears, captures stop interfering
  immediately.
- The popup controls two switches: intercept on/off, and cookie sharing.
  Cookie sharing is an optional permission - turning it on makes the
  BROWSER ask you; turning it off revokes the permission entirely. Cookies
  travel only to the local daemon, only attached to the downloads you fire.
- The daemon owns all policy. Nothing is hardcoded here by design.

## Hoster plugins (user-loadable)

The copy chip only knows generic URLs. Hoster plugins teach it links only
some pages understand - a share link that must be POSTed to the host before
a direct download URL exists, or a filename hidden in the URL fragment.

**No hoster ships built-in.** The only built-in is `generic` (direct links
with a known archive/media extension). Everything else is yours:

- **Recipes** (`.json`, inert data - no code, importable from the Plugins
  page): a declarative match/name/resolve map. `resolve` fills a URL
  template, sends one GET/POST, and extracts the direct URL from a response
  header or a JSON dot-path, guarded by an optional prefix. See the Plugins
  page for the current shape.
- **Scripts** (`.js`, full API: `id`, `match(url)`, `name(url)`,
  `resolve(link, ctx)`): run with the extension's powers, so they load ONLY
  from the selected extension's `hosts/local/` - never from the network. The
  easy way: `ripdl plugin add <file.js>` or Settings > "Install hoster plugin..."
  copies the file into the app-managed extension by default and regenerates
  `hosts/local/registry.js`; use `--ext-dir` or the Settings override for a
  manually loaded copy. Then reload the extension.
  By hand: drop the file in `hosts/local/`, add one namespace import + entry
  to `hosts/local/registry.js` (static imports: the service worker cannot use
  dynamic `import()` - the HTML spec forbids it on
  ServiceWorkerGlobalScope), then reload the extension.

The Plugins page (popup -> "Hoster plugins…" or the extension's Details >
Extension options) lists every plugin, shows a script's full code BEFORE its
enable toggle unlocks, and has a Test box: paste any URL, see the name it
would group under and the direct URL it would resolve to. Nothing is queued
by a test.

Trust rules, by design: no eval, no remote plugin code, no captcha/paywall
logic, no skipping of waiting timers or ads, no stored credentials. A plugin's `resolve()` may only talk to the
host it matches; a failed resolve never drops a link - it queues as-is and
the chip says how many parts were not resolved.

## Hunt offers (video popup)

- Open the popup on a video or stream page and
  a "Download page as video" button appears - it queues a *video* task the
  daemon runs through the `yt-dlp` sidecar.
- Direct media the page actually streamed (containers by content-type or
  file extension, observed in the same webRequest listeners capture uses)
  shows up as one-click "Download media file" rows (top 5, newest last).
- Detection is permission-minimal: no content scripts, no resident page
  access. The page URL is read once via `activeTab`, which Chrome grants
  only while the popup is open. Media sightings live in the worker's
  memory for 15 minutes and vanish with it.

Use responsibly: you are responsible for copyright and for the terms of any site you download from. ripdl does not bypass DRM, paywalls, logins, or captchas.

## Deliberately not present (yet)

- Referer rewrite of fallback reissues during daemon death: not needed by
  design. ripdl offers the task to the daemon BEFORE disturbing the browser
  download (offer-first), so the browser item is never canceled for a task
  the daemon didn't commit to. If the daemon dies after accepting, the task
  is resumable from the daemon side.
