// Model for the duplicate window (ext/duplicate.js renders it): what a row
// says and which choices it offers. Pure - no chrome.* calls - so node can test
// it: node ext/test/duplicate-model.test.mjs

/** Coarse bucket of a daemon task state. An unknown state counts as active:
 * the only safe offer for something this build does not understand is to keep
 * ripdl's download. */
export function bucketOf(task) {
    switch (task && task.state) {
        case "completed":
            return "done";
        case "paused":
            return "paused";
        case "failed":
            return "failed";
        case "cancelled":
            return "cancelled";
        default:
            return "active";
    }
}

export function isHttpUrl(url) {
    return /^https?:\/\//i.test(String(url || ""));
}

/** The choices for one existing download, primary first. `id` is what the
 * service worker does; `label` is the button text. `url` is only read when
 * there is no task (the daemon removed it between the answer and the window). */
export function actionsFor(task, url = "") {
    if (!task) {
        return isHttpUrl(url) ? [{ id: "browser", label: "Let browser download", primary: true }] : [];
    }
    const bucket = bucketOf(task);
    const direct = (task.kind || "http") === "http" && isHttpUrl(task.url);
    const hasFile = !!task.dest_file;
    const out = [];
    const add = (id, label) => out.push({ id, label, primary: false });
    if (bucket === "active") {
        add("keep", "Keep ripdl's download");
        if (hasFile) add("reveal", "Show in folder");
        if (direct) add("browser", "Let browser download");
    } else if (bucket === "done") {
        if (hasFile) add("open", "Open file");
        if (hasFile) add("reveal", "Show in folder");
        if (direct && hasFile) add("copy", "Download again");
        if (direct) add("browser", "Let browser download");
    } else if (bucket === "paused" || bucket === "failed") {
        // The daemon resumes only paused or failed tasks.
        add("resume", bucket === "failed" ? "Retry" : "Resume");
        if (hasFile) add("reveal", "Show in folder");
        if (direct) add("browser", "Let browser download");
    } else {
        // cancelled: a second copy is the only way back
        if (direct && hasFile) add("copy", "Download again");
        if (direct) add("browser", "Let browser download");
        if (hasFile) add("reveal", "Show in folder");
    }
    if (out.length) out[0].primary = true;
    return out;
}

/** The row's second line: what ripdl has, in a few words. */
export function stateLine(task) {
    if (!task) return "Already in ripdl";
    switch (bucketOf(task)) {
        case "done": {
            const size = task.total || task.downloaded;
            return size ? `Completed \u00b7 ${formatBytes(size)}` : "Completed";
        }
        case "paused":
            return `Paused${progressText(task)}`;
        case "failed":
            return task.error ? `Failed \u00b7 ${task.error}` : "Failed";
        case "cancelled":
            return "Cancelled";
        default:
            if (task.state === "queued") return "Queued";
            if (task.probing) return "Connecting\u2026";
            return `Downloading${progressText(task)}`;
    }
}

function progressText(task) {
    const pct = percentOf(task);
    const done = formatBytes(task.downloaded || 0);
    if (pct === null) return task.downloaded ? ` \u00b7 ${done}` : "";
    const of = task.total ? ` of ${formatBytes(task.total)}` : "";
    return ` \u00b7 ${pct}% \u00b7 ${done}${of}`;
}

/** Whole percent of a known total, or null when the size is unknown. */
export function percentOf(task) {
    const total = Number(task && task.total);
    if (!Number.isFinite(total) || total <= 0) return null;
    const got = Number(task.downloaded) || 0;
    return Math.max(0, Math.min(100, Math.floor((got * 100) / total)));
}

/** Binary units, one decimal under 10 of a unit, whole numbers above. */
export function formatBytes(n) {
    const value = Number(n);
    if (!Number.isFinite(value) || value <= 0) return "0 B";
    const units = ["B", "KB", "MB", "GB", "TB"];
    let size = value;
    let unit = 0;
    while (size >= 1024 && unit < units.length - 1) {
        size /= 1024;
        unit++;
    }
    const text = unit === 0 || size >= 10 ? String(Math.round(size)) : size.toFixed(1);
    return `${text} ${units[unit]}`;
}

/** The row's title: the file ripdl writes, else the browser's suggestion,
 * else the last part of the URL. */
export function displayName(task, item = {}) {
    if (task && task.dest_file) return baseName(task.dest_file);
    if (task && task.name) return task.name;
    if (item.filename) return item.filename;
    return nameFromUrl(item.url || (task && task.url) || "");
}

export function baseName(path) {
    const parts = String(path).split(/[\\/]/);
    return parts[parts.length - 1] || String(path);
}

export function nameFromUrl(url) {
    try {
        const tail = decodeURIComponent(new URL(url).pathname.split("/").pop() || "");
        return tail || String(url);
    } catch {
        return String(url || "") || "download";
    }
}

export function headline(count) {
    return count === 1 ? "ripdl already has this download" : `ripdl already has ${count} of these downloads`;
}

/** What the browser is doing with a link ripdl also has. */
export function browserNote(browser) {
    if (!browser) return "";
    return browser.taken
        ? "The browser's copy was stopped."
        : "The browser is still downloading this file. Choosing a ripdl option stops the browser's copy.";
}
