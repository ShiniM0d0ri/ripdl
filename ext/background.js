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
 *   4. No content scripts, no polling loops, no history permission, no
 *      remote messaging channels.
 *
 * The daemon owns capture policy (size floors, extension lists, fallback).
 * The extension only supplies what the wire protocol needs.
 */

const HOST_NAME = "org.ripdl.host";

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
    // Cheap liveness probe: the daemon replies `ok` to an empty listing.
    send({ id: `probe-${nextId++}`, cmd: { type: "list" } }).then((r) => {
        setHostAlive(!!r);
    });
}

/** Popup and other UI poke us here; answer is mirrored via storage. */
chrome.runtime.onMessage.addListener((msg) => {
    if (msg?.type === "ripdl_light_probe") {
        if (!port) {
            connectHost();
        } else {
            send({ id: `probe-${nextId++}`, cmd: { type: "list" } }).then((r) => setHostAlive(!!r));
        }
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

// Response sizes, for the service-worker download match.
chrome.webRequest.onHeadersReceived.addListener(
    (details) => {
        const rec = reqByRequestId.get(details.requestId);
        if (!rec) return;
        for (const h of details.responseHeaders || []) {
            if (h.name.toLowerCase() === "content-length") {
                const n = Number(h.value);
                if (Number.isFinite(n)) rec.contentLength = n;
            }
        }
    },
    { urls: ["<all_urls>"] },
    ["responseHeaders"]
);

chrome.webRequest.onSendHeaders.addListener(
    remember,
    { urls: ["<all_urls>"] },
    chrome.runtime.getManifest().manifest_version === 3
        ? ["requestHeaders", "extraHeaders"]
        : ["requestHeaders"]
);

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

/* --- context menu: explicit, on-demand handoff ------------------------ */

chrome.contextMenus.onClicked.addListener(async (info) => {
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
});

/* --- lifecycle -------------------------------------------------------- */

chrome.runtime.onStartup.addListener(() => {
    if (!port) connectHost();
});
chrome.runtime.onInstalled.addListener(() => {
    if (!port) connectHost();
});
connectHost();
