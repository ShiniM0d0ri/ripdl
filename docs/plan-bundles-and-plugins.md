# Plan: multi-part bundles + hoster plugins

Status: **phases 1-6 implemented**. No built-in hosters ship; user-loadable
plugins remain the locked choice.

## Goal

Select a set of direct download links, send them to ripdl, and get **one task**
that downloads every part into `<dest>/<base name>/` - the same way a torrent
owns `<dest>/<name>/` and an HTTP task owns one file. On supported file hosters
the extension resolves the links itself (plugins); on any other page it takes
the links you selected.

Not in scope, ever: captcha solving, paywall/account bypassing, DRM, remote
plugin code (MV3 forbids it anyway), telemetry.

## Today (facts the plan builds on)

- `Command::Add { url, dest_dir, headers, start_at, kind, sequential, only_files }`
  - one URL per task, `TaskKind::{Http, Video, Torrent}`. The bridge forwards
  `Command` JSON verbatim, so a new command reaches the daemon without touching
  the host.
- The daemon holds `records` (queue.json) and `progress`, one runner per task,
  `max_concurrent` enforced by one `Semaphore` permit per task.
- The extension has `webRequest` + `scripting` + `activeTab`, resolves
  per-request referer/cookies, and "Download selected links" groups links like the copy chip and queues them at once
  (parts of one set become one bundle; singles stay HTTP tasks).
- GUI rows are keyed by `TaskKind` (`torrent` / `http` / `video`), with a
  details dialog per kind and a sidebar transport filter for torrents.
- `ripdl-core::torrent` already owns `<dest>/<name>/` naming, so the bundle
  layout can mirror it.

## 1. New task type: `bundle`

### Data model

`TaskKind::Bundle` (serde `bundle`), plus on `TaskRecord`:

```rust
#[serde(default)] bundle_name: Option<String>,   // display name / folder
#[serde(default)] parts: Vec<PartRecord>,        // ordered as sent

struct PartRecord {
    url: String,
    filename: String,          // last path segment, sanitized
    headers: HashMap<String, String>,
    state: TaskState,          // Queued/Running/Completed/Failed/Paused
    downloaded: u64,
    total: Option<u64>,
    error: Option<String>,
}
```

`Command::AddBundle { name: Option<String>, dest_dir: Option<PathBuf>,
parts: Vec<BundlePart{ url, filename: Option<String>, headers }> }`;
`Command::PreviewBundle { links: Vec<String> }` →
`Reply::BundlePreview { groups: Vec<BundleGroup{ name, parts, missing, single }> }`
(no writes; `missing` are the gaps the numbering proves). `Reply::Added { id }`
is reused.

`TaskInfo` gains `parts_done: u32`, `parts_total: u32`; `downloaded`/`total`
are the **sums** over parts (`total` stays `None` until every part's size is
known, so no fake percentages).

Per-part `headers` are stored exactly like today's HTTP tasks (they are
persisted so resume can replay a referer/cookie). A plugin's `resolve()` can
supply fresh headers for one part through `RefreshUrl { part }` when a hoster
token expires.

### Layout

```
<dest>/<bundle name>/
    <part 1 filename>
    <part 2 filename>
    ...
    <part N>.ripdl.json        # existing per-part resume sidecar
```

`<bundle name>` = the grouped base name, sanitized (Windows-illegal chars ->
`_`, reserved device names prefixed, trailing dots/spaces trimmed), deduped as
`name (1)` like the LAN inbox. Name collisions never merge two bundles.

### Scheduling and state

- **Parts share the one global `max_concurrent` pool**: `max_concurrent = 4`
  means four transfers in total, whether they are HTTP tasks, torrents or
  parts of a bundle. A bundle reserves nothing up front - the coordinator keeps
  starting its next part as a slot frees, and the semaphore is fair, so other
  queued tasks interleave instead of waiting for the whole bundle.
- A bundle's coordinator holds **no** permit; only its currently running parts
  do, through the same acquire path a single download uses, so pause/resume/
  cancel behave identically and `max_concurrent` keeps its one meaning.
- Aggregate state: `running` while any part is unfinished, `completed` when
  every part is complete, `failed` when a part failed permanently (other parts
  are allowed to finish; the error names the part).
- Pause cancels the part runners; completed parts stay. Resume retries only
  unfinished/failed parts - each keeps its own sidecar, so bytes are never
  re-downloaded.
- One runtime entry per bundle (parent `CancellationToken`, part tokens are
  children) and the coordinator only returns after its parts have wound down,
  so the `rm --files` drain keeps its existing invariant.
- Remove with `delete_files` deletes the bundle folder (parts + sidecars).
- `RefreshUrl` grows `part: Option<usize>` for an expired link on one part
  (phase 5).
- `AddBundleParts { id, parts }` appends the missing links to an existing
  bundle (the "I forgot part 7" case) and resumes it (phase 5).
