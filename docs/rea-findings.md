# REA investigation findings

Working notes from REA (Reverse Engineer Anything) sessions. Purpose:
understand how the ad-supported incumbents implement browser capture and
what they phone home - so ripdl can do the useful part and none of the
creepy part. We record behavior and protocols here; no decompiled code or
proprietary assets are copied into ripdl.

## Target: Free Download Manager (browser extension)

- Status: analyzed (entry 1, 2026-10-17)
- Target: FDM Chrome extension MV3 v3.3.4 (ext id `ahmpjcflkgiildlgicmcieglgoilbfdp`,
  min Chrome 88), shipped CRX fetched via
  `https://clients2.google.com/service/update2/crx?response=redirect&prodversion=131.0.6778.204&acceptformat=crx2,crx3&x=id%3D…%26installsource%3Dondemand%26uc`
  (346,443 bytes, `Cr24` v3; header stripped: 1,309-byte protobuf header + raw zip).
- Analysis root: `research/fdm-ext` (gitignored), root sha256
  `1a7492ad3818d0c07fa0b96e865642316ec3e8d31fbabf57c13b17f56bb3dac5`.
- Method: REA MCP `rea.analyze_javascript_application` on the extracted tree
  (27 JS files parsed, summary Evidence `ev_362e9e9a…72a5069e`, complete
  Evidence `ev_5775a1a5…c4c704a6`, 40 MB evidence bundle exported to
  `research/rea-fdm-evidence.json`), then direct reads of every `src/js/*`
  module (they ship unminified sources alongside a bundled worker).
- Findings summary: capture = **watch + cancel + handoff** with graceful
  fallback; transport = Chrome **native messaging** (JSON over stdio); the
  desktop app owns all policy and the UI string pack; no network telemetry in
  the extension itself, but major privacy gouges in what it reads.

### Architecture map (`src/js`, background = `dist/js/service_worker.js` module bundle)

| File | Role |
|---|---|
| `main.js` | startup; MV3 clears dynamic DNR rules; `onMessageExternal` gate |
| `fdmextension.js` | wiring; pushes `ui_strings` + `query_settings` + `key_state` at ready |
| `webextension.js` | Chrome/Edge/Firefox compat shim (MV2/MV3 differences) |
| `dldsinterceptmgr.js` (+ `fdmdldsinterceptmgr.js`) | **the capture brain** (25 KB + subclass) |
| `nativehostmgr.js` + `RequestsManager.js` | native-messaging port, id-matched req/resp |
| `fdmbhtasks.js` | protocol task shapes (JSON payloads) |
| `cookiemgr.js` | cookie harvesting (`cookies.getAll`) |
| `netwrkmon.js` + `fdmnetwrkmon.js` | client-passthrough net header feed (video sniffing lane) |
| `tabsmanager.js` | per-tab video flag; polls app 1×/s for video detection |
| `contextmenumgr.js` + `contextmenuhlpr.js` | right-click menus driven by text selection events |
| `fdmscheme.js` + `fdmschemecatch.js` | `fdm:` URL scheme click-catching content script |
| `youtubeutils.js` | YouTube channel/playlist page detection (URL + DOM selectors) |
| `installationmgr.js` | one-shot "install our app" popup funnel |

### Capture mechanism (answers Q1)

TwoFull legacy lane and MV3 lane, both always compiled in:

- **MV2 blocking lane** (Firefox + any MV2 build): `onBeforeRequest` records
  `RequestDetails {url, method, postData (re-encoded FormData), tabId,
  initiator/documentUrl, time}` keyed by requestId for Firefox, by URL for
  Chrome (120 s TTL maps). `onHeadersReceived` decides *file-ish*: any
  `content-disposition` ⇒ file; content-type rules (json/image/text/js/protobuf
  skipped, `application/x-bittorrent` always file, most other
  `application/*` on GET ⇒ file); size floor (`skipSmallerThan`) applies only
  in the non-ODE path. To seize a GET it issues `{cancel: true}`; for POST it
  hijacks with `{redirectUrl: "javascript:"}` (a cancel here breaks the page,
  per their comment) and then returns the tab to the referrer. A sniffed
  download is handed to `onDownloadIntercepted`.
