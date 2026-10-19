// ripdl GUI frontend: talks only to the Tauri commands, which relay to the
// daemon. No logic lives here - this is a task-list skin.

const { invoke } = window.__TAURI__.core;

const $ = (sel) => document.querySelector(sel);

// Static skeleton, queried once (used by every 500ms poll tick).
const TBODY = document.getElementById("task-rows");
const ST_COUNTS = document.getElementById("st-counts");

const CATEGORY_EXT = {
    video: ["mp4", "mkv", "avi", "mov", "webm", "flv", "wmv", "m4v", "mpg", "mpeg"],
    music: ["mp3", "flac", "wav", "aac", "ogg", "m4a", "opus", "wma", "mid"],
    images: ["jpg", "jpeg", "png", "gif", "webp", "bmp", "ico", "tiff"],
    documents: ["pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "txt", "epub", "djvu"],
    archives: ["zip", "rar", "7z", "tar", "gz", "tgz", "bz2", "xz", "iso"],
    programs: ["exe", "msi", "apk", "deb", "rpm", "msix", "dmg", "jar", "bat"],
};
const CAT_LABEL = {
    all: "All Downloads",
    unfinished: "Unfinished",
    completed: "Completed",
    torrents: "Torrents",
    video: "Videos",
    music: "Music",
    images: "Images",
    documents: "Documents",
    archives: "Archives",
    programs: "Programs",
};
const CATALOG = ["all", "unfinished", "completed", "torrents", "video", "music", "images", "documents", "archives", "programs"];

let tasks = [];
let currentCategory = "all";
let selectedId = null;
let lastStructKey = null;

// Plain passthrough: a failed command (bad URL, daemon said no) is not a
// dead connection - the connection pill is owned by poll()/setConn only.
function invokeSafe(cmd, args) {
    return invoke(cmd, args);
}

function setConn(ok) {
    const el = $("#st-conn");
    el.textContent = ok ? "daemon connected" : "daemon unreachable";
    el.className = "st-pill " + (ok ? "st-on" : "st-off");
}

function ext(name) {
    const i = name.lastIndexOf(".");
    return i >= 0 ? name.slice(i + 1).toLowerCase() : "";
}

// ext -> category, built once (categoryOf runs per task on every render).
const EXT_CAT = {};
for (const [cat, list] of Object.entries(CATEGORY_EXT)) {
    for (const e of list) EXT_CAT[e] = cat;
}

function categoryOf(name) {
    return EXT_CAT[ext(name)] || "other";
}

// The basename behind a URL, memoized: structKey/inCurrentView/render recompute
// the same value several times per poll tick; split+decode never changes.
const nameCache = new Map();

function urlName(url) {
    let cached = nameCache.get(url);
    if (cached === undefined) {
        let base = splitPath(url.split(/[?#]/)[0]).pop() || url;
        try { base = decodeURIComponent(base); } catch (_) { /* keep raw */ }
        cached = base || url;
        if (nameCache.size > 1000) nameCache.clear();
        nameCache.set(url, cached);
    }
    return cached;
}

// Windows Explorer-style basename: handles both separators.
function splitPath(p) {
    return p.split(/[\\/]/).filter(Boolean);
}

function baseName(p) {
    return splitPath(p).pop() || p;
}

// One-line error for the status cell (full text in the tooltip).
function shortError(e) {
    const m = String(e).replace(/^.*error: /, "");
    return m.length > 26 ? m.slice(0, 25) + "…" : m;
}

// Actionable hint for common failure classes, shown on hover.
function errorHint(e) {
    const s = String(e);
    if (/403/.test(s)) return "403 forbidden: server refused. Try Referer / User-Agent / Cookie headers (site may block plain clients).";
    if (/401|407/.test(s) || /auth/i.test(s)) return "Authentication required: wrong or missing credentials.";
    if (/404|410/.test(s)) return "The file is gone from the server (404).";
    if (/416/.test(s)) return "Bad byte range (416): server resumed state mismatched - re-download.";
    if (/429/.test(s)) return "Rate limited (429): too many requests; retry later.";
    if (/timeout|timed out/i.test(s)) return "Connection timed out.";
    if (/refused/i.test(s)) return "Connection refused.";
    if (/dns|resolve/i.test(s)) return "Could not resolve the hostname.";
    if (/registry|engine|init/i.test(s)) return "Local engine error - see the CLI/daemon output.";
    return "";
}

// The row title: torrent display name when known, else URL basename.
function rowName(t) {
    if (t.kind === "torrent" && t.name) return t.name;
    return urlName(t.url);
}

// Multi-line title for a row: full error + diagnostics.
function detailsTitle(t) {
    const lines = [];
    lines.push(t.url || "");
    if (t.dest_dir) lines.push("Location: " + t.dest_dir);
    if (t.info_hash) lines.push("Info-hash: " + t.info_hash);
    if (t.error) { lines.push("Error: " + t.error); const h = errorHint(t.error); if (h) lines.push(h); }
    return lines.join("\n");
}

// Tooltip for the status cell only.
function errorTitle(e) {
    if (!e) return "";
    const h = errorHint(e);
    return h ? `${e}\n${h}` : e;
}

function humans(bytes) {
    if (bytes == null) return "?";
    const unit = ["B", "KB", "MB", "GB", "TB"];
    let v = bytes;
    let u = 0;
    while (v >= 1000 && u < unit.length - 1) { v /= 1000; u++; }
    return `${v >= 100 || u === 0 ? Math.round(v) : v.toFixed(1)} ${unit[u]}`;
}

function stateLabel(t) {
    if (t.state === "queued" && t.start_at && t.start_at * 1000 > Date.now()) return "timed";
    if (t.state === "running" && t.kind === "video") return "hunt";
    // Torrent still fetching metadata: no bytes and no total yet.
    if (t.state === "running" && t.kind === "torrent" && !t.total) return "metadata";
    return t.state;
}

function percentOf(t) {
    if (t.percent != null) return t.percent;
    if (t.total) return (t.downloaded / t.total) * 100;
    return null;
}

// Percentage text: one decimal while actively downloading (progress reads as
// moving), integers elsewhere.
function pctText(pct, state) {
    return ["running", "hunt", "downloading", "verifying"].includes(state) ? pct.toFixed(1) : pct.toFixed(0);
}

// Size column for a task row.
function sizeText(t) {
    if (t.state === "completed") return humans(t.total || t.downloaded);
    if (t.percent != null) return t.total ? humans(t.total) : "?";
    return t.total ? `${humans(t.downloaded)} / ${humans(t.total)}` : humans(t.downloaded);
}

function etaOf(t) {
    if (!t.total || !t.speed_bps || t.speed_bps <= 0) return null;
    const left = (t.total - t.downloaded) / t.speed_bps;
    if (left <= 0) return null;
    const h = Math.floor(left / 3600), m = Math.floor((left % 3600) / 60);
    return h > 0 ? `${h}h ${m}m` : `${m}m ${Math.floor(left % 60)}s`;
}

// Inline sprite lookup; `cls` adds a color/utility class to the <svg>.
function ico(name, cls = "") {
    return `<svg class="ico ${cls}" aria-hidden="true"><use href="#i-${name}"/></svg>`;
}
const STATE_ICON = {
    completed: ico("check", "green"), running: ico("download", "blue"),
    queued: ico("clock", "gray"), paused: ico("pause", "gray"),
    failed: ico("alert", "red"), cancelled: ico("x", "gray"),
    timed: ico("clock", "amber"), hunt: ico("film", "violet"),
    downloading: ico("download", "blue"), verifying: ico("search", "gray"),
    seeding: ico("up", "green"), metadata: ico("globe", "blue"),
};
const STATUS_ICON = {
    completed: ico("check"), running: ico("download"), queued: ico("clock"),
    timed: ico("clock"), paused: ico("pause"), failed: ico("alert"),
    cancelled: ico("x"), hunt: ico("film"), downloading: ico("download"),
    verifying: ico("search"), seeding: ico("up"), metadata: ico("globe"),
};

function renderSidebar(counts) {
    $("#side-items").innerHTML = CATALOG.map((cat) => `
      <div class="side-item ${cat === currentCategory ? "active" : ""}" data-cat="${cat}">
        <span>${CAT_LABEL[cat]}</span><span class="count">${counts[cat] ?? 0}</span>
      </div>`).join("");
}

function inCurrentView(t) {
    if (currentCategory === "all") return true;
    if (currentCategory === "unfinished") return t.state !== "completed";
    if (currentCategory === "completed") return t.state === "completed";
    if (currentCategory === "torrents") return t.kind === "torrent";
    return categoryOf(urlName(t.url)) === currentCategory;
}

// Everything a full row repaint owns. Downstream, poll() compares this per
// poll: identical -> volatile-refresh the cells in place instead of
// rebuilding the tbody (which cost hover/dblclick stability and burned CPU
// every 500ms during downloads).
function structKey(view) {
    return currentCategory + "|" + view.map((t) => [
        t.id, stateLabel(t), rowName(t), t.kind, t.dest_dir, t.dest_file,
        t.error, t.sequential, t.total ? 1 : 0,
    ].join("\u0001")).join("\u0002");
}

// id -> row element, refreshed on every full body render.
let rowCache = new Map();

// Volatile cells only: bytes, percent, bar width, ETA, speed. The layout
// (icons, state label, buttons, error text) is structural and untouched.
function refreshVolatile(t) {
    const tr = rowCache.get(t.id);
    // tr.cells: HTMLTableRowElement's live cell collection (columns: name,
    // status, size, progress, eta, speed, actions).
    if (!tr || tr.cells.length < 6) return;
    tr.cells[2].textContent = sizeText(t);
    tr.cells[4].textContent = etaOf(t) || "-";
    tr.cells[5].textContent = t.speed_bps ? `${humans(t.speed_bps)}/s` : "-";
    const pct = percentOf(t);
    if (pct == null) return; // pc-less layout is structural (no-total cases)
    const fill = tr.querySelector(".prog-fill");
    const pctSpan = tr.querySelector(".pct");
    if (fill) fill.style.width = `${Math.min(100, pct).toFixed(1)}%`;
    if (pctSpan) pctSpan.textContent = `${pctText(pct, stateLabel(t))}%`;
}

// Speed lives in the top strip; the status bar stays counts-only.
function statusbarText() {
    const running = tasks.filter((t) => ["running", "queued"].includes(t.state)).length;
    return `${running} active · ${tasks.length} in list`;
}

function render() {
    // Category counts (also shown in the sidebar). Counts only change with
    // the task set / state label / name extension - all structural - so the
    // sidebar is repainted here and never by the volatile path.
    const counts = { all: tasks.length, unfinished: 0, completed: 0, torrents: 0 };
    for (const cat of ["video", "music", "images", "documents", "archives", "programs"]) counts[cat] = 0;
    for (const t of tasks) {
        if (t.state === "completed") counts.completed++; else counts.unfinished++;
        if (t.kind === "torrent") counts.torrents++;
        const cat = categoryOf(urlName(t.url));
        if (cat in counts) counts[cat]++;
    }
    renderSidebar(counts);

    const rows = $("#task-rows");
    const view = tasks.filter(inCurrentView);
    $("#empty-hint").classList.toggle("hidden", view.length > 0);
    rows.innerHTML = view.map((t) => {
        const name = rowName(t);
        const state = stateLabel(t);
        const pct = percentOf(t);
        const barClass = state === "failed" || state === "cancelled" ? "failed" : (["paused", "queued", "timed"].includes(state) ? "paused" : "");
        const prog = pct == null
            ? `<div class="prog-track"><div class="prog-fill ${barClass}" style="width:0%"></div></div>`
            : `<div class="prog-track"><div class="prog-fill ${barClass}" style="width:${Math.min(100, pct).toFixed(1)}%"></div></div><span class="pct">${pctText(pct, state)}%</span>`;
        const size = sizeText(t);
        const eta = etaOf(t) || "-";
        const speed = t.speed_bps ? `${humans(t.speed_bps)}/s` : "-";
        const dir = t.dest_dir ? t.dest_dir.replace(/[\\/]+$/, "") : "";
        const cls = t.id === selectedId ? ' class="selected"' : "";
        return `<tr${cls} data-id="${t.id}">
          <td class="name-cell" title="${escapeHtml(detailsTitle(t))}">
            <div class="fname-cell"><span class="ficon">${STATE_ICON[state] || "&#128230;"}</span><span title="${escapeHtml(name)}">${escapeHtml(name)}</span></div>
            <div class="fdir" title="${escapeHtml(dir)}">${escapeHtml(dir)}</div>
          </td>
          <td class="st-${state}" title="${escapeHtml(errorTitle(t.error))}"><span class="st-ico">${STATUS_ICON[state] || ""}</span>${state}${t.error ? '<div class="st-err">' + escapeHtml(shortError(t.error)) + '</div>' : ""}</td>
          <td>${size}</td>
          <td class="prog-cell">${prog}</td>
          <td>${eta}</td>
          <td>${speed}</td>
          <td class="col-act">${actionButtons(t)}</td>
        </tr>`;
    }).join("");

    // Handlers below live on the tbody once (delegation, set up at init):
    // re-renders don't touch them, so mid-click replaces can't swallow
    // clicks/dblclicks/context menus and rows need no per-render binding.

    rowCache = new Map();
    for (const tr of rows.children) rowCache.set(tr.dataset.id, tr);

    renderStatusbar();
}

// One-time event wiring for the task table + sidebar (survives every
// repaint; .row-btn actions ride the same delegation).
TBODY.onclick = (ev) => {
    const btn = ev.target.closest(".row-btn");
    if (btn) {
        ev.stopPropagation();
        doTaskAction(btn.dataset.act, btn.dataset.id);
        return;
    }
    const tr = ev.target.closest("tr");
    if (!tr) return;
    selectedId = tr.dataset.id;
    TBODY.querySelectorAll("tr").forEach((x) => x.classList.toggle("selected", x === tr));
};
TBODY.ondblclick = (ev) => {
    const tr = ev.target.closest("tr");
    if (!tr) return;
    selectedId = tr.dataset.id;
    renderKeepSelection();
    openDetails(tr.dataset.id);
};
TBODY.oncontextmenu = (ev) => {
    const tr = ev.target.closest("tr");
    if (!tr) return;
    ev.preventDefault();
    selectedId = tr.dataset.id;
    renderKeepSelection();
    showCtx(ev.clientX, ev.clientY);
};
$("#side-items").onclick = (ev) => {
    const el = ev.target.closest(".side-item");
    if (!el) return;
    currentCategory = el.dataset.cat;
    lastStructKey = null;
    render();
};

// Rerender without invalidating the selection highlight (implicit: selectedId survives).
function renderKeepSelection() { render(); }

function actionButtons(t) {
    const idAttr = `data-id="${t.id}"`;
    const running = ["running", "queued"].includes(t.state);
    // Completed rows have nothing to pause/resume.
    const stop = t.state === "completed" ? "" : `
      <button class="row-btn" ${idAttr} data-act="toggle" title="${running ? "Pause" : "Resume"}">
        ${ico(running ? "pause" : "play")}
      </button>`;
    const openFile = t.state === "completed" ? `
      <button class="row-btn" ${idAttr} data-act="openFile" title="Open file">${ico("file")}</button>` : "";
    const open = `
      <button class="row-btn" ${idAttr} data-act="openFolder" title="Open download folder">${ico("folder")}</button>`;
    const rm = `
      <button class="row-btn" ${idAttr} data-act="deleteFiles" title="Delete from list AND disk">${ico("trash")}</button>`;
    return `<span class="actions">${stop}${openFile}${open}${rm}</span>`;
}

function escapeHtml(s) {
    return s.replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
}

function showCtx(x, y) {
    const menu = $("#ctx");
    menu.classList.remove("hidden");
    menu.style.left = Math.min(x, innerWidth - 190) + "px";
    menu.style.top = Math.min(y, innerHeight - 130) + "px";
}
document.addEventListener("click", () => { $("#ctx").classList.add("hidden"); closeMenus(); });
$("#ctx").onclick = (ev) => {
    const act = ev.target.dataset.act;
    if (!act) return;
    $("#ctx").classList.add("hidden");
    if (selectedId) doTaskAction(act, selectedId);
};

function findTask(id) {
    return tasks.find((x) => x.id === id);
}

async function doTaskAction(act, id) {
    const t = findTask(id);
    if (!t) return;
    try {
        if (act === "remove") await invokeSafe("remove_task", { id, deleteFiles: false });
        else if (act === "deleteFiles") {
            if (!confirm(`Delete the downloaded file(s) for "${rowName(t)}" from disk?\nThis cannot be undone.`)) return;
            await invokeSafe("remove_task", { id, deleteFiles: true });
        } else if (act === "details") {
            if (t.kind !== "torrent") { flashErr("Details are only available for torrent tasks."); return; }
            await openDetails(id);
        } else if (act === "openFolder") {
            await invokeSafe("open_path", { path: t.dest_dir || "", reveal: null });
        } else if (act === "openFile") {
            if (!t.dest_file) { flashErr("No produced file recorded for this task."); return; }
            const dir = t.dest_file.replace(/[^\\/]+$/, "");
            await invokeSafe("open_path", { path: dir, reveal: baseName(t.dest_file) });
        } else if (act === "toggle") {
            const cmd = ["running", "queued"].includes(t.state) ? "pause_task" : "resume_task";
            await invokeSafe(cmd, { id });
        }
        selectedId = id;
        await poll();
    } catch (e) {
        flashErr(String(e));
    }
}

function flashErr(msg) {
    const st = ST_COUNTS;
    st.textContent = msg;
    st.style.color = "#b02a2a";
    // Same suppression window as flashStatus: without it the 500ms status
    // writer would overwrite the message before it is readable.
    statusFlashUntil = Date.now() + 3500;
    setTimeout(() => { st.style.color = ""; }, 3500);
}

let pollInFlight = false;

// One round trip on the wire at a time: the command side can sleep ~900ms on
// a cold daemon spawn, so an unguarded 500ms poll would stack concurrent
// calls and could answer out of order (stale snapshot rolling back a fresh
// one).
async function poll() {
    if (pollInFlight) return;
    pollInFlight = true;
    try {
        const list = await invoke("list_tasks");
        tasks = list;
        // Selection must not outlive its task (ghost context-menu actions).
        if (selectedId && !tasks.some((t) => t.id === selectedId)) selectedId = null;
        setConn(true);
        // Structural changes (task set, state, name, errors) repaint the
        // tbody + sidebar; bytes-only changes update cells in place - no DOM
        // rebuild, no flicker, no lost hover.
        const view = tasks.filter(inCurrentView);
        const key = structKey(view);
        if (key !== lastStructKey) {
            lastStructKey = key;
            render();
        } else {
            for (const t of view) refreshVolatile(t);
            renderStatusbar();
        }
    } catch (_) { /* connection pill only flips via setConn */ }
    finally {
        pollInFlight = false;
    }
}

// Self-scheduling: 500ms while the window is visible (engine ticks every
// 500ms), 2s when hidden - close-to-tray shouldn't churn RPCs at all.
let pollTimer = null;
function schedulePoll() {
    clearTimeout(pollTimer);
    const ms = document.hidden ? 2000 : 500;
    pollTimer = setTimeout(() => { poll().then(schedulePoll); }, ms);
}
document.addEventListener("visibilitychange", schedulePoll);

/* Total-speed graph: 60 s rolling window, sampled on every poll tick. */
const SPEED_WINDOW_MS = 60000;
const speedHistory = []; // { t: epoch ms, v: bytes/sec }

function sampleSpeed(bps) {
    const now = Date.now();
    speedHistory.push({ t: now, v: bps });
    while (speedHistory.length > 2 && now - speedHistory[0].t > SPEED_WINDOW_MS) {
        speedHistory.shift();
    }
}

function drawSpeedGraph() {
    const c = $("#speedgraph");
    if (!c || !c.clientWidth) return;
    const dpr = window.devicePixelRatio || 1;
    const w = c.clientWidth;
    const h = c.clientHeight;
    if (c.width !== Math.round(w * dpr) || c.height !== Math.round(h * dpr)) {
        c.width = Math.round(w * dpr);
        c.height = Math.round(h * dpr);
    }
    const ctx = c.getContext("2d");
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, w, h);

    // Baseline/grid.
    ctx.strokeStyle = "#e3e7eb";
    ctx.lineWidth = 1;
    ctx.beginPath();
    ctx.moveTo(0, h - 0.5);
    ctx.lineTo(w, h - 0.5);
    ctx.stroke();
    if (speedHistory.length < 2) return;

    const now = Date.now();
    const peak = Math.max(1, ...speedHistory.map((p) => p.v));
    const ymax = peak * 1.18; // headroom so the peak never touches the top
    const px = (t) => w - ((now - t) / SPEED_WINDOW_MS) * w;
    const py = (v) => h - 2 - (v / ymax) * (h - 4);

    // Filled area under the curve.
    ctx.beginPath();
    ctx.moveTo(px(speedHistory[0].t), h);
    for (const p of speedHistory) ctx.lineTo(px(p.t), py(p.v));
    ctx.lineTo(px(speedHistory[speedHistory.length - 1].t), h);
    ctx.closePath();
    const fill = ctx.createLinearGradient(0, 0, 0, h);
    fill.addColorStop(0, "rgba(53,162,53,.40)");
    fill.addColorStop(1, "rgba(53,162,53,.04)");
    ctx.fillStyle = fill;
    ctx.fill();

    // Speed line.
    ctx.beginPath();
    speedHistory.forEach((p, i) => {
        const x = px(p.t);
        const y = py(p.v);
        if (i === 0) ctx.moveTo(x, y); else ctx.lineTo(x, y);
    });
    ctx.strokeStyle = "#2f8f3a";
    ctx.lineWidth = 1.5;
    ctx.stroke();
}
window.addEventListener("resize", drawSpeedGraph);

function renderStatusbar() {
    const totalSpeed = tasks.reduce((s, t) => s + (t.speed_bps || 0), 0);
    sampleSpeed(totalSpeed);
    const nowEl = $("#ss-now");
    if (nowEl) nowEl.textContent = `${humans(totalSpeed)}/s`;
    const avgEl = $("#ss-avg");
    if (avgEl) {
        const avg = speedHistory.length
            ? speedHistory.reduce((s, p) => s + p.v, 0) / speedHistory.length
            : 0;
        avgEl.textContent = `${humans(Math.round(avg))}/s`;
    }
    drawSpeedGraph();

    if (Date.now() < statusFlashUntil) return; // flashing a message
    const countsTxt = statusbarText();
    if (ST_COUNTS.textContent !== countsTxt) ST_COUNTS.textContent = countsTxt;
}

/* Toolbar */
let dlgDestTouched = false;

async function resumeAll() {
    for (const t of tasks.filter((x) => ["paused", "failed"].includes(x.state))) {
        try { await invokeSafe("resume_task", { id: t.id }); } catch (e) { flashErr(String(e)); break; }
    }
    poll();
}
async function stopAll() {
    for (const t of tasks.filter((x) => ["running", "queued"].includes(x.state))) {
        try { await invokeSafe("pause_task", { id: t.id }); } catch (e) { flashErr(String(e)); break; }
    }
    poll();
}
async function clearDone() {
    for (const t of tasks.filter((x) => x.state === "completed")) {
        try { await invokeSafe("remove_task", { id: t.id, deleteFiles: false }); } catch (e) { break; }
    }
    poll();
}

/* Menu bar: real dropdowns under File / Tasks / Help. */
const MENUS = {
    file: () => [
        { label: "Add URL", hint: "Ctrl+N", act: () => openAddDialog() },
        { label: "Add Torrent", hint: "Ctrl+T", act: () => pickTorrent() },
        null,
        { label: "Exit", act: () => { invokeSafe("exit_app"); } },
    ],
    tasks: () => [
        { label: "Resume All", act: () => resumeAll() },
        { label: "Stop All", act: () => stopAll() },
        null,
        { label: "Clear Completed", act: () => clearDone() },
    ],
    help: () => [
        { label: "Check for Updates", act: () => runUpdateCheck(false) },
        { label: "Open Release Page", act: () => openReleases() },
        null,
        { label: "About ripdl", act: () => openAbout() },
    ],
};
let openMenu = null;
function closeMenus() {
    if (openMenu) openMenu.classList.remove("open");
    openMenu = null;
    $("#menudrop").classList.add("hidden");
}
document.querySelectorAll("#menubar span").forEach((el) => {
    el.onclick = (ev) => {
        ev.stopPropagation();
        const wasOpen = openMenu === el;
        closeMenus();
        if (wasOpen) return;
        const items = MENUS[el.dataset.menu]();
        const drop = $("#menudrop");
        drop.innerHTML = items.map((it) => !it ? "<hr>" :
            `<div class="menu-item" data-i="${items.indexOf(it)}"><span>${it.label}</span>${it.hint ? `<span class="hint">${it.hint}</span>` : ""}</div>`).join("");
        drop.classList.remove("hidden");
        const r = el.getBoundingClientRect();
        drop.style.left = r.left + "px";
        drop.style.top = r.bottom + "px";
        drop.querySelectorAll(".menu-item").forEach((mi) => {
            mi.onclick = (e) => {
                e.stopPropagation();
                closeMenus();
                items[Number(mi.dataset.i)].act();
            };
        });
        openMenu = el;
        el.classList.add("open");
    };
});
$("#menudrop").onclick = (ev) => ev.stopPropagation();

$("#btn-add").onclick = () => openAddDialog();
$("#btn-add-torrent").onclick = pickTorrent;
$("#btn-resume-all").onclick = resumeAll;
$("#btn-stop-all").onclick = stopAll;
$("#btn-clear-done").onclick = clearDone;

/* Update check: banner + Help menu; installs go through the daemon. */
const RELEASES_URL = "https://github.com/ShiniM0d0ri/ripdl/releases";
const REPO_URL = "https://github.com/ShiniM0d0ri/ripdl";

/* About box: version comes from the running build, so it always matches the
   exe the user is looking at (Help > About ripdl). */
async function openAbout() {
    let ver = "";
    try { ver = await invokeSafe("app_version"); } catch (_) { /* leave blank */ }
    $("#abt-ver").textContent = ver ? `v${ver}` : "";
    $("#abt-backdrop").classList.remove("hidden");
}
function closeAbout() { $("#abt-backdrop").classList.add("hidden"); }

function openExternal(url) {
    invokeSafe("open_url", { url }).catch((e) => flashErr(String(e)));
}
$("#abt-close").onclick = closeAbout;
$("#abt-ok").onclick = closeAbout;
$("#abt-backdrop").onclick = (ev) => { if (ev.target === ev.currentTarget) closeAbout(); };
$("#abt-repo").onclick = () => openExternal(REPO_URL);
$("#abt-releases").onclick = () => openExternal(RELEASES_URL);
$("#abt-issues").onclick = () => openExternal(`${REPO_URL}/issues`);
$("#abt-check").onclick = () => { closeAbout(); runUpdateCheck(false); };

function openReleases() {
    invokeSafe("open_url", { url: RELEASES_URL }).catch((e) => flashErr(String(e)));
}

async function runUpdateCheck(initial) {
    let info;
    try {
        info = await invokeSafe("check_update");
    } catch (e) {
        // Startup checks stay silent; menu-triggered ones report why.
        if (!initial) flashErr(String(e).replace(/^.*error: /, ""));
        return;
    }
    if (info && info.update_available) {
        $("#update-text").innerHTML =
            `New version <b>v${escapeHtml(info.latest)}</b> is available` +
            ` <span class="update-dim">(running v${escapeHtml(info.current)})</span>`;
        $("#update-banner").classList.remove("hidden");
    } else if (!initial) {
        flashStatus(`ripdl v${info.current} is up to date`);
    }
}

// One-click upgrade: installer closes this GUI while replacing it, so the
// last visible act here is the banner text.
async function installUpdate() {
    const el = $("#update-text");
    el.textContent = "Downloading & verifying the new setup\u2026";
    try {
        await invokeSafe("install_update");
        el.textContent = "Installer launched \u2014 approve the Windows prompt to finish installing.";
    } catch (e) {
        const msg = String(e).replace(/^.*error: /, "").trim();
        flashErr(msg);
        el.textContent = `Install failed: ${msg.length > 200 ? msg.slice(0, 200) + "\u2026" : msg}`;
    }
}

$("#update-open").onclick = openReleases;
$("#update-install").onclick = installUpdate;
$("#update-dismiss").onclick = () => $("#update-banner").classList.add("hidden");

// Once per launch: silent check (default on) + opt-in auto-install.
// Failures here are deliberately invisible.
(async () => {
    try {
        const s = await invokeSafe("get_config");
        if (s.update_check ?? true) {
            runUpdateCheck(true);
            // Daemon booted with the same setting does this itself; this
            // covers a daemon predating the setting. "Already latest" is a
            // silent no-op daemon-side.
            if (s.update_auto_install ?? false) {
                invokeSafe("install_update").catch(() => {});
            }
        }
    } catch { /* config failure must not block the UI */ }
})();

/* Dialog */
// Torrent payloads get the qB-style flow: metadata first, then a file
// checklist inside this dialog before anything is queued.
function looksLikeTorrent(payload) {
    const p = payload.trim();
    if (p.toLowerCase().startsWith("magnet:")) return true;
    const bare = p.split("#")[0].split("?")[0];
    return bare.toLowerCase().endsWith(".torrent");
}
// Preview state for the add dialog: null | { payload, files?|failed }
// - `files` present (possibly empty) = metadata resolved, ready to add.
// - `failed` = preview failed; the next submit adds the torrent as-is.
let torrentPick = null;

function updateTorrentUi() {
    const url = $("#dlg-url").value.trim();
    const torrent = !$("#dlg-hunt").checked && looksLikeTorrent(url);
    $("#row-seq").hidden = !torrent;
    $("#row-tfiles").hidden = !torrent;
    if (torrent && torrentPick && torrentPick.payload === url) {
        $("#row-tfiles").hidden = false; // resolved for this exact payload
    } else if (!torrent || (torrentPick && torrentPick.payload !== url)) {
        if (!torrent) torrentPick = null;
        $("#tfiles-list").textContent = "";
        $("#tfiles-status").textContent = "";
    }
}

function openAddDialog(presetUrl) {
    $("#dlg-url").value = presetUrl || "";
    $("#dlg-err").classList.add("hidden");
    $("#dlg-hunt").checked = false;
    $("#dlg-seq").checked = false;
    $("#dlg-cat").value = "auto";
    dlgDestTouched = false;
    $("#dlg-dest").value = "";
    torrentPick = null;
    $("#tfiles-list").textContent = "";
    $("#tfiles-status").textContent = "";
    updateTorrentUi();
    $("#dlg-backdrop").classList.remove("hidden");
    $("#dlg-url").focus();
    $("#dlg-url").select();
}
$("#dlg-dest").oninput = () => { dlgDestTouched = true; };
$("#dlg-url").oninput = updateTorrentUi;
$("#dlg-hunt").onchange = updateTorrentUi;
$("#dlg-browse").onclick = async () => {
    try {
        const picked = await invokeSafe("pick_dir");
        if (picked) {
            $("#dlg-dest").value = picked;
            dlgDestTouched = true;
        }
    } catch (e) {
        flashErr(String(e));
    }
};
$("#dlg-cancel").onclick = () => {
    $("#dlg-backdrop").classList.add("hidden");
    torrentPick = null;
};
$("#dlg-ok").onclick = submitAdd;
$("#dlg-url").onkeydown = (ev) => { if (ev.key === "Enter") submitAdd(); };
$("#dlg-backdrop").onclick = (ev) => {
    if (ev.target === ev.currentTarget) {
        $("#dlg-backdrop").classList.add("hidden");
        torrentPick = null;
    }
};

/* Settings dialog (daemon-wide config) */
const SET_INPUTS = ["#set-conc", "#set-segs", "#set-minseg", "#set-speed", "#set-dir", "#set-ytdlp"];

function flashStatus(msg, color) {
    const st = ST_COUNTS;
    st.textContent = msg;
    st.style.color = color || "#2e7d32";
    // Poll re-renders the status bar every second; suppress it until the
    // flash has had time to be readable.
    statusFlashUntil = Date.now() + 3500;
    setTimeout(() => { st.style.color = ""; }, 3500);
}
let statusFlashUntil = 0;

// Loose byte parser for the settings fields: "4MiB", "2 MB", "1048576",
// "500k"... Empty input = no value; junk = NaN.
function parseBytesLoose(str) {
    const s = String(str).trim().toLowerCase();
    if (!s) return undefined;
    const m = s.match(/^([\d.]+)\s*(k|m|g|t)?i?b?$/);
    if (!m) return NaN;
    const n = parseFloat(m[1]);
    if (!Number.isFinite(n) || n < 0) return NaN;
    const mult = { "": 1, k: 1024, m: 1024 ** 2, g: 1024 ** 3, t: 1024 ** 4 }[m[2] || ""];
    if (mult === undefined) return NaN;
    return Math.round(n * mult);
}

function bytesForEdit(v) {
    // humans() renders "1.0 MiB", which parseBytesLoose reads back exactly.
    return v == null ? "" : humans(v);
}

async function openSettings() {
    const err = $("#set-err");
    err.classList.add("hidden");
    SET_INPUTS.forEach((sel) => { $(sel).value = ""; });
    $("#set-tray").checked = true;
    $("#set-bond").checked = false;
    $("#set-update").checked = true;
    $("#set-auto").checked = false;
    try {
        const s = await invokeSafe("get_config");
        $("#set-conc").value = s.max_concurrent ?? "";
        $("#set-segs").value = s.max_segments ?? "";
        $("#set-minseg").value = bytesForEdit(s.min_segment_size);
        $("#set-speed").value = bytesForEdit(s.max_speed_bytes);
        $("#set-dir").value = s.default_download_dir ?? "";
        $("#set-ytdlp").value = s.ytdlp_path ?? "";
        $("#set-tray").checked = s.hide_to_tray ?? true;
        $("#set-bond").checked = s.bond_interfaces ?? false;
        $("#set-update").checked = s.update_check ?? true;
        $("#set-auto").checked = s.update_auto_install ?? false;
        $("#set-backdrop").classList.remove("hidden");
        $("#set-conc").focus();
    } catch (e) {
        err.textContent = String(e).replace(/^.*error: /, "");
        err.classList.remove("hidden");
    }
}

function closeSettings() {
    $("#set-backdrop").classList.add("hidden");
}

async function saveSettings() {
    const err = $("#set-err");
    err.classList.add("hidden");
    const reject = (msg) => { err.textContent = msg; err.classList.remove("hidden"); };
    const num = (el) => {
        const raw = el.value.trim();
        if (raw === "") return undefined;
        const n = Number(raw);
        return Number.isFinite(n) ? n : NaN;
    };
    const conc = num($("#set-conc"));
    const segs = num($("#set-segs"));
    const minseg = parseBytesLoose($("#set-minseg").value);
    const speed = parseBytesLoose($("#set-speed").value);
    if (Number.isNaN(conc) || Number.isNaN(segs)) return reject("concurrency / segments need numbers");
    if (Number.isNaN(minseg) || Number.isNaN(speed)) return reject("segment size / speed need bytes (e.g. 4MiB) or zero/empty");
    if (conc !== undefined && conc < 1) return reject("concurrent downloads must be at least 1");
    if (segs !== undefined && segs < 1) return reject("max segments must be at least 1");
    if (minseg !== undefined && minseg < 1) return reject("min segment size must be at least 1");
    const dir = $("#set-dir").value.trim();
    // Full-replace semantics: cleared fields become absent -> daemon defaults.
    const payload = {
        max_concurrent: conc,
        max_segments: segs,
        min_segment_size: minseg,
        max_speed_bytes: speed,
        default_download_dir: dir === "" ? undefined : dir,
        ytdlp_path: $("#set-ytdlp").value.trim() || undefined,
        hide_to_tray: $("#set-tray").checked,
        bond_interfaces: $("#set-bond").checked,
        update_check: $("#set-update").checked,
        update_auto_install: $("#set-auto").checked,
    };
    try {
        await invokeSafe("set_config", { settings: payload });
        closeSettings();
        flashStatus("settings saved");
    } catch (e) {
        reject(String(e).replace(/^.*error: /, ""));
    }
}

$("#btn-settings").onclick = openSettings;
$("#set-cancel").onclick = closeSettings;
$("#set-close").onclick = closeSettings;
$("#set-ok").onclick = saveSettings;
$("#set-dir-browse").onclick = async () => {
    try {
        const picked = await invokeSafe("pick_dir");
        if (picked) $("#set-dir").value = picked;
    } catch (e) {
        flashErr(String(e));
    }
};
$("#set-backdrop").onclick = (ev) => { if (ev.target === ev.currentTarget) closeSettings(); };
$("#set-backdrop").onkeydown = (ev) => { if (ev.key === "Enter") saveSettings(); };
$("#set-ytdlp").onkeydown = (ev) => { if (ev.key === "Enter") saveSettings(); };
$("#set-dir").onkeydown = (ev) => { if (ev.key === "Enter") saveSettings(); };

// True while any dialog is open: global accelerators shouldn't fire into
// them (Ctrl+N while typing in the Add dialog would wipe the typed URL).
function dialogOpen() {
    return ["#dlg-backdrop", "#set-backdrop", "#tdg-backdrop", "#abt-backdrop"]
        .some((s) => !$(s).classList.contains("hidden"));
}

document.addEventListener("keydown", (ev) => {
    if (ev.key === "Escape") {
        $("#dlg-backdrop").classList.add("hidden");
        torrentPick = null;
        closeDetails();
        closeSettings();
        closeAbout();
        closeMenus();
    }
    if (dialogOpen()) return;
    if ((ev.ctrlKey || ev.metaKey) && ev.key.toLowerCase() === "n") {
        ev.preventDefault();
        openAddDialog();
    }
    if ((ev.ctrlKey || ev.metaKey) && ev.key.toLowerCase() === "t") {
        ev.preventDefault();
        pickTorrent();
    }
});

// Pick a .torrent from disk and route it through the add dialog: that keeps
// one entry point (and lets the user check "sequential" before queuing).
async function pickTorrent() {
    try {
        const file = await invokeSafe("pick_file");
        if (!file) return;
        openAddDialog(file);
    } catch (e) {
        flashErr(String(e).replace(/^.*error: /, ""));
    }
}

/* Torrent details dialog */
let detailsTaskId = null;
let detailsTab = "files";
let detailsTimer = null;
// Files-tab selection the user changed since the last Apply: file index ->
// wanted. Kept separate from the daemon state so the 2s auto-refresh never
// clobbers checks the user just made.
let pendingSel = null;
let lastDetails = null;
let filesRenderKey = "";

function wantsFile(idx, fallback) {
    return pendingSel && pendingSel.has(idx) ? pendingSel.get(idx) : fallback;
}

async function openDetails(id) {
    const t = findTask(id);
    if (!t || t.kind !== "torrent") return;
    detailsTaskId = id;
    detailsTab = "files";
    filesRenderKey = "";
    pendingSel = null;
    lastDetails = null;
    for (const el of document.querySelectorAll("#tdg-tabs .tdg-tab")) {
        el.classList.toggle("active", el.dataset.tab === "files");
    }
    $("#tdg-backdrop").classList.remove("hidden");
    await refreshDetails();
    clearInterval(detailsTimer);
    detailsTimer = setInterval(async () => {
        if (!detailsTaskId || $("#tdg-backdrop").classList.contains("hidden")) return;
        if (!findTask(detailsTaskId)) { closeDetails(); return; }
        await refreshDetails();
    }, 2000);
}

function closeDetails() {
    detailsTaskId = null;
    pendingSel = null;
    lastDetails = null;
    clearInterval(detailsTimer);
    $("#tdg-backdrop").classList.add("hidden");
}

$("#tdg-close").onclick = closeDetails;
$("#tdg-backdrop").onclick = (ev) => { if (ev.target === ev.currentTarget) closeDetails(); };
document.querySelectorAll("#tdg-tabs .tdg-tab").forEach((el) => {
    el.onclick = () => {
        detailsTab = el.dataset.tab;
        filesRenderKey = ""; // body content swapped; next Files view re-renders
        document.querySelectorAll("#tdg-tabs .tdg-tab").forEach((x) => x.classList.toggle("active", x.dataset.tab === detailsTab));
        refreshDetails();
    };
});

let detailsRefreshBusy = false;

async function refreshDetails() {
    if (!detailsTaskId || detailsRefreshBusy) return;
    detailsRefreshBusy = true;
    // Title from the task list first: available even when the snapshot
    // fetch fails (e.g. paused session resets don't blank the dialog).
    const t = findTask(detailsTaskId);
    $("#tdg-name").textContent = (t && t.name) || "";
    let d;
    try {
        d = await invokeSafe("torrent_details", { id: detailsTaskId });
    } catch (e) {
        $("#tdg-body").innerHTML = `<div class="trk-off">${escapeHtml(String(e).replace(/^.*error: /, ""))}</div>`;
        $("#tdg-apply").classList.add("hidden");
        lastDetails = null;
        return;
    } finally {
        detailsRefreshBusy = false;
    }
    const name = (t && t.name) || d.name || "(unknown)";
    $("#tdg-name").textContent = name;
    const pct = d.total > 0 ? ((d.downloaded / d.total) * 100).toFixed(1) : "0";
    $("#tdg-summary").textContent =
        `${pct}% / ${humans(d.downloaded)} / ${humans(d.total)} / ${d.finished ? "finished" : d.paused ? "paused" : "running"}` +
        (t && t.sequential ? " / sequential" : "") +
        `\n${d.info_hash}` +
        (d.root ? `\n${d.root}` : "");
    lastDetails = d;

    const body = $("#tdg-body");
    // Files tab: render the table once and update cells in place on the
    // 2s refresh; a full innerHTML rewrite every 2s races real/synthetic
    // clicks (mousedown on one node, mouseup on its replaced clone) and
    // swallows checkbox toggles.
    if (detailsTab === "files") {
        const key = `${detailsTaskId}|${d.files.length}|${d.files.map((f) => (f.included ? 1 : 0)).join("")}`;
        const fullRender = filesRenderKey !== key;
        filesRenderKey = key;
        if (fullRender) {
            const rows = d.files.map((f, i) => {
                const p = f.size > 0 ? Math.min(100, (f.downloaded / f.size) * 100) : 100;
                return `<tr>
              <td><input type="checkbox" class="tdg-file-chk" data-idx="${i}" ${wantsFile(i, f.included) ? "checked" : ""} /></td>
              <td title="${escapeHtml(f.name)}">${escapeHtml(baseName(f.name) || f.name)}</td>
              <td class="num">${humans(f.size)}</td>
              <td class="num">${p.toFixed(0)}%</td>
              <td><div class="file-bar"><div class="file-fill" style="width:${p.toFixed(1)}%"></div></div></td>
            </tr>`;
            }).join("");
            body.innerHTML = `
          <table><thead><tr><th>Want</th><th>Name</th><th>Size</th><th>Done</th><th></th></tr></thead>
          <tbody>${rows}</tbody></table>`;
        } else {
            // In-place: progress cells, bars, checkbox state (pending wins).
            body.querySelectorAll("tbody tr").forEach((tr, i) => {
                const f = d.files[i];
                if (!f) return;
                const p = f.size > 0 ? Math.min(100, (f.downloaded / f.size) * 100) : 100;
                tr.children[3].textContent = `${p.toFixed(0)}%`;
                tr.querySelector(".file-fill").style.width = `${p.toFixed(1)}%`;
                const chk = tr.querySelector(".tdg-file-chk");
                if (chk && document.activeElement !== chk) {
                    chk.checked = wantsFile(i, f.included);
                }
            });
        }
        const apply = $("#tdg-apply");
        apply.classList.remove("hidden");
        apply.disabled = !pendingSel || pendingSel.size === 0;
    } else {
        $("#tdg-apply").classList.add("hidden");
    }
    if (detailsTab === "peers") {
        const rows = d.peers.map((p) => `
          <tr>
            <td>${escapeHtml(p.addr)}</td>
            <td>${escapeHtml(p.client || "?")}</td>
            <td>${escapeHtml(p.state)}</td>
            <td class="num">${humans(p.downloaded)}</td>
            <td class="num">${humans(p.uploaded)}</td>
          </tr>`).join("");
        body.innerHTML = `
          <table><thead><tr><th>Address</th><th>Client</th><th>State</th><th>Downloaded</th><th>Uploaded</th></tr></thead>
          <tbody>${rows}</tbody></table>`;
    } else if (detailsTab === "trackers") {
        const rows = d.trackers.map((u) => `<tr><td>${escapeHtml(u)}</td></tr>`).join("");
        body.innerHTML = d.trackers.length
            ? `<table><thead><tr><th>URL</th></tr></thead><tbody>${rows}</tbody></table>`
            : '<div class="trk-off">No trackers (DHT only).</div>';
    }
}

async function submitAdd() {
    if ($("#dlg-ok").disabled) return; // Enter-double-fire / double-click
    const url = $("#dlg-url").value.trim();
    if (!url) return;
    // Torrents go two-phase like qBittorrent: the first submit resolves
    // metadata and fills the file checklist; the second one queues the
    // task with the checked files. A failed preview falls through and
    // adds as-is instead of dead-ending a dead swarm.
    if (!$("#dlg-hunt").checked && looksLikeTorrent(url) &&
        !(torrentPick && torrentPick.payload === url &&
          (torrentPick.files || torrentPick.failed))) {
        $("#dlg-ok").disabled = true;
        $("#row-tfiles").hidden = false;
        const st = $("#tfiles-status");
        st.textContent = "Retrieving metadata\u2026";
        try {
            const pv = await invokeSafe("torrent_preview", { payload: url });
            torrentPick = { payload: url, files: pv.files || [] };
            st.textContent = pv.files && pv.files.length > 1
                ? `${pv.files.length} files \u00B7 ${humans(pv.total ?? 0)}`
                : "";
            $("#tfiles-list").innerHTML = (pv.files || []).map((f, i) => `
              <label><input type="checkbox" class="tfile-chk" data-idx="${i}" checked />
                      ${escapeHtml(f.name)} <span style="color:#8d98a4">(${humans(f.size)})</span></label>`).join("");
        } catch (e) {
            torrentPick = { payload: url, failed: true };
            st.textContent = "Metadata unavailable \u2014 the torrent will be added as-is.";
            $("#dlg-err").textContent = String(e).replace(/^.*error:\s*/, "");
            $("#dlg-err").classList.remove("hidden");
        } finally {
            $("#dlg-ok").disabled = false;
        }
        return;
    }
    $("#dlg-ok").disabled = true;
    const onlyFiles = torrentPick && torrentPick.payload === url && torrentPick.files
        ? torrentPick.files
              .map((_, i) => i)
              .filter((i) => $(`#tfiles-list .tfile-chk[data-idx="${i}"]`)?.checked)
        : null;
    try {
        const dest = $("#dlg-dest").value.trim();
        if ($("#dlg-hunt").checked) await invokeSafe("add_video_task", { url, destDir: dest || null });
        else await invokeSafe("add_task", {
            url,
            category: $("#dlg-cat").value,
            destDir: dest || null,
            sequential: $("#dlg-seq").checked,
            onlyFiles,
        });
        torrentPick = null;
        $("#dlg-backdrop").classList.add("hidden");
        await poll();
    } catch (e) {
        const err = $("#dlg-err");
        err.textContent = String(e).replace(/^.*error: /, "");
        err.classList.remove("hidden");
    } finally {
        $("#dlg-ok").disabled = false;
    }
}

// Files-tab selection: checkbox changes are parked in `pendingSel` until
// Apply pushes them to the daemon; rows re-render every 2s without losing it.
$("#tdg-body").addEventListener("change", (ev) => {
    if (!ev.target.classList.contains("tdg-file-chk")) return;
    const idx = Number(ev.target.dataset.idx);
    if (!Number.isInteger(idx)) return;
    if (!pendingSel) pendingSel = new Map();
    pendingSel.set(idx, ev.target.checked);
    // Drop entries that equal the latest served state (all-clean => no Apply).
    if (lastDetails && lastDetails.files[idx] &&
        lastDetails.files[idx].included === ev.target.checked) {
        pendingSel.delete(idx);
    }
    $("#tdg-apply").disabled = !pendingSel || pendingSel.size === 0;
});

// Clicking anywhere in the Want cell toggles the checkbox (the native 13px
// input is a tiny hit target).
$("#tdg-body").addEventListener("click", (ev) => {
    if (ev.target.classList.contains("tdg-file-chk")) return;
    const cell = ev.target.closest("td");
    if (!cell) return;
    const chk = cell.querySelector(".tdg-file-chk");
    if (chk) chk.click();
});

$("#tdg-apply").onclick = async () => {
    if (!detailsTaskId || !lastDetails) return;
    const files = lastDetails.files;
    if (!files.length) return;
    const wanted = files.map((f, i) => (wantsFile(i, f.included) ? i : -1)).filter((i) => i >= 0);
    // Every file wanted = no only_files at all (the "everything" encoding).
    const everything = wanted.length === files.length;
    $("#tdg-apply").disabled = true;
    try {
        await invokeSafe("update_selection", { id: detailsTaskId, onlyFiles: everything ? null : wanted });
        pendingSel = null;
        await refreshDetails();
    } catch (e) {
        flashErr(String(e).replace(/^.*error: /, ""));
        $("#tdg-apply").disabled = !pendingSel || pendingSel.size === 0;
    }
};

setConn(false);
poll().then(schedulePoll);
