// ripdl GUI frontend: talks only to the Tauri commands, which relay to the
// daemon. No logic lives here - this is a task-list skin.

const { invoke } = window.__TAURI__.core;

const $ = (sel) => document.querySelector(sel);

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
let lastSnapshot = "";

function invokeSafe(cmd, args) {
    return invoke(cmd, args).catch((e) => {
        setConn(false);
        throw e;
    });
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

function categoryOf(name) {
    const e = ext(name);
    for (const [cat, list] of Object.entries(CATEGORY_EXT)) {
        if (list.includes(e)) return cat;
    }
    return "other";
}

function urlName(url) {
    let base = splitPath(url.split(/[?#]/)[0]).pop() || url;
    try { base = decodeURIComponent(base); } catch (_) { /* keep raw */ }
    return base || url;
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

function etaOf(t) {
    if (!t.total || !t.speed_bps || t.speed_bps <= 0) return null;
    const left = (t.total - t.downloaded) / t.speed_bps;
    if (left <= 0) return null;
    const h = Math.floor(left / 3600), m = Math.floor((left % 3600) / 60);
    return h > 0 ? `${h}h ${m}m` : `${m}m ${Math.floor(left % 60)}s`;
}

const STATE_ICON = {
    completed: "✔", running: "▶", queued: "⏳",
    paused: "⏸", failed: "✖", cancelled: "⏸", timed: "⏰", hunt: "🎬",
    downloading: "▶", verifying: "🔍", seeding: "⇅", metadata: "🌐",
};
const STATUS_ICON = {
    completed: "✅", running: "⬇", queued: "🕒", timed: "⏰",
    paused: "⏸", failed: "❌", cancelled: "❌", hunt: "🎬",
    downloading: "⬇", verifying: "🔍", seeding: "🌱", metadata: "🌐",
};

function renderSidebar(counts) {
    $("#side-items").innerHTML = CATALOG.map((cat) => `
      <div class="side-item ${cat === currentCategory ? "active" : ""}" data-cat="${cat}">
        <span>${CAT_LABEL[cat]}</span><span class="count">${counts[cat] ?? 0}</span>
      </div>`).join("");
    $("#side-items").querySelectorAll(".side-item").forEach((el) => {
        el.onclick = () => { currentCategory = el.dataset.cat; lastSnapshot = ""; render(); };
    });
}

function inCurrentView(t) {
    if (currentCategory === "all") return true;
    if (currentCategory === "unfinished") return t.state !== "completed";
    if (currentCategory === "completed") return t.state === "completed";
    if (currentCategory === "torrents") return t.kind === "torrent";
    return categoryOf(urlName(t.url)) === currentCategory;
}

function render() {
    // Category counts (also shown in the sidebar).
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
            : `<div class="prog-track"><div class="prog-fill ${barClass}" style="width:${Math.min(100, pct).toFixed(1)}%"></div></div><span class="pct">${pct.toFixed(0)}%</span>`;
        const size = t.state === "completed"
            ? humans(t.total || t.downloaded)
            : t.percent != null
            ? (t.total ? humans(t.total) : "?")
            : t.total ? `${humans(t.downloaded)} / ${humans(t.total)}` : humans(t.downloaded);
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

    // Handlers live on the tbody (delegation) so re-renders mid-click
    // don't swallow double-clicks / context menus. Selection toggles a
    // class in place instead of re-rendering (a re-render replaces the
    // row node, which swallows the browser's dblclick).
    const tbody = $("#task-rows");
    tbody.onclick = (ev) => {
        const tr = ev.target.closest("tr");
        if (!tr) return;
        selectedId = tr.dataset.id;
        tbody.querySelectorAll("tr").forEach((x) => x.classList.toggle("selected", x === tr));
    };
    tbody.ondblclick = (ev) => {
        const tr = ev.target.closest("tr");
        if (!tr) return;
        selectedId = tr.dataset.id;
        renderKeepSelection();
        openDetails(tr.dataset.id);
    };
    tbody.oncontextmenu = (ev) => {
        const tr = ev.target.closest("tr");
        if (!tr) return;
        ev.preventDefault();
        selectedId = tr.dataset.id;
        renderKeepSelection();
        showCtx(ev.clientX, ev.clientY);
    };
    $("#task-table").querySelectorAll(".row-btn").forEach((btn) => {
        btn.onclick = (ev) => { ev.stopPropagation(); doTaskAction(btn.dataset.act, btn.dataset.id); };
    });

    const running = tasks.filter((t) => ["running", "queued"].includes(t.state)).length;
    const totalSpeed = tasks.reduce((s, t) => s + (t.speed_bps || 0), 0);
    $("#st-counts").textContent = `${running} active · ${tasks.length} in list`;
    $("#st-speed").textContent = `${humans(totalSpeed)} per second`;
}

// Rerender without invalidating the selection highlight (implicit: selectedId survives).
function renderKeepSelection() { render(); }

function actionButtons(t) {
    const idAttr = `data-id="${t.id}"`;
    const stop = `
      <button class="row-btn" ${idAttr} data-act="toggle" title="Pause / Resume">
        ${["running", "queued"].includes(t.state) ? "&#9646;&#8709;" : "&#10148;"}
      </button>`;
    const open = `
      <button class="row-btn" ${idAttr} data-act="openFolder" title="Open download folder">&#128446;</button>`;
    const rm = `
      <button class="row-btn" ${idAttr} data-act="deleteFiles" title="Delete from list AND disk">&#128465;</button>`;
    return `<span class="actions">${stop}${open}${rm}</span>`;
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
document.addEventListener("click", () => $("#ctx").classList.add("hidden"));
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
    const st = $("#st-counts");
    const prev = st.textContent;
    st.textContent = msg;
    st.style.color = "#b02a2a";
    setTimeout(() => { st.textContent = prev; st.style.color = ""; }, 3500);
}

async function poll() {
    try {
        const list = await invokeSafe("list_tasks");
        tasks = list;
        setConn(true);
        // Render once per actual data change (speed/pct shifts re-render;
        // identical payloads don't, which keeps hover/selection steady).
        const snap = JSON.stringify(tasks) + "|" + currentCategory;
        if (snap !== lastSnapshot) {
            lastSnapshot = snap;
            render();
        } else {
            renderStatusbar();
        }
    } catch (_) { /* stays red */ }
}

function renderStatusbar() {
    if (Date.now() < statusFlashUntil) return; // flashing a message
    const running = tasks.filter((t) => ["running", "queued"].includes(t.state)).length;
    const totalSpeed = tasks.reduce((s, t) => s + (t.speed_bps || 0), 0);
    $("#st-counts").textContent = `${running} active · ${tasks.length} in list`;
    $("#st-speed").textContent = `${humans(totalSpeed)} per second`;
}

/* Toolbar */
let dlgDestTouched = false;

$("#btn-add").onclick = () => openAddDialog();
$("#btn-add-torrent").onclick = pickTorrent;
$("#btn-resume-all").onclick = async () => {
    for (const t of tasks.filter((x) => ["paused", "failed"].includes(x.state))) {
        try { await invokeSafe("resume_task", { id: t.id }); } catch (e) { flashErr(String(e)); break; }
    }
    poll();
};
$("#btn-stop-all").onclick = async () => {
    for (const t of tasks.filter((x) => ["running", "queued"].includes(x.state))) {
        try { await invokeSafe("pause_task", { id: t.id }); } catch (e) { flashErr(String(e)); break; }
    }
    poll();
};
$("#btn-clear-done").onclick = async () => {
    for (const t of tasks.filter((x) => x.state === "completed")) {
        try { await invokeSafe("remove_task", { id: t.id, deleteFiles: false }); } catch (e) { break; }
    }
    poll();
};

/* Dialog */
function openAddDialog(presetUrl) {
    $("#dlg-url").value = presetUrl || "";
    $("#dlg-err").classList.add("hidden");
    $("#dlg-hunt").checked = false;
    $("#dlg-seq").checked = false;
    $("#dlg-cat").value = "auto";
    dlgDestTouched = false;
    $("#dlg-dest").value = "";
    $("#dlg-backdrop").classList.remove("hidden");
    $("#dlg-url").focus();
}
$("#dlg-cat").onchange = () => {};
$("#dlg-dest").oninput = () => { dlgDestTouched = true; };
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
$("#dlg-cancel").onclick = () => $("#dlg-backdrop").classList.add("hidden");
$("#dlg-ok").onclick = submitAdd;
$("#dlg-url").onkeydown = (ev) => { if (ev.key === "Enter") submitAdd(); };
$("#dlg-backdrop").onclick = (ev) => { if (ev.target === ev.currentTarget) $("#dlg-backdrop").classList.add("hidden"); };

/* Settings dialog (daemon-wide config) */
const SET_INPUTS = ["#set-conc", "#set-segs", "#set-minseg", "#set-speed", "#set-dir", "#set-ytdlp"];

function flashStatus(msg, color) {
    const st = $("#st-counts");
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
    try {
        const s = await invokeSafe("get_config");
        $("#set-conc").value = s.max_concurrent ?? "";
        $("#set-segs").value = s.max_segments ?? "";
        $("#set-minseg").value = bytesForEdit(s.min_segment_size);
        $("#set-speed").value = bytesForEdit(s.max_speed_bytes);
        $("#set-dir").value = s.default_download_dir ?? "";
        $("#set-ytdlp").value = s.ytdlp_path ?? "";
        $("#set-tray").checked = s.hide_to_tray ?? true;
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

document.addEventListener("keydown", (ev) => {
    if (ev.key === "Escape") {
        $("#dlg-backdrop").classList.add("hidden");
        closeDetails();
        closeSettings();
    }
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

async function refreshDetails() {
    if (!detailsTaskId) return;
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
    const url = $("#dlg-url").value.trim();
    if (!url) return;
    $("#dlg-ok").disabled = true;
    try {
        const dest = $("#dlg-dest").value.trim();
        if ($("#dlg-hunt").checked) await invokeSafe("add_video_task", { url, destDir: dest || null });
        else await invokeSafe("add_task", {
            url,
            category: $("#dlg-cat").value,
            destDir: dest || null,
            sequential: $("#dlg-seq").checked,
        });
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
poll().then(() => {
    setInterval(poll, 1000);
});