- **MV3 lane** (Chrome 88+, blocking webRequest forbidden):
  `downloads.onDeterminingFilename` — when the browser is *about to* open a
  download item, match `requestDetailsByRequestUrl`, then kill the browser's
  own download (`downloads.cancel` + `downloads.erase`) and hand the details
  to the app. Non-blocking `onSendHeaders`/`onHeadersReceived` still supply
  headers; a 700 ms + content-length-match heuristic covers service-worker
  downloads where zero-length items race the intercept.
- Capture is unconditional-but-filtered: `blob:`/`data:`/`filesystem:` and
  Google-Docs MIME types are excluded; lists for skip/catch extensions and
  skip-servers come from the app; a modifier key held while clicking disables
  capture (`key_state` push from the app); capture can be paused globally.
- **Fallback is always available**: `allowBrowserDownload` (app setting) +
  `returnDownload()` re-issues the download through `browser.downloads.download`
  (method/body/saveAs preserved; referer rewritten at the wire by a temporary
  `onBeforeSendHeaders` listener because Chrome's `downloads.download` can't
  set referer), then the temp listener self-cleans on completion. If the app
  rejects a task (`result == "0"`) or errors, the extension falls back
  automatically. SloppyMoment: app offline ⇒ extension idle ⇒ normal browser
  behavior.

### Native messaging protocol (answers Q2)

- Host name `org.freedownloadmanager.fdm5.cnh` (FDM5+), legacy fallback
  `com.vms.fdm`; JSON values over `runtime.connectNative`.
- Every message is `{id: "<string>", type: "...", ...payload}` — ids are
  *strings* but first id `"0"`/missing gets assigned (old servers expect
  numeric-compatible strings); responses carry the same id; unanswered pushes
  (settings, key state) arrive without an id. `RequestsManager` keeps
  `id → callback` in a Map (performs request = assign + send).
- Task types seen: `handshake {api_version: "1", browser}` → response carries
  app `version` (feature-gated);
  `ui_strings` (the **extension pulls its UI text pack from the app**);
  `query_settings` → all capture policy; `post_settings`;
  `key_state {pressed}` (unsolicited);
  `video_sniffer` request per tab url (1×s polling);
  `network_request_notification` / `network_request_response_notification`
  (raw HTTP transcripts for every tab request);
  **`create_downloads {catchedDownloads: "1", waitResponse: "1",
  downloads: [...]}`** where each entry is
  `{originalUrl, finalUrl, httpReferer, httpPostData, httpCookies,
  userAgent (navigator.userAgent), documentUrl, suggestedName, ...}` —
  suggestedName comes from parsing `Content-Disposition` in-page.
  Response `{id, result}`: `result != "0"` = accepted by the app;
  `"0"`/error = rejected ⇒ browser fallback.
- `fdm:` scheme path: content script catches trusted `<a href="fdm:…">`
  clicks (`document.documentElement` click listener, `e.isTrusted`) →
  `create_downloads` with cookies + UA. Workable for our own scheme links.
- Context-menu implementation is selection-driven: content script fires
  `selectionchange`/mousedown/visibility events to the worker with the
  link list from the text selection; the worker builds menus from app settings
  (`browser.menu.{dllink,dlall,dlselected,dlpage,dlvideo,dlYtChannel}`).

### Cookie handoff

`cookies.getAll({url, partitionKey: {}})` (partitioned too on Chrome 110+),
joined as `name=value;` strings, forwarded to the app on every capture and
every `fdm:` click. **No user prompt, no per-site memory** — silent harvest of
everything clickable.

### Third-party domains (answers Q3)

- **Zero non-target network traffic found in extension JS.** No analytics,
  no beacons; the only remote touches are passive:
  `files2.freedownloadmanager.org` (sole allowed `onMessageExternal` sender,
  and the only external command is `"uninstall"` =
  `management.uninstallSelf()` — vendor site can order it to remove itself),
  `*.freedownloadmanager.org` (`externally_connectable`) support/download
  links, YouTube URL detection.
