/**
 * ripdl WebExtension - capture -> handoff -> fallback.
 *
 * Behavior contract (clean-room; original code, no third-party code):
 *   1. ripdl NEVER watches what you browse. Context menus hand URLs over on
 *      demand; downloads.onDeterminingFilename seizes a download only while
 *      the daemon answers, and only after the same request was observed.
 *   2. Cookies flow to the daemon ONLY if the user turns sharing on in the
 *      popup (an optional permission the browser asks about explicitly).
 *      Otherwise the engine gets the Referer and the browser User-Agent -
 *      just enough to fetch the same bytes the browser would.
 *   3. If the daemon is missing or rejects a task, the browser downloads
 *      normally. The extension exists to make ripdl better, not to nag.
 *   4. One content script (`capture.js`) and it only runs a `copy` listener:
 *      no polling loops, no history permission, no remote messaging
 *      channels, no page content read outside a copy the user made.
 *   5. A host whose direct link needs its own page (that is its right and no
 *      solver is implemented): for links the user sent, the host's page may be
 *      opened in a hidden tab of the user's session, the page decides - its
 *      widget, its click - and the direct link is captured and handed to the
 *      plugin that asked. The tab closes as soon as the link shows up.
 *
 * The daemon owns capture policy (size floors, extension lists, fallback).
 * The extension only supplies what the wire protocol needs.
 */

/* --- hoster plugins (user-loadable; see plugins.js) ------------------- */

import { pluginMatches, pluginNameFor, pluginResolve, registry, validateRecipe } from "./plugins.js";
import { filenameListener } from "./handoff.js";

/** Fill in filenames a page never knew: a plugin may read a name carried in
 * the URL fragment, so grouping works before any network happens. */
async function nameLinks(links) {
    const out = [];
    for (const l of links) {
        if (l.filename) { out.push(l); continue; }
        try {
            const name = await pluginNameFor(l.url);
            out.push(name ? { ...l, filename: name } : l);
        } catch {
            out.push(l);
        }
    }
    return out;
}

/** Resolve share links into direct ones at send time. A part that fails
 * (plugin errors, host refuses, no plugin) keeps its original URL - the
 * honest-fallback rule: queue it as-is rather than lose it. `unresolved`
 * counts only links an enabled plugin claimed but could not resolve. Parts
 * resolve concurrently (a page resolve queues itself, two tabs at a time). */
async function resolveParts(parts, ctx) {
    const out = new Array(parts.length);
    let unresolved = 0;
    await Promise.all(parts.map(async (p, i) => {
        let r = null;
        try { r = await pluginResolve(p, ctx); } catch { r = null; }
        if (r && r.url && r.url !== p.url) {
            out[i] = {
                ...p,
                url: r.url,
                filename: r.name || p.filename,
                headers: r.headers && typeof r.headers === "object" ? { ...p.headers, ...r.headers } : p.headers,
            };
            return;
        }
        if (await pluginMatches(p.url, true)) unresolved++;
        out[i] = p;
    }));
    return { parts: out, unresolved };
}

const HOST_NAME = "org.ripdl.host";

/** Async message handlers must ALWAYS answer: a dropped sendResponse hangs
 * the caller (the options page polls forever). Wrap, catch, reply. */
function replyAsync(sendResponse, fn) {
    (async () => {
        try { sendResponse(await fn()); }
        catch (e) { try { sendResponse({ ok: false, error: String(e?.message || e) }); } catch { /* channel closed */ } }
    })();
}

function compareVersions(a, b) {
    const left = String(a).split("-")[0].split(".").map((n) => Number.parseInt(n, 10) || 0);
    const right = String(b).split("-")[0].split(".").map((n) => Number.parseInt(n, 10) || 0);
    for (let i = 0; i < Math.max(left.length, right.length); i++) {
        const delta = (left[i] || 0) - (right[i] || 0);
        if (delta) return Math.sign(delta);
    }
    return 0;
}

/** webRequest requestHeaders we keep (MV3 hides UA without extraHeaders). */
const KEPT_HEADERS = new Set(["referer", "user-agent"]);
/** How long a request record stays matchable (browser items settle fast). */
const MAP_TTL_MS = 120000;
/** Service-worker downloads race onDeterminingFilename; wait once, then
 * match by response content-length + initiator (tabId -1 = not page-driven). */
const SW_RACE_WAIT_MS = 700;

let port = null;
let hostAlive = false;
let nextId = 1;

/** Single writer for daemon liveness (throws to the badge + popup). */
function setHostAlive(v) {
    hostAlive = v;
    chrome.storage.local.set({ hostAlive: v }).catch(() => {});
    setBadge(v ? "" : "off");
}

/** webRequest requestId -> {url, method, tabId, initiator, at,
 *  contentLength, headers: {name: value}} */
const reqByRequestId = new Map();
/** url -> same record (downloads items carry no requestIds). */
const reqByUrl = new Map();
const MAP_BUDGET = 512;

/* --- hunt: per-tab direct-media sightings for the popup ---------------- */

/** tabId -> [{url, at}] — what this tab visibly streamed/fetched as media.
 * Fed from the same webRequest listeners capture already uses; no new
 * permissions, no content scripts, never leaves the SW. */
const mediaByTab = new Map();
/** URL paths ending in a media container are media even without a
 * contents-type sniff (direct file links). */
