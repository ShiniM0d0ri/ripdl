// ripdl popup: state + the two privacy-relevant toggles.
"use strict";

const statusEl = document.getElementById("status");
const captureEl = document.getElementById("capture");
const shareCookiesEl = document.getElementById("shareCookies");

async function probeDaemon() {
  statusEl.textContent = "daemon: connecting…";
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

// Ask the SW for fresh liveness; it mirrors it into storage.
chrome.runtime.sendMessage({ type: "ripdl_light_probe" }).catch(() => {});
probeDaemon();
