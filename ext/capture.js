/**
 * ripdl copy capture - the extension's only resident page code.
 *
 * Contract (same spirit as background.js: the extension makes ripdl better,
 * never a watcher):
 *   1. It looks at the selection ONLY when the user copies, and only forwards
 *      http(s) links the selection actually contains. No page content, no
 *      history, no telemetry, nothing sent anywhere but the service worker.
 *   2. The clipboard is never read back and never written; the copy event is
 *      never cancelled or modified.
 *   3. Nothing is queued until the user clicks Send. Dismiss means dismissed.
 *   4. The offer chip lives in a shadow root: page CSS cannot break it, and it
 *      cannot leak styles back into the page.
 *
 * The service worker does the grouping (daemon-side `preview_bundle`), so the
 * same rules apply here, in the GUI and on the command line.
 */
(() => {
    "use strict";
    if (window.top !== window) return; // a selection belongs to the top frame

    const MAX_LINKS = 100;
    const AUTO_DISMISS_MS = 30000;
    const SLOW_MS = 200; // a reply slower than this shows a progress line
    const URL_RE = /https?:\/\/[^\s<>"'`]+/gi;
    /** Anchor text that names a file, never a command ("Download", "here"). */
    const FILENAME_RE = /^[^\\/:*?"<>|\r\n]{1,120}\.[A-Za-z0-9]{1,8}$/;

    let host = null; // <ripdl-offer-chip> element
    let root = null; // its shadow root
    let offer = null; // { groups, bundled }
    let picks = null; // edit-mode rows: [{ group, on, name }]
    let editing = false;
    let anchor = null; // where the selection was, in viewport coords
    let timer = 0;
    let offerSeq = 0; // a newer copy or status supersedes pending replies
    let loading = false; // the chip is showing "Reading links" right now

    document.addEventListener("copy", onCopy, true);

    /* --- selection -> links ------------------------------------------------ */

    function onCopy(ev) {
        if (host && ev.composedPath && ev.composedPath().includes(host)) return;
        const target = ev.target;
        const field = target && (target.tagName === "INPUT" || target.tagName === "TEXTAREA")
            && typeof target.selectionStart === "number" ? target : null;
        const sel = window.getSelection();
        const text = field ? String(field.value).slice(field.selectionStart, field.selectionEnd) : String(sel || "");
        const links = collect(field ? null : sel, text);
        if (!links.length) return;
        rememberAnchor(field, sel);
        const seq = ++offerSeq;
        // "Reading" appears only when the answer is slow; most copies are instant.
        const slow = setTimeout(() => {
            if (seq !== offerSeq) return;
            loading = true;
            showStatus(`Reading ${links.length} link${links.length === 1 ? "" : "s"}\u2026`, "", false);
        }, SLOW_MS);
        chrome.runtime.sendMessage({ type: "ripdl_offer", links, page: location.href }, (resp) => {
            clearTimeout(slow);
            if (seq !== offerSeq) return;
            // No daemon, no chip: silence beats an error bubble on a page.
            if (chrome.runtime.lastError || !resp || !resp.groups || !resp.groups.length) {
                if (loading) dismiss();
                return;
            }
            offer = resp;
            editing = false;
            picks = null;
            loading = false;
            render();
            arm();
        });
    }

    /** Links from a copied selection, strongest first (a URL keeps its best tier):
     *   1. anchors whose text names a file, and every anchor when the selected
     *      text itself carries http(s) URLs (a pasted list of parts);
     *   2. bare URLs in the copied text;
     *   3. other anchors, marked weak. The service worker keeps a weak link only
     *      when a plugin or the file-type list claims its URL, so a "Download"
     *      button for a part is offered and a prose link is not. */
    function collect(sel, text) {
        const out = [];
        const seen = new Set();
        const push = (raw, name, weak) => {
            const url = httpUrl(raw);
            if (!url || seen.has(url) || out.length >= MAX_LINKS) return;
            seen.add(url);
            out.push({ url, name: name || "", weak });
        };
        const textHasUrl = /https?:\/\//i.test(text);
        const anchors = [];
        if (sel && sel.rangeCount) {
            const scratch = document.createElement("div");
            for (let i = 0; i < sel.rangeCount; i++) {
                scratch.appendChild(sel.getRangeAt(i).cloneContents());
            }
            anchors.push(...scratch.querySelectorAll("a[href]"));
        }
        for (const a of anchors) {
            const label = (a.textContent || "").trim();
            const named = FILENAME_RE.test(label);
            if (named || textHasUrl) push(a.href, named ? label : "", false);
        }
        for (const m of text.matchAll(URL_RE)) push(trimPunctuation(m[0]), "", false);
        for (const a of anchors) push(a.href, "", true);
        return out;
    }

    function httpUrl(raw) {
        if (!raw) return "";
        try {
            const u = new URL(String(raw), document.baseURI);
            return u.protocol === "http:" || u.protocol === "https:" ? u.href : "";
        } catch {
            return "";
        }
    }

    function trimPunctuation(s) {
        return s.replace(/[.,;:!?]+$/, "").replace(/[)\]}>]+$/, "");
    }

    function rememberAnchor(field, sel) {
        const fallback = { left: 24, top: 80, bottom: 100 };
        try {
            if (field) {
                const r = field.getBoundingClientRect();
                anchor = r.width || r.height ? { left: r.left, top: r.top, bottom: r.bottom } : fallback;
                return;
            }
            const r = sel && sel.rangeCount ? sel.getRangeAt(sel.rangeCount - 1).getBoundingClientRect() : null;
            anchor = r && (r.width || r.height) ? { left: r.left, top: r.top, bottom: r.bottom } : fallback;
        } catch {
            anchor = fallback;
        }
    }

    /* --- chip ------------------------------------------------------------- */

    const FONT = "font: 12px/1.5 'Segoe UI', system-ui, sans-serif; color: #22303c;";
    const BOX = "box-sizing: border-box; width: 336px; max-height: 60vh; overflow: auto; " +
        "background: #fbfcfd; border: 1px solid #b9c1c9; border-radius: 8px; " +
        "box-shadow: 0 10px 28px #0003; padding: 10px 12px; display: flex; " +
        "flex-direction: column; gap: 6px; " + FONT;
    const HEAD = "display: flex; align-items: center; gap: 8px; " +
        "font-weight: 600; color: #26313b;";
    const ROW = "display: flex; align-items: center; gap: 6px; color: #3c4854;";
    const NOTE = "color: #8a6a2f; font-size: 11px;";
    const ERR = "color: #b02a2a; font-size: 11px;";
    const QUIET = "color: #5b6772; font-size: 11px;";
    const ACTIONS = "display: flex; justify-content: flex-end; gap: 6px; margin-top: 2px;";
    const BTN = "flex: none; padding: 4px 10px; border: 1px solid #c3c9cf; border-radius: 4px; " +
        "background: #f1f3f5; cursor: pointer; " +
        "font: 12px 'Segoe UI', system-ui, sans-serif; color: #22303c;";
    const PRIMARY = BTN + "background: #2e6ea6; border-color: #2a639a; color: #fff; font-weight: 600;";
    const FIELD = "flex: 1; min-width: 0; padding: 3px 6px; border: 1px solid #c3c9cf; " +
        "border-radius: 4px; font: 12px 'Segoe UI', system-ui, sans-serif; color: #22303c; background: #fff;";

    function el(tag, css, text) {
        const node = document.createElement(tag);
        if (css) node.style.cssText = css;
        if (text != null) node.textContent = text;
        return node;
    }

    function button(label, css, hover, onClick) {
        const b = el("button", css, label);
        b.type = "button";
        if (hover) {
            b.addEventListener("mouseenter", () => { b.style.background = hover; });
            b.addEventListener("mouseleave", () => { b.style.background = ""; });
        }
        b.addEventListener("click", onClick);
        return b;
    }

    function ensureChip() {
        if (host) return;
        // A custom tag keeps page rules (`div { ... !important }`) from
        // matching; everything visual is inline CSS set through the CSSOM.
        host = document.createElement("ripdl-offer-chip");
        host.style.cssText = "all: initial; display: block; position: fixed; left: 0px; top: 0px; " +
            "z-index: 2147483647;";
        root = host.attachShadow({ mode: "open" });
        (document.body || document.documentElement).appendChild(host);
        reposition();
        window.addEventListener("scroll", reposition, true);
        window.addEventListener("resize", reposition);
        document.addEventListener("keydown", onKey, true);
    }

    function reposition() {
        if (!host || !anchor) return;
        const pad = 8;
        const box = host.getBoundingClientRect();
        const w = box.width || 336;
        const h = box.height || 96;
        const left = Math.min(Math.max(anchor.left, pad), Math.max(pad, window.innerWidth - w - pad));
        let top = anchor.bottom + pad;
        if (top + h > window.innerHeight - pad) top = Math.max(pad, anchor.top - h - pad);
        host.style.left = `${Math.round(left)}px`;
        host.style.top = `${Math.round(top)}px`;
    }

    function render() {
        ensureChip();
        root.textContent = "";
        const box = el("div", BOX);
        box.append(editing ? editHead() : offerHead());
        box.append(editing ? editRows() : offerRows());
        box.append(editing ? editActions() : offerActions());
        root.append(box);
        reposition();
    }

    function linkCount() {
        return offer.groups.reduce((n, g) => n + g.parts.length, 0);
    }

    function multiGroups() {
        return offer.groups.filter((g) => g.parts.length > 1);
    }

    function closeButton() {
        const b = button("\u00d7", BTN + "padding: 1px 7px; line-height: 1.1;", "#e6eaee", dismiss);
        b.title = "Dismiss";
        return b;
    }

    function offerHead() {
        const head = el("div", HEAD);
        const n = linkCount();
        head.append(el("span", "flex: 1;", `Send ${n} link${n === 1 ? "" : "s"} to ripdl`));
        head.append(closeButton());
        return head;
    }

    function offerRows() {
        const frag = document.createDocumentFragment();
        for (const g of multiGroups()) {
            frag.append(el("div", ROW, groupLine(g)));
        }
        const singles = linkCount() - multiGroups().reduce((n, g) => n + g.parts.length, 0);
        if (singles) frag.append(el("div", ROW, `${singles} single download${singles === 1 ? "" : "s"}`));
        if (offer.refresh) frag.append(refreshRow());
        if (!offer.bundled) {
            frag.append(el("div", NOTE, "This daemon predates bundles - the links are queued one by one."));
        }
        return frag;
    }

    /** One matched unfinished task: the copied link can refresh it instead of
     * queueing a second copy. The daemon already probed the fresh link
     * through the real refresh gate, so "keeps X" is a verified number. */
    function refreshRow() {
        const r = offer.refresh;
        const state = r.state === "stale" ? "expired" : r.state;
        const done = r.kept
            ? `${humans(r.kept)}${r.total ? " of " + humans(r.total) : ""} already on disk`
            : "nothing on disk yet";
        const row = el("div", ROW);
        const label = el("span", "flex: 1; min-width: 0;", "");
        label.append(
            el("div", NOTE, `\u21bb ${r.name} is already in ripdl (${state})`),
            el("div", QUIET, `Refresh keeps ${done}.`)
        );
        row.append(label, button("Refresh", BTN, "#e6eaee", () => doRefresh()));
        return row;
    }

    /** Bytes the way ripdl everywhere else says them. */
    function humans(n) {
        const UNITS = ["B", "KB", "MB", "GB", "TB"];
        let v = Number(n) || 0;
        let u = 0;
        while (v >= 1024 && u < UNITS.length - 1) { v /= 1024; u++; }
        return `${v >= 100 ? Math.round(v) : v.toFixed(v >= 10 ? 1 : 2)} ${UNITS[u]}`;
    }

    /** Swap the copied link into the matched task; the daemon resumes it. */
    function doRefresh() {
        const r = offer.refresh;
        const url = (offer.groups[0] && offer.groups[0].parts[0] && offer.groups[0].parts[0].url) || "";
        showStatus("refreshing\u2026", "", false);
        chrome.runtime.sendMessage({ type: "ripdl_refresh_task", id: r.id, url }, (resp) => {
            if (chrome.runtime.lastError || !resp || !resp.ok) {
                showStatus("refresh failed", (resp && resp.error) || "ripdl could not refresh it.", true);
                return;
            }
            const kept = resp.kept ? ` Kept ${humans(resp.kept)}${resp.total ? " of " + humans(resp.total) : ""}.` : "";
            showStatus("address refreshed - resuming", `ripdl continues where it stopped.${kept}`, false);
            setTimeout(dismiss, 2500);
        });
    }

    /** "x - 3 of 7 parts" (plus the numbers the selection skipped). */
    function groupLine(g) {
        const n = g.parts.length;
        const count = n === 1 ? "1 part"
            : g.total ? `${n} of ${g.total} parts`
            : `${n} parts from ${g.first}`;
        const missing = g.missing && g.missing.length ? ` - missing ${g.missing.join(", ")}` : "";
        return `${g.name} - ${count}${missing}`;
    }

    function offerActions() {
        const row = el("div", ACTIONS);
        row.append(button("Dismiss", BTN, "#e6eaee", dismiss));
        row.append(button("Edit", BTN, "#e6eaee", enterEdit));
        row.append(button("Send", PRIMARY, "#357ab5", doSend));
        return row;
    }

    function enterEdit() {
        editing = true;
        picks = offer.groups.map((g) => ({ group: g, on: true, name: g.name }));
        arm();
        render();
        const first = root.querySelector("input");
        if (first) first.focus();
    }

    function editHead() {
        const head = el("div", HEAD);
        head.append(el("span", "flex: 1;", "Adjust what gets sent"));
        head.append(closeButton());
        return head;
    }

    function editRows() {
        const frag = document.createDocumentFragment();
        for (const p of picks) {
            const row = el("div", ROW);
            const chk = el("input", "flex: none; margin: 0;");
            chk.type = "checkbox";
            chk.checked = p.on;
            chk.addEventListener("change", () => { p.on = chk.checked; arm(); });
            row.append(chk);
            if (p.group.parts.length > 1) {
                const name = el("input", FIELD);
                name.type = "text";
                name.value = p.name;
                name.spellcheck = false;
                name.title = "Folder name";
                name.addEventListener("input", () => { p.name = name.value; arm(); });
                row.append(name, el("span", "flex: none; color: #6b7680;", `${p.group.parts.length} parts`));
            } else {
                row.append(el("span", "flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; " +
                    "white-space: nowrap;", p.group.name));
            }
            frag.append(row);
        }
        return frag;
    }

    function editActions() {
        const row = el("div", ACTIONS);
        row.append(button("Back", BTN, "#e6eaee", () => { editing = false; arm(); render(); }));
        row.append(button("Send selected", PRIMARY, "#357ab5", doSend));
        return row;
    }

    /* --- sending ---------------------------------------------------------- */

    /** The groups the current mode would queue (edit honours checkboxes). */
    function payload() {
        const chosen = editing ? picks.filter((p) => p.on) : offer.groups.map((g) => ({ group: g, on: true, name: g.name }));
        return chosen.map((p) => ({
            name: p.group.parts.length > 1 ? String(p.name || p.group.name).trim() : "",
            parts: p.group.parts.map((part) => ({ url: part.url, filename: part.filename || "" })),
        }));
    }

    function doSend() {
        const groups = payload();
        if (!groups.length) {
            showStatus("nothing selected", "Nothing was queued.", true);
            return;
        }
        showStatus("sending\u2026", "", false);
        chrome.runtime.sendMessage({ type: "ripdl_offer_send", groups, page: location.href }, (resp) => {
            if (chrome.runtime.lastError || !resp || !resp.ok) {
                showStatus("daemon unreachable", "Nothing was queued.", true);
                return;
            }
            // Links ripdl already had open their own window: the chip points there
            // and stays until closed.
            const queued = (resp.bundles || 0) + (resp.singles || 0);
            const dupes = resp.duplicates || 0;
            const note = resp.failed
                ? "Some links were not queued."
                : dupes ? "Choose what to do in the ripdl window." : "Look for the new row in ripdl.";
            showStatus(queued ? `queued ${resp.summary}` : resp.summary, note, !!resp.failed);
            if (!resp.failed && !dupes) setTimeout(dismiss, 1600);
        });
    }

    /** One message in the chip: progress, a result, or an error. */
    function showStatus(title, note, bad, ms) {
        ensureChip();
        root.textContent = "";
        const box = el("div", BOX);
        box.append(el("div", bad ? HEAD + "color: #b02a2a;" : HEAD, title));
        if (note) box.append(el("div", bad ? ERR : QUIET, note));
        const row = el("div", ACTIONS);
        row.append(button("Close", BTN, "#e6eaee", dismiss));
        box.append(row);
        root.append(box);
        arm(ms);
        reposition();
    }

    /* --- lifecycle -------------------------------------------------------- */

    function arm(ms = AUTO_DISMISS_MS) {
        clearTimeout(timer);
        timer = setTimeout(dismiss, ms);
    }

    function onKey(ev) {
        if (ev.key === "Escape") dismiss();
    }

    function dismiss() {
        clearTimeout(timer);
        offerSeq++; // a reply still in flight must not reopen the chip
        loading = false;
        if (host) {
            window.removeEventListener("scroll", reposition, true);
            window.removeEventListener("resize", reposition);
            document.removeEventListener("keydown", onKey, true);
            host.remove();
        }
        host = null;
        root = null;
        offer = null;
        picks = null;
        editing = false;
    }

    /* --- menu feedback from the service worker ---------------------------- */

    chrome.runtime.onMessage.addListener((msg) => {
        if (msg?.type !== "ripdl_status" || typeof msg.text !== "string") return;
        offerSeq++; // a pending copy reply must not replace this message
        rememberAnchor(null, window.getSelection());
        showStatus(msg.text, "", !!msg.bad, 6000);
    });
})();