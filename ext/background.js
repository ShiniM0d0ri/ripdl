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
 *
 * The daemon owns capture policy (size floors, extension lists, fallback).
 * The extension only supplies what the wire protocol needs.
 */

/* --- hoster plugins (user-loadable; see plugins.js) ------------------- */

import { pluginNameFor, pluginResolve, registry, validateRecipe } from "./plugins.js";

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
 * honest-fallback rule: queue it as-is rather than lose it. */
async function resolveParts(parts) {
    let resolved = 0;
    let unresolved = 0;
    const out = [];
    for (const p of parts) {
        let r = null;
        try { r = await pluginResolve(p); } catch { r = null; }
        if (r && r.url && r.url !== p.url) {
            resolved++;
            out.push({
                ...p,
                url: r.url,
                filename: r.name || p.filename,
                headers: r.headers && typeof r.headers === "object" ? { ...p.headers, ...r.headers } : p.headers,
            });
        } else {
            if (r) { /* matched but returned the same URL */ resolved++; }
            else { unresolved++; }
            out.push(p);
        }
    }
    return { parts: out, resolved, unresolved };
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

/** Keep only what the wire protocol needs: http(s), deduped, bounded. */
function sanitizeLinks(list) {
    const out = [];
    const seen = new Set();
    for (const item of (Array.isArray(list) ? list : [])) {
        if (out.length >= MAX_OFFER_LINKS) break;
        const url = httpOnly(typeof item === "string" ? item : item && item.url);
        if (!url || seen.has(url)) continue;
        seen.add(url);
        const raw = item && typeof item.name === "string" ? item.name.trim().slice(0, 200) : "";
        out.push({ url, filename: raw || null });
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
        // keeps its original URL and is counted, never dropped.
        const { parts, unresolved: unres } = await resolveParts(raw);
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
            if (await sendSingle(p)) singles++;
            else failed++;
        }
    }
    return { ok: bundles + singles > 0, bundles, singles, failed, unresolved };
}

