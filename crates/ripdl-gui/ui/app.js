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
    bundles: "Bundles",
    video: "Videos",
    music: "Music",
    images: "Images",
    documents: "Documents",
    archives: "Archives",
    programs: "Programs",
};
const CATALOG = ["all", "unfinished", "completed", "torrents", "bundles", "video", "music", "images", "documents", "archives", "programs"];

let tasks = [];
// The last category survives a restart; anything unknown falls back to "all".
let currentCategory = (() => {
    const saved = loadJson("ripdl.view", "all");
    return CATALOG.includes(saved) ? saved : "all";
})();
let selectedId = null;
// Every highlighted row (Ctrl/Shift-click). `selectedId` stays the "primary"
// row: details, the status bar and Enter/Double-click refer to it.
let selectedIds = new Set();
// Shift-click range origin.
let anchorId = null;
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

// The row title: torrent display name when known, else the file the task
// actually writes, else the URL basename. The URL often ends in an opaque
// token (GitHub release assets: .../12e29558-517b-...?rscd=...filename=X), so
// the on-disk name recorded by the engine wins as soon as there is one.
function rowName(t) {
    // Torrent and bundle rows carry the display name the user/daemon chose
    // (a bundle's is its folder name).
    if ((t.kind === "torrent" || t.kind === "bundle") && t.name) return t.name;
    if (t.dest_file) {
        const base = baseName(t.dest_file);
        if (base) return base;
    }
    return urlName(t.url);
}

// Multi-line title for a row: full error + diagnostics.
function detailsTitle(t) {
    const lines = [];
    const k = kindOf(t);
    lines.push(`${k.label}: ${t.url || ""}`);
    if (t.dest_dir) lines.push("Location: " + t.dest_dir);
    if (t.info_hash) lines.push("Info-hash: " + t.info_hash);
    if (t.kind === "bundle" && t.parts_total) {
        let line = `Parts: ${t.parts_done || 0} of ${t.parts_total}`;
        if (t.parts_missing && t.parts_missing.length) line += ` (missing ${t.parts_missing.join(", ")})`;
        lines.push(line);
    }
    if (t.kind === "torrent" && (t.uploaded > 0 || t.seeding)) {
        const ratio = t.downloaded > 0 ? (t.uploaded / t.downloaded).toFixed(2) : "0.00";
        lines.push(`Uploaded: ${humans(t.uploaded)} (ratio ${ratio}) \u00b7 ${t.peers || 0} peers`);
    }
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
    // Finished torrent still serving the swarm: distinct row label + icon.
    if (t.state === "completed" && t.seeding) return "seeding";
    if (t.state === "running" && t.kind === "video") return "hunt";
    // Torrent hashing on-disk data (resume/recheck): progress moves at disk
    // speed, so the row must not read as a download.
    if (t.state === "running" && t.verifying) return "verifying";
    // Torrent still fetching metadata: no bytes and no total yet.
    if (t.state === "running" && t.kind === "torrent" && !t.total) return "metadata";
    // Probing the URL / planning segments: no bytes have arrived yet.
    if (t.state === "running" && t.probing) return "connecting";
    // The link died (auth/expiry): the bytes are safe, the address is not.
    // "expired" says what happened; "stale" stays the internal name.
    if (t.state === "stale") return "expired";
    return t.state;
}

// Speed cell: download rate while moving, upload rate while seeding.
function speedText(t) {
    if (t.state === "completed" && t.seeding) {
        return t.upload_bps ? `\u2191 ${humans(t.upload_bps)}/s` : "\u2191 -";
    }
    // A parked row keeps its last sample; showing it reads as live bandwidth.
    if (!["running", "queued"].includes(t.state)) return "-";
    return t.speed_bps ? `${humans(t.speed_bps)}/s` : "-";
}

// Live transfer rate only: a paused/failed row's stored rate must not inflate
// the strip or the graph.
function liveSpeed(t) {
    return ["running", "queued"].includes(t.state) ? t.speed_bps || 0 : 0;
}

// Optional Peers column: torrent rows only, "-" for everything else.
function peersText(t) {
    if (!optCols.has("peers")) return null;
    return t.kind === "torrent" && t.peers ? String(t.peers) : "-";
}

