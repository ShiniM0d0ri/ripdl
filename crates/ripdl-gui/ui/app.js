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
    video: "Videos",
    music: "Music",
    images: "Images",
    documents: "Documents",
    archives: "Archives",
    programs: "Programs",
};
const CATALOG = ["all", "unfinished", "completed", "video", "music", "images", "documents", "archives", "programs"];

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

// Multi-line title for a row: full error + diagnostics.
function detailsTitle(t) {
    const lines = [];
    lines.push(t.url || "");
    if (t.dest_dir) lines.push("Location: " + t.dest_dir);
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
    return categoryOf(urlName(t.url)) === currentCategory;
}

function render() {
    // Category counts (also shown in the sidebar).
    const counts = { all: tasks.length, unfinished: 0, completed: 0 };
    for (const cat of ["video", "music", "images", "documents", "archives", "programs"]) counts[cat] = 0;
    for (const t of tasks) {
        if (t.state === "completed") counts.completed++; else counts.unfinished++;
        const cat = categoryOf(urlName(t.url));
        if (cat in counts) counts[cat]++;
    }
    renderSidebar(counts);

    const rows = $("#task-rows");
    const view = tasks.filter(inCurrentView);
    $("#empty-hint").classList.toggle("hidden", view.length > 0);
    rows.innerHTML = view.map((t) => {
        const name = urlName(t.url);
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

    $("#task-table").querySelectorAll("tbody tr").forEach((tr) => {
        tr.onclick = () => { selectedId = tr.dataset.id; renderKeepSelection(); };
        tr.oncontextmenu = (ev) => { ev.preventDefault(); selectedId = tr.dataset.id; renderKeepSelection(); showCtx(ev.clientX, ev.clientY); };
    });
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
            if (!confirm(`Delete the downloaded file(s) for "${urlName(t.url)}" from disk?\nThis cannot be undone.`)) return;
            await invokeSafe("remove_task", { id, deleteFiles: true });
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
    const running = tasks.filter((t) => ["running", "queued"].includes(t.state)).length;
    const totalSpeed = tasks.reduce((s, t) => s + (t.speed_bps || 0), 0);
    $("#st-counts").textContent = `${running} active · ${tasks.length} in list`;
    $("#st-speed").textContent = `${humans(totalSpeed)} per second`;
}

/* Toolbar */
let dlgDestTouched = false;

$("#btn-add").onclick = () => openAddDialog();
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
document.addEventListener("keydown", (ev) => {
    if (ev.key === "Escape") $("#dlg-backdrop").classList.add("hidden");
    if ((ev.ctrlKey || ev.metaKey) && ev.key.toLowerCase() === "n") {
        ev.preventDefault();
        openAddDialog();
    }
});

async function submitAdd() {
    const url = $("#dlg-url").value.trim();
    if (!url) return;
    $("#dlg-ok").disabled = true;
    try {
        const dest = $("#dlg-dest").value.trim();
        if ($("#dlg-hunt").checked) await invokeSafe("add_video_task", { url, destDir: dest || null });
        else await invokeSafe("add_task", { url, category: $("#dlg-cat").value, destDir: dest || null });
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

setConn(false);
poll().then(() => {
    setInterval(poll, 1000);
});
