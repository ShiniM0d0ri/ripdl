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
    /** A filename-looking token anywhere in a row ("... [DODI].part01.rar" on
     * crypter/hoster rows whose button carries no name itself). The class
     * holds everything a release name uses; ] and - escaped/placed last. */
    const FILE_TOKEN_RE =
        /(?:[\w [\]().+_'&{}-]{2,116}\.)?(?:rar|zip|7z|tar|gz|tgz|cab|iso|mkv|mp4|avi|part\d{2})\b/i;

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
            if (label && isSiteAnchor(a, label)) continue;
            const named = FILENAME_RE.test(label);
            if (named || textHasUrl) push(a.href, named ? label : "", false);
        }
        for (const m of text.matchAll(URL_RE)) push(trimPunctuation(m[0]), "", false);
        for (const a of anchors) {
            const label = (a.textContent || "").trim();
            if (label && isSiteAnchor(a, label)) continue;
            push(a.href, rowFileName(a), true);
        }
        return out;
    }

    /** A link labelled with its own address ("filekeeper.net" -> the site)
     * is navigation, not a file: a domain-shaped label matches the filename
     * test by accident and would ride the offer as junk parts. */
    function isSiteAnchor(a, label) {
        if (/[\s/]/.test(label)) return false;
        let host;
        try {
            host = new URL(a.href, document.baseURI).hostname.toLowerCase().replace(/^www\./, "");
        } catch {
            return false;
        }
        const lab = label.toLowerCase().replace(/^https?:\/\//, "").replace(/[/.]+$/, "");
        return lab === host || lab.endsWith("." + host);
    }

    /** A weak anchor ("Download") that names no file borrows the name its
     * row shows: hoster/crypter pages put the filename beside the button,
     * and the daemon's grouping keys on those names. Other anchors' labels
     * are not the row's name (a paragraph may hold part1 ... part3 and one
     * "Download"), so they are stripped before matching. Bounded walk -
     * nearest ancestor whose stripped text stays row-sized. */
    function rowFileName(a) {
        let node = a.parentElement;
        for (let hop = 0; node && hop < 5; hop++, node = node.parentElement) {
            if (node.tagName === "BODY" || node.tagName === "HTML") return "";
            const text = textWithoutOtherAnchors(node, a).replace(/\s+/g, " ").trim();
            if (text.length > 200) continue; // a page-sized container, not a row
            const m = text.match(FILE_TOKEN_RE);
            if (m) return m[0].trim().slice(0, 160);
        }
        return "";
    }

    /** node's visible text, dropping every <a> but `self` (their labels are
     * their own names - the prose between the links is the row's text).
     * Element children are separated with a space: textContent concatenates
     * siblings directly, and "...part01.rar" followed by "2.00 GB" must not
     * read as "...part01.rar2.00" (no word boundary, no match). */
    function textWithoutOtherAnchors(node, self) {
        if (node === self) return "";
        let out = "";
        for (const child of node.childNodes) {
            if (child.nodeType === 3) { out += child.nodeValue; continue; }
            if (child.nodeType !== 1) continue;
            if (child.tagName === "A" && child !== self) continue;
            out += ` ${textWithoutOtherAnchors(child, self)} `;
        }
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

    const SVG_NS = "http://www.w3.org/2000/svg";
    function pal() {
        const dark = !!(window.matchMedia && window.matchMedia("(prefers-color-scheme: dark)").matches);
        return dark ? {
            font: "font: 12px/1.5 'Segoe UI', system-ui, sans-serif; color: #e6eff2;",
            box: "box-sizing: border-box; width: 344px; max-height: 60vh; overflow: auto; " +
                "background: #142026; border: 1px solid #2b4049; border-radius: 10px; " +
                "box-shadow: 0 12px 32px #0008; padding: 11px 13px; display: flex; " +
                "flex-direction: column; gap: 7px; ",
            head: "display: flex; align-items: center; gap: 8px; font-weight: 600; color: #f0f6f8;",
            row: "display: flex; align-items: center; gap: 6px; color: #c7d6dc;",
            note: "color: #f0b45a; font-size: 11px;",
            err: "color: #f27c75; font-size: 11px;",
            headErr: "display: flex; align-items: center; gap: 8px; font-weight: 600; color: #f27c75;",
            quiet: "color: #8ea3ac; font-size: 11px;",
            actions: "display: flex; justify-content: flex-end; gap: 6px; margin-top: 2px;",
            btnBg: "#1d2d34",
            btnHover: "#263a43",
            btn: "flex: none; display: inline-flex; align-items: center; gap: 5px; padding: 4px 10px; " +
                "border: 1px solid #324a53; border-radius: 6px; background: #1d2d34; cursor: pointer; " +
                "font: 12px 'Segoe UI', system-ui, sans-serif; font-weight: 500; color: #e2edf1;",
            primaryBg: "#138577",
            primaryHover: "#0f6e62",
            primaryBorder: "#1aa392",
            field: "flex: 1; min-width: 0; padding: 4px 7px; border: 1px solid #324a53; " +
                "border-radius: 5px; font: 12px 'Segoe UI', system-ui, sans-serif; color: #e6eff2; background: #0e171b;",
            countMuted: "flex: none; color: #8ea3ac;",
        } : {
            font: "font: 12px/1.5 'Segoe UI', system-ui, sans-serif; color: #22303c;",
            box: "box-sizing: border-box; width: 344px; max-height: 60vh; overflow: auto; " +
                "background: #fbfcfd; border: 1px solid #b9c5c9; border-radius: 10px; " +
                "box-shadow: 0 10px 28px #0b323026; padding: 11px 13px; display: flex; " +
                "flex-direction: column; gap: 7px; ",
            head: "display: flex; align-items: center; gap: 8px; font-weight: 600; color: #172522;",
            row: "display: flex; align-items: center; gap: 6px; color: #364542;",
            note: "color: #9e590e; font-size: 11px;",
            err: "color: #b02a2a; font-size: 11px;",
            headErr: "display: flex; align-items: center; gap: 8px; font-weight: 600; color: #b02a2a;",
            quiet: "color: #5b6a66; font-size: 11px;",
            actions: "display: flex; justify-content: flex-end; gap: 6px; margin-top: 2px;",
            btnBg: "#f2f5f4",
            btnHover: "#e4ebe8",
            btn: "flex: none; display: inline-flex; align-items: center; gap: 5px; padding: 4px 10px; " +
                "border: 1px solid #c3d0cc; border-radius: 6px; background: #f2f5f4; cursor: pointer; " +
                "font: 12px 'Segoe UI', system-ui, sans-serif; font-weight: 500; color: #1f2e2a;",
            primaryBg: "#0c7669",
            primaryHover: "#075c53",
            primaryBorder: "#095d53",
            field: "flex: 1; min-width: 0; padding: 4px 7px; border: 1px solid #c3d0cc; " +
                "border-radius: 5px; font: 12px 'Segoe UI', system-ui, sans-serif; color: #172522; background: #fff;",
            countMuted: "flex: none; color: #687772;",
        };
    }

    function el(tag, css, text) {
        const node = document.createElement(tag);
        if (css) node.style.cssText = css;
        if (text != null) node.textContent = text;
        return node;
    }

    function svgEl(tag, attrs) {
        const node = document.createElementNS(SVG_NS, tag);
        for (const [k, v] of Object.entries(attrs)) node.setAttribute(k, String(v));
        return node;
    }

    function lucideIcon(specs, size = 13) {
        const svg = svgEl("svg", {
            viewBox: "0 0 24 24", width: size, height: size,
            fill: "none", stroke: "currentColor", "stroke-width": 2,
            "stroke-linecap": "round", "stroke-linejoin": "round",
            "aria-hidden": "true",
        });
        svg.style.cssText = "flex: none; display: block;";
        for (const [tag, attrs] of specs) svg.append(svgEl(tag, attrs));
        return svg;
    }

    function brandTile(size = 18) {
        const svg = svgEl("svg", { viewBox: "0 0 512 512", width: size, height: size, "aria-hidden": "true" });
        svg.style.cssText = "flex: none; display: block; border-radius: 4px;";
        svg.append(
            svgEl("rect", { width: 512, height: 512, rx: 112, fill: "#0b3230" }),
            svgEl("rect", { x: 104, y: 176, width: 86, height: 20, rx: 10, fill: "#4ee0b5", opacity: "0.55" }),
            svgEl("rect", { x: 104, y: 242, width: 64, height: 20, rx: 10, fill: "#4ee0b5", opacity: "0.75" }),
            svgEl("rect", { x: 104, y: 308, width: 42, height: 20, rx: 10, fill: "#4ee0b5" }),
            svgEl("path", { d: "M222 132h68v122h62L256 384 160 254h62z", fill: "#ffffff" }),
        );
        return svg;
    }

    const ICON_CLOSE = [["path", { d: "M18 6 6 18" }], ["path", { d: "m6 6 12 12" }]];
    const ICON_EDIT = [["path", { d: "M21.174 6.812a1 1 0 0 0-3.986-3.987L3.842 16.174a2 2 0 0 0-.5.83l-1.321 4.352a.5.5 0 0 0 .623.622l4.353-1.32a2 2 0 0 0 .83-.497z" }]];
    const ICON_DOWNLOAD = [["path", { d: "M12 15V3" }], ["path", { d: "M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" }], ["path", { d: "m7 10 5 5 5-5" }]];
    const ICON_REFRESH = [["path", { d: "M3 12a9 9 0 0 1 9-9 9.75 9.75 0 0 1 6.74 2.74L21 8" }], ["path", { d: "M21 3v5h-5" }], ["path", { d: "M21 12a9 9 0 0 1-9 9 9.75 9.75 0 0 1-6.74-2.74L3 16" }], ["path", { d: "M8 16H3v5" }]];

    function button(label, css, hover, onClick, iconSpecs, primaryPal) {
        const b = el("button", css, "");
        b.type = "button";
        if (primaryPal) {
            b.style.background = primaryPal.primaryBg;
            b.style.borderColor = primaryPal.primaryBorder;
            b.style.color = "#fff";
            b.style.fontWeight = "600";
        }
        if (iconSpecs) b.append(lucideIcon(iconSpecs, 13));
        if (label) b.append(document.createTextNode(label));
        const baseBg = b.style.background;
        if (hover) {
            b.addEventListener("mouseenter", () => { b.style.background = hover; });
            b.addEventListener("mouseleave", () => { b.style.background = baseBg; });
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
        const p = pal();
        const box = el("div", p.box + p.font);
        box.append(editing ? editHead(p) : offerHead(p));
        box.append(editing ? editRows(p) : offerRows(p));
        box.append(editing ? editActions(p) : offerActions(p));
        root.append(box);
        reposition();
    }

    function linkCount() {
        return offer.groups.reduce((n, g) => n + g.parts.length, 0);
    }

    function multiGroups() {
        return offer.groups.filter((g) => g.parts.length > 1);
    }

    function closeButton(p) {
        const b = button("", p.btn + "padding: 4px 6px;", p.btnHover, dismiss, ICON_CLOSE);
        b.title = "Dismiss";
        b.setAttribute("aria-label", "Dismiss");
        return b;
    }

    function offerHead(p) {
        const head = el("div", p.head);
        const n = linkCount();
        head.append(brandTile(18));
        head.append(el("span", "flex: 1;", `Send ${n} link${n === 1 ? "" : "s"} to ripdl`));
        head.append(closeButton(p));
        return head;
    }

    function offerRows(p) {
        const frag = document.createDocumentFragment();
        for (const g of multiGroups()) {
            frag.append(el("div", p.row, groupLine(g)));
        }
        const singles = linkCount() - multiGroups().reduce((n, g) => n + g.parts.length, 0);
        if (singles) frag.append(el("div", p.row, `${singles} single download${singles === 1 ? "" : "s"}`));
        if (offer.refresh) frag.append(refreshRow(p));
        if (!offer.bundled) {
            frag.append(el("div", p.note, "This daemon predates bundles - the links are queued one by one."));
        }
        return frag;
    }

    /** One matched unfinished task: the copied link can refresh it instead of
     * queueing a second copy. The daemon already probed the fresh link
     * through the real refresh gate, so "keeps X" is a verified number. */
    function refreshRow(p) {
        const r = offer.refresh;
        const state = r.state === "stale" ? "expired" : r.state;
        const done = r.kept
            ? `${humans(r.kept)}${r.total ? " of " + humans(r.total) : ""} already on disk`
            : "nothing on disk yet";
        const row = el("div", p.row);
        const label = el("span", "flex: 1; min-width: 0;", "");
        const titleRow = el("div", p.note + " display: flex; align-items: center; gap: 4px;");
        titleRow.append(lucideIcon(ICON_REFRESH, 12), document.createTextNode(`${r.name} is already in ripdl (${state})`));
        label.append(
            titleRow,
            el("div", p.quiet, `Refresh keeps ${done}.`)
        );
        row.append(label, button("Refresh", p.btn, p.btnHover, () => doRefresh(), ICON_REFRESH));
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

    function offerActions(p) {
        const row = el("div", p.actions);
        row.append(button("Dismiss", p.btn, p.btnHover, dismiss));
        row.append(button("Edit", p.btn, p.btnHover, enterEdit, ICON_EDIT));
        row.append(button("Send", p.btn, p.primaryHover, doSend, ICON_DOWNLOAD, p));
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

    function editHead(p) {
        const head = el("div", p.head);
        head.append(brandTile(18));
        head.append(el("span", "flex: 1;", "Adjust what gets sent"));
        head.append(closeButton(p));
        return head;
    }

    function editRows(p) {
        const frag = document.createDocumentFragment();
        for (const item of picks) {
            const row = el("div", p.row);
            const chk = el("input", "flex: none; margin: 0; accent-color: #0c7669;");
            chk.type = "checkbox";
            chk.checked = item.on;
            chk.addEventListener("change", () => { item.on = chk.checked; arm(); });
            row.append(chk);
            if (item.group.parts.length > 1) {
                const name = el("input", p.field);
                name.type = "text";
                name.value = item.name;
                name.spellcheck = false;
                name.title = "Folder name";
                name.addEventListener("input", () => { item.name = name.value; arm(); });
                row.append(name, el("span", p.countMuted, `${item.group.parts.length} parts`));
            } else {
                row.append(el("span", "flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; " +
                    "white-space: nowrap;", item.group.name));
            }
            frag.append(row);
        }
        return frag;
    }

    function editActions(p) {
        const row = el("div", p.actions);
        row.append(button("Back", p.btn, p.btnHover, () => { editing = false; arm(); render(); }));
        row.append(button("Send selected", p.btn, p.primaryHover, doSend, ICON_DOWNLOAD, p));
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
        const p = pal();
        const box = el("div", p.box + p.font);
        const head = el("div", bad ? p.headErr : p.head);
        head.append(brandTile(16), el("span", "flex: 1;", title));
        box.append(head);
        if (note) box.append(el("div", bad ? p.err : p.quiet, note));
        const row = el("div", p.actions);
        row.append(button("Close", p.btn, p.btnHover, dismiss));
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