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

## Target: JDownloader 2 (JD2 core + Extraction extension)

- Status: analyzed (entry 1, 2026-10-19)
- Target: JD2 as shipped 2026-10-19 — official bootstrapper
  `installer.jdownloader.org/JDownloader.jar` (5.19 MB) self-updated headless
  (`java -jar JDownloader.jar -headless`) into `research/jd/install`:
  `Core.jar` (9.19 MB), `JDownloader.jar`, `extensions/Extraction.jar`,
  `libs/sevenzipjbindingWin.jar`, `libs/zip4j.jar`, plus **loose plugin
  classes** `jd/plugins/hoster/*.class` (1144) and `jd/plugins/decrypter/*.class`
  (686).
- Method: bootstrapper run + `jar tf` inventory + `javap` superclass census +
  CFR 0.152 decompilation of the relevant packages into `research/jd/src`
  (456 files), then targeted reads. Decompiled output is GPLv3 — behavior only,
  nothing copied into ripdl. A PrivateBin paste was decrypted with the site's
  own open client format to see a real payload.

### Architecture map

| Artifact | Contents |
|---|---|
| `Core.jar` | entire core app: `jd.controlling.*` (linkcollector/linkcrawler/downloadcontroller), `jd.plugins.*`, `org.jdownloader.gui.views.{linkgrabber,downloads}`, `org.jdownloader.settings.*` |
| `JDownloader.jar` | launcher/updater only (`org.jdownloader.update.launcher.JDLauncher`) |
| `extensions/Extraction.jar` | `org.jdownloader.extensions.extraction.*` |
| `jd/plugins/{hoster,decrypter}` | loose `.class` files, hot-updated independently of the core |

Plugin census (`javap` over every plugin class):
- hosters: 653 `extends PluginForHost`, 214 `XFileSharingProBasic`,
  87 `KernelVideoSharingComV2`, 51 `YetiShareCore`, 50 `antiDDoSForHost`;
- decrypters: 543 `PluginForDecrypt`, 64 `antiDDoSForDecrypt`,
  25 `PornEmbedParser`, 12 `MightyScriptAdLinkFly`, 7 `AbstractPastebinCrawler`.

Takeaway: per-site support is ~4 generic base engines + ~1200 bespoke classes.
There is no universal resolver; the reusable part is only the HTTP plumbing
plus the generic container/crawler logic.

### Extraction extension — adoptable specification

Multipart detection (`ArchiveType` enum, priority order; filename regex first,
magic bytes validate/disambiguate):

| Type | Pattern | first/min part |
|---|---|---|
| `RAR_MULTI` | `(?i)(.*)\.(part\|p)(\.?)(\d{1,3})(\..*?\|)\.rar$` | 1 / 2 |
| `RAR_MULTI2` | `.*\.(\d{3})\.rar$` | 0 / 1 |
| `RAR_MULTI3` | `.*\.([r-z]\d{2})$` + `.*\.rar$` | 0 / 0 |
| `RAR_SINGLE` | `.*\.rar$` | — |
| `SEVENZIP_PARTS` | `.*\.7z\.(\d{1,3})$` | 1 / 1 |
| `ZIP_MULTI` | `.*\.zip\.(\d{1,3})$` | 1 / 2 |
| `ZIP_MULTI2` | `.*\.z(ip\|\d{1,3})$` | 0 / 0 |
| `RAR_MULTI4` | `.*\.(\d{3})$` (RAR signature required) | 1 / 2 |
| singles | 7z, zip, lzh, tar, arj, cpio, tgz, gzip, bzip2 | — |

- Completeness gate: `finalizeArchive` pads a positional list with
  `MissingArchiveFile` up to `max(getMinimumNeededPartIndex(), highestFound)`,
  so a lone `x.part01.rar` is *incomplete*; jobs are queued only for complete
  sets (`DummyArchive.isComplete`), and an incomplete set is silently re-checked
  on the next `onNewFile` (never queued in a broken state).
- Trigger: `ExtractionExtension implements FileCreationListener` — extraction
  fires when the *set* is complete, not when the whole package is; concurrent
  jobs allowed (`ExtractionQueue`), deep extraction on by default for archives
  inside archives (blacklist patterns default to `.exe/.msi/.bat/.xbe/.elf/.sh`).
