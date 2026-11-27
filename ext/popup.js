// ripdl popup: state + the two privacy-relevant toggles.
"use strict";

const statusEl = document.getElementById("status");
const captureEl = document.getElementById("capture");
const shareCookiesEl = document.getElementById("shareCookies");
const versionEl = document.getElementById("version");

async function loadVersion() {
    versionEl.textContent = `Extension ${chrome.runtime.getManifest().version} · app version unavailable`;
    try {
        const info = await chrome.runtime.sendMessage({ type: "ripdl_ext_info" });
        if (!info?.appVersion) return;
        if (info.skew === 0) {
            versionEl.textContent = `Extension ${info.extVersion} · app ${info.appVersion}`;
        } else if (info.skew < 0) {
            versionEl.textContent = `Extension ${info.extVersion} · app ${info.appVersion} · Reload in chrome://extensions`;
            versionEl.className = "warn";
        } else {
            versionEl.textContent = `Extension ${info.extVersion} · app ${info.appVersion} · update ripdl`;
            versionEl.className = "warn";
        }
    } catch {
        // Daemon may be unavailable; the extension version remains useful.
    }
}

loadVersion();

async function probeDaemon() {
    statusEl.textContent = "daemon: connecting";
    statusEl.className = "down";
    try {
        const resp = await chrome.runtime.sendMessage({ type: "ripdl_light_probe" });
        const alive = typeof resp?.hostAlive === "boolean"
            ? resp.hostAlive
            : (await chrome.storage.local.get({ hostAlive: false })).hostAlive;
        statusEl.textContent = alive ? "daemon: connected" : "daemon: not running";
        statusEl.className = alive ? "ok" : "down";
        if (alive) loadVersion();
    } catch {
        statusEl.textContent = "daemon: not running";
        statusEl.className = "down";
    }
}

async function loadSettings() {
  const s = await chrome.storage.local.get({ capture: true, shareCookies: false, hostAlive: false });
  captureEl.checked = s.capture;
  shareCookiesEl.checked = s.shareCookies;
  statusEl.textContent = s.hostAlive ? "daemon: connected" : "daemon: not running";
  statusEl.className = s.hostAlive ? "ok" : "down";
}

loadSettings();

captureEl.addEventListener("change", async () => {
  await chrome.storage.local.set({ capture: captureEl.checked });
});

shareCookiesEl.addEventListener("change", async () => {
  if (!shareCookiesEl.checked) {
    // Off: revoke the permission too, so cookies stop being readable at all.
    await chrome.storage.local.set({ shareCookies: false });
    const granted = await chrome.permissions.contains({ permissions: ["cookies"] });
    if (granted) await chrome.permissions.remove({ permissions: ["cookies"] });
    return;
  }
  // On: ask the browser for the cookies permission, explicitly.
  const granted = await chrome.permissions.request({ permissions: ["cookies"] });
  await chrome.storage.local.set({ shareCookies: granted });
  shareCookiesEl.checked = granted;
});

document.getElementById("probe").addEventListener("click", probeDaemon);

/* --- hunt offer: video/stream detection for the active page ------------ */

const huntEl = document.getElementById("hunt");
const huntMediaEl = document.getElementById("hunt-media");
const shortUrl = (u) => {
    try {
        const p = new URL(u);
        return p.hostname + p.pathname;
    } catch {
        return u;
    }
};

const SVG_NS = "http://www.w3.org/2000/svg";
function makeIcon(paths, size = 13) {
    const svg = document.createElementNS(SVG_NS, "svg");
    svg.setAttribute("class", "icon");
    svg.setAttribute("viewBox", "0 0 24 24");
    svg.setAttribute("width", String(size));
    svg.setAttribute("height", String(size));
    svg.setAttribute("fill", "none");
    svg.setAttribute("stroke", "currentColor");
    svg.setAttribute("stroke-width", "2");
    svg.setAttribute("stroke-linecap", "round");
    svg.setAttribute("stroke-linejoin", "round");
    svg.setAttribute("aria-hidden", "true");
    for (const [tag, attrs] of paths) {
        const node = document.createElementNS(SVG_NS, tag);
        for (const [k, v] of Object.entries(attrs)) node.setAttribute(k, String(v));
        svg.append(node);
    }
    return svg;
}

function markQueued(btn, ok, text, fallbackText) {
    const lbl = btn.querySelector(".btn-label") || btn;
    lbl.textContent = text;
    btn.disabled = ok;
    if (!ok) {
        setTimeout(() => { lbl.textContent = fallbackText; }, 2400);
    }
}

async function sendTask(cmd) {
    try {
        const resp = await chrome.runtime.sendMessage({ type: "ripdl_send", cmd });
        return { ok: resp?.ok === true, error: resp?.error || null };
    } catch {
        return { ok: false, error: "daemon unreachable" };
    }
}

chrome.runtime.sendMessage({ type: "ripdl_hunt_query" }, (hunt) => {
    if (chrome.runtime.lastError || !hunt) return;  // SW restart race: retry via storage later
    const { pageUrl, videoish, media } = hunt;
    if (!videoish && media.length === 0) return;    // nothing to offer: popup stays quiet

    huntEl.classList.remove("hidden");
    if (videoish) {
        const pageBtn = document.getElementById("hunt-page");
        const idleText = pageBtn.textContent || "Download page as video (yt-dlp)";
        pageBtn.addEventListener("click", async () => {
            pageBtn.disabled = true;
            const res = await sendTask({ type: "add", url: pageUrl, dest_dir: null, headers: {}, kind: "video" });
            markQueued(
                pageBtn,
                res.ok,
                res.ok ? "queued for yt-dlp \u2713" : `${res.error || "daemon unreachable"} \u2717`,
                idleText,
            );
        });
    } else {
        document.getElementById("hunt-page").classList.add("hidden");
    }

    for (const m of media.slice(-5)) {
        const row = document.createElement("div");
        row.className = "hunt-media-row";
        const urlText = document.createElement("div");
        urlText.className = "hunt-url";
        urlText.textContent = shortUrl(m.url);
        const btn = document.createElement("button");
        btn.className = "hunt-file-btn";
        btn.type = "button";
        const span = document.createElement("span");
        span.className = "btn-label";
        span.textContent = "Download media file";
        btn.append(
            makeIcon([
                ["path", { d: "M12 15V3" }],
                ["path", { d: "M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" }],
                ["path", { d: "m7 10 5 5 5-5" }],
            ]),
            span,
        );
        btn.addEventListener("click", async () => {
            btn.disabled = true;
            const res = await sendTask({ type: "add", url: m.url, dest_dir: null, headers: {} });
            markQueued(
                btn,
                res.ok,
                res.ok ? "queued \u2713" : `${res.error || "daemon unreachable"} \u2717`,
                "Download media file",
            );
        });
        row.append(urlText, btn);
        huntMediaEl.appendChild(row);
    }
    if (videoish && media.length === 0) {
        const note = document.createElement("div");
        note.className = "hunt-note";
        note.textContent = "video page detected — yt-dlp will find the streams";
        huntEl.appendChild(note);
    }
});

probeDaemon();

// Hoster plugins options page & setup welcome page.
document.getElementById("plugins-link").addEventListener("click", (e) => { e.preventDefault(); chrome.runtime.openOptionsPage(); });
document.getElementById("welcome-link")?.addEventListener("click", (e) => {
    e.preventDefault();
    chrome.tabs.create({ url: chrome.runtime.getURL("welcome.html") });
});