- All data instead flows **locally** to the desktop app: every request/response
  header pair (net monitor), all cookies, all selection contents. That local
  funnel is the privacy cost — the browser is instrumented `<all_urls>`, 4
  content scripts at `document_start` on every frame, plus the `history`
  permission (used *only* in the install-funnel check, but granted forever).

### Install funnel

`installationmgr` stores `installedFromStore / hasShownInstallWindow /
hasConnectedToHost`; a store-detected install without a native host shows
`install.html` popup once ("install our desktop app"); store-vs-sideload is
guessed by scanning open tabs and `history.search` for the store URL at
install time (why it wants `history`). Firefox gets a separate `consent.html`
page on install (disclosure requirement for the data it collects).

### Adopt vs. reject for ripdl

Adopt (behavioral patterns, clean-room reimplementation):
1. **Watch → cancel → handoff → fallback** shape, MV3-first:
   `downloads.onDeterminingFilename` (cancel + erase) as primary seizure, with
   identical `DownloadInfo` fields (original/final url, referer, post data,
   cookies, document url, suggested name, UA).
2. **JSON-over-stdio native host with id-matched requests** — same framing we
   planned (u32 LE length + UTF-8 JSON); string ids; `handshake {api_version,
   browser}` carrying the engine version; response contract "rejected ⇒
   browser proceeds" instead of error dialogs. Our daemon's existing RPC
   (TCP 5593) stays authoritative; the native host process is a thin stdio↔RPC
   bridge, so CLI/GUI/extension speak one protocol.
3. **Policy lives in the app**: size floor, skip/catch extensions, skip
   servers, global pause, hold-key bypass, `allowFallback` — streamed to the
   extension via a `settings` push; the extension never hardcodes policy.
4. Header collection: capture **request** headers + response headers of the
   very request that produced the download (that's what the engine's new
   `--header` pass-through consumes); POST body preserved via non-blocking
   `onBeforeRequest` observation.
5. Return-to-browser fallback leverages `downloads.download({method, body,
   saveAs})` + transient `onBeforeSendHeaders` referer rewrite; extension
   degrades silently when ripdl daemon is off.
6. `fdm:`-style scheme click-catching (`ripdl:` links) — cheap, useful.

Reject (or do inversely — the honest product diff):
1. No `history` permission. Store-vs-sideload guessing is not worth集体
   surveillance; detect *first app connect* instead.
2. No silent cookie harvesting: cookies ship only with per-site opt-in
   (extension action badge shows "sharing with ripdl" state; default ask).
3. No `<all_urls>` content scripts at `document_start`. Content scripts only
   for context-menu selection support (on demand), no one-per-second tab
   polling, no every-requestnet header broadcast unless the user turns on
   "link/capture hunt" mode explicitly.
4. No vendor-domain remote-control messaging (`externally_connectable` /
   `onMessageExternal`); no remote uninstall self-execution.
5. No "install our app" popup funnel; a state badge + one settings entry.
6. UI strings ship in the extension (our own `i18n/`), not pulled from a
   proprietary binary.

### Threads to pull later

- FDM's Windows native host binary + app IPC (only if we need deeper protocol
  parity; the extension-side contract above suffices for our own protocol).
- IDM Integration Module (id `ngpaspampappnmepgiliojookmmbnmkpigb` unconfirmed)
  — same three questions; expect MV2 blocking-WebRequest + a server-free
  "agent" process.
- MV3 edge cases worth porting: their 700 ms service-worker download
  workaround race, `onDeterminingFilename` response `{filename: "", conflict:
  "uniquify"}` trick to let the browser proceed without our rewrite.

## Target: IDM (Internet Download Manager) extension

- Status: pending first analysis
- Same questions as above.

## Template for entries

- Target / version
- Method (REA commands used)
- Findings (protocol, endpoints, telemetry)
- What ripdl will adopt (protocol ideas) vs. reject (telemetry, nags)