- Output: per-archive `extractPath` → global custom path → default
  (package/download folder); template variables `%PACKAGENAME%`, `%ARCHIVENAME%`,
  `%HOSTER%`, `$DATE:…$`, `%SUBFOLDER%`; optional subfolder only when
  `subpathEnabled` (default **false**, `subPath` default `%PACKAGENAME%`) and the
  target has ≥ threshold entries. Entry paths are segment-cleaned but JD has
  **no canonical containment check** — do not copy that lapse.
- Defaults (`ExtractionConfig`): `ifFileExistsAction=ASK_FOR_EACH_FILE`,
  `deleteArchiveFilesAfterExtractionAction=NO_DELETE`,
  `deleteArchiveDownloadlinksAfterExtraction=false`,
  `deleteInfoFilesAfterExtraction=false`, `deepExtractionEnabled=true`,
  `passwordList=[]`, `askForUnknownPasswordsEnabled=true`,
  `useOriginalFileDate=true`, `restoreFilePermissions=true`,
  `CPUPriority=HIGH`, `IOModeForCrcHashing=NORMAL`, `FlushMode=NONE`.
  Auto-extract gate is per-archive (`ArchiveSettings.autoExtract`, tri-state)
  falling back to `CFG_LINKGRABBER.AUTO_EXTRACTION_ENABLED`.
- Passwords: per-archive list, guessed candidates (download password, archive
  filename/base name), global list, then an interactive dialog. No
  `password.txt`/archive-comment reading.
- Statuses: `ExtractionStatus` = NA/IDLE/RUNNING/ERROR_PW/ERROR/SUCCESSFUL/
  ERROR_CRC/ERROR_NOT_ENOUGH_SPACE/ERRROR_FILE_NOT_FOUND (sic); exit codes
  0/1/2/3/4/5/9/10/255 in `ExtractionControllerConstants`; partial outputs are
  deleted on failure; no automatic retry (`ExtractArchiveNowAction`,
  `ValidateArchivesAction` are the manual re-triggers).
- Engine: sevenzipjbinding (in-process native 7-Zip) via `Multi` for
  rar/rar5/7z/tar/gz/bz2/lzh/arj/cpio, `Zip4J` for zip, plus four byte-level
  split mergers (`SplitUtil`, HJSplit/Unix/Xtream/Hacha).
- An external `7z x` invocation covers the archive part but *not*: discovering
  and validating the part set, split-type files, password-list trials
  (single `-p` only), CRC pre-check (`7z t`), permission restore, or
  delete-after cleanup.

### Pastes, plugins, and a real FitGirl payload

- `AbstractPastebinCrawler.decryptIt` = fetch page → extract plaintext field →
  `HTMLParser.getHttpLinks(text)` → one link per URL; package name = paste
  title; plus an optional `DirectHTTP` link for the raw paste text
  (`PastebinCrawlerSettings.pastebinPlaintextCrawlMode`).
- Password sniffing: `PasswordUtils.getPasswords(text)` — four regexes around
  the localized word "password" (quote/colon/eq/space forms), then filters
  (empty, len<2, `<span`/`<td` fragments, anything ending in a file extension).
  Hits are attached to every extracted link
  (`DownloadLink.setSourcePluginPasswordList`) and feed the extractor.
- The user's FitGirl paste is a **PrivateBin 1.6.22** page. Decrypted with its
  open client format: JSON `{"paste":"##Download links\n- https://fuckingfast.co/<id>#<Name>.part01.rar\n…"}` —
  9 links, filenames carried in the **URL fragment**, forming a 9-part RAR set,
  no password line.
- PrivateBin format (from the shipped `privatebin.js`, reimplemented in
  `research/jd/paste_probe.js`): base58 fragment key → PBKDF2-SHA256
  (salt/iterations from `adata`) → AES-256-GCM; GCM AAD = JSON of the whole
  `adata` array; ciphertext base64 with 16-byte tag appended; raw-deflate
  payload (`zlib` label); API `GET /?<id>` + `X-Requested-With:
  JSONHttpRequest`.