const MEDIA_EXT_RE = /\.(mp4|mkv|webm|mov|avi|flv|m4v|ts|mp3|m4a|aac|opus|ogg|flac)([?#]|$)/i;
const MEDIA_PER_TAB = 40;
const MEDIA_TTL_MS = 15 * 60 * 1000;

function forgetOldTabMedia(tabId) {
    const list = mediaByTab.get(tabId);
    if (!list) return;
    const cutoff = Date.now() - MEDIA_TTL_MS;
    const kept = list.filter((m) => m.at >= cutoff);
    if (kept.length) mediaByTab.set(tabId, kept);
    else mediaByTab.delete(tabId);
}

function rememberMedia(tabId, url) {
    if (typeof tabId !== "number" || tabId < 0) return;
    let list = mediaByTab.get(tabId);
    if (!list) {
        list = [];
        mediaByTab.set(tabId, list);
    }
    if (!list.some((m) => m.url === url)) {
        list.push({ url, at: Date.now() });
        // Bounded per tab: newest wins.
        while (list.length > MEDIA_PER_TAB) list.shift();
        if (mediaByTab.size > 32) {
            // Drop the oldest tab entry wholesale.
            const oldest = [...mediaByTab.entries()].sort((a, b) =>
                a[1][a[1].length - 1].at - b[1][b[1].length - 1].at
            )[0];
            if (oldest && oldest[0] !== tabId) mediaByTab.delete(oldest[0]);
        }
    }
}

/** Stream/video sites where handing the *page URL* to yt-dlp is the
 * right move (hunt mode). Enough coverage for common cases; the
 * direct-media list below covers everything else. */
const VIDEO_HOSTS = [
    "youtube.com", "youtu.be", "youtube-nocookie.com", "vimeo.com",
    "twitch.tv", "dailymotion.com", "bilibili.com",
    "tiktok.com", "rumble.com", "odysee.com", "nicovideo.jp",
    "rutube.ru", "streamable.com", "tubitv.com", "vkvideo.ru",
    "x.com", "twitter.com", "ted.com", "soundcloud.com",
];

/** Is handing this page URL to yt-dlp a sensible offer? */
function videoishPage(url) {
    let u;
    try { u = new URL(url); } catch { return false; }
    if (u.protocol !== "https:" && u.protocol !== "http:") return false;
    const host = u.hostname.toLowerCase().replace(/^www\./, "");
    return VIDEO_HOSTS.some((h) => host === h || host.endsWith("." + h));
}

/** Fresh media sightings for a tab, newest last. */
function mediaForTab(tabId) {
    forgetOldTabMedia(tabId);
    return [...(mediaByTab.get(tabId) || [])].filter((m) => {
        try { return !!new URL(m.url); } catch { return false; }
    });
}

function setBadge(text) {
    chrome.action.setBadgeText({ text });
    chrome.action.setBadgeBackgroundColor({ color: hostAlive ? "#2e7d32" : "#9e9e9e" });
}

function settings() {
    return chrome.storage.local.get({ capture: true, shareCookies: false }).catch(() => ({ capture: true, shareCookies: false }));
}

/* --- native messaging host ------------------------------------------- */

const pending = new Map(); // id -> {resolve}

function send(msg) {
    if (!port) return Promise.resolve(null);
    return new Promise((resolve) => {
        pending.set(msg.id, { resolve });
        // A wedged host must not wedge captures: 15 s and fall back.
        setTimeout(() => {
            if (pending.has(msg.id) && pending.get(msg.id).resolve === resolve) {
                pending.delete(msg.id);
                resolve(null);
            }
        }, 15000);
        port.postMessage(msg);
    });
}

function onHostMessage(resp) {
    const waiter = pending.get(resp.id);
    if (waiter) {
        pending.delete(resp.id);
        waiter.resolve(resp);
    }
}

function connectHost() {
    try {
        port = chrome.runtime.connectNative(HOST_NAME);
    } catch {
        setHostAlive(false);
        return;
    }
    port.onMessage.addListener(onHostMessage);
    port.onDisconnect.addListener(() => {
        port = null;
        setHostAlive(false);
        // Waiters resolve null -> captures fall back to the browser.
        for (const p of pending.values()) p.resolve(null);
        pending.clear();
    });
    send({
        id: `ext-${nextId++}`,
        cmd: { type: "extension_hello", version: chrome.runtime.getManifest().version },
    });
    // Cheap liveness probe: the daemon replies `ok` to an empty listing.
    send({ id: `probe-${nextId++}`, cmd: { type: "list" } }).then((r) => {
        setHostAlive(!!r);
    });
}

/* --- multi-part offers (copy capture, selection menu) ------------------ */

/** Links one offer may carry (capture.js caps too; the daemon validates). */
const MAX_OFFER_LINKS = 100;
/** Daemon bundle support, learned once from `get_config`: null = unknown.
 * Older daemons omit the `bundles` capability field, so the offer falls back
 * to the per-link path they already know - no probing with unknown commands. */
let daemonBundles = null;

function httpOnly(url) {
    try {
        const u = new URL(String(url));
        return u.protocol === "http:" || u.protocol === "https:" ? u.href : "";
    } catch {
        return "";
    }
}

/** Keep only what the wire protocol needs: http(s), deduped, bounded.
 * `weak` marks a candidate the page only suggested (see capture.js). */
function sanitizeLinks(list) {
    const out = [];
    const seen = new Set();
    for (const item of (Array.isArray(list) ? list : [])) {
        if (out.length >= MAX_OFFER_LINKS) break;
        const url = httpOnly(typeof item === "string" ? item : item && item.url);
        if (!url || seen.has(url)) continue;
        seen.add(url);
        const raw = item && typeof item.name === "string" ? item.name.trim().slice(0, 200) : "";
        out.push({ url, filename: raw || null, weak: !!(item && item.weak) });
    }
    return out;
}

/** Weak candidates (plain anchors in a copied selection) stay only when an
 * enabled plugin or the built-in file-type list claims their URL. */
async function keepRecognized(links) {
    const out = [];
    for (const l of links) {
        if (!l.weak || (await pluginMatches(l.url))) out.push(l);
    }
    return out;
}

/** Referer for links the page never requested (hotlink checks). */
function pageHeaders(page) {
    const p = httpOnly(page);
    return p ? { Referer: p } : {};
}

/** Ask the daemon how a selection groups; a pre-bundle daemon degrades to
 * "every link is its own download", so the offer still works there. */
async function previewGroups(links) {
    // Plugins first: filenames from the URLs themselves (cheap, no network)
    // make the grouping right even when the page only had bare links.
    links = await nameLinks(links);
    if (daemonBundles === null) {
        // Capability, not a probe-unknown-command timeout: one `get_config`
        // answers the same on every daemon, and `bundles: false` (older
        // daemons just omit the field) says the bundle path is unavailable.
        // A missing reply (host down) stays unknown: the next offer retries.
        const cfg = await send({ id: `cfg-${nextId++}`, cmd: { type: "get_config" } });
        if (cfg?.reply?.type === "config") {
            daemonBundles = !!cfg.reply.bundles;
        }
    }
    if (daemonBundles) {
        const resp = await send({
            id: `pv-${nextId++}`,
            cmd: { type: "preview_bundle", links: links.map((l) => ({ url: l.url, filename: l.filename })) },
        });
        const groups = resp?.reply?.type === "bundle_preview" ? resp.reply.groups : null;
        if (groups) return { groups, bundled: true };
        // The daemon said it supports bundles but refused this preview; show
        // the links ungrouped rather than nothing.
        return { groups: links.map(singleGroup), bundled: false };
    }
    return { groups: links.map(singleGroup), bundled: false };
}

function singleGroup(link) {
    return {
        name: tailName(link.filename || link.url),
        parts: [{ url: link.url, filename: link.filename, seq: null }],
        first: 1,
        total: null,
        missing: [],
    };
}

function tailName(url) {
    try {
        const tail = decodeURIComponent(new URL(url).pathname.split("/").pop() || "");
        return tail || "download";
    } catch {
        return "download";
    }
}

/** Queue the offered groups: a multi-part group becomes ONE bundle task, a
 * single link stays an ordinary download (same rule as the GUI dialog). */
async function sendGroups(groups, page) {
    const headers = pageHeaders(page);
    let bundles = 0;
    let singles = 0;
    let failed = 0;
    let unresolved = 0;
    const dupes = [];
    for (const g of Array.isArray(groups) ? groups : []) {
        const raw = (g.parts || [])
            .map((p) => ({
                url: httpOnly(p && p.url),
                filename: p && typeof p.filename === "string" ? p.filename : "",
                headers,
            }))
            .filter((p) => p.url);
        if (!raw.length) continue;
        // Plugins turn share links into direct ones here - after the user
        // said go, before anything is queued. A part no plugin could resolve
        // keeps its original URL and is counted, never dropped. ctx carries
        // the framework capabilities a plugin may ask for (see plugins.js).
        const { parts, unresolved: unres } = await resolveParts(raw, { resolveViaPage });
        unresolved += unres;
        if (parts.length > 1 && daemonBundles === true) {
            const name = typeof g.name === "string" && g.name.trim() ? g.name.trim() : null;
            const resp = await send({
                id: `bundle-${nextId++}`,
                cmd: { type: "add_bundle", name, dest_dir: null, parts },
            });
            if (resp?.reply?.type === "added") {
                bundles++;
                continue;
            }
            // Daemon too old (or a bad group): keep the promise - every link
            // still gets queued, one task each.
            if (resp) daemonBundles = false;
        }
        for (const p of parts) {
            const r = await addOne(p.url, p.headers);
            if (r.status === "added") singles++;
            else if (r.status === "duplicate") dupes.push({ url: p.url, filename: p.filename || "", headers: p.headers || {}, task: r.task });
            else failed++;
        }
    }
    // Links ripdl already has: one window lists them, each with its own choices.
    if (dupes.length) openDuplicateWindow({ items: dupes, browser: null }).catch(() => {});
    const result = { ok: bundles + singles + dupes.length > 0, bundles, singles, duplicates: dupes.length, failed, unresolved };
    return { ...result, summary: summarize(result) };
}

/** What a send did, in a few words ("2 bundles + 1 download"). */
function summarize(r) {
    const bits = [];
    if (r.bundles) bits.push(`${r.bundles} bundle${r.bundles === 1 ? "" : "s"}`);
    if (r.singles) bits.push(`${r.singles} download${r.singles === 1 ? "" : "s"}`);
    if (r.duplicates) bits.push(`${r.duplicates} already in ripdl`);
    if (r.failed) bits.push(`${r.failed} refused`);
    if (r.unresolved) bits.push(`${r.unresolved} not resolved`);
    return bits.join(" + ") || "nothing";
}

/** One direct download. An existing copy comes back as `duplicate` (its own
 * window lets the user choose) instead of being shared silently. */
async function addOne(url, headers) {
    const resp = await send({
        id: `link-${nextId++}`,
        cmd: { type: "add", url, dest_dir: null, headers: headers || {}, on_duplicate: "report" },
    });
    const reply = resp?.reply;
    if (reply?.type === "added") return { status: "added", task: null };
    if (reply?.type === "duplicate") return { status: "duplicate", task: reply.task ?? null };
    return { status: "failed", task: null };
}

/** Popup and other UI poke us here; answer is mirrored via storage. */
chrome.runtime.onMessage.addListener((msg, sender, sendResponse) => {
    if (msg?.type === "ripdl_light_probe") {
        if (!port) {
            connectHost();
        } else {
            send({ id: `probe-${nextId++}`, cmd: { type: "list" } }).then((r) => setHostAlive(!!r));
        }
    }
    if (msg?.type === "ripdl_offer") {
        // The copy chip: group the selection and describe it. Nothing is
        // queued here - the user still has to click Send.
        replyAsync(sendResponse, async () => {
            const links = await keepRecognized(sanitizeLinks(msg.links));
            if (!links.length) return { groups: [], bundled: daemonBundles !== false };
            const preview = await previewGroups(links);
            return { groups: preview.groups, bundled: preview.bundled, hostAlive };
        });
        return true; // async reply
    }
    if (msg?.type === "ripdl_offer_send") {
        replyAsync(sendResponse, () => sendGroups(msg.groups, msg.page));
        return true; // async reply
    }
    if (msg?.type === "ripdl_hunt_query") {
        // activeTab is granted by the popup click, so the active page URL
        // is readable *while the user is looking at the popup* — never
        // resident, never pushed elsewhere.
        (async () => {
            let pageUrl = "";
            let tabId = -1;
            try {
                const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
                pageUrl = tab?.url || "";
                tabId = typeof tab?.id === "number" ? tab.id : -1;
            } catch {
                // No tab access: offer only from generic heuristics.
            }
            const media = tabId >= 0 ? mediaForTab(tabId) : [];
            const videoish = videoishPage(pageUrl);
            sendResponse({ pageUrl, videoish, media });
        })();
        return true; // async reply
    }
    if (msg?.type === "ripdl_plugins_list") {
        replyAsync(sendResponse, async () => {
            const seen = (await chrome.storage.local.get("plugin_seen")).plugin_seen || {};
            return {
                plugins: (await registry()).map((e) => ({
                    id: e.id, label: e.label, kind: e.kind, file: e.file || null,
                    error: e.error || null, enabled: e.enabled, seen: e.kind !== "script" || seen[e.file] === true,
                })),
                dir: "hosts/local/",
            };
        });
        return true;
    }
    if (msg?.type === "ripdl_ext_info") {
        replyAsync(sendResponse, async () => {
            if (!port) connectHost();
            const resp = await send({ id: `cfg-${nextId++}`, cmd: { type: "get_config" } });
            const appVersion = resp?.reply?.type === "config" && typeof resp.reply.version === "string"
                ? resp.reply.version
                : null;
            const extVersion = chrome.runtime.getManifest().version;
            const skew = appVersion === null ? null : compareVersions(extVersion, appVersion);
            return { extVersion, appVersion, skew };
        });
        return true;
    }
    if (msg?.type === "ripdl_plugin_toggle" && typeof msg.id === "string") {
        replyAsync(sendResponse, async () => {
            const all = (await chrome.storage.local.get("plugins_enabled")).plugins_enabled || {};
            all[msg.id] = !!msg.enabled;
            await chrome.storage.local.set({ plugins_enabled: all });
            return { ok: true };
        });
        return true;
    }
    if (msg?.type === "ripdl_plugin_seen" && typeof msg.file === "string") {
        replyAsync(sendResponse, async () => {
            const seen = (await chrome.storage.local.get("plugin_seen")).plugin_seen || {};
            seen[msg.file] = true;
            await chrome.storage.local.set({ plugin_seen: seen });
            return { ok: true };
        });
        return true;
    }
    if (msg?.type === "ripdl_recipe_import" && typeof msg.text === "string") {
        replyAsync(sendResponse, async () => {
            // Validate, then store the RAW object: validateRecipe's output is
            // engine-shaped, not recipe-shaped, and would fail re-validation
            // (and therefore never load) on the next registry read.
            const raw = JSON.parse(msg.text);
            const recipe = validateRecipe(raw);
            const all = (await chrome.storage.local.get("plugin_recipes")).plugin_recipes || [];
            const kept = all.filter((r) => r && r.id !== recipe.id);
            kept.push(raw);
            await chrome.storage.local.set({ plugin_recipes: kept });
            const en = (await chrome.storage.local.get("plugins_enabled")).plugins_enabled || {};
            en[recipe.id] = true;
            await chrome.storage.local.set({ plugins_enabled: en });
            return { ok: true, id: recipe.id };
        });
        return true;
    }
    if (msg?.type === "ripdl_recipe_delete" && typeof msg.id === "string") {
        replyAsync(sendResponse, async () => {
            const all = (await chrome.storage.local.get("plugin_recipes")).plugin_recipes || [];
            await chrome.storage.local.set({ plugin_recipes: all.filter((r) => r?.id !== msg.id) });
            return { ok: true };
        });
        return true;
    }
    if (msg?.type === "ripdl_plugin_test" && typeof msg.id === "string" && typeof msg.url === "string") {
        // Try one URL against one plugin: the name it would group under and
        // the direct URL it would resolve to. Runs wherever the plugin runs
        // in production; nothing is queued.
        replyAsync(sendResponse, async () => {
            const entry = (await registry()).find((e) => e.id === msg.id);
            if (!entry?.plugin) return { ok: false, error: entry?.error || "plugin not found" };
            const out = {};
            if (entry.plugin.match) out.matched = !!entry.plugin.match(msg.url);
            if (out.matched && entry.plugin.name) out.name = (await entry.plugin.name(msg.url)) ?? null;
            if (out.matched && entry.plugin.resolve) {
                const r = await entry.plugin.resolve({ url: msg.url, filename: null }, {
                    fetch: (u, o) => fetch(u, o),
                    resolveViaPage,
                });
                out.resolved = r ? r.url : null;
                if (r?.name) out.name = r.name;
            }
            return { ok: true, ...out };
        });
        return true;
    }
    if (msg?.type === "ripdl_dup_action" && typeof msg.key === "string") {
        // Only the duplicate window may act on a stored job.
        if (!String(sender?.url || "").startsWith(chrome.runtime.getURL("duplicate.html"))) {
            sendResponse({ ok: false, error: "Not allowed." });
            return;
        }
        replyAsync(sendResponse, () => duplicateAction(msg.key, Number(msg.index), String(msg.action)));
        return true; // async reply
    }
    if (msg?.type === "ripdl_send" && msg.cmd && typeof msg.cmd.type === "string") {
        // Popup-initiated task add (hunt buttons). Same rejection contract:
        // not accepted -> the popup shows the daemon-unreachable state.
        (async () => {
            const resp = await send({ id: `pop-${nextId++}`, cmd: msg.cmd });
            sendResponse({ ok: resp?.reply?.type === "added" });
            if (!resp) setHostAlive(false);
            else if (!hostAlive) setHostAlive(true);
        })();
        return true;
    }
});

/* --- request details capture (non-blocking, bounded) ------------------ */

function remember(details) {
    const headers = {};
    for (const h of details.requestHeaders || []) {
        const name = h.name.toLowerCase();
        if (KEPT_HEADERS.has(name) && h.value) headers[name] = h.value;
    }
    const rec = {
        url: details.url,
        method: details.method,
        tabId: typeof details.tabId === "number" ? details.tabId : -1,
        initiator: details.initiator || "",
        contentLength: 0,
        headers,
        at: Date.now(),
    };
    reqByRequestId.set(details.requestId, rec);
    reqByUrl.set(details.url, rec);
    setTimeout(() => {
        reqByRequestId.delete(details.requestId);
        reqByUrl.delete(details.url);
    }, MAP_TTL_MS);
    // Bounded: drop the oldest when over budget.
    for (const m of [reqByRequestId, reqByUrl]) {
        if (m.size > MAP_BUDGET) m.delete(m.keys().next().value);
    }
}

// Response sizes, for the service-worker download match; content-type
// video/audio sightings feed the popup's hunt offer; every response header
// itself is kept (capped) because the page resolve reads it: a host that
// answers its own go-request with a redirect header is resolved from a real
// page, and the plugin tells the framework which values count as the direct
// link - no header name baked into the framework.
chrome.webRequest.onHeadersReceived.addListener(
    (details) => {
        const rec = reqByRequestId.get(details.requestId);
        if (rec) {
            rec.responded = true;
            rec.resHeaders = {};
            for (const h of details.responseHeaders || []) {
                const name = h.name.toLowerCase();
                if (name === "content-length") {
                    const n = Number(h.value);
                    if (Number.isFinite(n)) rec.contentLength = n;
                }
                if (Object.keys(rec.resHeaders).length < 24) rec.resHeaders[name] = String(h.value ?? "");
            }
            const ct = rec.resHeaders["content-type"] || "";
            if (ct.startsWith("video/") || ct.startsWith("audio/")) {
                rememberMedia(details.tabId, details.url);
            }
        }
    },
    { urls: ["<all_urls>"] },
    ["responseHeaders"]
);

chrome.webRequest.onSendHeaders.addListener(
    (details) => {
        remember(details);
        if (MEDIA_EXT_RE.test(details.url)) rememberMedia(details.tabId, details.url);
    },
    { urls: ["<all_urls>"] },
    chrome.runtime.getManifest().manifest_version === 3
        ? ["requestHeaders", "extraHeaders"]
        : ["requestHeaders"]
);

// Dead tabs shouldn't squat on the media budget.
chrome.tabs.onRemoved.addListener((tabId) => mediaByTab.delete(tabId));

/** Cookies only reach the daemon through explicit opt-in + permission. */
async function cookiesForIfShared(url) {
    const { shareCookies } = await settings();
    if (!shareCookies) return null;
    const granted = await chrome.permissions.contains({ permissions: ["cookies"] });
    if (!granted) return null;
    try {
        const cookies = await chrome.cookies.getAll({ url, partitionKey: {} });
        const str = (cookies || []).map((c) => `${c.name}=${c.value};`).join(" ");
        return str || null;
    } catch {
        return null;
    }
}

/** Headers the engine needs to fetch the same bytes the browser would. */
async function headersFor(item, rec) {
    const headers = {};
    if (rec) {
        if (rec.headers.referer) headers["Referer"] = rec.headers.referer;
        if (rec.headers["user-agent"]) headers["User-Agent"] = rec.headers["user-agent"];
    }
    const cookie = await cookiesForIfShared(item.finalUrl || item.url);
    if (cookie) headers["Cookie"] = cookie;
    return headers;
}

/** Until daemon-side capture policy exists: don't run off with the
 * browser's tiny files (sub-256 KiB downloads are usually fine natively). */
const MIN_SEIZE_BYTES = 256 * 1024;

/** Match a download item to an observed request. */
function findRecord(item, ignoreContentLength) {
    let rec = reqByUrl.get(item.finalUrl) || reqByUrl.get(item.url);
    if (rec) return rec;
    // Service-worker download race: no page tab, size known -> match by
    // response content-length against recently observed tab-less requests.
    if (ignoreContentLength && item.totalBytes > 0) {
        let best = null;
        for (const r of reqByRequestId.values()) {
            if (r.tabId !== -1) continue;
            if (r.contentLength <= 0 || r.contentLength !== item.totalBytes) continue;
            const org = r.initiator;
            if (item.url.startsWith(org) || item.finalUrl.startsWith(org)) {
                if (!best || r.at > best.at) best = r;
            }
        }
        if (best) return best;
    }
    return null;
}

/* --- capture: decide -> take the browser's copy (or let it proceed) --- */

/** The daemon's verdict on this link: `{verdict: "new"|"duplicate", task}`,
 * or null (keep the browser's copy). The browser's copy goes only on `new` or
 * `duplicate`. */
async function decideHandoff(item, ctx) {
    if (takeOwnBrowserDownload(item)) return null; // ripdl handed this one back to the browser
    // A page resolve's own click started this copy: only its URL was wanted,
    // and a duplicate window (or a stray propose) for a download ripdl itself
    // opened would be wrong. The request runs through a tab resolveViaPage has
    // open - that tab is the decisive owner, referrers can lie. The record has
    // the same arrival race as everywhere else here, so wait for it once.
    if (pageResolves.size || pageResolveByTab.size) {
        pageResolveSweep();
        let rec = findRecord(item, false);
        if (!rec) {
            rec = await new Promise((resolve) => setTimeout(() => resolve(findRecord(item, true)), SW_RACE_WAIT_MS));
        }
        const p = (rec && pageResolveByTab.get(rec.tabId)) || pageResolves.get(item.referrer || "") || null;
        if (p) {
            p.capture(item.finalUrl || item.url);
            return { verdict: "self", task: null };
        }
    }
    const { capture } = await settings();
    if (!capture || !hostAlive) return null;
    if (item.totalBytes > 0 && item.totalBytes < MIN_SEIZE_BYTES) return null; // small files: browser is fine.
    let rec = findRecord(item, false);
    if (!rec) {
        // Possibly a service-worker download whose request details are still
        // arriving: wait once for the race, then decide.
        await new Promise((resolve) => setTimeout(resolve, SW_RACE_WAIT_MS));
        rec = findRecord(item, true);
        if (!rec) return null; // unknown origin: browser keeps it.
    }
    const headers = await headersFor(item, rec);
    const proposal = `dl-${Date.now().toString(36)}-${nextId++}`;
    if (!ctx.beginProposal(proposal)) return null; // the browser already kept it.
    const resp = await send({
        id: proposal,
        cmd: {
            type: "propose",
            proposal,
            url: item.finalUrl || item.url,
            headers,
            expected_bytes: item.totalBytes > 0 ? item.totalBytes : null,
        },
    });
    setHostAlive(!!resp); // a null reply means the host is gone.
    if (resp?.reply?.type !== "proposed") return null;
    const { outcome, task } = resp.reply;
    if (outcome === "new" || outcome === "duplicate") return { verdict: outcome, task: task ?? null, headers };
    return null;
}

/** Cancel and erase the browser's copy. False when Chrome would not let go. */
async function takeBrowserItem(item) {
    try {
        await chrome.downloads.cancel(item.id);
    } catch {
        return false;
    }
    try {
        await chrome.downloads.erase({ id: item.id });
    } catch {
        // Already gone (service-worker races): the cancel is what matters.
    }
    return true;
}

/* --- page resolve: the host's own page produces the direct link --------- */

/**
 * Some hosts answer a bare share-URL post with their anti-bot challenge: the
 * go endpoint wants a token their page has already embedded. External
 * managers answer that with captcha-solver services (JDownloader's route) -
 * ripdl has none and wants none. What a real browser session can do that an
 * outside client can never: open the host's page in a tab of the user's
 * browser (their own cookies, their own fingerprint), let the page arm
 * itself, click its real download control, and watch the direct link appear -
 * as the tab address, in a response header of the page's go-request, or as
 * the download that starts. The starter of that download is the browser
 * itself, so the disposable browser copy is cancelled and erased, and the
 * direct link goes back to the plugin that asked.
 *
 * The site knowledge (which control to click, what a direct link looks like)
 * stays in the plugin, exactly where createLinks lives; the framework only
 * offers the browser mechanic through plugins' ctx (resolveViaPage).
 */

/** Hidden-tab resolves run two at a time; the rest queue in order. */
const PAGE_RESOLVE_SLOTS = 2;
let pageResolveLive = 0;
const pageResolveFree = [];

/** Same share link, one tab: offers with repeated parts resolve once. */
const pageResolveInFlight = new Map();
/** referrer -> {capture(url), expiry} - decideHandoff stands down for these. */
const pageResolves = new Map();
/** tabId -> {capture(url)}: the decisive owner (the download's request went
 * through this tab), independent of what referrer the item reports. */
const pageResolveByTab = new Map();

const PAGE_RESOLVE_HIDDEN_MS = 25 * 1000; // page load + silent widget work
const PAGE_RESOLVE_REVEALED_MS = 40 * 1000; // a challenge wants the user
const PAGE_RESOLVE_TTL_MS = 2 * 60 * 1000;
const PAGE_RESOLVE_TICK_MS = 600;
const PAGE_RESOLVE_CLICKS = 8;
const PAGE_RESOLVE_CLICK_MS = 1200;

/** The plugin's choices. `pick` runs in the page's own context (self-contained
 * like the menu's collectors); it clicks the real control and returns true
 * once. `direct(url)` decides whether a captured candidate is the direct link
 * the plugin promised - it is deliberately plugin-owned. */
async function resolveViaPage(pageUrl, opts) {
    const u = httpOnly(pageUrl);
    if (!u || !opts || typeof opts.pick !== "function" || typeof opts.direct !== "function") {
        throw new Error("resolveViaPage: pick(url) and direct(url) are required");
    }
    pageResolveSweep();
    const existing = pageResolveInFlight.get(u);
    if (existing) return existing;
    const run = (async () => {
        await pageResolveSlot();
        try {
            return await runPageResolve(u, opts);
        } finally {
            pageResolveRelease();
            pageResolveInFlight.delete(u);
        }
    })().catch(() => null);
    pageResolveInFlight.set(u, run);
    return run;
}

async function pageResolveSlot() {
    if (pageResolveLive < PAGE_RESOLVE_SLOTS) {
        pageResolveLive++;
        return;
    }
    await new Promise((free) => pageResolveFree.push(free));
    pageResolveLive++;
}

function pageResolveRelease() {
    pageResolveLive--;
    const next = pageResolveFree.shift();
    if (next) next();
}

function pageResolveSweep() {
    const now = Date.now();
    for (const [ref, p] of pageResolves) if (p.expiry <= now) pageResolves.delete(ref);
}

async function runPageResolve(pageUrl, opts) {
    const job = { capture: null };
    const looksDirect = (s) => {
        if (typeof s !== "string") return false;
        try {
            const proto = new URL(s).protocol;
            return (proto === "http:" || proto === "https:") && opts.direct(s);
        } catch {
            return false;
        }
    };
    const settle = (url) => {
        if (job.capture !== null || !looksDirect(url)) return;
        job.capture = url;
    };
    pageResolves.set(pageUrl, { expiry: Date.now() + PAGE_RESOLVE_TTL_MS, capture: settle });
    const onCreated = (item) => {
        // The referrer is the page we opened; other tabs' downloads never match.
        if ((item?.referrer || "") !== pageUrl) return;
        settle(item.finalUrl || item.url);
    };
    chrome.downloads.onCreated.addListener(onCreated);
    let tabId = -1;
    try {
        tabId = (await chrome.tabs.create({ url: pageUrl, active: false })).id;
        pageResolveByTab.set(tabId, { capture: settle });
        let deadline = Date.now() + PAGE_RESOLVE_HIDDEN_MS;
        let loaded = false;
        let clicks = 0;
        let revealed = false;
        while (job.capture === null) {
            const rest = deadline - Date.now();
            if (rest <= 0) break;
            const tabState = await chrome.tabs.get(tabId).then((t) => ({ status: t.status, url: t.url })).catch(() => null);
            if (!tabState) break; // the tab vanished; nothing further to click
            settle(tabState.url);
            for (const r of reqByRequestId.values()) {
                // Only requests our own tab made: the map is process-wide and a
                // stale record from another resolve must not supply the badge.
                if (r.responded && r.resHeaders && r.tabId === tabId) {
                    for (const v of Object.values(r.resHeaders)) settle(v);
                }
            }
            if (!loaded && tabState.status === "complete") loaded = true;
            if (loaded && clicks < PAGE_RESOLVE_CLICKS) {
                try {
                    const [{ result }] = await chrome.scripting.executeScript({ target: { tabId }, func: opts.pick });
                    if (result === true) clicks = PAGE_RESOLVE_CLICKS; // armed; wait for the capture
                    else clicks++;
                } catch {
                    // The world navigated (capture may already be running): stop clicking.
                    clicks = PAGE_RESOLVE_CLICKS;
                }
            }
            // The host wanted the user: show the page once and keep waiting.
            if (!revealed && Date.now() + PAGE_RESOLVE_TICK_MS >= deadline) {
                revealed = true;
                deadline += PAGE_RESOLVE_REVEALED_MS;
                await chrome.tabs.update(tabId, { active: true }).catch(() => {});
            }
            if (job.capture !== null) break;
            await new Promise((resolve) => setTimeout(resolve, Math.min(rest, PAGE_RESOLVE_TICK_MS)));
        }
        return job.capture;
    } finally {
        chrome.downloads.onCreated.removeListener(onCreated);
        pageResolves.delete(pageUrl);
        if (tabId >= 0) {
            pageResolveByTab.delete(tabId);
            chrome.tabs.remove(tabId).catch(() => {});
        }
    }
}

/** The browser kept its copy after a `new` answer: drop the daemon's task. */
function abandonProposal(proposal) {
    send({ id: `ab-${nextId++}`, cmd: { type: "abandon", proposal } }).catch(() => {});
}

/* --- duplicates: a small window lists what ripdl already has --------- */

/** How long a duplicate window's job stays readable. */
const DUP_JOB_TTL_MS = 15 * 60 * 1000;
/** Links ripdl handed back to the browser ("Let browser download"), with the
 * time each expires. The browser's own copy must not be seized again, or the
 * choice would loop straight back into the window. */
const BROWSER_OWN_TTL_MS = 60 * 1000;
const browserOwned = new Map();

function ownBrowserDownload(url) {
    const now = Date.now();
    for (const [known, until] of browserOwned) if (until <= now) browserOwned.delete(known);
    browserOwned.set(url, now + BROWSER_OWN_TTL_MS);
}

/** True once for a download the browser started on ripdl's behalf. */
function takeOwnBrowserDownload(item) {
    for (const url of [item.url, item.finalUrl]) {
        const until = url ? browserOwned.get(url) : undefined;
        if (until === undefined) continue;
        browserOwned.delete(url);
        return until > Date.now();
    }
    return false;
}

/** A `duplicate` verdict on a browser download. The browser's copy is already
 * stopped when `taken`; otherwise the browser keeps going and the window says so. */
function onCaptureDuplicate(item, verdict, taken) {
    openDuplicateWindow({
        items: [{
            url: item.finalUrl || item.url,
            filename: item.filename || "",
            headers: verdict.headers || {},
            task: verdict.task ?? null,
        }],
        browser: { id: item.id, taken },
    }).catch(() => {});
}

/** One window per send. Its job (rows, choices, the browser copy) lives in
 * session storage, is never sent to a page, and is dropped when the window closes. */
async function openDuplicateWindow(job) {
    const key = `${Date.now().toString(36)}-${nextId++}`;
    const slot = `dup:${key}`;
    const cutoff = Date.now() - DUP_JOB_TTL_MS;
    const stored = await chrome.storage.session.get(null);
    const stale = Object.keys(stored).filter((k) => k.startsWith("dup:") && (stored[k]?.createdAt || 0) < cutoff);
    if (stale.length) await chrome.storage.session.remove(stale);
    await chrome.storage.session.set({ [slot]: { ...job, createdAt: Date.now() } });
    const win = await chrome.windows.create({
        url: chrome.runtime.getURL(`duplicate.html?key=${key}`),
        type: "popup",
        width: 480,
        height: Math.min(640, 210 + 116 * Math.min(job.items.length, 4)),
        focused: true,
    });
    const saved = (await chrome.storage.session.get(slot))[slot];
    if (saved) await chrome.storage.session.set({ [slot]: { ...saved, windowId: win.id } });
}

chrome.windows.onRemoved.addListener((windowId) => {
    chrome.storage.session.get(null).then((all) => {
        const gone = Object.keys(all).filter((k) => k.startsWith("dup:") && all[k]?.windowId === windowId);
        if (gone.length) return chrome.storage.session.remove(gone);
    }).catch(() => {});
});

/** One request to the daemon on behalf of the duplicate window. */
function ask(cmd) {
    return send({ id: `dup-${nextId++}`, cmd });
}

/** The daemon's answer, as the window's answer. */
function outcomeOf(resp, okMessage) {
    const type = resp?.reply?.type;
    if (type === "ok" || type === "added") return { ok: true, message: okMessage };
    if (type === "error") return { ok: false, error: resp.reply.message || "ripdl could not do that." };
    return { ok: false, error: "ripdl is not running." };
}

/** "Let browser download": Chrome fetches the link itself. The URL is marked
 * first, so the handoff gives that one download back to the browser. */
async function startBrowserCopy(url) {
    if (!httpOnly(url)) return { ok: false, error: "The browser cannot fetch this link." };
    ownBrowserDownload(url);
    try {
        await chrome.downloads.download({ url });
        return { ok: true, message: "The browser is downloading it." };
    } catch {
        browserOwned.delete(url);
        return { ok: false, error: "The browser refused the download." };
    }
}

/** The window's choice for row `index` of job `key`. The window only names a
 * row and a choice; the URL, headers and task come from the stored job. */
async function duplicateAction(key, index, action) {
    const slot = `dup:${key}`;
    const job = (await chrome.storage.session.get(slot))[slot];
    const item = job?.items?.[index];
    if (!item) return { ok: false, error: "This choice has expired. Start the download again." };
    // The browser's copy, while it still runs: a ripdl choice stops it below.
    const browser = job.browser && !job.browser.taken ? job.browser : null;
    if (action === "browser") {
        if (browser) return { ok: true, message: "The browser keeps its copy." };
        return startBrowserCopy(item.url);
    }
    const taskId = item.task?.id || null;
    let out;
    if (action === "copy") {
        out = outcomeOf(
            await ask({ type: "add", url: item.url, dest_dir: null, headers: item.headers || {}, on_duplicate: "copy" }),
            "Downloading a second copy.",
        );
    } else if (!taskId) {
        return { ok: false, error: "ripdl no longer has this download." };
    } else if (action === "keep") {
        const listed = await ask({ type: "list" });
        const exists = (listed?.reply?.tasks || []).some((t) => t.id === taskId);
        out = exists
            ? { ok: true, message: "Kept ripdl's download." }
            : { ok: false, error: "ripdl no longer has this download." };
    } else if (action === "open" || action === "reveal") {
        out = outcomeOf(
            await ask({ type: "open", id: taskId, folder: action === "reveal" }),
            action === "reveal" ? "Shown in its folder." : "Opened.",
        );
    } else if (action === "resume") {
        out = outcomeOf(await ask({ type: "resume", id: taskId }), "Resumed.");
    } else {
        return { ok: false, error: "Unknown choice." };
    }
    if (!out.ok || !browser) return out;
    // ripdl has the file now: the browser's copy stops, so the bytes come once.
    if (await takeBrowserItem({ id: browser.id })) {
        job.browser.taken = true;
        await chrome.storage.session.set({ [slot]: job });
    } else {
        out.message = `${out.message} The browser's copy could not be stopped.`;
    }
    return out;
}

chrome.downloads.onDeterminingFilename.addListener(
    filenameListener({
        decide: decideHandoff,
        takeItem: takeBrowserItem,
        abandon: abandonProposal,
        onDuplicate: onCaptureDuplicate,
    }),
);

/* --- selection / all-links: on-demand scripting, never resident ------- */

/** Injected at menu-click time, so it must be self-contained. Anchor text is
 * kept only when it names a file: a page full of "Download" anchors must not
 * turn every link into a part called "Download". Bare URLs count too: a list
 * pasted as plain text has no anchors at all. */
function collectLinksInView(mode, selectedText) {
    const FILENAME_RE = /^[^\\/:*?"<>|\r\n]{1,120}\.[A-Za-z0-9]{1,8}$/;
    const URL_RE = /https?:\/\/[^\s<>"'`]+/gi;
    const seen = new Set();
    const links = [];
    const add = (raw, name) => {
        try {
            const u = new URL(raw, document.baseURI);
            if (u.protocol !== "http:" && u.protocol !== "https:") return;
            if (seen.has(u.href) || links.length >= 100) return;
            seen.add(u.href);
            links.push({ url: u.href, name: name || "" });
        } catch {
            // unresolvable href: skip
        }
    };
    const addAnchor = (a) => {
        const text = (a.textContent || "").trim();
        add(a.href, FILENAME_RE.test(text) ? text : "");
    };
    if (mode !== "selection") {
        document.querySelectorAll("a[href]").forEach(addAnchor);
        return { links };
    }
    const sel = window.getSelection();
    if (sel && sel.rangeCount) {
        // Selection ranges must be cloned into a scratch node to expose
        // their anchor elements (apply-to-selection standard technique).
        const scratch = document.createElement("div");
        for (let i = 0; i < sel.rangeCount; ++i) {
            scratch.appendChild(sel.getRangeAt(i).cloneContents());
        }
        scratch.querySelectorAll("a[href]").forEach(addAnchor);
    }
    // Text typed into a field is not part of the document selection.
    const field = document.activeElement;
    const inField = !!field && (field.tagName === "INPUT" || field.tagName === "TEXTAREA")
        && typeof field.selectionStart === "number";
    const fieldText = inField ? field.value.slice(field.selectionStart, field.selectionEnd) : "";
    for (const m of (fieldText || String(selectedText || "")).matchAll(URL_RE)) {
        add(m[0].replace(/[.,;:!?]+$/, "").replace(/[)\]}>]+$/, ""), "");
    }
    return { links };
}

/* --- context menu: explicit, on-demand handoff ------------------------ */

/** Tell the page what happened, as a chip line. Protected pages have no
 * content script, so a failed delivery is simply ignored. */
function pageStatus(tabId, text, bad) {
    if (tabId < 0) return;
    chrome.tabs.sendMessage(tabId, { type: "ripdl_status", text, bad: !!bad }).catch(() => {});
}

/** Show a short badge result, then the liveness marker again. */
function flashBadge(text) {
    setBadge(text);
    setTimeout(() => setBadge(hostAlive ? "" : "off"), 1500);
}

/** One result for a menu send: the page line and the badge. */
function reportSend(tabId, res) {
    const done = res ? res.bundles + res.singles : 0;
    const dupes = res ? res.duplicates || 0 : 0;
    const text = done ? `Queued ${res.summary}` : dupes ? `${res.summary}: choose in the ripdl window` : "Nothing was queued";
    pageStatus(tabId, text, (!done && !dupes) || !!res?.failed);
    // The badge counts what ripdl gained: a bundle is one row. "=" marks links ripdl already had.
    flashBadge(done ? String(done) : dupes ? "=" : (hostAlive ? "!" : "off"));
}

/** Bare http(s) URLs in a selection's text - the same rule
 * collectLinksInView applies to plain text. This is the fallback when the page
 * itself cannot be read, because the menu already carries the selection text. */
function linksFromText(text) {
    const out = [];
    const seen = new Set();
    for (const m of String(text || "").matchAll(/https?:\/\/[^\s<>"'`]+/gi)) {
        const url = m[0].replace(/[.,;:!?]+$/, "").replace(/[)\]}>,]+$/, "");
        if (!url || seen.has(url) || out.length >= MAX_OFFER_LINKS) continue;
        seen.add(url);
        out.push({ url, name: "" });
    }
    return out;
}

