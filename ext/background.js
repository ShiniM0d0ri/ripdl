/**
 * ripdl WebExtension - capture -> handoff -> fallback.
 *
 * Behavior contract (clean-room; do not import any third-party code):
 *   1. ripdl NEVER watches what you browse. Context menus hand URLs over on
 *      demand; downloads.onDeterminingFilename seizes a download only while
 *      the daemon is confirmed alive.
 *   2. ripdl NEVER harvests cookies. Headers passed to the daemon are the
 *      Referer and the browser's own User-Agent (values needed to fetch the
 *      same bytes the browser would get). Cookie sharing is opt-in later.
 *   3. If the daemon is missing or rejects a task, the browser downloads
 *      normally. The extension exists to make ripdl better, not to nag.
 *   4. No content scripts, no polling loops, no history permission, no
 *      remote messaging channels.
 *
 * The daemon owns capture policy (size floors, extension lists, fallback).
 * The extension only supplies what the wire protocol needs.
 */

const HOST_NAME = "org.ripdl.host";

/** webRequest requestHeaders we keep (MV3 needs extraHeaders to see UA). */
const KEPT_HEADERS = new Set(["referer", "user-agent"]);

let port = null;
let hostAlive = false;
let nextId = 1;

/** webRequest requestId -> {url, method, headers: {name: value}} */
const reqByRequestId = new Map();
/** url -> details (downloads.onDeterminingFilename cannot see requestIds). */
const reqByUrl = new Map();
const MAP_BUDGET = 512;
const MAP_TTL_MS = 120000;

function setBadge(text) {
    chrome.action.setBadgeText({ text });
    chrome.action.setBadgeBackgroundColor({ color: hostAlive ? "#2e7d32" : "#9e9e9e" });
}

function settings() {
    return chrome.storage.local.get({ capture: true }).catch(() => ({ capture: true }));
}

/* --- native messaging host ------------------------------------------- */

function connectHost() {
    try {
        port = chrome.runtime.connectNative(HOST_NAME);
    } catch {
        hostAlive = false;
        setBadge("off");
        return;
    }
    port.onMessage.addListener(onHostMessage);
    port.onDisconnect.addListener(() => {
        port = null;
        hostAlive = false;
        setBadge("off");
        // Rejecting waiters resolves their fallbacks (browser downloads on).
        for (const p of pending.values()) p.resolve(null);
        pending.clear();
    });
    // Cheap liveness probe: the daemon replies `ok` to an empty queue.
    send({ id: `probe-${nextId++}`, cmd: { type: "list" } }).then((r) => {
        hostAlive = !!r;
        setBadge(hostAlive ? "" : "off");
    });
}

const pending = new Map(); // id -> {resolve, timer}

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

/* --- request details capture (non-blocking, bounded) ------------------ */

function remember(details) {
    const headers = {};
    for (const h of details.requestHeaders || []) {
        const name = h.name.toLowerCase();
        if (KEPT_HEADERS.has(name) && h.value) headers[name] = h.value;
    }
    const rec = { url: details.url, method: details.method, headers, at: Date.now() };
    reqByRequestId.set(details.requestId, rec);
    reqByUrl.set(details.url, rec);
    // Grow old entries out; maps are always bounded.
    for (const m of [reqByRequestId, reqByUrl]) {
        if (m.size > MAP_BUDGET) {
            const firstKey = m.keys().next().value;
            m.delete(firstKey);
        }
    }
    setTimeout(() => {
        reqByRequestId.delete(details.requestId);
        reqByUrl.delete(details.url);
    }, MAP_TTL_MS);
}

chrome.webRequest.onSendHeaders.addListener(
    remember,
    { urls: ["<all_urls>"] },
    chrome.runtime.getManifest().manifest_version === 3
        ? ["requestHeaders", "extraHeaders"]
        : ["requestHeaders"]
);

/** Headers the engine needs to fetch the same bytes the browser would. */
function headersFor(item) {
    const rec = reqByUrl.get(item.finalUrl) || reqByUrl.get(item.url);
    const out = {};
    if (rec?.headers.referer) out["Referer"] = rec.headers.referer;
    if (rec?.headers["user-agent"]) out["User-Agent"] = rec.headers["user-agent"];
    return out;
}

/* --- capture: watch -> cancel -> handoff (or let the browser proceed) -- */

chrome.downloads.onDeterminingFilename.addListener(async (item, suggest) => {
    const { capture } = await settings();
    if (!capture || !hostAlive) {
        suggest();
        return;
    }

    const headers = headersFor(item);
    const url = item.finalUrl || item.url;

    // Handoff offer goes out BEFORE killing the browser item when the daemon
    // is known-alive: if the offer fails, nothing has been disturbed yet.
    const resp = await send({ id: `dl-${nextId++}`, cmd: { type: "add", url, dest_dir: null, headers } });
    const accepted = resp?.reply?.type === "added";
    if (!accepted) {
        hostAlive = !!resp; // a null reply means the host is gone.
        setBadge(hostAlive ? "" : "off");
        suggest();
        return;
    }

    // Accepted: stop the browser's copy and let the daemon take the bytes.
    try {
        await chrome.downloads.cancel(item.id);
        await chrome.downloads.erase({ id: item.id });
    } catch {
        // Already gone (service-worker races); erase-only is fine.
    }
});

chrome.downloads.onChanged.addListener((delta) => {
    // Best-effort badge pulse on daemon-owned completions is out of scope
    // for the skeleton; the daemon owns progress notification.
    void delta;
});

/* --- context menu: explicit, on-demand handoff ------------------------ */

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

chrome.contextMenus.onClicked.addListener(async (info) => {
    const url = info.linkUrl || info.pageUrl;
    if (!url) return;
    const resp = await send({ id: `ctx-${nextId++}`, cmd: { type: "add", url, dest_dir: null, headers: {} } });
    const accepted = resp?.reply?.type === "added";
    setBadge(accepted ? "1" : "off");
    setTimeout(() => setBadge(hostAlive ? "" : "off"), 1500);
});

/* --- lifecycle -------------------------------------------------------- */

chrome.runtime.onStartup.addListener(() => {
    connectHost();
});
chrome.runtime.onInstalled.addListener(() => {
    connectHost();
});
connectHost();