- `jd.plugins.hoster.FuckingfastCo` shows why this is not a plain-HTTP job:
  filename from the URL fragment, size/title from the page, then
  `handleFree` needs a **Cloudflare Turnstile** token
  (`CaptchaHelperHostPluginCloudflareTurnstile`) before it can follow
  `Hx-Redirect` to the real file URL; `hasCaptcha()` = true.

### Grabber / link collection

Data flow (each step a class):
`LinkCollectingJob`(raw text + origin + modifiers) → `LinkCollector.addCrawlerJob`
→ `JobLinkCrawler` → `LinkCrawler.find()` runs `HTMLParser.getHttpLinks(text)`
over the text → one `CrawledLink` per URL → decrypter plugins then hoster
plugins (`PluginForDecrypt.decryptIt`, `PluginForHost.getDownloadLinks`) →
`DownloadLink` wrapped as `CrawledLink` → `LinkCollector.handleFinalLink`
→ optional `LinkChecker` → packagizer → `addCrawledLink` (the single insert
point; fires `ADDED_LINK` and `AutoStartManager.onLinkAdded`). Provenance is
kept: each `CrawledLink` carries `sourceUrls[]` (original page/URL chain).

Two lists, two models: grabber = `LinkCollector` over
`CrawledPackage`/`CrawledLink`; download list = `DownloadController` over
`FilePackage`/`DownloadLink`. "Move to download list"
(`LinkCollector.moveLinksToDownloadList` → `convert`) builds `FilePackage`s and
**removes** the children from the grabber.
The grabber **is persisted**: `cfg/linkcollector.zip` + rotated
`linkcollectorN.zip`, autosave debounced 5 s/60 s, enabled by
`GeneralSettings.SaveLinkgrabberListEnabled` (true), 5 rotations kept, saved on
shutdown and restored at boot. Filtered-out links are in-memory only.

Grouping: the target package comes from a `PackageInfo`
(name/folder/uniqueId/packageKey/comment/ignoreVarious) supplied by the add
dialog, a packagizer rule, or a plugin. If no name is given, JD derives it
**from the filename** (`LinknameCleaner.derivePackagenameFromFilename`) or from
the archive name (`ArchivePackagizerEnabled` true, multi-part archive). Merge
key = normalized `(uniqueId, packageName, downloadFolder)`; unnamed links can
also merge by shared container/origin URL. Special buckets: "Permanently
Offline", "Offline", and "**Various**" (`VariousPackageLimit` = **1**, so with
defaults a 1-link package is literally titled "Various").
Default behavior is therefore **one package per file name, not per paste** —
the opposite of what a download-a-release workflow wants.

