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
        button.textContent = choice.label;
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