async function sendSingle(part) {
    const resp = await send({
        id: `link-${nextId++}`,
        cmd: { type: "add", url: part.url, dest_dir: null, headers: part.headers || {} },
    });
    return resp?.reply?.type === "added";
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
        (async () => {
            const links = sanitizeLinks(msg.links);
            if (!links.length) {
                sendResponse({ groups: [], bundled: daemonBundles !== false });
                return;
            }
            const preview = await previewGroups(links);
            sendResponse({ groups: preview.groups, bundled: preview.bundled, hostAlive });
        })();
        return true; // async reply
    }
    if (msg?.type === "ripdl_offer_send") {
        (async () => {
            sendResponse(await sendGroups(msg.groups, msg.page));
        })();
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
                const r = await entry.plugin.resolve({ url: msg.url, filename: null }, { fetch: (u, o) => fetch(u, o) });
                out.resolved = r ? r.url : null;
                if (r?.name) out.name = r.name;
            }
            return { ok: true, ...out };
        });
        return true;
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
// video/audio sightings feed the popup's hunt offer.
chrome.webRequest.onHeadersReceived.addListener(
    (details) => {
        const rec = reqByRequestId.get(details.requestId);
        if (rec) {
            for (const h of details.responseHeaders || []) {
                const name = h.name.toLowerCase();
                if (name === "content-length") {
                    const n = Number(h.value);
                    if (Number.isFinite(n)) rec.contentLength = n;
                }
            }
            const ct = (details.responseHeaders || [])
                .find((h) => h.name.toLowerCase() === "content-type")
                ?.value || "";
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

/* --- capture: watch -> offer -> cancel (or let the browser proceed) ---- */

chrome.downloads.onDeterminingFilename.addListener(async (item, suggest) => {
    const { capture } = await settings();
    if (!capture || !hostAlive) {
        suggest();
        return false;
    }

    const rec = findRecord(item, false);
    if (!rec) {
        // Possibly a service-worker download whose request details are still
        // arriving: wait once for the race, then decide.
        return new Promise((resolve) => {
            setTimeout(async () => {
                const late = findRecord(item, true);
                if (!late) {
                    resolve(false);
                    suggest(); // unknown origin: browser keeps it.
                    return;
                }
                await seize(item, suggest, late);
                resolve(true);
            }, SW_RACE_WAIT_MS);
        });
    }
    await seize(item, suggest, rec);
    return true;
});

/** Offer the task to the daemon; accepted -> stop the browser's copy. */
async function seize(item, suggest, rec) {
    if (item.totalBytes > 0 && item.totalBytes < MIN_SEIZE_BYTES) {
        suggest(); // below the floor: browser handles small files fine.
        return;
    }
    const headers = await headersFor(item, rec);
    const url = item.finalUrl || item.url;
    const resp = await send({ id: `dl-${nextId++}`, cmd: { type: "add", url, dest_dir: null, headers } });
    const accepted = resp?.reply?.type === "added";
    if (!accepted) {
        setHostAlive(!!resp); // a null reply means the host is gone.
        suggest(); // daemon refused: the browser keeps its download.
        return;
    }
    try {
        await chrome.downloads.cancel(item.id);
        await chrome.downloads.erase({ id: item.id });
    } catch {
        // Already gone (service-worker races); erase-only is fine.
    }
}

/* --- selection / all-links: on-demand scripting, never resident ------- */

/** Injected at menu-click time; collects links absolutely. Anchor text is
 * kept only when it names a file: a page full of "Download" anchors must not
 * turn every link into a part called "Download". */
function collectLinksInView(mode) {
    const FILENAME_RE = /^[^\\/:*?"<>|\r\n]{1,120}\.[A-Za-z0-9]{1,8}$/;
    const seen = new Set();
    const links = [];
    const push = (a) => {
        try {
            const u = new URL(a.href, document.baseURI);
            if (u.protocol !== "http:" && u.protocol !== "https:") return;
            if (seen.has(u.href) || links.length >= 100) return;
            seen.add(u.href);
            const text = (a.textContent || "").trim();
            links.push({ url: u.href, name: FILENAME_RE.test(text) ? text : "" });
        } catch {
            // unresolvable href: skip
        }
    };
    if (mode === "selection") {
        const sel = window.getSelection();
        if (sel && sel.rangeCount) {
            // Selection ranges must be cloned into a scratch node to expose
            // their anchor elements (apply-to-selection standard technique).
            const scratch = document.createElement("div");
            for (let i = 0; i < sel.rangeCount; ++i) {
                scratch.appendChild(sel.getRangeAt(i).cloneContents());
            }
            scratch.querySelectorAll("a[href]").forEach(push);
        }
    } else {
        document.querySelectorAll("a[href]").forEach(push);
    }
    return { links };
}

/* --- context menu: explicit, on-demand handoff ------------------------ */

chrome.contextMenus.onClicked.addListener(async (info) => {
    if (info.menuItemId === "ripdl-selection" || info.menuItemId === "ripdl-all-links") {
        const mode = info.menuItemId === "ripdl-selection" ? "selection" : "all";
        if (!info.tabId) return;
        let links = [];
        try {
            const [{ result }] = await chrome.scripting.executeScript({
                target: { tabId: info.tabId },
                func: collectLinksInView,
                args: [mode],
            });
            links = (result && result.links) || [];
        } catch {
            // chrome:// or protected pages: scripting is refused; stay quiet.
            setBadge("!");
            setTimeout(() => setBadge(hostAlive ? "" : "off"), 1500);
            return;
        }
        if (!links.length) {
            setBadge("0");
            setTimeout(() => setBadge(hostAlive ? "" : "off"), 1500);
            return;
        }
        // An offer, not a pile: group it exactly the way the copy chip does,
        // so twelve parts become one task with one folder.
        const preview = await previewGroups(sanitizeLinks(links));
        const res = await sendGroups(preview.groups, info.pageUrl);
        // The badge counts what ripdl gained: bundles are one row each.
        setBadge(String(res.bundles + res.singles));
        setTimeout(() => setBadge(hostAlive ? "" : "off"), 1500);
        return;
    }

    const url = info.linkUrl || info.pageUrl;
    if (!url) return;
    const resp = await send({ id: `ctx-${nextId++}`, cmd: { type: "add", url, dest_dir: null, headers: {} } });
    const accepted = resp?.reply?.type === "added";
    setBadge(accepted ? "1" : "off");
    setTimeout(() => setBadge(hostAlive ? "" : "off"), 1500);
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
});

/* --- lifecycle -------------------------------------------------------- */

chrome.runtime.onStartup.addListener(() => {
    if (!port) connectHost();
});
chrome.runtime.onInstalled.addListener(() => {
    if (!port) connectHost();
});
connectHost();
