// ripdl duplicate window: one row per download ripdl already has, or a link the
// browser is downloading while ripdl has the same file. Choices run in the
// service worker (ripdl_dup_action); closing the window changes nothing. The
// job was stored in session storage before the window opened: see
// openDuplicateWindow in background.js.
import { actionsFor, browserNote, displayName, headline, stateLine } from "./duplicate-model.js";

const key = new URLSearchParams(location.search).get("key") || "";
const slot = `dup:${key}`;
const titleEl = document.getElementById("title");
const noteEl = document.getElementById("note");
const rowsEl = document.getElementById("rows");
const expiredEl = document.getElementById("expired");

document.getElementById("close").addEventListener("click", () => window.close());
document.addEventListener("keydown", onKey);
rowsEl.addEventListener("click", (event) => {
    const button = event.target instanceof Element ? event.target.closest("button[data-action]") : null;
    if (button && !button.disabled) choose(button);
});

start();

async function start() {
    const job = (await chrome.storage.session.get(slot))[slot];
    if (!job || !Array.isArray(job.items) || job.items.length === 0) {
        titleEl.textContent = "ripdl";
        expiredEl.classList.remove("hidden");
        return;
    }
    titleEl.textContent = headline(job.items.length);
    noteEl.textContent = browserNote(job.browser);
    job.items.forEach((item, index) => rowsEl.append(renderRow(item, index)));
    rowsEl.querySelector("button.primary")?.focus();
}

const SVG_NS = "http://www.w3.org/2000/svg";
const ACTION_ICONS = {
    open: [["path", { d: "M15 3h6v6" }], ["path", { d: "M10 14 21 3" }], ["path", { d: "M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6" }]],
    reveal: [["path", { d: "m6 14 1.5-2.9A2 2 0 0 1 9.24 10H20a2 2 0 0 1 1.94 2.5l-1.54 6a2 2 0 0 1-1.95 1.5H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h3.9a2 2 0 0 1 1.69.9l.81 1.2a2 2 0 0 0 1.67.9H18a2 2 0 0 1 2 2v2" }]],
    copy: [["rect", { width: 14, height: 14, x: 8, y: 8, rx: 2, ry: 2 }], ["path", { d: "M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2" }]],
    resume: [["path", { d: "M5 5a2 2 0 0 1 3.008-1.728l11.997 6.998a2 2 0 0 1 .003 3.458l-12 7A2 2 0 0 1 5 19z" }]],
    keep: [["path", { d: "M20 13c0 5-3.5 7.5-7.66 8.95a1 1 0 0 1-.67-.01C7.5 20.5 4 18 4 13V6a1 1 0 0 1 1-1c2 0 4.5-1.2 6.24-2.72a1.17 1.17 0 0 1 1.52 0C14.51 3.81 17 5 19 5a1 1 0 0 1 1 1z" }], ["path", { d: "m9 12 2 2 4-4" }]],
    browser: [["circle", { cx: 12, cy: 12, r: 10 }], ["path", { d: "M12 2a14.5 14.5 0 0 0 0 20 14.5 14.5 0 0 0 0-20" }], ["path", { d: "M2 12h20" }]],
};

function iconFor(actionId) {
    const specs = ACTION_ICONS[actionId];
    if (!specs) return null;
    const svg = document.createElementNS(SVG_NS, "svg");
    svg.setAttribute("class", "icon");
    svg.setAttribute("viewBox", "0 0 24 24");
    svg.setAttribute("width", "13");
    svg.setAttribute("height", "13");
    svg.setAttribute("fill", "none");
    svg.setAttribute("stroke", "currentColor");
    svg.setAttribute("stroke-width", "2");
    svg.setAttribute("stroke-linecap", "round");
    svg.setAttribute("stroke-linejoin", "round");
    svg.setAttribute("aria-hidden", "true");
    for (const [tag, attrs] of specs) {
        const node = document.createElementNS(SVG_NS, tag);
        for (const [k, v] of Object.entries(attrs)) node.setAttribute(k, String(v));
        svg.append(node);
    }
    return svg;
}

function renderRow(item, index) {
    const li = document.createElement("li");
    li.className = "row";
    li.dataset.index = String(index);
    const title = displayName(item.task, item);
    const name = text("div", "name", title);
    name.title = title;
    const url = text("div", "url", item.url);
    url.title = item.url;
    const actions = document.createElement("div");
    actions.className = "actions";
    const list = actionsFor(item.task, item.url);
    for (const choice of list) {
        const button = document.createElement("button");
        button.type = "button";
        button.className = choice.primary ? "action primary" : "action";
        const ic = iconFor(choice.id);
        if (ic) button.append(ic);
        button.append(document.createTextNode(choice.label));
        button.dataset.action = choice.id;
        actions.append(button);
    }
    if (!list.length) actions.append(text("div", "none", "No choice is available for this download."));
    li.append(name, text("div", "state", stateLine(item.task)), url, actions, text("div", "result", ""));
    return li;
}

function text(tag, cls, value) {
    const node = document.createElement(tag);
    node.className = cls;
    node.textContent = value;
    return node;
}

/** Runs one choice in the service worker and shows its answer on the row. */
async function choose(button) {
    const row = button.closest(".row");
    setBusy(row, true);
    let reply;
    try {
        reply = await chrome.runtime.sendMessage({
            type: "ripdl_dup_action",
            key,
            index: Number(row.dataset.index),
            action: button.dataset.action,
        });
    } catch (err) {
        reply = { ok: false, error: String(err && err.message ? err.message : err) };
    }
    if (reply && reply.ok) {
        row.classList.add("done");
        showResult(row, reply.message || "Done.", false);
        if (!rowsEl.querySelector(".row:not(.done)")) setTimeout(() => window.close(), 1200);
        return;
    }
    showResult(row, (reply && reply.error) || "ripdl did not answer.", true);
    setBusy(row, false);
}

function setBusy(row, busy) {
    row.querySelectorAll("button").forEach((b) => { b.disabled = busy; });
}

function showResult(row, message, bad) {
    const out = row.querySelector(".result");
    out.textContent = message;
    out.classList.toggle("bad", bad);
}

function onKey(event) {
    if (event.key === "Escape") {
        window.close();
        return;
    }
    // Enter with focus on a button clicks it already; with focus elsewhere it
    // runs the first pending primary choice.
    if (event.key === "Enter" && !(document.activeElement instanceof HTMLButtonElement)) {
        const first = rowsEl.querySelector(".row:not(.done) button.primary:not(:disabled)");
        if (first) {
            event.preventDefault();
            choose(first);
        }
    }
}