- The engine needs one small addition: `Engine` always names the file from
  Content-Disposition/URL, so bundles pass an explicit part filename
  (`download_named`) instead of letting the server rename a part.
- No watcher thread: unlike seeding, a bundle has nothing to do once complete.

### Grouping rules (pure `ripdl_core::bundle`, unit-tested)

Strip the query/fragment, percent-decode, take the last path segment, match the
first rule that fits (case-insensitive):

| pattern | example | base |
| --- | --- | --- |
| `.partNN.rar` / `.partN.zip` | `x.part01.rar` | `x` |
| `.rNN` + `x.rar` | `x.r00 ... x.rar` | `x` |
| `.zNN` + `x.zip` | `x.z01 ... x.zip` | `x` |
| `.<3 digits>` (7-Zip split) | `x.001` | `x` |
| `.zip.NNN` | `x.zip.001` | `x` |
| ` CDn` / ` Discn` pairs | `x CD1.rar` | `x` |
| no marker | `x.mkv` | `x` (single) |

- Group key = lowercase base name. Every group with **one** link is a plain
  `Add` (unchanged path); two or more become a bundle.
- Two links in one group with the **same filename** (mirrors of one part) are
  not merged: the group splits into "name" and "name (2)" and the chip shows
  both, so mirror links never overwrite each other.
- Marker-less files that merely share a stem (`x.srt`, `x.en.srt`) are
  **not** parts: a group must contain at least one part marker or an explicit
  `force_bundle` from the popup.
- Gaps are allowed (send 1,2,5 of 1-5): the row shows "3 of 5 parts" and names
  the missing numbers in the details dialog. A gap is a warning, not a refusal.
- The total is only what the **numbering proves**: highest marker seen = count.
  A selection starting at part 5 shows "parts 5+" and no percentage rather than
  inventing a denominator.
- Adding a URL that is already a part of a bundle is idempotent: it is skipped,
  and an identical re-add returns the existing row instead of a second folder.
- Filename kept per part exactly as the URL says; only sanitized.

### Surfaces

- **GUI**: kind icon `bundle` (stacked layers, amber), row title = bundle name,
  aggregate progress + "3/7 parts" next to the percent; the HTTP details dialog
  gains a parts table (per-part progress, bytes, missing numbers) and shows
  "Open folder" instead of "Open file"; sidebar filter "Bundles" next to
  "Torrents"; File > Add Multi-part Links (Ctrl+B) pastes links and previews the
  grouping; Remove & delete files removes the folder.
- **CLI**: `ripdl bundle <url>... [--name N] [--dest D]`,
  `ripdl bundle --preview <url>...` (prints the grouping + missing numbers),
  `ripdl ls` row shows `[3/7 parts]`.
- **Details reply**: `TaskDetailsSnapshot.parts` (the existing `Details`
  command answers per kind), so the CLI and GUI share one shape.

## 2. Extension overhaul

### Copy capture

`ext/capture.js` in a content script (`<all_urls>`, `document_idle`; a single
`copy` listener, nothing else resident). Built as shipped (commit `32bef30`):

- The content script collects the links itself (anchors in the selection plus
  bare URLs from its text) and sends `{type:"ripdl_offer", links, page}`; the
  background calls `PreviewBundle` and answers with the groups. The chip is
  the content script's DOM, so it survives a service-worker restart between
  preview and Send.
- An anchor's visible text becomes the part filename only when it **names a
  file** (`stem.ext`, no path characters, <= 120 chars). A page full of
  "Download" anchors must not rename every part "Download", and - worse -
  identical link texts would wrongly merge unrelated links into one bundle.
  An anchor without a filename-looking label still counts when the selection
  text itself carries `http(s)` URLs. Otherwise such an anchor is weak: it is offered only when a plugin claims its URL (the built-in generic plugin claims known archive and media extensions).
- The chip lists what would be queued ("Send 3 links", then
  `x - 3 of 3 parts`, singles as "2 single downloads", missing numbers in
  the group line) with **Send / Edit / Dismiss**. Nothing is queued until
  Send. Edit flips the chip in place into per-group checkboxes plus a
  folder-name field (the popup-prefill idea was dropped: `action.openPopup`
  cannot be relied on from a chip click).
- The chip is a `<ripdl-offer-chip>` element with a shadow root, all visual
  CSS set through the CSSOM (`style.cssText`) so page CSP cannot block it
  and page CSS cannot reach it; positioned near the selection, follows
  scroll, auto-dismisses after 30 s, Escape dismisses.
- The clipboard is never written or read, and the copy event is never
  cancelled.
- `ripdl_offer_send` sends one `AddBundle` per multi-part group and one
  `Add` per single link (same rule as the GUI dialog), with the page URL as
  Referer on every part. A refused group falls back to one `add` per link.