Duplicates: key is `DownloadLink.getLinkID()` (plugin `LINKDUPEID`, else the
plugin's URL pattern matcher) — not name/size. Two independent switches
(`LinkCollectorConfig.DupeManagerEnabled` for the grabber,
`GeneralSettings.DupeManagerEnabled` for the download list). Confirmed dupes
are handled by `OnDupesLinksAction` with default **ASK**; nothing is merged
automatically.

Availability: `LinkChecker` (on when `LinkCollectorConfig.DoLinkCheck` = true)
runs after crawling on a per-host pool (`LinkCheckerConfig.MaxThreads` = 4).
States `AvailableStatus` UNCHECKED/FALSE/UNCHECKABLE/TRUE →
`AvailableLinkState` UNKNOWN/OFFLINE/TEMP_UNKNOWN/ONLINE, shown in a
grabber-only "Availability" column ("online/total" for packages). It never
blocks adding; the offline decision at confirm is `OnOfflineLinksAction`
(default **ASK**), and auto-start always skips OFFLINE links.

Auto-start is really auto-*confirm*: `AutoStartManager` moves links to the
download list and starts them. Globals: `LinkgrabberAutoStartEnabled` **true**
but `LinkgrabberAutoConfirmEnabled` **false** ⇒ with defaults nothing moves by
itself; links wait in the grabber. When enabled it fires after crawl
completion, debounced (`AutoConfirmDelay` 15000 ms), is cancellable
(`AutoConfirmStopAction.interrupt()`), and in AUTO mode all dialogs are
suppressed.

Clipboard: `ClipboardMonitoring` thread; on Windows
`WindowsClipboardChangeDetector` polls `GetClipboardSequenceNumber()` every
**100 ms** (fallback sleep-poll 200→1000 ms). Qualification is change-only
plus a length floor (text > 7 chars, HTML > 14, URI-list > 7) — no URL regex at
this layer. Settings: `ClipboardMonitored` true, `ClipboardSkipMode`
`ON_STARTUP`, `ClipboardMonitorProcessHTMLFlavor` true, process blacklist
(empty). A grab builds a `LinkCollectingJob` with origin CLIPBOARD, attaches
`PasswordUtils.getPasswords` results, and takes the browser's HTML source as
the source URL. The visible reaction is `NewLinksActionV2` = **switch to the
grabber tab** — no dialog, no modal.

Ordering/metadata: children are appended but `AutoSortChildrenEnabled` is
**true** (name-sorted), new packages go last, `LinkgrabberAddAtTop` false.
Per-link: forced filename, `comment`, `sourceUrls[]`, password, priority,
enabled, auto-extract; per-package: comment (line-wise merged on merge
actions) and custom destination (`PackageInfo.destinationFolder`).
Extras: job-level cancel, a deep-analyse fallback when a crawl yields nothing
(can learn persistent `LinkCrawlerRule` DIRECTHTTP rules), crawler pool
`LinkCrawlerConfig.MaxThreads` = 12, and "filtered" is a distinct, restorable
state from "deleted".

### Package & download model

`FilePackage` extends a generic `Property` bag: `downloadDirectory` (**absolute
only**; relative or empty falls back to the default folder), child list, `name`,
`uniqueID`, `sorter`, and properties `COMMENT`, `PRIORITY`, `PACKAGE_KEY`,
`ALLOW_MERGE`, `ALLOW_INHERITANCE`, `CLEANUP_NAME` (true). There is **no
package-level password/forced flag** — those live on the link. Children
ordering: binary insert when a sorter exists (default `SORTER_ASC`, natural
name; `AutoSortChildrenEnabled` true), else append.

`DownloadLink` filename precedence (`getName`): `FORCED_FILENAME` →
`FINAL_FILENAME` → raw name → plugin default → URL-derived →
`"unknownFileName"`. `setName` sanitizes via `LinknameCleaner.cleanFilename` +
`CrossSystem.alleviatePathParts`. Distinct URLs per link: plugin pattern
matcher (real download), content/origin/referrer/container. Sizes:
`VERIFIEDFILESIZE` vs transient estimate. `CHUNKS` (0 = auto) capped by
`MaxChunksPerFile` (**default 1**); per-host concurrency
`MaxSimultaneDownloadsPerHost` = 1 behind a gate that is **off** by default
(`MaxDownloadsPerHostEnabled` false).

Destination = `<package.downloadDirectory>/<link name>`. Layers in order:
1. `GeneralSettings.getDefaultDownloadFolder()` — OS Downloads known folder,
   **no placeholders**;
2. package folder (`LinkTreeUtils.getDownloadDirectory`: absolute as-is,
   relative = default + relative, empty = default);
3. built-in rules `SubFolderByPackageRule` (`<jd:packagename>`, **on**) and
   `SubFolderByPluginRule` (`<jd:subfolderbyplugin>`);
4. user Packagizer rules (`PackagizerSettings.RuleList` default `[]`, engine
   on, first `isStopAfterThisRule` wins);
5. placeholders `<jd:packagename|orgpackagename|orgfilename|orgfiletype|`
   `default_download_path|subfolderbyplugin|hoster|source|job.*|env:NAME|`
   `simpledate|prop|globprop|indexof|append>` — note there is **no
   `%USERNAME%`**, only `<jd:env:USERNAME>`;
6. package naming heuristic `LinknameCleaner.derivePackagenameFromFilename`
   strips multipart suffixes (`.part01.rar`, `.r01`, `.z01`, `.7z.001`, …);
   otherwise "Various" for singletons.

Extraction uses a *different* template syntax: `%PACKAGENAME%`,
`%ARCHIVENAME%`, `%HOSTER%`, `%SUBFOLDER%`, `$DATE:…$`.

Conflicts: `GeneralSettings.getIfFileExistsAction()` = OVERWRITE / SKIP /
AUTO_RENAME / ASK, **default ASK**. Enforced in `DownloadWatchDog.localFileCheck`:
directory target → invalid destination; write probe; names locked by another
controller in the same package are treated as mirrors instead of conflicts;
else global setting → remembered per-package choice → dialog. SKIP refines via
`OnSkipDueToAlreadyExistsAction` (SKIP, or mark successful after hash/size
verification). **AUTO_RENAME produces `name_2.ext`, `name_3.ext`, …** (underscore
counter continuing from existing `_N`), not `name (2).ext`. Paths > 259 chars
raise `PathTooLongException`.

Mirrors: `MirrorDetectionDecision` = SAFE / AUTO / FILENAME_FILESIZE /
FILENAME / DISABLED, **default AUTO**. Match = plugin `getMirrorID` when both
sides provide one, else display-name compare (case-insensitive on Windows or
with `ForceMirrorDetectionCaseInsensitive`), then size/hash per mode
(`MirrorDetectionFileSizeEquality` default 100%). The controller runs one
candidate and marks siblings `FINISHED_MIRROR` via a conditional skip; the
package view collapses mirror rows so progress/ETA count logical files once.

Persistence: download list in `cfg/downloadList.zip` + rotations
(`downloadList<N>.zip`); each package/link is a `FilePackageStorable` /
`DownloadLinkStorable` JSON entry plus `extraInfo` (`rootPath`, timestamp);
keep 5 old lists; corrupt files renamed `.backup`; saves debounced 5 s/60 s.
Grabber list in `cfg/linkcollector.zip`; extraction state is separate per
archive: `cfg/archives/v2_<id>.json` (`autoExtract` tri-state, `extractPath`,
passwords, if-exists action, delete flags). `ConvertRelativePathsJDRoot`
(true) makes lists movable between installs.

Key defaults: 3 simultaneous downloads (hard max 40), 5 forced starts,
1 download per host (gate off), **1 chunk per file**, 3 plugin retries,
10 link-history entries, ask-on-exists, mirror AUTO, auto-sort children on,
keep 5 old lists, folder creation at `ON_DOWNLOAD_START`, speed limit off,
free-space check 128 MB, auto-extraction on, linkgrabber auto-start on,
filename replace map `{":"→";", "|"→"¦", "<"→"[", ">"→"]", "/"→"⁄",`
`"\"→"∖", "*"→"#", "?"→"¿", "\""→"'"}`, cleanup packagenames off.

### Adopt vs reject for ripdl

Adopt:
1. The paste→package→folder→extract pipeline shape, with **link lists kept as
   text extraction** (regex over text) — no site-specific bots.
2. **Filename from URL fragment** (`…#name.part01.rar`) in the naming chain:
   `Content-Disposition` → fragment → path basename.
3. **Password sniffing** over grabbed text → package field → extractor.
4. Native **PrivateBin reader** (open format) for paste URLs — JD itself has no
   PrivateBin support, so this is a genuine gap we can fill.
5. The `ArchiveType` pattern table + completeness gate + magic-byte
   disambiguation, implemented as a pure-Rust module.
6. Extraction defaults mapped to non-interactive equivalents, plus `7z t`
   pre-check, password-list loop with cached winner, and a real containment
   check on entry paths.

Reject:
1. Per-site hoster plugins (~1200 classes) and their captcha-solver coupling.
2. Turnstile/anti-bot bypass: for hosts like `fuckingfast.co`, route through
   our own browser extension capture (user solves the challenge) instead.
3. `ASK_FOR_EACH_FILE`-style interactive defaults in a headless daemon.
4. `FinalizeArchive`'s lack of canonical path containment for entries.
5. Its update/launcher and account ("MyJDownloader") surface — irrelevant and
   network-facing.

### Threads to pull later

- `jd.controlling.linkcrawler.LinkCrawler` deep-crawl heuristics (how deep it
  follows pages/containers) if we ever add recursive page grabs.
- `DownloadLinkProperty`/`MirrorDetectionDecision` mirror grouping (see the
  model track notes) if we add mirror support for the same file on N hosts.
- `org.jdownloader.extensions.extraction.gui.bubble` notification semantics for
  GUI polish parity.

## Template for entries

- Target / version
- Method (REA commands used)
- Findings (protocol, endpoints, telemetry)
- What ripdl will adopt (protocol ideas) vs. reject (telemetry, nags)