/** Selection and all-links: read the page on demand, then queue the groups
 * the same way the copy chip would. The page read is best effort: when it is
 * refused (site access off the page, a protected page, a policy), the selection
 * text the menu already carries still holds the URLs, so those are queued
 * instead of the attempt failing without a trace. */
async function queueFromPage(tabId, info, mode) {
    if (tabId < 0) return;
    setBadge("...");
    let links = [];
    let refused = null;
    try {
        const [{ result }] = await chrome.scripting.executeScript({
            target: { tabId },
            func: collectLinksInView,
            args: [mode, info.selectionText || ""],
        });
        links = (result && result.links) || [];
    } catch (err) {
        // chrome:// or protected pages, or site access off the page: the
        // selection text the menu passes is still the user's pick, so use it.
        refused = String(err?.message || err).slice(0, 90);
        console.warn(`ripdl: could not read the page (${refused}); using the selection text instead`);
        links = mode === "selection" ? linksFromText(info.selectionText) : [];
    }
    if (!links.length) {
        pageStatus(tabId, refused
            ? `ripdl could not read this page (${refused}); nothing was queued`
            : mode === "selection" ? "No links found in the selection" : "No links found on this page",
            true);
        flashBadge("!");
        return;
    }
    pageStatus(tabId, `Queuing ${links.length} link${links.length === 1 ? "" : "s"}\u2026`, false);
    try {
        // Grouped like the copy chip: twelve parts become one task, one folder.
        const preview = await previewGroups(sanitizeLinks(links));
        reportSend(tabId, await sendGroups(preview.groups, info.pageUrl));
    } catch (err) {
        console.warn(`ripdl: queuing failed: ${String(err?.message || err)}`);
        reportSend(tabId, null);
    }
}