function peersOf(t) {
    return t.kind === "torrent" ? t.peers || 0 : -1;
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

// Bundle rows carry how many parts are done next to the percent: one task,
// many files, and "3/7 parts" is what makes the aggregate legible.
function partsText(t) {
    if (t.kind !== "bundle" || !t.parts_total) return "";
    return `${t.parts_done || 0}/${t.parts_total} parts`;
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

// One icon per sidebar category, reused for the row's file-type glyph.
const CAT_ICON = {
    all: "inbox", unfinished: "clock", completed: "check", torrents: "tfile",
    bundles: "layers",
    video: "film", music: "music", images: "image", documents: "doc",
    archives: "box", programs: "app", other: "file",
};

// Transport, not content: this is what tells a torrent row apart from a plain
// HTTP download at a glance (and hints that torrent rows have a details view).
const KIND = {
    torrent: { icon: "tfile", cls: "green", label: "BitTorrent", hint: "double-click for files, peers and trackers" },
    bundle: { icon: "layers", cls: "amber", label: "Bundle", hint: "multi-part download in one folder - double-click for the parts" },
    http: { icon: "download", cls: "blue", label: "HTTP", hint: "multi-segment download - double-click for segments" },
    video: { icon: "film", cls: "violet", label: "Video", hint: "stream page handled by yt-dlp - double-click for details" },
};

function kindOf(t) {
    return KIND[t.kind] || KIND.http;
}

// Row glyph: what the file *is* (film/music/archive/...). Torrent payloads
// without an extension are multi-file folders, so they get the box icon.
function fileIcon(t) {
    let cat = categoryOf(rowName(t));
    if (cat === "other" && (t.kind === "torrent" || t.kind === "bundle")) cat = "archives";
    return ico(CAT_ICON[cat] || "file", "cat-" + cat);
}

function kindCell(t) {
    const k = kindOf(t);
    const tip = `${k.label} \u2014 ${k.hint}`;
    return `<td class="kind-cell" title="${escapeHtml(tip)}">${ico(k.icon, k.cls)}</td>`;
}

const STATUS_ICON = {
    completed: ico("check"), running: ico("download"), queued: ico("clock"),
    timed: ico("clock"), paused: ico("pause"), failed: ico("alert"),
    // The address died; the fix is a refresh, so the icon points there
    // (same globe the refresh entries use).
    expired: ico("globe"),
    cancelled: ico("x"), hunt: ico("film"), downloading: ico("download"),
    verifying: ico("search"), seeding: ico("up"), metadata: ico("globe"),
    connecting: ico("globe"),
};

function renderSidebar(counts) {
    $("#side-items").innerHTML = CATALOG.map((cat) => `
      <div class="side-item ${cat === currentCategory ? "active" : ""}" data-cat="${cat}">
        <span class="side-ico">${ico(CAT_ICON[cat])}</span>
        <span class="side-label">${CAT_LABEL[cat]}</span>
        <span class="count">${counts[cat] ?? 0}</span>
      </div>`).join("");
}

function inCurrentView(t) {
    if (currentCategory === "all") return true;
    if (currentCategory === "unfinished") return t.state !== "completed";
    if (currentCategory === "completed") return t.state === "completed";
    if (currentCategory === "torrents") return t.kind === "torrent";
    if (currentCategory === "bundles") return t.kind === "bundle";
    return categoryOf(rowName(t)) === currentCategory;
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
// Cells are addressed by class, not index, so inserting a column (the kind
// column did exactly this) can never silently shift these writes.
function refreshVolatile(t) {
    const tr = rowCache.get(t.id);
    if (!tr) return;
    const set = (sel, txt) => {
        const el = tr.querySelector(sel);
        if (el && el.textContent !== txt) el.textContent = txt;
    };
    set(".size-cell", sizeText(t));
    set(".eta-cell", etaOf(t) || "-");
    set(".speed-cell", speedText(t));
    set(".parts", partsText(t));
    const peers = peersText(t);
    if (peers != null) set(".peers-cell", peers);
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

/* ---------------------------------------------------------------------------
 * Column layout: widths and sort order are the user's, so both persist in
 * localStorage. Widths drive the <colgroup>; the trailing fill column eats the
 * slack so the table always spans the window. Anything that cannot be loaded
 * (first run, corrupt value) falls back to the defaults.
 * ------------------------------------------------------------------------- */
const COLS = [
    { key: "name", def: 320, min: 120, label: "File Name" },
    { key: "kind", def: 44, min: 44, label: "Kind", icon: "layers" },
    { key: "status", def: 150, min: 90, label: "Status" },
    { key: "size", def: 120, min: 70, label: "Size" },
    { key: "prog", def: 190, min: 90, label: "Progress" },
    { key: "eta", def: 90, min: 80, label: "Time Left" },
    // Optional: torrent rows only, off by default (View > Peers column).
    { key: "peers", def: 70, min: 56, label: "Peers", optional: true },
    { key: "speed", def: 110, min: 70, label: "Speed" },
    // Row actions stay a fixed strip at the far right; the spacer column
    // before them absorbs window slack so the buttons never float mid-table.
    { key: "act", def: 130, min: 110, label: "" },
];
const COL_BY_KEY = Object.fromEntries(COLS.map((c) => [c.key, c]));

// Optional columns the user turned on (persisted).
let optCols = new Set(
    (loadJson("ripdl.optcols", []) || []).filter((k) => COL_BY_KEY[k] && COL_BY_KEY[k].optional)
);

// Columns to render, in order: optional ones only when enabled.
function visibleCols() {
    return COLS.filter((c) => !c.optional || optCols.has(c.key));
}

function loadJson(key, fallback) {
    try {
        const raw = localStorage.getItem(key);
        return raw ? JSON.parse(raw) : fallback;
    } catch (_) {
        return fallback;
    }
}

function saveJson(key, value) {
    try {
        if (value == null) localStorage.removeItem(key);
        else localStorage.setItem(key, JSON.stringify(value));
    } catch (_) { /* private mode / quota: layout is a nicety, not state */ }
}

// A window that never had its columns touched: scale the defaults to the
// table area, leaving the fixed actions strip its room. Keeps the first launch
// free of a horizontal scrollbar on a 900-1200 px window.
function defaultWidths() {
    const avail = Math.max(560, ($("#main") && $("#main").clientWidth) || 900);
    const body = COLS.filter((c) => c.key !== "act");
    const total = body.reduce((s, c) => s + c.def, 0);
    const act = COL_BY_KEY.act.def;
    const room = Math.max(320, avail - act - 8);
    const out = {};
    let spent = 0;
    for (const c of body) {
        out[c.key] = Math.max(c.min, Math.round((c.def / total) * room));
        spent += out[c.key];
    }
    // Minimums can push the sum over the window (a narrow window); take it
    // back from the widest column instead of growing a scrollbar.
    let over = spent + act - avail + 2;
    while (over > 0) {
        const widest = body.reduce((a, b) => (out[a.key] >= out[b.key] ? a : b));
        if (out[widest.key] <= widest.min) break;
        const cut = Math.min(over, out[widest.key] - widest.min);
        out[widest.key] -= cut;
        over -= cut;
    }
    out.act = act;
    return out;
}

let colWidths = (() => {
    const saved = loadJson("ripdl.cols", null);
    const widths = defaultWidths();
    if (!saved || typeof saved !== "object") return widths;
    for (const c of COLS) {
        const w = Number(saved[c.key]);
        // Older layouts may have stored act as null (auto); rewrite to its
        // fixed default so the strip keeps its buttons.
        if (Number.isFinite(w) && w >= c.min) widths[c.key] = Math.round(w);
    }
    return widths;
})();

let sortState = (() => {
    const s = loadJson("ripdl.sort", null);
    return s && COL_BY_KEY[s.key] && (s.dir === 1 || s.dir === -1) ? s : null;
})();

function applyColWidths() {
    for (const [key, w] of Object.entries(colWidths)) {
        const col = document.querySelector(`#task-table col[data-col="${key}"]`);
        if (!col) continue;
        col.style.width = w == null ? "auto" : `${w}px`;
    }
}

function renderSortIndicators() {
    for (const th of document.querySelectorAll("#task-table thead th[data-col]")) {
        const on = !!sortState && sortState.key === th.dataset.col;
        th.classList.toggle("sorted", on);
        const ind = th.querySelector(".sort-ind");
        if (ind) ind.textContent = on ? (sortState.dir === 1 ? "\u25B2" : "\u25BC") : "";
    }
}

function setSort(key) {
    if (key === "act") return;
    if (!sortState || sortState.key !== key) sortState = { key, dir: 1 };
    else if (sortState.dir === 1) sortState.dir = -1;
    else sortState = null;
    saveJson("ripdl.sort", sortState);
    renderSortIndicators();
    render();
}

const STATE_RANK = {
    running: 0, hunt: 1, verifying: 2, connecting: 2, metadata: 3, queued: 4, timed: 5,
    seeding: 6, paused: 7, expired: 8, failed: 9, completed: 10, cancelled: 11,
};
const KIND_RANK = { torrent: 0, bundle: 1, http: 2, video: 3 };

// Seconds left, for sorting only (the cell keeps its own formatting).
function etaSeconds(t) {
    if (!t.total || !t.speed_bps || t.speed_bps <= 0) return null;
    const left = (t.total - t.downloaded) / t.speed_bps;
    return left > 0 ? left : null;
}

function speedOf(t) {
    if (t.state === "completed" && t.seeding) return t.upload_bps || 0;
    return liveSpeed(t);
}

function compareBy(key, a, b) {
    switch (key) {
        case "name":
            return rowName(a).localeCompare(rowName(b), undefined, { numeric: true, sensitivity: "base" });
        case "kind":
            return (KIND_RANK[a.kind] ?? 9) - (KIND_RANK[b.kind] ?? 9);
        case "status":
            return (STATE_RANK[stateLabel(a)] ?? 9) - (STATE_RANK[stateLabel(b)] ?? 9);
        case "size":
            return (a.total ?? a.downloaded) - (b.total ?? b.downloaded);
        case "prog":
            return (percentOf(a) ?? -1) - (percentOf(b) ?? -1);
        case "eta":
            return (etaSeconds(a) ?? Infinity) - (etaSeconds(b) ?? Infinity);
        case "peers":
            return peersOf(a) - peersOf(b);
        case "speed":
            return speedOf(a) - speedOf(b);
        default:
            return 0;
    }
}

// The view as shown: category filter first, then the user's sort (if any).
// No sort = daemon order (queue order), which is what users expect for a
// task list that just grew a row.
function visibleTasks() {
    const view = tasks.filter(inCurrentView);
    if (!sortState) return view;
    const { key, dir } = sortState;
    return view.sort((a, b) => {
        const r = compareBy(key, a, b);
        return r !== 0 ? r * dir : 0;
    });
}

// Header cells come from COLS, so an optional column can be turned on or off
// without the header drifting out of sync with the rows.
function buildHeader() {
    const cols = visibleCols();
    // The spacer column sits just before the actions strip so the buttons stay
    // pinned to the right edge no matter how much slack the window has.
    const body = cols.filter((c) => c.key !== "act");
    const act = cols.filter((c) => c.key === "act");
    const ordered = [...body, { key: "fill" }, ...act];
    $("#task-cols").innerHTML = ordered.map((c) => `<col data-col="${c.key}" class="${c.key === "fill" ? "col-fill" : ""}" />`).join("");
    $("#task-head").innerHTML = "<tr>" + ordered.map((c) => {
        if (c.key === "fill") return '<th class="col-fill"></th>';
        const label = c.icon ? `<svg class="ico"><use href="#i-${c.icon}"/></svg>` : c.label;
        const title = c.label
            ? ` title="${c.label} - click to sort, drag the edge to resize"`
            : "";
        return `<th class="col-${c.key}" data-col="${c.key}"${title}>${label}</th>`;
    }).join("") + "</tr>";

    for (const th of $("#task-head").querySelectorAll("th[data-col]")) {
        const key = th.dataset.col;
        const meta = COL_BY_KEY[key];
        if (!meta) continue;
        th.insertAdjacentHTML("beforeend", '<span class="sort-ind"></span>');
        const grip = document.createElement("span");
        grip.className = "col-resizer";
        grip.title = "Drag to resize \u00b7 double-click to reset this column";
        th.appendChild(grip);
        grip.addEventListener("mousedown", (ev) => {
            ev.preventDefault();
            ev.stopPropagation();
            const startX = ev.clientX;
            const startW = colWidths[key] ?? th.offsetWidth;
            document.body.classList.add("col-resizing");
            const move = (e) => {
                const w = Math.max(meta.min, Math.round(startW + (e.clientX - startX)));
                colWidths[key] = w;
                applyColWidths();
            };
            const up = () => {
                document.removeEventListener("mousemove", move);
                document.removeEventListener("mouseup", up);
                document.body.classList.remove("col-resizing");
                saveJson("ripdl.cols", colWidths);
            };
            document.addEventListener("mousemove", move);
            document.addEventListener("mouseup", up);
        });
        grip.addEventListener("dblclick", (ev) => {
            ev.preventDefault();
            ev.stopPropagation();
            colWidths[key] = meta.def;
            applyColWidths();
            saveJson("ripdl.cols", colWidths);
        });
    }
    applyColWidths();
    renderSortIndicators();
}

// Keep the saved layout inside the window: shrink (never grow) when the
// window is narrower than the stored widths, so a window resize or a column
// toggle cannot leave a horizontal scrollbar behind.
function fitWidths() {
    const main = $("#main");
    if (!main) return;
    const room = main.clientWidth - 2 - (colWidths.act ?? COL_BY_KEY.act.def);
    const cols = COLS.filter((c) => c.key !== "act" && (!c.optional || optCols.has(c.key)));
    const widthOf = (c) => colWidths[c.key] ?? c.def;
    let total = cols.reduce((s, c) => s + widthOf(c), 0);
    if (total <= room) return;

    // Take the overflow from every column that has slack above its minimum,
    // proportionally, then shave the remainder off the widest one (rounding).
    const slack = cols.reduce((s, c) => s + Math.max(0, widthOf(c) - c.min), 0);
    if (slack > 0) {
        const over = total - room;
        for (const c of cols) {
            const s = Math.max(0, widthOf(c) - c.min);
            if (!s) continue;
            const cut = Math.min(s, Math.round(over * (s / slack)));
            if (cut > 0) colWidths[c.key] = widthOf(c) - cut;
        }
    }
    for (let guard = 0; guard < 64 && total > room; guard++) {
        const widest = cols.reduce((a, b) => (widthOf(a) >= widthOf(b) ? a : b));
        if (widthOf(widest) <= widest.min) break;
        colWidths[widest.key] = widthOf(widest) - 1;
        total = cols.reduce((s, c) => s + widthOf(c), 0);
    }
    saveJson("ripdl.cols", colWidths);
    applyColWidths();
}

// Show/hide an optional column (View menu) and repaint.
function toggleColumn(key) {
    const on = !optCols.has(key);
    if (on) optCols.add(key);
    else optCols.delete(key);
    saveJson("ripdl.optcols", [...optCols]);
    // Sorting by a column nobody can see is confusing: drop it.
    if (!on && sortState && sortState.key === key) {
        sortState = null;
        saveJson("ripdl.sort", null);
    }
    // Make room by narrowing the name column instead of pushing the table
    // wider than the window (a new column should not sprout a scrollbar).
    const meta = COL_BY_KEY[key];
    const nameMeta = COL_BY_KEY.name;
    const nameW = colWidths.name ?? nameMeta.def;
    const addW = colWidths[key] || meta.def;
    if (on) {
        const take = Math.min(addW, Math.max(0, nameW - nameMeta.min));
        colWidths[key] = take || addW;
        if (take > 0) colWidths.name = nameW - take;
    } else if (colWidths.name != null) {
        colWidths.name = Math.min(nameMeta.def, nameW + addW);
    }
    saveJson("ripdl.cols", colWidths);
    buildHeader();
    lastStructKey = null;
    render();
}

// Resize grips + sort indicators on every header (the delegation is bound
// once; the cells themselves are rebuilt by buildHeader).
function initColumnUi() {
    buildHeader();
    fitWidths();
    $("#task-head").addEventListener("click", (ev) => {
        if (ev.target.classList.contains("col-resizer")) return;
        const th = ev.target.closest("th[data-col]");
        if (th) setSort(th.dataset.col);
    });
    // A narrower window shrinks the stored widths instead of scrolling.
    let resizeTimer = null;
    window.addEventListener("resize", () => {
        clearTimeout(resizeTimer);
        resizeTimer = setTimeout(fitWidths, 250);
    });
}

// --- selection -------------------------------------------------------------
// One row is the "primary" (details / status / Enter); the rest are extras a
// Ctrl/Shift-click added, so bulk actions stay one click away without a
// separate mode.

function selectedTasks() {
    const ids = selectedIds;
    return visibleTasks().filter((t) => ids.has(t.id));
}

function paintSelection() {
    for (const tr of TBODY.children) tr.classList.toggle("selected", selectedIds.has(tr.dataset.id));
}

function setSelection(ids, primary = null) {
    selectedIds = new Set(ids);
    selectedId = primary || (ids.length ? ids[ids.length - 1] : null);
    if (!selectedId) anchorId = null;
    paintSelection();
    renderStatusbar();
}

function toggleSelection(id) {
    if (selectedIds.has(id)) {
        selectedIds.delete(id);
        if (selectedId === id) selectedId = [...selectedIds].pop() || null;
    } else {
        selectedIds.add(id);
        selectedId = id;
    }
    anchorId = id;
    paintSelection();
    renderStatusbar();
}

function extendSelection(id) {
    const view = visibleTasks().map((t) => t.id);
    const from = view.indexOf(anchorId || selectedId);
    const to = view.indexOf(id);
    if (from < 0 || to < 0) {
        setSelection([id], id);
        return;
    }
    const [a, b] = from <= to ? [from, to] : [to, from];
    setSelection(view.slice(a, b + 1), id);
}

// Select a row by id and keep it visible (keyboard navigation, sort changes).
function selectRow(id) {
    setSelection([id], id);
    const tr = rowCache.get(id);
    if (tr) tr.scrollIntoView({ block: "nearest" });
}

function render() {
    // Category counts (also shown in the sidebar). Counts only change with
    // the task set / state label / name extension - all structural - so the
    // sidebar is repainted here and never by the volatile path.
    const counts = { all: tasks.length, unfinished: 0, completed: 0, torrents: 0, bundles: 0 };
    for (const cat of ["video", "music", "images", "documents", "archives", "programs"]) counts[cat] = 0;
    for (const t of tasks) {
        if (t.state === "completed") counts.completed++; else counts.unfinished++;
        if (t.kind === "torrent") counts.torrents++;
        if (t.kind === "bundle") counts.bundles++;
        const cat = categoryOf(rowName(t));
        if (cat in counts) counts[cat]++;
    }
    renderSidebar(counts);

    const rows = $("#task-rows");
    const view = visibleTasks();
    $("#empty-hint").classList.toggle("hidden", view.length > 0);
    rows.innerHTML = view.map((t) => {
        const name = rowName(t);
        const state = stateLabel(t);
        const pct = percentOf(t);
        const barClass = state === "failed" || state === "cancelled" ? "failed" : (["paused", "queued", "expired", "timed"].includes(state) ? "paused" : "");
        const parts = partsText(t);
        const partsSpan = parts ? `<span class="parts">${parts}</span>` : "";
        const prog = pct == null
            ? `<div class="prog-track"><div class="prog-fill ${barClass}" style="width:0%"></div></div>${partsSpan}`
            : `<div class="prog-track"><div class="prog-fill ${barClass}" style="width:${Math.min(100, pct).toFixed(1)}%"></div></div><span class="pct">${pctText(pct, state)}%</span>${partsSpan}`;
        const size = sizeText(t);
        const eta = etaOf(t) || "-";
        const speed = speedText(t);
        const peers = peersText(t);
        const dir = t.dest_dir ? t.dest_dir.replace(/[\\/]+$/, "") : "";
        // Expired rows explain themselves in the cell tooltip; other states
        // keep the error text there.
        const stateTip = state === "expired"
            ? (t.stale_reason === "auth"
                ? "The link expired - refresh the address to keep the bytes already on disk."
                : t.stale_reason === "size"
                    ? "The file changed on the server - refresh to the same file, or re-add it."
                    : "The address stopped working - refresh it to keep the bytes already on disk.")
            : errorTitle(t.error);
        const cls = selectedIds.has(t.id) ? ' class="selected"' : "";
        return `<tr${cls} data-id="${t.id}">
          <td class="name-cell" title="${escapeHtml(detailsTitle(t))}">
            <div class="fname-cell"><span class="ficon">${fileIcon(t)}</span><span title="${escapeHtml(name)}">${escapeHtml(name)}</span></div>
            <div class="fdir" title="${escapeHtml(dir)}">${escapeHtml(dir)}</div>
          </td>
          ${kindCell(t)}
          <td class="st-${state}" title="${escapeHtml(stateTip)}"><span class="st-ico">${STATUS_ICON[state] || ""}</span>${state}${t.error ? '<div class="st-err">' + escapeHtml(shortError(t.error)) + '</div>' : ""}</td>
          <td class="size-cell">${size}</td>
          <td class="prog-cell">${prog}</td>
          <td class="eta-cell">${eta}</td>
          ${peers == null ? "" : `<td class="peers-cell">${peers}</td>`}
          <td class="speed-cell">${speed}</td>
          <td class="col-fill"></td>
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
        // A row button acts on that row only: clicking it is not a way to
        // bulk-pause, so the selection is left alone.
        doTaskAction(btn.dataset.act, btn.dataset.id);
        return;
    }
    const tr = ev.target.closest("tr");
    if (!tr) {
        // Empty space below the rows: drop the selection.
        if (ev.target === TBODY) setSelection([]);
        return;
    }
    const id = tr.dataset.id;
    if (ev.shiftKey) extendSelection(id);
    else if (ev.ctrlKey || ev.metaKey) toggleSelection(id);
    else setSelection([id], id);
};
TBODY.ondblclick = (ev) => {
    const tr = ev.target.closest("tr");
    if (!tr) return;
    setSelection([tr.dataset.id], tr.dataset.id);
    openDetails(tr.dataset.id);
};
TBODY.oncontextmenu = (ev) => {
    const tr = ev.target.closest("tr");
    if (!tr) return;
    ev.preventDefault();
    const id = tr.dataset.id;
    // Right-clicking a row inside a multi-selection keeps it (that is how you
    // act on the group); anywhere else the menu targets just this row.
    if (!selectedIds.has(id)) setSelection([id], id);
    else selectedId = id;
    paintSelection();
    showCtx(ev.clientX, ev.clientY);
};
$("#side-items").onclick = (ev) => {
    const el = ev.target.closest(".side-item");
    if (!el) return;
    currentCategory = el.dataset.cat;
    saveJson("ripdl.view", currentCategory);
    lastStructKey = null;
    render();
};

// What the row / context-menu toggle does for a task, or null when there is
// nothing to toggle. States that are doing work count as "pause"; anything
// parked counts as "resume" (a paused row used to show no button at all, so
// the only way back was a context menu that did not work either).
const ACTIVE_STATES = ["running", "queued", "verifying", "metadata"];
function toggleIntent(t) {
    if (!t) return null;
    if (ACTIVE_STATES.includes(t.state)) return "pause";
    // A finished torrent keeps seeding until the user stops it.
    if (t.state === "completed") {
        return t.kind === "torrent" ? (t.seeding ? "pause" : "resume") : null;
    }
    // A stale row's fix is a new address, not a resume (resuming would only
    // re-confirm the failure): its row button opens the refresh dialog and
    // bulk resume skips it.
    if (t.state === "stale") return null;
    if (["paused", "failed", "timed"].includes(t.state)) return "resume";
    return null;
}

function toggleTitle(t) {
    const intent = toggleIntent(t);
    if (!intent) return "";
    if (t.state === "completed") return intent === "pause" ? "Stop seeding" : "Start seeding";
    return intent === "pause" ? "Pause" : "Resume";
}

function actionButtons(t) {
    const idAttr = `data-id="${t.id}"`;
    const intent = toggleIntent(t);
    const stop = intent ? `
      <button class="row-btn" ${idAttr} data-act="toggle" title="${toggleTitle(t)}">
        ${ico(intent === "pause" ? "pause" : "play")}
      </button>` : "";
    // An expired link has no play button: resuming would only re-confirm the
    // failure. The globe opens the refresh dialog, which keeps the bytes.
    const refresh = t.state === "stale" ? `
      <button class="row-btn" ${idAttr} data-act="refresh" title="Refresh address - keeps the downloaded bytes">${ico("globe")}</button>` : "";
    const openFile = t.state === "completed" ? `
      <button class="row-btn" ${idAttr} data-act="openFile" title="${t.kind === "bundle" ? "Open bundle folder" : "Open file"}">${ico("file")}</button>` : "";
    const open = `
      <button class="row-btn" ${idAttr} data-act="openFolder" title="Open download folder">${ico("folder")}</button>`;
    const rm = `
      <button class="row-btn" ${idAttr} data-act="deleteFiles" title="Delete from list AND disk">${ico("trash")}</button>`;
    return `<span class="actions">${stop}${refresh}${openFile}${open}${rm}</span>`;
}

function escapeHtml(s) {
    return s.replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
}

function showCtx(x, y) {
    // Retitle the toggle per task (Pause/Resume vs seed start/stop) and hide
    // entries that do not apply: details exist for every kind, refresh only
    // for HTTP downloads that have not finished. With more than one row
    // selected the per-task entries give way to bulk ones.
    const t = selectedId ? findTask(selectedId) : null;
    const many = selectedIds.size > 1;
    const group = many ? selectedTasks() : (t ? [t] : []);
    // A group toggles the way its label says: pause when anything is moving,
    // otherwise resume. Mixing both in one click reads as a random result.
    const anyActive = group.some((x) => toggleIntent(x) === "pause");
    const intent = group.length ? (anyActive ? "pause" : "resume") : null;
    // Only the rows this click would actually change are counted: a mixed
    // selection must not promise "Pause 4 tasks" and move two.
    const affected = group.filter((x) => toggleIntent(x) === intent);
    const useEl = $("#ctx-toggle-ico use");
    if (useEl) useEl.setAttribute("href", intent === "pause" ? "#i-pause" : "#i-play");
    $("#ctx-toggle-lbl").textContent = many && affected.length
        ? `${intent === "pause" ? "Pause" : "Resume"} ${affected.length} ${affected.length === 1 ? "task" : "tasks"}`
        : (toggleTitle(t) || "Pause");
    $("#ctx [data-act=toggle]").classList.toggle("hidden", !affected.length);
    // Per-task entries only make sense for one row.
    for (const act of ["details", "refresh", "openFile", "openFolder", "copyAddress"]) {
        $("#ctx [data-act=" + act + "]").classList.toggle("hidden", many || !t);
    }
    $("#ctx-details-lbl").textContent = t && t.kind === "torrent" ? "Torrent details…"
        : t && t.kind === "bundle" ? "Bundle details…" : "Details…";
    $("#ctx [data-act=refresh]").classList.toggle(
        "hidden",
        many || !t || t.kind !== "http" || t.state === "completed"
    );
    // Nothing on disk to open until the task produced a file.
    $("#ctx [data-act=openFile]").classList.toggle("hidden", many || !t || !t.dest_file);
    $("#ctx-open-lbl").textContent = t && t.kind === "bundle" ? "Open bundle folder" : "Open file";
    $("#ctx [data-act=copyAddress]").classList.toggle("hidden", many || !t || !t.url);
    $("#ctx-remove-lbl").textContent = many ? `Remove ${group.length} from list` : "Remove from list";
    $("#ctx-delete-lbl").textContent = many ? `Remove ${group.length} & delete files` : "Remove & delete files";

    const menu = $("#ctx");
    menu.classList.remove("hidden");
    // Position after it is laid out so the clamp uses the real height (the
    // menu grew with the refresh/details entries and used to overflow short
    // windows at the old fixed 130px guess).
    const w = menu.offsetWidth || 190;
    const h = menu.offsetHeight || 130;
    menu.style.left = Math.max(0, Math.min(x, innerWidth - w - 4)) + "px";
    menu.style.top = Math.max(0, Math.min(y, innerHeight - h - 4)) + "px";
}

document.addEventListener("click", () => { $("#ctx").classList.add("hidden"); closeMenus(); });
$("#ctx").onclick = (ev) => {
    // The entry's own <div> carries data-act; the visible target is its icon
    // or label, so resolve the ancestor instead of reading the target (that
    // made every entry except the blank strip a no-op).
    const entry = ev.target.closest("#ctx [data-act]");
    if (!entry) return;
    $("#ctx").classList.add("hidden");
    if (selectedId) doTaskAction(entry.dataset.act, selectedId);
};

function findTask(id) {
    return tasks.find((x) => x.id === id);
}

// Which tasks an action applies to: single-row entries always hit the row the
// user aimed at; bulk-capable ones (toggle / remove / delete files) follow a
// multi-selection.
function actionTargets(act, id) {
    const perRow = ["details", "refresh", "openFile", "openFolder", "copyAddress"];
    if (perRow.includes(act)) return [id];
    if (selectedIds.size > 1 && selectedIds.has(id)) return selectedTasks().map((t) => t.id);
    return [id];
}

async function removeTasks(ids, deleteFiles) {
    const list = ids.map(findTask).filter(Boolean);
    if (!list.length) return;
    if (deleteFiles) {
        const what = list.length === 1 ? `"${rowName(list[0])}"` : `${list.length} tasks`;
        if (!confirm(`Delete the downloaded file(s) for ${what} from disk?\nThis cannot be undone.`)) return;
    } else {
        // Removing a finished task loses nothing (the file stays); removing an
        // unfinished one throws away its resume position, so ask first.
        const risky = list.filter((t) => t.state !== "completed");
        if (risky.length) {
            const what = risky.length === 1
                ? `"${rowName(risky[0])}"`
                : `${risky.length} unfinished tasks`;
            if (!confirm(`Remove ${what} from the list?\nThe partial download stays on disk, but its resume position and settings are lost.`)) return;
        }
    }
    for (const t of list) {
        try {
            await invokeSafe("remove_task", { id: t.id, deleteFiles });
        } catch (e) {
            flashErr(String(e));
            break;
        }
        selectedIds.delete(t.id);
    }
    if (!selectedIds.size) selectedId = null;
    await poll();
}

async function toggleTasks(ids) {
    const list = ids.map(findTask).filter(Boolean);
    // One intent for the whole group, matching the menu label.
    const pause = list.some((t) => toggleIntent(t) === "pause");
    for (const t of list) {
        const intent = toggleIntent(t);
        if (intent !== (pause ? "pause" : "resume")) continue;
        try {
            await invokeSafe(pause ? "pause_task" : "resume_task", { id: t.id });
        } catch (e) {
            flashErr(String(e));
            break;
        }
    }
    await poll();
}

async function doTaskAction(act, id) {
    const t = findTask(id);
    if (!t) return;
    const ids = actionTargets(act, id);
    try {
        if (act === "remove") await removeTasks(ids, false);
        else if (act === "deleteFiles") await removeTasks(ids, true);
        else if (act === "details") {
            await openDetails(id);
        } else if (act === "refresh") {
            openRefreshDialog(id);
        } else if (act === "copyAddress") {
            await copyText(t.url);
            flashStatus("address copied");
        } else if (act === "openFolder") {
            await invokeSafe("open_path", { path: t.dest_dir || "", reveal: null });
        } else if (act === "openFile") {
            if (!t.dest_file) { flashErr("No produced file recorded for this task."); return; }
            const dir = t.dest_file.replace(/[^\\/]+$/, "");
            await invokeSafe("open_path", { path: dir, reveal: baseName(t.dest_file) });
        } else if (act === "toggle") {
            await toggleTasks(ids);
        }
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

// Clipboard without pulling in a plugin: the async API first, a selection
// copy as the fallback for a page the runtime did not treat as secure.
async function copyText(text) {
    if (navigator.clipboard && navigator.clipboard.writeText) {
        try {
            await navigator.clipboard.writeText(text);
            return;
        } catch (_) { /* fall through */ }
    }
    const ta = document.createElement("textarea");
    ta.value = text;
    ta.style.position = "fixed";
    ta.style.opacity = "0";
    document.body.appendChild(ta);
    ta.select();
    const ok = document.execCommand("copy");
    document.body.removeChild(ta);
    if (!ok) throw new Error("copy failed");
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
        // Selection must not outlive its tasks (ghost context-menu actions).
        const live = new Set(tasks.map((t) => t.id));
        for (const id of [...selectedIds]) if (!live.has(id)) selectedIds.delete(id);
        if (selectedId && !live.has(selectedId)) selectedId = [...selectedIds].pop() || null;
        setConn(true);
        // Structural changes (task set, state, name, errors) repaint the
        // tbody + sidebar; bytes-only changes update cells in place - no DOM
        // rebuild, no flicker, no lost hover.
        const view = visibleTasks();
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
    const totalSpeed = tasks.reduce((s, t) => s + liveSpeed(t), 0);
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
    // Selection readout doubles as a "what am I looking at" hint; the plain
    // counts return as soon as nothing is selected. A multi-selection says so
    // instead of pretending only the primary row is selected.
    const sel = selectedId ? findTask(selectedId) : null;
    const n = selectedIds.size;
    const countsTxt = n > 1
        ? `${n} selected · ${sel ? rowName(sel) : ""}`.trim()
        : (sel ? `${rowName(sel)} · ${stateLabel(sel)}` : statusbarText());
    if (ST_COUNTS.textContent !== countsTxt) ST_COUNTS.textContent = countsTxt;
}

/* Uplink chips in the status bar: one per bonded NIC, "name rate spark
 * state", sampled every 2 s while the window is visible. The chip is a
 * pointer into Settings > Network (where the toggle lives) - the state
 * word is in the chip, not just a color, so a quick glance still reads. */
const UPLINK_WINDOW = 40; // samples kept per link (40 x 2 s = 80 s of spark)
let uplinks = []; // last fetch: [{ name, ip, default_route, rate_bps, weight, in_use, fails }]
const uplinkHist = new Map(); // ip -> [bytes/s ...] at the 2 s cadence
let uplinkTimer = null;
let uplinksOpen = false; // one fetch in flight at a time

function uplinkClass(l) {
    if (l.weight === 0) return "st-off"; // quarantined: its dead dial streak won
    if (l.fails > 0) return "st-warn"; // a dial just failed; it is being re-probed
    return l.in_use > 0 ? "st-on" : "st-idle";
}

function uplinkWord(l) {
    const c = uplinkClass(l);
    if (c === "st-off") return "dead";
    if (c === "st-warn") return "flaky";
    return c === "st-on" ? "active" : "idle";
}

function uplinkSpark(hist) {
    const w = 44, h = 12;
    if (hist.length < 2) return "";
    const peak = Math.max(...hist) || 1;
    const pts = hist
        .map((v, i) => {
            const x = (i / (hist.length - 1)) * w;
            const y = h - 1 - (v / peak) * (h - 2);
            return `${x.toFixed(1)},${y.toFixed(1)}`;
        })
        .join(" ");
    return `<svg class="u-spark" width="${w}" height="${h}" viewBox="0 0 ${w} ${h}" aria-hidden="true"><polyline points="${pts}" fill="none" stroke="currentColor" stroke-width="1.2" stroke-linejoin="round"/></svg>`;
}

function renderUplinks() {
    const box = $("#st-uplinks");
    if (!uplinks.length) {
        box.hidden = true;
        box.replaceChildren();
        return;
    }
    box.hidden = false;
    // Keyed rebuild only when the set changed: rates/sparks re-render anyway,
    // but the two-second cadence keeps this cheap (<5 chips).
    box.replaceChildren(
        ...uplinks.map((l) => {
            let hist = uplinkHist.get(l.ip);
            if (!hist) {
                hist = [];
                uplinkHist.set(l.ip, hist);
            }
            const avg = hist.length
                ? hist.reduce((s, v) => s + v, 0) / hist.length
                : l.rate_bps;
            const chip = document.createElement("span");
            chip.className = `uplink-chip ${uplinkClass(l)}`;
            chip.textContent = `${l.name} `;
            const rate = document.createElement("b");
            rate.textContent = l.rate_bps > 0 ? `${humans(l.rate_bps)}/s` : "\u2014";
            chip.appendChild(rate);
            const spark = uplinkSpark(hist);
            if (spark) chip.insertAdjacentHTML("beforeend", spark);
            const word = document.createElement("i");
            word.textContent = ` ${uplinkWord(l)}`;
            word.style.fontStyle = "normal";
            chip.appendChild(word);
            chip.title = [
                `${l.name} - ${l.ip}${l.default_route ? " (default route)" : ""}`,
                `now ${humans(l.rate_bps)}/s - avg ${humans(Math.round(avg))}/s`,
                `connections: ${l.in_use} - dial fails: ${l.fails}`,
                `state: ${uplinkWord(l)} (preference weight ${l.weight})`,
            ].join("\n");
            chip.onclick = () => openSettings("network");
            return chip;
        })
    );
}

async function pollUplinks() {
    if (uplinksOpen) return;
    uplinksOpen = true;
    try {
        const links = await invokeSafe("bond_links");
        uplinks = links;
        for (const l of uplinks) {
            let hist = uplinkHist.get(l.ip);
            if (!hist) {
                hist = [];
                uplinkHist.set(l.ip, hist);
            }
            hist.push(l.rate_bps);
            while (hist.length > UPLINK_WINDOW) hist.shift();
        }
        // Links that disappeared (toggle off, adapter gone) leave their
        // history behind; a returning adapter starts from its old curve -
        // harmless, it fades out through the window.
        renderUplinks();
    } catch (_) {
        /* daemon briefly unreachable at open: keep the last render */
    } finally {
        uplinksOpen = false;
    }
}

function scheduleUplinks() {
    clearTimeout(uplinkTimer);
    // Hidden window: no RPM reason to sample, tray-hibernate skips onto the
    // slow poll; chips just wait.
    const ms = document.hidden ? 5000 : 2000;
    uplinkTimer = setTimeout(() => { pollUplinks().then(scheduleUplinks); }, ms);
}

/* Toolbar */
let dlgDestTouched = false;

async function resumeAll() {
    // Stale rows are skipped on purpose: resuming one only re-confirms the
    // dead link; the fix is the refresh dialog (the row's globe button).
    for (const t of tasks.filter((x) => ["paused", "failed", "timed"].includes(x.state))) {
        try { await invokeSafe("resume_task", { id: t.id }); } catch (e) { flashErr(String(e)); break; }
    }
    poll();
}
async function stopAll() {
    for (const t of tasks.filter((x) => ACTIVE_STATES.includes(x.state))) {
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
        { label: "Add .torrent File", hint: "Ctrl+T", act: () => pickTorrent() },
        { label: "Add Multi-part Links", hint: "Ctrl+B", act: () => openBundleDialog() },
        null,
        { label: "Send / Receive on Network", act: () => openLan() },
        null,
        { label: "Exit", act: () => { invokeSafe("exit_app"); } },
    ],
    tasks: () => [
        { label: "Resume All", act: () => resumeAll() },
        { label: "Stop All", act: () => stopAll() },
        null,
        { label: "Clear Completed", act: () => clearDone() },
    ],
    view: () => [
        {
            label: `${optCols.has("peers") ? "\u2713 " : "\u2003"}Peers column`,
            hint: "torrents",
            act: () => toggleColumn("peers"),
        },
        {
            label: "Reset column widths",
            act: () => {
                colWidths = defaultWidths();
                saveJson("ripdl.cols", colWidths);
                applyColWidths();
            },
        },
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
$("#btn-add-bundle").onclick = () => openBundleDialog();
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
    } else {
        // A check that says "current" must clear a banner left over from an
        // earlier daemon/session (e.g. an in-app update swapped the daemon
        // after the banner was rendered - it must not keep offering a version
        // that is already installed).
        $("#update-banner").classList.add("hidden");
        if (!initial) flashStatus(`ripdl v${info.current} is up to date`);
    }
}

// One-click upgrade: installer closes this GUI while replacing it, so the
// last visible act here is the banner text.
async function installUpdate() {
    const el = $("#update-text");
    el.textContent = "Downloading & verifying the new setup\u2026";
    try {
        await invokeSafe("install_update");
        el.textContent = "Installing \u2014 approve the Windows prompt; ripdl will restart when it finishes.";
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

/* Multi-part (bundle) dialog: paste direct links, get one task per group.
 * Grouping is the daemon's (`PreviewBundle` -> pure `ripdl_core::bundle`), so
 * the dialog only shows what the links would become and never guesses. */
let bundleDestTouched = false;
let bundlePreviewTimer = null;

function openBundleDialog(presetUrls) {
    $("#bdlg-urls").value = presetUrls || "";
    $("#bdlg-name").value = "";
    $("#bdlg-dest").value = "";
    bundleDestTouched = false;
    $("#bdlg-err").classList.add("hidden");
    $("#bdlg-err").textContent = "";
    $("#bdlg-preview").textContent = "";
    $("#bdlg-backdrop").classList.remove("hidden");
    $("#bdlg-urls").focus();
    scheduleBundlePreview();
}

function closeBundleDialog() {
    clearTimeout(bundlePreviewTimer);
    $("#bdlg-backdrop").classList.add("hidden");
}

function bundleLines() {
    return $("#bdlg-urls").value
        .split(/\r?\n/)
        .map((s) => s.trim())
        .filter(Boolean);
}

// One wording for the group count, mirroring `Group::parts_summary`.
function groupSummary(g) {
    const n = g.parts.length;
    const count = n === 1 ? "1 part"
        : g.total ? `${n} of ${g.total} parts`
        : `${n} parts from ${g.first}`;
    return `${g.name} \u2014 ${count}`;
}

function scheduleBundlePreview() {
    clearTimeout(bundlePreviewTimer);
    bundlePreviewTimer = setTimeout(refreshBundlePreview, 250);
}

async function refreshBundlePreview() {
    const urls = bundleLines();
    const el = $("#bdlg-preview");
    if (!urls.length) { el.textContent = ""; return; }
    try {
        const groups = await invokeSafe("preview_bundle", { urls });
        el.textContent = groups.length
            ? `Will queue: ${groups.map(groupSummary).join(" \u00b7 ")}`
            : "No usable http(s) links.";
    } catch (e) {
        el.textContent = "";
    }
}

function bundleError(msg) {
    const el = $("#bdlg-err");
    el.textContent = msg;
    el.classList.remove("hidden");
}

async function submitBundle() {
    const urls = bundleLines();
    if (!urls.length) { bundleError("Paste at least one link first."); return; }
    const btn = $("#bdlg-ok");
    btn.disabled = true;
    try {
        const groups = await invokeSafe("preview_bundle", { urls });
        if (!groups.length) throw new Error("no usable http(s) links");
        const dest = $("#bdlg-dest").value.trim();
        const name = $("#bdlg-name").value.trim();
        let bundles = 0;
        let singles = 0;
        for (const g of groups) {
            // A group of one is an ordinary download (same rule as the
            // extension offer): a folder around a single file helps nobody.
            if (g.parts.length === 1 && !name) {
                await invokeSafe("add_task", {
                    url: g.parts[0].url,
                    category: "auto",
                    destDir: dest || null,
                    sequential: false,
                    onlyFiles: null,
                });
                singles++;
            } else {
                await invokeSafe("add_bundle", {
                    urls: g.parts.map((p) => p.url),
                    name: name || g.name,
                    destDir: dest || null,
                });
                bundles++;
            }
        }
        closeBundleDialog();
        const bits = [];
        if (bundles) bits.push(`${bundles} bundle${bundles === 1 ? "" : "s"}`);
        if (singles) bits.push(`${singles} download${singles === 1 ? "" : "s"}`);
        flashStatus(`queued ${bits.join(" + ")}`);
        await poll();
    } catch (e) {
        bundleError(String(e).replace(/^.*error: /, ""));
    } finally {
        btn.disabled = false;
    }
}

$("#bdlg-urls").oninput = scheduleBundlePreview;
$("#bdlg-dest").oninput = () => { bundleDestTouched = true; };
$("#bdlg-browse").onclick = async () => {
    try {
        const picked = await invokeSafe("pick_dir");
        if (picked) {
            $("#bdlg-dest").value = picked;
            bundleDestTouched = true;
        }
    } catch (e) {
        flashErr(String(e));
    }
};
$("#bdlg-cancel").onclick = closeBundleDialog;
$("#bdlg-ok").onclick = submitBundle;
$("#bdlg-backdrop").onclick = (ev) => { if (ev.target === ev.currentTarget) closeBundleDialog(); };
$("#bdlg-urls").onkeydown = (ev) => {
    if (ev.key === "Enter" && (ev.ctrlKey || ev.metaKey)) { ev.preventDefault(); submitBundle(); }
};

/* Settings dialog (daemon-wide config) */
const SET_INPUTS = ["#set-conc", "#set-segs", "#set-minseg", "#set-speed", "#set-dir", "#set-ytdlp", "#set-ext"];
let extensionStatusTimer = null;

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

function setPathField(selector, value) {
    const input = $(selector);
    input.value = value ?? "";
    input.title = input.value;
}

async function openSettings(tab) {
    const err = $("#set-err");    err.classList.add("hidden");
    const network = tab === "network";
    selectSettingsTab(network ? $("#set-tab-network") : $("#set-tab-general"));
    if (network) highlightBondRow();
    SET_INPUTS.forEach((sel) => { $(sel).value = ""; });
    $("#set-tray").checked = true;
    $("#set-bond").checked = false;
    $("#set-update").checked = true;
    $("#set-auto").checked = false;
    $("#set-seed").checked = true;
    $("#set-ratio").value = "1";
    $("#set-seedtime").value = "0";
    try {
        const s = await invokeSafe("get_config");
        // Save is a full-struct replace: keep the values this dialog does not
        // edit (lan_*, extension_dir, bond_exclude...) so a save can never
        // reset them to defaults, and the edited fields are merged on top.
        settingsAtOpen = s;
        $("#set-conc").value = s.max_concurrent ?? "";
        $("#set-segs").value = s.max_segments ?? "";
        $("#set-minseg").value = bytesForEdit(s.min_segment_size);
        $("#set-speed").value = bytesForEdit(s.max_speed_bytes);
        setPathField("#set-dir", s.default_download_dir);
        setPathField("#set-ytdlp", s.ytdlp_path);
        setPathField("#set-ext", s.extension_dir);
        $("#set-tray").checked = s.hide_to_tray ?? true;
        $("#set-bond").checked = s.bond_interfaces ?? false;
        bondAtOpen = s.bond_interfaces ?? false;
        // NIC card draft: names left out of bonding until Save. Local, so
        // unchecking a card then cancelling loses the change, matching every
        // other field in this dialog.
        bondExcludeDraft = new Set((s.bond_exclude ?? []).map((n) => String(n).toLowerCase()));
        $("#set-update").checked = s.update_check ?? true;
        $("#set-auto").checked = s.update_auto_install ?? false;
        // serde skips None on the wire: absent = built-in default.
        $("#set-seed").checked = s.seed_enabled ?? true;
        $("#set-ratio").value = s.seed_ratio ?? "1";
        $("#set-seedtime").value = s.seed_time_min ?? "0";
        renderBondNics();
        renderExtInfo();
        renderBrowserExtension();
        startBondLive();
        if (extensionStatusTimer === null) {
            extensionStatusTimer = setInterval(() => {
                if (!$("#set-backdrop").classList.contains("hidden")) renderBrowserExtension();
            }, 15000);
        }
        $("#set-backdrop").classList.remove("hidden");
        $("#set-conc").focus();
    } catch (e) {
        err.textContent = String(e).replace(/^.*error: /, "");
        err.classList.remove("hidden");
    }
}

function closeSettings() {
    $("#set-backdrop").classList.add("hidden");
    if (extensionStatusTimer !== null) {
        clearInterval(extensionStatusTimer);
        extensionStatusTimer = null;
    }
    clearInterval(bondLiveTimer);
    bondLiveTimer = null;
}

// Bonding value when the dialog opened: changing it only affects the
// torrent session (created once per daemon run), so the save path nudges a
// restart instead of pretending it applied.
let bondAtOpen = false;

// Full settings snapshot taken when the dialog opened (the save merges the
// edited fields over this, full-replace protocol).
let settingsAtOpen = null;
// Names currently excluded on the NIC cards (lowercase; Set of strings).
let bondExcludeDraft = new Set();
// 2 s refresher for the NIC cards' live rate/state, while the dialog is open.
let bondLiveTimer = null;

// Show exactly which uplinks bonding would spread connections across: one
// uplink cannot add bandwidth, and a dead-end adapter is the classic
// "combining made it slower" trap. Each detected uplink is a card with a
// checkbox (uncheck = this adapter never carries download traffic; a
// metered tether is the user here) and a live rate/state badge.
async function renderBondNics() {
    const el = $("#set-bond-info");
    const live = $("#set-bond-state");
    const cards = $("#set-bond-nics");
    try {
        const nics = await invokeSafe("bond_nics");
        if (!nics.length) {
            el.textContent = "No internet uplink detected.";
            cards.replaceChildren();
        } else {
            const list = nics
                .map((n) => `${n.name} (${n.addr}${n.default ? ", default route" : ""})`)
                .join(" \u00b7 ");
            el.textContent = nics.length === 1
                ? `Only one uplink found: ${list}. Bonding needs at least two.`
                : `${nics.length} uplinks found: ${list}.`;
            cards.replaceChildren(
                ...nics.map((n) => {
                    const row = document.createElement("label");
                    row.className = "bond-nic";
                    const box = document.createElement("input");
                    box.type = "checkbox";
                    const on = !bondExcludeDraft.has(n.name.toLowerCase());
                    box.checked = on;
                    if (!on) row.classList.add("is-excluded");
                    box.onchange = () => {
                        const key = n.name.toLowerCase();
                        if (box.checked) bondExcludeDraft.delete(key);
                        else bondExcludeDraft.add(key);
                        row.classList.toggle("is-excluded", !box.checked);
                    };
                    row.appendChild(box);
                    const name = document.createElement("span");
                    name.className = "nic-name";
                    name.textContent = n.name;
                    row.appendChild(name);
                    const addr = document.createElement("span");
                    addr.className = "nic-addr";
                    addr.textContent = `${n.addr}${n.default ? " \u00b7 default route" : ""}`;
                    row.appendChild(addr);
                    const badge = document.createElement("span");
                    badge.className = "nic-live";
                    badge.dataset.ip = n.addr;
                    badge.textContent = "\u2026";
                    row.appendChild(badge);
                    row.title = "Unchecked: excluded from bonding. Checked: this uplink carries its share of download traffic.";
                    return row;
                }),
            );
            await refreshBondBadges();
        }
    } catch (e) {
        el.textContent = "";
    }
    // What the *running* torrent session actually bound. This follows the
    // machine now (the daemon re-reads uplinks every 5 s and hot-swaps the
    // session's bind list), so the checkbox is no longer "restart to apply".
    try {
        const st = await invokeSafe("bond_state");
        live.textContent = `Torrents: ${st.text}`;
        live.className = st.active ? "set-hint ok" : "set-hint";
    } catch (e) {
        live.textContent = "";
    }
}

// Live half of the NIC cards: rate + connections + state word per uplink.
// Updates badges only - the checkboxes are the user's interaction surface
// and must not be rebuilt under a click.
async function refreshBondBadges() {
    const cards = $("#set-bond-nics");
    if (!cards.children.length) return;
    let links;
    try {
        links = await invokeSafe("bond_links");
    } catch (_) {
        return; // daemon briefly unreachable: keep the last badges
    }
    const byIp = new Map(links.map((l) => [l.ip, l]));
    for (const badge of cards.querySelectorAll(".nic-live")) {
        const l = byIp.get(badge.dataset.ip);
        badge.textContent = l
            ? [
                l.rate_bps > 0 ? `${humans(l.rate_bps)}/s` : "idle",
                `${l.in_use} conn`,
                l.weight === 0 ? "dead" : l.fails > 0 ? "flaky" : l.in_use > 0 ? "active" : "idle",
            ].join(" \u00b7 ")
            : "-";
    }
}

function startBondLive() {
    clearInterval(bondLiveTimer);
    bondLiveTimer = setInterval(() => {
        // Only while the Network tab of an open settings dialog shows.
        const panel = $("#set-panel-network");
        if (panel && !panel.classList.contains("hidden")) refreshBondBadges();
    }, 2000);
}

// The Extension folder's manifest version against this app. Only the folder
// is checked here; the browser's own loaded copy is shown on the popup and
// Plugins pages, which is where a pending Reload is visible.
async function renderExtInfo() {
    const el = $("#set-ext-info");
    const dir = $("#set-ext").value.trim();
    if (!dir) {
        el.textContent = "Using the built-in extension copy.";
        el.className = "set-hint ok";
        return;
    }
    try {
        const info = await invokeSafe("extension_folder", { dir });
        if ($("#set-ext").value.trim() !== dir) return; // typed on while we asked
        if (info.skew === "same") {
            el.textContent = `Folder is v${info.version}. It matches this app.`;
            el.className = "set-hint ok";
        } else if (info.skew === "older") {
            el.textContent = `Folder is v${info.version}, but this app is v${info.app}. The folder predates this app.`;
            el.className = "set-hint warn";
        } else if (info.skew === "newer") {
            el.textContent = `Folder is v${info.version}, newer than this app (v${info.app}). Update ripdl.`;
            el.className = "set-hint warn";
        } else {
            el.textContent = "No manifest.json in this folder. Choose the unpacked extension folder.";
            el.className = "set-hint warn";
        }
    } catch (e) {
        el.textContent = "";
    }
}

function compareExtensionVersions(a, b) {
    const left = String(a).split("-")[0].split(".").map((n) => Number.parseInt(n, 10) || 0);
    const right = String(b).split("-")[0].split(".").map((n) => Number.parseInt(n, 10) || 0);
    for (let i = 0; i < Math.max(left.length, right.length); i++) {
        const delta = (left[i] || 0) - (right[i] || 0);
        if (delta) return Math.sign(delta);
    }
    return 0;
}

function ageText(unixSeconds) {
    const minutes = Math.max(0, Math.floor(Date.now() / 1000 - unixSeconds) / 60);
    if (minutes < 1) return "just now";
    if (minutes < 60) return `${Math.floor(minutes)} min ago`;
    const hours = Math.floor(minutes / 60);
    if (hours < 48) return `${hours} hr ago`;
    return `${Math.floor(hours / 24)} days ago`;
}

function setExtBadge(text, tone) {
    const badge = $("#set-ext-badge");
    badge.textContent = text;
    badge.className = tone ? `set-badge ${tone}` : "set-badge";
}

async function renderBrowserExtension() {
    const el = $("#set-browser-info");
    const dir = $("#set-ext").value.trim();
    try {
        const info = await invokeSafe("browser_extension_info", { dir });
        if ($("#set-ext").value.trim() !== dir) return;
        $("#set-ext-path").textContent = info.folder;
        const loaded = info.loaded;
        const who = info.browser || "The browser";
        const inBrowser = info.browser || "your browser";
        if (!info.folder_version) {
            setExtBadge("Folder not valid", "warn");
            el.textContent = `No manifest.json in this folder. The built-in copy is v${info.bundled_version}.`;
            el.className = "set-hint warn";
            return;
        }
        if (!loaded.last_seen) {
            setExtBadge("Not loaded", "warn");
            el.textContent = `Folder is v${info.folder_version}. Load it in ${inBrowser} using the steps below.`;
            el.className = "set-hint";
            return;
        }
        const seen = `Last seen ${ageText(loaded.last_seen)}.`;
        const order = compareExtensionVersions(loaded.version, info.folder_version);
        if (order < 0) {
            setExtBadge("Reload needed", "warn");
            el.textContent = `${who} has v${loaded.version} loaded, but the folder is v${info.folder_version}. Click Reload on the ripdl card. ${seen}`;
            el.className = "set-hint warn";
        } else if (order > 0) {
            setExtBadge("Folder is older", "warn");
            el.textContent = `${who} has v${loaded.version} loaded, newer than this folder (v${info.folder_version}). ${seen}`;
            el.className = "set-hint warn";
        } else {
            setExtBadge("Connected", "ok");
            el.textContent = `v${info.folder_version} is loaded in ${inBrowser}. ${seen}`;
            el.className = "set-hint ok";
        }
    } catch (e) {
        setExtBadge("Unavailable", "warn");
        el.textContent = String(e).replace(/^.*error: /, "");
        el.className = "set-hint warn";
    }
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
    if (Number.isNaN(conc) || Number.isNaN(segs)) return reject("Downloads at once and connections per download must be numbers.");
    if (Number.isNaN(minseg) || Number.isNaN(speed)) return reject("Chunk size and speed limit need a size such as 4MiB, or must be empty.");
    if (conc !== undefined && conc < 1) return reject("Downloads at once must be at least 1.");
    if (segs !== undefined && segs < 1) return reject("Connections per download must be at least 1.");
    if (minseg !== undefined && minseg < 1) return reject("Minimum chunk size must be at least 1 byte.");
    const dir = $("#set-dir").value.trim();
    const ratio = num($("#set-ratio"));
    const seedTime = num($("#set-seedtime"));
    if (Number.isNaN(ratio) || ratio < 0) return reject("Seed ratio must be 0 (no limit) or a positive number.");
    if (Number.isNaN(seedTime) || seedTime < 0) return reject("Seed time must be 0 (no limit) or a positive number of minutes.");
    // Full-replace protocol: the save merges the dialog's edited fields over
    // the snapshot taken at open. Cleared edited fields become absent ->
    // daemon defaults; the values this dialog does not edit (lan_*, the
    // pairing code, seeds config...) ride along unchanged instead of being
    // reset by the replace.
    const payload = {
        ...(settingsAtOpen ?? {}),
        max_concurrent: conc,
        max_segments: segs,
        min_segment_size: minseg,
        max_speed_bytes: speed,
        default_download_dir: dir === "" ? undefined : dir,
        ytdlp_path: $("#set-ytdlp").value.trim() || undefined,
        extension_dir: $("#set-ext").value.trim() || undefined,
        hide_to_tray: $("#set-tray").checked,
        bond_interfaces: $("#set-bond").checked,
        bond_exclude: bondExcludeDraft.size ? [...bondExcludeDraft] : [],
        bond_links: undefined, // not settings; never let a reply field loop back in
        update_check: $("#set-update").checked,
        update_auto_install: $("#set-auto").checked,
        seed_enabled: $("#set-seed").checked,
        seed_ratio: ratio,
        seed_time_min: seedTime === undefined ? undefined : Math.round(seedTime),
    };
    try {
        await invokeSafe("set_config", { settings: payload });
        closeSettings();
        const bondingChanged = $("#set-bond").checked !== bondAtOpen;
        if (bondingChanged) {
            // The daemon hot-swaps the torrent session's bind list on save, so
            // no restart: new peer connections spread over the new uplinks.
            flashStatus(
                $("#set-bond").checked
                    ? "settings saved - torrent bonding applied"
                    : "settings saved - torrent bonding turned off",
            );
        } else {
            flashStatus("settings saved");
        }
    } catch (e) {
        reject(String(e).replace(/^.*error: /, ""));
    }
}

$("#btn-settings").onclick = openSettings;
$("#set-cancel").onclick = closeSettings;
$("#set-close").onclick = closeSettings;
$("#set-ok").onclick = saveSettings;

const setTabs = [...document.querySelectorAll(".set-tab")];
function selectSettingsTab(tab) {
    setTabs.forEach((t) => {
        const on = t === tab;
        t.setAttribute("aria-selected", String(on));
        t.tabIndex = on ? 0 : -1;
        document.getElementById(t.getAttribute("aria-controls")).classList.toggle("hidden", !on);
    });
    $("#set-body").scrollTop = 0;
}

// The status-bar chips' jump target: the bond row flashes so the click
// visibly connects a chip to its setting (row = the toggle + its uplink
// list, see the plan's "Settings > Network, row highlighted").
function highlightBondRow() {
    const row = document.querySelector("#set-panel-network .set-status");
    const toggle = document.querySelector("#set-panel-network #set-bond");
    if (toggle) {
        toggle.closest(".set-row").style.background = "var(--teal-wash, #e8f2ee)";
        setTimeout(() => { toggle.closest(".set-row").style.background = ""; }, 2200);
    }
    if (row) {
        row.style.background = "var(--teal-wash, #e8f2ee)";
        setTimeout(() => { row.style.background = ""; }, 2200);
    }
}
setTabs.forEach((t, i) => {
    t.onclick = () => selectSettingsTab(t);
    t.onkeydown = (ev) => {
        if (ev.key !== "ArrowRight" && ev.key !== "ArrowLeft") return;
        ev.preventDefault();
        const next = setTabs[(i + (ev.key === "ArrowRight" ? 1 : setTabs.length - 1)) % setTabs.length];
        next.focus();
        selectSettingsTab(next);
    };
});
$("#set-dir-browse").onclick = async () => {
    try {
        const picked = await invokeSafe("pick_dir");
        if (picked) $("#set-dir").value = picked;
        if (picked) $("#set-dir").title = picked;
    } catch (e) {
        flashErr(String(e));
    }
};
$("#set-ext-browse").onclick = async () => {
    try {
        const picked = await invokeSafe("pick_dir");
        if (picked) {
            $("#set-ext").value = picked;
            $("#set-ext").title = picked;
            renderExtInfo();
            renderBrowserExtension();
        }
    } catch (e) {
        flashErr(String(e));
    }
};
let copyLabelTimer = null;
$("#set-ext-copy").onclick = async () => {
    const label = $("#set-ext-copy-label");
    const path = $("#set-ext-path").textContent;
    if (!path) return;
    try {
        await copyText(path);
        label.textContent = "Copied";
    } catch (e) {
        label.textContent = "Copy failed";
    }
    clearTimeout(copyLabelTimer);
    copyLabelTimer = setTimeout(() => { label.textContent = "Copy path"; }, 1500);
};
$("#set-ext-open").onclick = async () => {
    const button = $("#set-ext-open");
    const error = $("#set-err");
    error.classList.add("hidden");
    button.disabled = true;
    try {
        const result = await invokeSafe("open_browser_extensions", {
            dir: $("#set-ext").value.trim(),
            browser: $("#set-ext-browser").value,
        });
        $("#set-browser-info").textContent = result;
        $("#set-browser-info").className = "set-hint ok";
    } catch (e) {
        error.textContent = String(e).replace(/^.*error: /, "");
        error.classList.remove("hidden");
    } finally {
        button.disabled = false;
    }
};
$("#set-plugbtn").onclick = async () => {
    try {
        const picked = await invokeSafe("pick_js_file");
        if (!picked) return;
        const msg = await invokeSafe("install_plugin", { path: picked });
        flashStatus(`${msg} - now: chrome://extensions -> Reload, enable it on the Plugins page`);
    } catch (e) {
        flashErr(String(e).replace(/^.*error: /, ""));
    }
};
$("#set-restart").onclick = async () => {
    if (!confirm("Restart the background service?\nActive transfers pause and resume automatically; torrent sessions are rebuilt.")) return;
    try {
        await invokeSafe("restart_daemon");
        flashStatus("background service restarted");
        await poll();
    } catch (e) {
        flashErr(String(e));
    }
};
$("#set-backdrop").onclick = (ev) => { if (ev.target === ev.currentTarget) closeSettings(); };
// Enter saves from a field only; on a button it keeps its click meaning.
$("#set-backdrop").onkeydown = (ev) => { if (ev.key === "Enter" && ev.target.tagName === "INPUT") saveSettings(); };
$("#set-ytdlp").oninput = () => { $("#set-ytdlp").title = $("#set-ytdlp").value; };
$("#set-dir").oninput = () => { $("#set-dir").title = $("#set-dir").value; };
$("#set-ext").oninput = () => {
    $("#set-ext").title = $("#set-ext").value;
    renderExtInfo();
    renderBrowserExtension();
};

// True while any dialog is open: global accelerators shouldn't fire into
// them (Ctrl+N while typing in the Add dialog would wipe the typed URL).
function dialogOpen() {
    return ["#dlg-backdrop", "#bdlg-backdrop", "#set-backdrop", "#tdg-backdrop", "#hdt-backdrop", "#rdlg-backdrop", "#abt-backdrop", "#lan-backdrop", "#apdlg-backdrop"]
        .some((s) => !$(s).classList.contains("hidden"));
}

/* ------------------------------------------------------------------ */
/* Send / receive on the local network                                 */
/* ------------------------------------------------------------------ */
let lanInfo = null;
let lanTimer = null;

function lanErr(msg) {
    const el = $("#lan-err");
    if (!msg) {
        el.classList.add("hidden");
        el.textContent = "";
        return;
    }
    el.textContent = String(msg).replace(/^.*error: /, "");
    el.classList.remove("hidden");
}

function lanJoin(dir, name) {
    const sep = dir.includes("\\") ? "\\" : "/";
    return dir.endsWith(sep) ? dir + name : dir + sep + name;
}

function applyLanInfo(info) {
    lanInfo = info;
    $("#lan-on").checked = !!info.enabled;
    $("#lan-open").checked = !!info.open;
    $("#lan-url").textContent = info.running ? info.url : "(not listening)";
    $("#lan-code").textContent = info.code || "------";
    $("#lan-dir").textContent = info.dir || "";
    const st = $("#lan-status");
    if (!info.enabled) {
        st.className = "set-hint warn";
        st.textContent = "Receiving is off. Turn it on to accept files from other devices.";
    } else if (!info.running) {
        st.className = "set-hint warn";
        st.textContent = `Receiving is on but nothing is listening on port ${info.port} (is another app using it?)`;
    } else if (info.open) {
        st.className = "set-hint ok";
        st.textContent = `Listening on port ${info.port}. Anyone on this network can send files.`;
    } else {
        st.className = "set-hint ok";
        st.textContent = `Listening on port ${info.port}. The code is required to send or list files.`;
    }
    $("#lan-tip").textContent = info.running
        ? "If another device cannot connect, allow ripdl through the firewall for private networks."
        : "";
    const box = $("#lan-files");
    if (!info.files || !info.files.length) {
        box.innerHTML = '<div class="lan-hint">nothing received yet</div>';
    } else {
        box.innerHTML = info.files
            .map(
                (f) => `<div class="lan-file">
                  <span class="lan-name" title="${escapeHtml(f.name)}">${escapeHtml(f.name)}</span>
                  ${f.partial ? '<span class="lan-hint">receiving&hellip;</span>' : ""}
                  <span class="lan-size">${humans(f.size)}</span>
                  ${
                      f.partial
                          ? ""
                          : `<button data-open="${escapeHtml(f.name)}"><svg class="ico"><use href="#i-file"/></svg>Open</button>`
                  }
                </div>`
            )
            .join("");
        box.querySelectorAll("button[data-open]").forEach((b) => {
            b.onclick = () => {
                invokeSafe("open_path", { path: lanJoin(info.dir, b.dataset.open) }).catch((e) => lanErr(e));
            };
        });
    }
}

async function refreshLan() {
    try {
        applyLanInfo(await invokeSafe("lan_info"));
        lanErr(null);
    } catch (e) {
        lanErr(e);
    }
}

async function findLanPeers() {
    const box = $("#lan-peers");
    box.innerHTML = '<div class="lan-hint">Looking for ripdl on this network&hellip;</div>';
    try {
        const peers = await invokeSafe("lan_peers");
        if (!peers.length) {
            box.innerHTML =
                '<div class="lan-hint">Nobody answered. Start ripdl on the other device, turn receiving on there, and check that both are on the same network.</div>';
            return;
        }
        box.innerHTML = peers
            .map(
                (p, i) => `<div class="lan-peer" data-i="${i}" title="${escapeHtml(p.url)}">
                  <span class="lan-name">${escapeHtml(p.name)}</span>
                  <span class="lan-url">${escapeHtml(p.url)}</span>
                  <span class="lan-hint">${p.open ? "no code" : "code needed"}</span>
                </div>`
            )
            .join("");
        box.querySelectorAll(".lan-peer").forEach((el) => {
            el.onclick = () => {
                const p = peers[Number(el.dataset.i)];
                $("#lan-to").value = p.url;
                if (p.open) $("#lan-send-code").value = "";
                $("#lan-send-code").focus();
            };
        });
    } catch (e) {
        box.innerHTML = `<div class="lan-hint">discovery failed: ${escapeHtml(String(e))}</div>`;
    }
}

async function refreshLanSends() {
    const box = $("#lan-sends");
    let sends = [];
    try {
        sends = await invokeSafe("lan_sends");
    } catch (_) {
        return;
    }
    if (!sends.length) {
        box.innerHTML = '<div class="lan-hint">no outgoing copies</div>';
        return;
    }
    box.innerHTML = sends
        .map((s) => {
            const pct = s.total > 0 ? Math.min(100, Math.round((s.sent / s.total) * 1000) / 10) : 0;
            const failed = !!s.error;
            const detail = failed
                ? escapeHtml(String(s.error))
                : s.done
                  ? `sent to ${escapeHtml(s.peer)}`
                  : `${humans(s.sent)} / ${humans(s.total)} \u00b7 ${escapeHtml(s.peer)}`;
            return `<div class="lan-send${failed ? " err" : ""}" title="${escapeHtml(s.path)}">
              <span class="lan-name">${escapeHtml(s.name)}</span>
              <span class="lan-bar"><i style="width:${s.done && !failed ? 100 : pct}%"></i></span>
              <span class="lan-size">${failed ? "failed" : s.done ? "sent" : pct + "%"}</span>
            </div>
            <div class="lan-hint">${detail}</div>`;
        })
        .join("");
}

function lanLoop() {
    clearTimeout(lanTimer);
    lanTimer = setTimeout(async () => {
        if ($("#lan-backdrop").classList.contains("hidden")) return;
        await refreshLanSends();
        lanLoop();
    }, 800);
}

async function openLan() {
    lanErr(null);
    $("#lan-backdrop").classList.remove("hidden");
    await refreshLan();
    refreshLanSends();
    findLanPeers();
    lanLoop();
}

function closeLan() {
    clearTimeout(lanTimer);
    $("#lan-backdrop").classList.add("hidden");
}

$("#btn-lan").onclick = openLan;
$("#lan-close").onclick = closeLan;
$("#lan-done").onclick = closeLan;
$("#lan-backdrop").onclick = (ev) => {
    if (ev.target === ev.currentTarget) closeLan();
};
$("#lan-find").onclick = findLanPeers;
$("#lan-on").onchange = async () => {
    const on = $("#lan-on").checked;
    try {
        applyLanInfo(await invokeSafe("lan_configure", { enabled: on }));
        flashStatus(on ? "receiving files on this network" : "LAN receiving off");
    } catch (e) {
        $("#lan-on").checked = !on;
        lanErr(e);
    }
};
$("#lan-open").onchange = async () => {
    const open = $("#lan-open").checked;
    if (open && !confirm("Accept files from any device on this network without the code?")) {
        $("#lan-open").checked = false;
        return;
    }
    try {
        applyLanInfo(await invokeSafe("lan_configure", { open }));
    } catch (e) {
        $("#lan-open").checked = !open;
        lanErr(e);
    }
};
$("#lan-newcode").onclick = async () => {
    try {
        applyLanInfo(await invokeSafe("lan_new_code"));
        flashStatus("new pairing code generated");
    } catch (e) {
        lanErr(e);
    }
};
$("#lan-copy").onclick = () => {
    const text = lanInfo && lanInfo.url ? lanInfo.url : "";
    if (!text) return;
    copyText(text)
        .then(() => flashStatus("address copied"))
        .catch(() => {});
};
$("#lan-open-dir").onclick = () => {
    if (lanInfo && lanInfo.dir) {
        invokeSafe("open_path", { path: lanInfo.dir }).catch((e) => lanErr(e));
    }
};
$("#lan-pick").onclick = async () => {
    lanErr(null);
    let to = $("#lan-to").value.trim();
    if (!to) return lanErr("pick a device above, or type its address");
    if (!/^https?:\/\//i.test(to)) to = "http://" + to;
    let path = null;
    try {
        path = await invokeSafe("pick_any_file");
    } catch (e) {
        return lanErr(e);
    }
    if (!path) return;
    try {
        await invokeSafe("lan_send", { url: to, code: $("#lan-send-code").value.trim(), path });
        $("#lan-send-code").value = "";
        await refreshLanSends();
    } catch (e) {
        lanErr(e);
    }
};

document.addEventListener("keydown", (ev) => {
    if (ev.key === "Escape") {
        $("#ctx").classList.add("hidden");
        $("#dlg-backdrop").classList.add("hidden");
        torrentPick = null;
        closeBundleDialog();
        closeDetails();
        closeHttpDetails();
        closeRefreshDialog();
        closeSettings();
        closeAbout();
        closeLan();
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
    if ((ev.ctrlKey || ev.metaKey) && ev.key.toLowerCase() === "b") {
        ev.preventDefault();
        openBundleDialog();
    }
    // Task-list keyboard: move the selection, inspect, remove. Only when no
    // text field has focus, so typing in the toolbar/filter still works.
    const typing = /^(INPUT|TEXTAREA|SELECT)$/.test(document.activeElement && document.activeElement.tagName);
    if (typing) return;
    if ((ev.ctrlKey || ev.metaKey) && ev.key.toLowerCase() === "a") {
        ev.preventDefault();
        const all = visibleTasks().map((t) => t.id);
        if (all.length) setSelection(all, selectedId || all[0]);
    } else if (ev.key === "ArrowDown" || ev.key === "ArrowUp") {
        const view = visibleTasks();
        if (!view.length) return;
        ev.preventDefault();
        const i = view.findIndex((t) => t.id === selectedId);
        const next = ev.key === "ArrowDown"
            ? Math.min(view.length - 1, i + 1)
            : Math.max(0, i < 0 ? 0 : i - 1);
        if (ev.shiftKey && selectedIds.size >= 1) {
            if (!anchorId) anchorId = selectedId;
            extendSelection(view[next].id);
        } else {
            selectRow(view[next].id);
        }
    } else if (ev.key === "Enter" && selectedId) {
        ev.preventDefault();
        openDetails(selectedId);
    } else if (ev.key === "Delete" && selectedId) {
        ev.preventDefault();
        doTaskAction("remove", selectedId);
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
    if (!t) return;
    if (t.kind === "torrent") return openTorrentDetails(id);
    return openHttpDetails(id);
}

async function openTorrentDetails(id) {
    const t = findTask(id);
    if (!t || t.kind !== "torrent") return;
    closeHttpDetails();
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
        filesRenderKey = ""; // next success must repaint, not patch a missing table
        lastDetails = null;
        return;
    } finally {
        detailsRefreshBusy = false;
    }
    const name = (t && t.name) || d.name || "(unknown)";
    $("#tdg-name").textContent = name;
    const pct = d.total > 0 ? ((d.downloaded / d.total) * 100).toFixed(1) : "0";
    const ratio = d.downloaded > 0 ? d.uploaded / d.downloaded : 0;
    const upRate = d.upload_speed_bps > 0 ? `${humans(d.upload_speed_bps)}/s` : "0 B/s";
    const phase = d.paused ? "paused" : d.finished ? (d.seeding ? "seeding" : "finished") : "running";
    $("#tdg-summary").textContent =
        `${pct}% / ${humans(d.downloaded)} / ${humans(d.total)} / ${phase}` +
        (t && t.sequential ? " / sequential" : "") +
        `\n\u2191 ${humans(d.uploaded)} (ratio ${ratio.toFixed(2)}) \u00b7 ${d.peers_connected} peers \u00b7 ${upRate}` +
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

/* HTTP / video task details dialog */
let httpDetailsId = null;
let httpDetailsTimer = null;
let httpDetailsBusy = false;
// Last task_details snapshot, so per-part actions (refresh) can prefill the
// part's current address without a second round-trip.
let httpDetailsData = null;
// JS cannot represent u64::MAX exactly; no real byte-range end comes close.
const UNBOUNDED_END = Number.MAX_SAFE_INTEGER;

async function openHttpDetails(id) {
    const t = findTask(id);
    if (!t) return;
    closeDetails(); // the torrent dialog and this one never stack
    httpDetailsId = id;
    partsRenderKey = "";
    $("#hdt-name").textContent = rowName(t);
    $("#hdt-body").innerHTML = "";
    $("#hdt-summary").textContent = "";
    $("#hdt-backdrop").classList.remove("hidden");
    await refreshHttpDetails();
    clearInterval(httpDetailsTimer);
    httpDetailsTimer = setInterval(() => {
        if (!httpDetailsId || $("#hdt-backdrop").classList.contains("hidden")) return;
        if (!findTask(httpDetailsId)) { closeHttpDetails(); return; }
        refreshHttpDetails();
    }, 1000);
}

function closeHttpDetails() {
    httpDetailsId = null;
    httpDetailsData = null;
    clearInterval(httpDetailsTimer);
    $("#hdt-backdrop").classList.add("hidden");
}

async function refreshHttpDetails() {
    if (!httpDetailsId || httpDetailsBusy) return;
    httpDetailsBusy = true;
    try {
        const d = await invokeSafe("task_details", { id: httpDetailsId });
        httpDetailsData = d;
        renderHttpDetails(d);
    } catch (e) {
        $("#hdt-body").innerHTML = `<div class="trk-off">${escapeHtml(String(e).replace(/^.*error: /, ""))}</div>`;
    } finally {
        httpDetailsBusy = false;
    }
}

// Address, output file, resume identity and the engine's byte-range map.
// Everything comes from the sidecar the engine resumes from, so what the
// dialog shows is exactly what a resume would do.
function renderHttpDetails(d) {
    const t = findTask(d.id);
    $("#hdt-name").textContent = t ? rowName(t) : urlName(d.url);
    const pct = d.percent != null ? d.percent : (d.total ? (d.downloaded / d.total) * 100 : null);
    const kind = d.kind === "video" ? "yt-dlp stream" : d.kind === "bundle" ? "multi-part bundle" : "HTTP ranges";
    $("#hdt-summary").textContent =
        `${kind} \u00b7 ${t ? stateLabel(t) : d.state}` +
        `\n${humans(d.downloaded)}${d.total ? " / " + humans(d.total) : ""}` +
        `${pct != null ? ` (${pct.toFixed(1)}%)` : ""}`;

    const meta = [`<tr><th>Address</th><td class="hdt-url">${escapeHtml(d.url)}</td></tr>`];
    if (d.dest_file) {
        meta.push(`<tr><th>File</th><td class="hdt-url">${escapeHtml(d.dest_file)}</td></tr>`);
    } else if (t && t.dest_dir) {
        meta.push(`<tr><th>Folder</th><td class="hdt-url">${escapeHtml(t.dest_dir)}</td></tr>`);
    }
    if (d.etag) meta.push(`<tr><th>ETag</th><td>${escapeHtml(d.etag)}</td></tr>`);
    if (d.error) {
        const hint = errorHint(d.error);
        meta.push(
            `<tr><th>Last error</th><td class="hdt-err">${escapeHtml(d.error)}` +
            `${hint ? `<div class="trk-off">${escapeHtml(hint)}</div>` : ""}</td></tr>`
        );
    }
    let html = `<table>${meta.join("")}</table>`;

    // Bundle parts: the row is an aggregate, this is the itemized view, with
    // the part numbers the selection's own numbering proved (gaps included).
    // The table lives in its own container and re-renders only when a part's
    // structure changes; volatile cells (bar, percent, bytes) are patched in
    // place, so the per-part Refresh buttons never race the 1 s rebuild the
    // way a full innerHTML swap would (same trap as the Files-tab checkboxes).
    const parts = d.parts || [];
    html += `<div id="hdt-parts"></div>`;

    const segs = d.segments || [];
    let segHtml = "";
    if (segs.length) {
        const finished = segs.filter((s) => s.end > UNBOUNDED_END || s.done >= s.end - s.start + 1).length;
        segHtml += `<div class="hdt-segs-title">${segs.length} segment${segs.length === 1 ? "" : "s"} \u00b7 ${finished} finished</div>`;
        segHtml += `<table><tr><th>#</th><th>Byte range</th><th>Progress</th><th class="r">Durable</th></tr>`;
        for (const s of segs) {
            const open = s.end > UNBOUNDED_END;
            const span = open ? 0 : s.end - s.start + 1;
            const p = open || !span ? null : Math.min(100, (s.done / span) * 100);
            const range = open ? `${humans(s.start)} \u2013 end` : `${humans(s.start)} \u2013 ${humans(s.end)}`;
            const bar = p == null
                ? `<span class="trk-off">single stream</span>`
                : `<div class="hdt-bar"><i class="${p >= 100 ? "full" : ""}" style="width:${p.toFixed(1)}%"></i></div>` +
                  `<span class="hdt-pct">${p.toFixed(0)}%</span>`;
            segHtml += `<tr>
              <td>${s.index}</td>
              <td class="hdt-range">${range}</td>
              <td>${bar}</td>
              <td class="r">${humans(s.done)}${open || !span ? "" : " / " + humans(span)}</td>
            </tr>`;
        }
        segHtml += `</table>`;
    } else if (d.kind === "http") {
        segHtml += d.state === "completed"
            ? `<div class="trk-off">Finished - the resume sidecar is removed once a download completes.</div>`
            : `<div class="trk-off">No byte-range plan yet - the engine writes it once the download starts.</div>`;
    }
    html += `<div id="hdt-segs">${segHtml}</div>`;
    $("#hdt-body").innerHTML = html;
    renderPartsTable(parts, d.id);

    $("#hdt-refresh").classList.toggle("hidden", d.kind !== "http" || d.state === "completed");
    $("#hdt-addparts").classList.toggle("hidden", d.kind !== "bundle");
    // A bundle's dest_file is its folder, so "Open file" gives way to
    // "Open folder" (which reveals the folder itself).
    $("#hdt-open").classList.toggle("hidden", d.kind === "bundle");
    // A partial file is not worth launching; reveal it in Explorer instead.
    $("#hdt-open").disabled = d.state !== "completed" || !d.dest_file;
    $("#hdt-folder").disabled = !(d.dest_file || (t && t.dest_dir));
}

// The itemized bundle-parts view: full render on structure change (part set,
// states, errors), byte-level patching otherwise. `head` rides in the key so
// the "missing parts" line tracks the daemon's numbering without a repaint.
let partsRenderKey = "";

function renderPartsTable(parts, id) {
    const host = $("#hdt-parts");
    if (!host) return;
    if (!parts.length) { partsRenderKey = ""; host.innerHTML = ""; return; }
    const done = parts.filter((p) => p.state === "completed").length;
    const t0 = findTask(id);
    // The daemon's numbering is the authority on the total (a selection
    // starting at part 5 proves no denominator, so it stays 0 = unknown).
    const total = t0 && t0.parts_total ? t0.parts_total : parts.length;
    let head = `${done} of ${total} part${total === 1 ? "" : "s"}`;
    if (t0 && t0.parts_missing && t0.parts_missing.length) {
        head += ` \u00b7 missing ${t0.parts_missing.join(", ")}`;
    }
    const key = head + "|" + parts.map((p) => `${p.seq}:${p.state}:${p.error ? 1 : 0}:${p.filename || ""}`).join(",");
    if (key !== partsRenderKey || !host.querySelector("table")) {
        partsRenderKey = key;
        let html = `<div class="hdt-segs-title">${head}</div>`;
        html += `<table><tr><th class="r">#</th><th>Part</th><th>Progress</th><th class="r">Bytes</th><th></th></tr>`;
        for (const p of parts) {
            // Finished parts own their bytes; every other part can swap its
            // address for a fresh link without losing what is on disk.
            const refresh = p.state !== "completed" && p.seq != null
                ? `<button class="hdt-part-refresh" data-seq="${p.seq}" title="Refresh this part's address (expired link, same file)"><svg class="ico"><use href="#i-globe"/></svg></button>`
                : "";
            const err = p.error ? `<div class="trk-off">${escapeHtml(p.error)}</div>` : "";
            html += `<tr data-seq="${p.seq != null ? p.seq : ""}">
              <td class="r">${p.seq != null ? p.seq : ""}</td>
              <td class="hdt-range" title="${escapeHtml(p.url || "")}">${escapeHtml(p.filename || urlName(p.url))}${err}</td>
              <td class="hdt-part-prog"></td>
              <td class="r hdt-part-bytes"></td>
              <td class="hdt-part-act">${refresh}</td>
            </tr>`;
        }
        html += `</table>`;
        host.innerHTML = html;
    }
    for (const p of parts) {
        const row = host.querySelector(`tr[data-seq="${p.seq}"]`);
        if (!row) continue;
        const partPct = p.total ? Math.min(100, (p.downloaded / p.total) * 100) : null;
        const bar = partPct == null
            ? `<span class="trk-off">${escapeHtml(p.state || "queued")}</span>`
            : `<div class="hdt-bar"><i class="${partPct >= 100 ? "full" : ""}" style="width:${partPct.toFixed(1)}%"></i></div>` +
              `<span class="hdt-pct">${partPct.toFixed(0)}%</span>`;
        row.querySelector(".hdt-part-prog").innerHTML = bar;
        row.querySelector(".hdt-part-bytes").textContent =
            `${humans(p.downloaded)}${p.total ? " / " + humans(p.total) : ""}`;
    }
}

$("#hdt-close").onclick = closeHttpDetails;
$("#hdt-backdrop").onclick = (ev) => { if (ev.target === ev.currentTarget) closeHttpDetails(); };
$("#hdt-refresh").onclick = () => { if (httpDetailsId) openRefreshDialog(httpDetailsId); };
$("#hdt-addparts").onclick = () => { if (httpDetailsId) openAddPartsDialog(); };
// Per-part refresh buttons live inside a table that is patched in place, so
// the listener is delegated and survives re-renders.
$("#hdt-body").addEventListener("click", (ev) => {
    const btn = ev.target.closest(".hdt-part-refresh");
    if (btn && httpDetailsId) openRefreshDialog(httpDetailsId, Number(btn.dataset.seq));
});
$("#hdt-copy").onclick = async () => {
    try {
        const d = await invokeSafe("task_details", { id: httpDetailsId });
        if (!d.url) throw new Error("no address");
        await copyText(d.url);
        flashStatus("address copied");
    } catch (e) {
        flashErr("clipboard unavailable");
    }
};
$("#hdt-open").onclick = async () => {
    try {
        const d = await invokeSafe("task_details", { id: httpDetailsId });
        if (d.dest_file) await invokeSafe("open_path", { path: d.dest_file, reveal: null });
    } catch (e) {
        flashErr(String(e).replace(/^.*error: /, ""));
    }
};
$("#hdt-folder").onclick = async () => {
    try {
        const d = await invokeSafe("task_details", { id: httpDetailsId });
        const t = findTask(httpDetailsId);
        if (d.dest_file) {
            const dir = d.dest_file.replace(/[^\\/]+$/, "");
            await invokeSafe("open_path", { path: dir, reveal: baseName(d.dest_file) });
        } else if (t && t.dest_dir) {
            await invokeSafe("open_path", { path: t.dest_dir, reveal: null });
        }
    } catch (e) {
        flashErr(String(e).replace(/^.*error: /, ""));
    }
};

/* Refresh download address (expired signed links, same file).
 * For bundles the dialog targets ONE part: the bundle pauses for the swap
 * and resumes, finished parts stay finished.
 *
 * The dialog validates before it commits: 400 ms after open or edit it runs
 * the daemon's probe in `probe_only` (read-only, nothing pauses or swaps)
 * and shows the verdict inline - "Same file - keeps 5.4 GB of 50 GB" or the
 * precise rejection. The swap button stays disabled until the check passes,
 * so a bad paste never reaches the daemon as a real swap. */
let refreshTargetId = null;
let refreshPartSeq = null;
let refreshCheckTimer = null;

function openRefreshDialog(id, seq = null) {
    const t = findTask(id);
    if (!t) return;
    if (seq == null) {
        if (t.kind !== "http") { flashErr("Only HTTP downloads can refresh their address."); return; }
        if (t.state === "completed") { flashErr("This download already finished."); return; }
    }
    refreshTargetId = id;
    refreshPartSeq = seq;
    $("#rdlg-title").textContent = seq != null ? `Refresh part ${seq}` : "Refresh download address";
    const prefill = seq != null ? partUrl(id, seq) : null;
    $("#rdlg-url").value = prefill != null ? prefill : (t.url || "");
    $("#rdlg-hint").textContent =
        (seq != null
            ? "Paste a fresh link for this part's file. The bundle pauses for the swap and resumes; parts that already finished stay finished."
            : "Paste a fresh link for the same file (signed URLs expire). ripdl keeps the bytes already on disk and continues from there; the new link must serve the same file name and size and support byte ranges.") +
        (["running", "queued"].includes(t.state) ? " The download is paused first." : "");
    $("#rdlg-err").classList.add("hidden");
    $("#rdlg-err").textContent = "";
    refreshVerdict(null);
    $("#rdlg-backdrop").classList.remove("hidden");
    const field = $("#rdlg-url");
    field.focus();
    field.setSelectionRange(field.value.length, field.value.length);
    // Diagnose the address that is already in the box: for an expired task
    // this is the answer to "what is wrong" before anything is pasted.
    scheduleRefreshCheck(400);
}

// The part's current address, from the details the dialog is showing.
function partUrl(id, seq) {
    if (httpDetailsId === id && httpDetailsData) {
        const p = (httpDetailsData.parts || []).find((p) => p.seq === seq);
        if (p && p.url) return p.url;
    }
    return null;
}

function closeRefreshDialog() {
    refreshTargetId = null;
    refreshPartSeq = null;
    if (refreshCheckTimer) { clearTimeout(refreshCheckTimer); refreshCheckTimer = null; }
    $("#rdlg-backdrop").classList.add("hidden");
}

// Inline verdict under the URL box: null hides, "ok"/"warn"/"err"/"checking"
// recolors. The swap button follows the verdict (enabled on ok/warn only).
function refreshVerdict(kind, msg) {
    const el = $("#rdlg-verdict");
    if (kind == null) { el.className = "rdlg-verdict hidden"; }
    else { el.className = `rdlg-verdict ${kind}`; el.textContent = msg; }
    $("#rdlg-ok").disabled = kind == null || kind === "checking" || kind === "err";
}

function scheduleRefreshCheck(delay = 500) {
    if (refreshCheckTimer) clearTimeout(refreshCheckTimer);
    refreshCheckTimer = setTimeout(runRefreshCheck, delay);
}

async function runRefreshCheck() {
    refreshCheckTimer = null;
    const id = refreshTargetId;
    if (!id) return;
    const url = $("#rdlg-url").value.trim();
    if (!url) { refreshVerdict(null); return; }
    refreshVerdict("checking", "Checking the new address...");
    try {
        const args = { id, url, probeOnly: true };
        if (refreshPartSeq != null) args.part = refreshPartSeq;
        const out = await invokeSafe("refresh_url", args);
        if (refreshTargetId !== id || $("#rdlg-url").value.trim() !== url) return; // moved on meanwhile
        const kept = out && out.kept ? humans(out.kept) : null;
        const total = out && out.total ? humans(out.total) : null;
        const have = kept ? `keeps ${kept}${total ? " of " + total : ""} already on disk` : "ready to download";
        if (out && out.warning) refreshVerdict("warn", `Same file, but ${out.warning} (${have}).`);
        else refreshVerdict("ok", `Same file - ${have}.`);
    } catch (e) {
        if (refreshTargetId !== id || $("#rdlg-url").value.trim() !== url) return;
        refreshVerdict("err", String(e).replace(/^.*error: /, ""));
    }
}

function refreshError(msg) {
    const el = $("#rdlg-err");
    el.textContent = msg;
    el.classList.remove("hidden");
}

async function submitRefresh() {
    if (!refreshTargetId || $("#rdlg-ok").disabled) return;
    const id = refreshTargetId;
    const url = $("#rdlg-url").value.trim();
    if (!url) { refreshError("Paste the fresh URL first."); return; }
    const btn = $("#rdlg-ok");
    btn.disabled = true;
    try {
        const args = { id, url };
        if (refreshPartSeq != null) args.part = refreshPartSeq;
        const out = await invokeSafe("refresh_url", args);
        const kept = out && out.kept != null
            ? ` (kept ${humans(out.kept)}${out.total ? " of " + humans(out.total) : ""})`
            : "";
        const seq = refreshPartSeq;
        closeRefreshDialog();
        flashStatus(seq != null
            ? `part ${seq} address refreshed${kept} - resuming`
            : `address refreshed${kept} - resuming`);
        await poll();
        if (httpDetailsId === id) await refreshHttpDetails();
    } catch (e) {
        refreshError(String(e).replace(/^.*error: /, ""));
        $("#rdlg-ok").disabled = false;
    }
}

$("#rdlg-cancel").onclick = closeRefreshDialog;
$("#rdlg-ok").onclick = submitRefresh;
$("#rdlg-url").addEventListener("input", () => scheduleRefreshCheck());
$("#rdlg-backdrop").onclick = (ev) => { if (ev.target === ev.currentTarget) closeRefreshDialog(); };
$("#rdlg-url").onkeydown = (ev) => {
    // A URL has no newlines: Enter submits, Shift+Enter still breaks the line.
    if (ev.key === "Enter" && !ev.shiftKey) { ev.preventDefault(); submitRefresh(); }
};

/* Add missing parts to an open bundle ("I forgot part 7"). */
function openAddPartsDialog() {
    if (!httpDetailsId) return;
    $("#apdlg-urls").value = "";
    $("#apdlg-err").classList.add("hidden");
    $("#apdlg-err").textContent = "";
    $("#apdlg-backdrop").classList.remove("hidden");
    $("#apdlg-urls").focus();
}

function closeAddPartsDialog() {
    $("#apdlg-backdrop").classList.add("hidden");
}

function addPartsError(msg) {
    const el = $("#apdlg-err");
    el.textContent = msg;
    el.classList.remove("hidden");
}

async function submitAddParts() {
    if (!httpDetailsId) return;
    const id = httpDetailsId;
    const urls = $("#apdlg-urls").value.split("\n").map((l) => l.trim()).filter(Boolean);
    if (!urls.length) { addPartsError("Paste the missing part links first."); return; }
    const btn = $("#apdlg-ok");
    btn.disabled = true;
    try {
        const added = await invokeSafe("add_bundle_parts", { id, urls });
        closeAddPartsDialog();
        flashStatus(added > 0 ? `added ${added} part${added === 1 ? "" : "s"} - bundle restarting` : "nothing to add: the bundle already carries those links");
        await poll();
        if (httpDetailsId === id) await refreshHttpDetails();
    } catch (e) {
        addPartsError(String(e).replace(/^.*error: /, ""));
    } finally {
        btn.disabled = false;
    }
}

$("#apdlg-cancel").onclick = closeAddPartsDialog;
$("#apdlg-ok").onclick = submitAddParts;
$("#apdlg-backdrop").onclick = (ev) => { if (ev.target === ev.currentTarget) closeAddPartsDialog(); };
$("#apdlg-urls").onkeydown = (ev) => {
    if (ev.key === "Enter" && ev.ctrlKey) { ev.preventDefault(); submitAddParts(); }
};

setConn(false);
initColumnUi();
initWindowMemory();
initDragDrop();
poll().then(schedulePoll);
pollUplinks().then(scheduleUplinks);

/* Window size: remembered across launches (a 1100 px task list is not a
 * preference you should have to redo every morning). Maximized windows are
 * not recorded, so restoring never turns into "opens maximized by accident".
 */
function initWindowMemory() {
    const api = window.__TAURI__ && window.__TAURI__.window;
    if (!api || !api.getCurrentWindow) return;
    let win;
    try {
        win = api.getCurrentWindow();
    } catch (_) {
        return;
    }
    const saved = loadJson("ripdl.window", null);
    if (saved && Number.isFinite(saved.w) && Number.isFinite(saved.h) && saved.w > 400 && saved.h > 300) {
        try {
            const size = new window.__TAURI__.dpi.LogicalSize(saved.w, saved.h);
            win.setSize(size).catch(() => {});
        } catch (_) { /* older runtime: skip */ }
    }
    let timer = null;
    const onResize = () => {
        clearTimeout(timer);
        timer = setTimeout(async () => {
            try {
                if (await win.isMaximized()) return;
                const [size, scale] = await Promise.all([win.innerSize(), win.scaleFactor()]);
                saveJson("ripdl.window", {
                    w: Math.round(size.width / scale),
                    h: Math.round(size.height / scale),
                });
            } catch (_) { /* not every resize has a settled size */ }
        }, 500);
    };
    win.onResized(onResize).catch(() => {});
}

/* Drop a .torrent anywhere on the window: the fastest path from "downloaded a
 * torrent file" to "queued", and it reuses the add dialog (file checkboxes,
 * sequential toggle) instead of inventing a second add flow. */
function initDragDrop() {
    const api = window.__TAURI__ && window.__TAURI__.webviewWindow;
    const win = (api && api.getCurrentWebviewWindow && api.getCurrentWebviewWindow())
        || (window.__TAURI__ && window.__TAURI__.window && window.__TAURI__.window.getCurrentWindow && window.__TAURI__.window.getCurrentWindow());
    if (!win || !win.onDragDropEvent) return;
    win.onDragDropEvent((event) => {
        const p = event && event.payload;
        if (!p) return;
        if (p.type === "over" || p.type === "enter") {
            document.body.classList.add("drop-target");
            return;
        }
        document.body.classList.remove("drop-target");
        if (p.type !== "drop" || !Array.isArray(p.paths)) return;
        const torrent = p.paths.find((x) => /\.torrent$/i.test(x));
        if (torrent) {
            openAddDialog(torrent);
        } else if (p.paths.length) {
            flashStatus("drop a .torrent file (or Ctrl+T) to add BitTorrent downloads");
        }
    }).catch(() => {});
}
