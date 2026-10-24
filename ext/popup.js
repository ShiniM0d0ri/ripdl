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
  chrome.runtime.sendMessage({ type: "ripdl_light_probe" }); // wakes the SW
  // The SW answers through the badge; also try the storage of last state.
  setTimeout(async () => {
    const { hostAlive } = await chrome.storage.local.get({ hostAlive: false });
    statusEl.textContent = hostAlive ? "daemon: connected" : "daemon: not running";
    statusEl.className = hostAlive ? "ok" : "down";
  }, 400);
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

function markQueued(btn, text) {
    btn.textContent = text || "queued ✓";
    btn.disabled = true;
    setTimeout(() => { btn.textContent = text || "queued ✓"; }, 1200);
}

async function sendTask(cmd) {
    try {
        const resp = await chrome.runtime.sendMessage({ type: "ripdl_send", cmd });
        return resp?.ok === true;
    } catch {
        return false;
    }
}

chrome.runtime.sendMessage({ type: "ripdl_hunt_query" }, (hunt) => {
    if (chrome.runtime.lastError || !hunt) return;  // SW restart race: retry via storage later
    const { pageUrl, videoish, media } = hunt;
    if (!videoish && media.length === 0) return;    // nothing to offer: popup stays quiet

    huntEl.classList.remove("hidden");
    if (videoish) {
        const pageBtn = document.getElementById("hunt-page");
        pageBtn.addEventListener("click", async () => {
            pageBtn.disabled = true;
            const ok = await sendTask({ type: "add", url: pageUrl, dest_dir: null, headers: {}, kind: "video" });
            pageBtn.disabled = false;
            markQueued(pageBtn, ok ? "queued for yt-dlp ✓" : "daemon unreachable ✗");
            if (!ok) setTimeout(() => { pageBtn.textContent = "Download page as video (yt-dlp)"; }, 2400);
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
        btn.textContent = "Download media file";
        btn.addEventListener("click", async () => {
            btn.disabled = true;
            const ok = await sendTask({ type: "add", url: m.url, dest_dir: null, headers: {} });
            btn.disabled = false;
            markQueued(btn, ok ? "queued ✓" : "daemon unreachable ✗");
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

// Ask the SW for fresh liveness; it mirrors it into storage.
chrome.runtime.sendMessage({ type: "ripdl_light_probe" }).catch(() => {});
probeDaemon();

// Hoster plugins options page (chrome.runtime.openOptionsPage works from any extension page).
document.getElementById("plugins-link").addEventListener("click", (e) => { e.preventDefault(); chrome.runtime.openOptionsPage(); });