/** Single link: the same pipeline as a selection, so a share link is resolved
 * to its file rather than queued as the web page behind it. */
async function queueLink(tabId, info) {
    const links = sanitizeLinks([info.linkUrl]);
    if (!links.length) {
        reportSend(tabId, null);
        return;
    }
    setBadge("...");
    try {
        const preview = await previewGroups(links);
        reportSend(tabId, await sendGroups(preview.groups, info.pageUrl));
    } catch {
        reportSend(tabId, null);
    }
}

chrome.contextMenus.onClicked.addListener(async (info, tab) => {
    // Chromium hands the tab as the event's SECOND argument (tab), never on
    // OnClickInfo: info.tabId does not exist, so a menu click used to fall
    // to -1 and queueFromPage() would return silently (filed 2026-10-27).
    const tabId = typeof (tab && tab.id) === "number" ? tab.id : -1;
    if (info.menuItemId === "ripdl-selection" || info.menuItemId === "ripdl-all-links") {
        await queueFromPage(tabId, info, info.menuItemId === "ripdl-selection" ? "selection" : "all");
        return;
    }
    if (info.menuItemId === "ripdl-link" && info.linkUrl) {
        await queueLink(tabId, info);
        return;
    }
    if (info.menuItemId === "ripdl-page" && info.pageUrl) {
        setBadge("...");
        const r = await addOne(info.pageUrl, {});
        const accepted = r.status === "added";
        const dupe = r.status === "duplicate";
        if (dupe) {
            openDuplicateWindow({ items: [{ url: info.pageUrl, filename: "", headers: {}, task: r.task }], browser: null }).catch(() => {});
        }
        pageStatus(tabId, accepted ? "Queued 1 download" : dupe ? "Already in ripdl: choose in the ripdl window" : "Nothing was queued", !accepted && !dupe);
        flashBadge(accepted ? "1" : dupe ? "=" : (hostAlive ? "!" : "off"));
    }
});

chrome.runtime.onInstalled.addListener(() => {
    chrome.contextMenus.create({
        id: "ripdl-link",
        title: "Download link with ripdl",
        contexts: ["link"],
    });
    chrome.contextMenus.create({
        id: "ripdl-page",
        title: "Download this page URL with ripdl",
        contexts: ["page"],
    });
    chrome.contextMenus.create({
        id: "ripdl-selection",
        title: "Download selected links with ripdl",
        contexts: ["selection"],
    });
    chrome.contextMenus.create({
        id: "ripdl-all-links",
        title: "Download all links on this page with ripdl",
        contexts: ["page"],
    });
});

/* --- lifecycle -------------------------------------------------------- */

chrome.runtime.onStartup.addListener(() => {
    if (!port) connectHost();
});
chrome.runtime.onInstalled.addListener(() => {
    if (!port) connectHost();
});
connectHost();