### Old-daemon detection (capability, not probing)

`Reply::Config` carries a `bundles: bool` capability flag (`#[serde(default)]`,
so older daemons simply omit it). The extension reads it once via
`get_config` and caches the answer for the worker's lifetime; a missing reply
(host down) stays "unknown" and retries next time. This replaced the original
plan - probing with `preview_bundle` - because a pre-bundle daemon answers an
unknown command with an uncorrelatable id: the daemon's parse error used to
carry `id: 0`, the bridge re-raised it as a transport failure and retried
20 times, and the extension's `send()` waited out its full 15 s timeout
(measured: a 15 s chip on 0.4.1 vs 0.3 s with the flag). The daemon now also
echoes the caller's id in its bad-request reply, so future version skew
produces a correlated error instead of a retry storm. With support ruled out
the chip degrades to one ordinary download per link and says so
("This daemon predates bundles - the links are queued one by one").

Note the mixed-version matrix: the flag is only visible when *both* the host
exe and the daemon are new (the host deserializes `Reply::Config` and
re-serializes it, dropping unknown fields). A new daemon behind an old host
therefore gets the singles fallback too - harmless, and both ship together in
real installs.

### Plugin API (first-party JS only)

```
ext/hosts/<id>.js
  export default {
    id, label,
    match(url) -> bool,
    name(url) -> string | null,   // optional, cheap pre-group name, no network
    async links({ doc, url, fetch }) -> [{ url, name?, headers? }],
    async resolve(link, ctx) -> { url, name?, headers? } | null,   // optional
  }
ext/hosts/index.js   // built-in registry; per-host enable flag in chrome.storage
```

- `fetch` runs with the page's cookies only when the user granted the optional
  `cookies` permission **and** enabled it for that host.
- `name(url)` exists because some hosts carry the filename client-side;
  grouping needs it before any network happens, and the daemon's preview
  groups on `Link.filename`.
- A `generic` plugin always exists: every `a[href]` with a known archive/media
  extension, so plugins are an enhancement, not a requirement.
- Plugins may only read what the page already offers or calls the host's own
  public endpoints; no captcha/paywall logic, no skipping of waiting timers or ads, no credentials stored.
- Ships with `[generic]`; adding a built-in is a single file +
  registry line, covered by a node test that loads every plugin and checks
  `match()` and shape.

### User-loadable plugins (decision)

Users load their own plugins; MV3 forbids `eval` and remotely-fetched code
(platform-enforced), so "load" means files on disk, in two formats:

- **Recipe (`.json`, data only)** — URL match patterns, link selectors,
  filename rules, optional resolve template. `JSON.parse`, never executed,
  so it can come from anywhere: file picker, pasted text, even a URL (inert
  data, not code). Covers most hosters.
- **Script (`.js`, the full API above)** — loaded only from the extension's
  own folder. Drop it into `hosts/local/` and add one namespace import +
  entry to `hosts/local/registry.js`; the worker statically imports that
  module. (Verified live 2026-10-24: dynamic `import()` is disallowed on
  ServiceWorkerGlobalScope by the HTML spec — Chromium rejects it — so the
  original `index.json` + dynamic-import design cannot work in the worker;
  the generated registry module is the MV3-legal equivalent. A failed
  static import would kill background.js, so the add-plugin tool must keep
  registry.js in sync with the files present.) Never fetched from the
  network.

Import UX: an extension Plugins page (options page) lists built-ins and
locals with per-host enable toggles, imports recipes via file picker or
paste, and shows a script's full code before it can be enabled. ripdl the
app gets the same via a GUI/CLI "add plugin" that copies the file into
`hosts/local/`, rewrites `registry.js`, and prompts for the one browser
"Reload" click (the browser caches the package; unavoidable). Trust is
stated, not hidden: recipes are inert by construction; a script runs with
the extension's powers, which is why it must live on local disk, be shown
before enabling, and get only capability-scoped handles (wrapped `fetch`,
page-DOM read).

Shipped as `ripdl_core::extplug` (phase 5): `install`/`uninstall` regenerate
`registry.js` from the files actually on disk and swap it atomically, so a
crash mid-write can never leave a stale registry that would kill
background.js. Plugins use the app-managed extension by default;
`extension_dir` (`ripdl config set extension_dir=...`) and
`--ext-dir <folder>` select a manually loaded copy. The GUI exposes plugin
installation in Settings.

Still locked: no remote plugin code, no eval, no captcha/paywall logic, no
credential storage.

### Extension install/update from the app (phase 6) [x]

Unpacked extensions never auto-update, so the app owns the whole loop:

- **App-managed copy** at `<data_dir>/extension`: the GUI copies the bundled
  `ext/` (next to the exe; repo `ext/` in dev builds) there and updates it
  when the bundled manifest version is newer. Debug builds also refresh
  equal-version sources so UI edits show up without a version bump. Both paths
  preserve `hosts/local` scripts and regenerate `registry.js`
  (`ripdl_core::extplug`). User
  writable, so plugin installs never need elevation - unlike `{app}\ext` in
  Program Files.
- **Heartbeat**: the service worker sends `Hello { version }` on every SW
  wake via `ExtensionHello`; the daemon records version + last-seen in
  `extension-status.json` and exposes them on `Reply::Config.extension`.
  `extension_dir` stays as an override for a manually loaded copy.
- **Guided install**: Settings > Browser extension shows the status
  (never seen / seen vN N min ago / newer copy awaiting reload), detects the
  default Chromium browser (https UserChoice ProgId; Chrome/Edge/Brave), and
  one button copies + opens the browser's extensions page with the steps.
- **Installer/CI**: release workflow asserts `ext/manifest.json` matches the
  workspace version; the Inno upgrade preserves `hosts/local` across the
  `{app}\ext` wipe.

Not attempted (and why): policy-forced installs (`ExtensionInstallForcelist`
needs a signed CRX + forced-install semantics), `--load-extension` (branded
Chrome >= 137 ignores it), and driving chrome:// pages via CDP (requires
launching the user's browser with debugging on). The browser only accepts
the final clicks from the user.

### Message flow

```
Ctrl+C / context menu / popup
  -> extension: collect links (anchors, bare URLs; see copy capture for which anchors count)
  -> host: {"cmd":{"type":"preview_bundle","links":[...]}}
  <- groups (name + parts)
  -> user clicks Send
  -> host: {"cmd":{"type":"add_bundle", ...}} per multi-part group,
            {"cmd":{"type":"add", ...}} per single link
  <- added id; the GUI row appears as one bundle
```

Back-compat: the `config` reply's `bundles` flag gates the whole path (see
above); a refused `add_bundle` still falls back to one `add` per link, and the
chip states the limitation instead of failing silently.

## 3. Testing

- `ripdl-core::bundle`: every pattern above, mixed selections, dupes, query
  strings, unicode, marker-less stems, 60-part sanity.
- Daemon integration (existing offline mock server): 3-part bundle completes,
  per-part SHA256 matches; one part 404s -> bundle `failed`, resume retries
  only it; pause mid-way keeps completed parts; remove deletes the folder.
- Extension: node test that every plugin's `match`/`links` return the right
  shape; the CDP harness fires real `copy` events on a fixture hoster page
  (`research/ext-bundle-e2e.js`, 26 checks: chip content/geometry, Send ->
  one bundle per group with hash-verified parts, Edit -> renamed folder,
  page-wide copy, no sidecars) and a second one pins the old-daemon fallback
  (`research/ext-bundle-compat.js`, 7 checks against the installed pre-bundle
  daemon: chip in ~0.3 s, singles stated, per-link queue lands).
- GUI: extend the CDP harness - bundle row renders, details lists parts,
  bulk pause/resume/remove still work.

## 4. Phases (each one shippable)

1. **Core bundles** - `TaskKind::Bundle`, grouping module + tests, daemon
   runner/aggregate/state, `AddBundle`/`PreviewBundle`/`Details`, CLI.
2. **GUI** - kind icon, row, parts tab, sidebar filter, delete-folder.
3. **Extension core** - copy capture + offer chip, selection context menu
   sends a bundle, old-daemon fallback via the `bundles` capability flag. [x]
4. **Plugins** - API + registry + generic + user-loadable (recipe import
   anywhere, scripts via `hosts/local/`; no built-in hosters - locked), first
  hoster reference plugin proven live, `plugins/` local-only; multipart
  archive paste flows work through copy capture - fragments survive text
  extraction. [x] reference plugin
5. **Polish** - per-part link refresh UI, "add missing parts", plugin settings UI. [x]

Phases 1-2 are useful without the extension (CLI/`add` with several links);
phase 3 is useful without plugins (works on any page).

## 5. Decisions (locked)

- **Ctrl+C** shows the offer chip; nothing auto-queues. No silent mode.
- **Concurrency** is the existing `max_concurrent` pool: 4 = four transfers
  total across HTTP tasks, torrents and bundle parts. No per-bundle cap.
- **Layout** is `<dest>/<base name>/<parts…>` with per-part sidecars.
- **Gaps** are allowed and surfaced ("3 of 5 parts", missing numbers listed).
- **Mirrors** never merge: same base *and* same filename in one selection
  splits into separate bundles.

Remaining: **which hosters ship first** (phase 4 only - phases 1-3 do not need
the answer).

- **No built-in hoster plugins** (locked 2026-10-24): the only built-in is
  `generic`. Site-specific plugins stay user-loadable (recipe paste/file
  import, or script via `hosts/local/`). The working reference remains local
  and ignored for production import.
