/** Plugins options page: list, view code, toggle, import recipes. */
"use strict";

const $ = (id) => document.getElementById(id);

function entryDiv(e) {
    const div = document.createElement("div");
    div.className = "entry";

    const row = document.createElement("div");
    row.className = "row";
    const left = document.createElement("div");
    const name = document.createElement("b");
    name.textContent = e.label;
    left.append(name);
    const id = document.createElement("span");
    id.className = "plugin-id";
    id.textContent = e.id;
    left.append(id);
    row.append(left);

    const kind = document.createElement("span");
    kind.className = `kind ${e.kind}`;
    kind.textContent = e.kind;
    row.append(kind);
    div.append(row);

    if (e.error) {
        const err = document.createElement("div");
        err.className = "err";
        err.textContent = e.error;
        div.append(err);
        return div;
    }

    const controls = document.createElement("div");
    controls.className = "row plugin-controls";

    // Test box: paste any URL, see name + resolved direct URL. Nothing queues.
    const testInput = document.createElement("input");
    testInput.type = "text";
    testInput.placeholder = "test: paste a URL";
    testInput.style.cssText = "flex:1; font-size:11px; padding:2px 4px";
    const testOut = document.createElement("div");
    testOut.className = "hint";
    testOut.style.wordBreak = "break-all";
    const testBtn = document.createElement("button");
    testBtn.textContent = "Test";
    testBtn.style.fontSize = "11px";
    testBtn.addEventListener("click", async () => {
        testOut.textContent = "testing...";
        const r = await chrome.runtime.sendMessage({ type: "ripdl_plugin_test", id: e.id, url: testInput.value });
        if (!r?.ok) { testOut.textContent = "error: " + (r?.error || "?"); return; }
        const bits = [`match: ${r.matched}`];
        if (r.name) bits.push(`name: ${r.name}`);
        if ("resolved" in r) bits.push(r.resolved ? `direct: ${r.resolved}` : "resolve: null (queued as-is)");
        testOut.textContent = bits.join(" | ");
    });
    const testRow = document.createElement("div");
    testRow.className = "row test-row";
    testRow.append(testInput, testBtn);
    div.append(testRow, testOut);

    if (e.kind === "script") {
        const details = document.createElement("details");
        const summary = document.createElement("summary");
        summary.textContent = "View code";
        details.append(summary);
        const pre = document.createElement("pre");
        pre.textContent = "loading...";
        details.append(pre);
        details.addEventListener("toggle", async () => {
            if (!details.open || pre.dataset.loaded) return;
            const r = await fetch(chrome.runtime.getURL(`hosts/local/${e.file}`));
            pre.textContent = await r.text();
            pre.dataset.loaded = "1";
            if (!e.seen) {
                const seen = await chrome.runtime.sendMessage({ type: "ripdl_plugin_seen", file: e.file });
                if (seen?.ok) {
                    e.seen = true;
                    toggle.disabled = false;
                    refresh();
                }
            }
        });
        div.append(details);
    }

    const toggle = document.createElement("input");
    toggle.type = "checkbox";
    toggle.id = `en-${e.id}`;
    toggle.checked = e.enabled;
    if (e.kind === "script" && !e.seen) {
        toggle.disabled = true;
        toggle.title = "View the code first";
    }
    toggle.addEventListener("change", async () => {
        await chrome.runtime.sendMessage({ type: "ripdl_plugin_toggle", id: e.id, enabled: toggle.checked });
    });
    const lbl = document.createElement("label");
    lbl.htmlFor = toggle.id;
    lbl.textContent = "Enabled";
    lbl.style.marginLeft = "auto";
    controls.append(lbl, toggle);
    div.append(controls);

    if (e.kind === "recipe") {
        const del = document.createElement("button");
        del.className = "recipe-delete";
        del.textContent = "Remove";
        del.addEventListener("click", async () => {
            await chrome.runtime.sendMessage({ type: "ripdl_recipe_delete", id: e.id });
            refresh();
        });
        div.append(del);
    }
    return div;
}

async function refresh() {
    const list = $("list");
    try {
        const resp = await chrome.runtime.sendMessage({ type: "ripdl_plugins_list" });
        if (!resp || !Array.isArray(resp.plugins)) throw new Error("Plugin service did not respond.");
        list.textContent = "";
        for (const e of resp.plugins) list.append(entryDiv(e));
        if (!resp.plugins.length) list.textContent = "No plugins found.";
    } catch {
        list.textContent = "Plugin service unavailable. Reload ripdl in your browser's Extensions page. If it still fails, check the extension's Errors panel for missing files.";
    }
}

async function loadVersion() {
    const el = $("version");
    el.textContent = `Extension ${chrome.runtime.getManifest().version}; app version unavailable`;
    try {
        const info = await chrome.runtime.sendMessage({ type: "ripdl_ext_info" });
        if (!info?.appVersion) return;
        if (info.skew === 0) {
            el.textContent = `Extension ${info.extVersion}; app ${info.appVersion}`;
        } else if (info.skew < 0) {
            el.textContent = `Extension ${info.extVersion}; app ${info.appVersion}. Reload the extension in chrome://extensions.`;
            el.className = "warn";
        } else {
            el.textContent = `Extension ${info.extVersion}; app ${info.appVersion}. Update ripdl.`;
            el.className = "warn";
        }
    } catch {
        // The app version is unavailable while the daemon is offline.
    }
}

$("file").addEventListener("change", async () => {
    const f = $("file").files[0];
    if (!f) return;
    try {
        if (!f.name.toLowerCase().endsWith(".json")) {
            throw new Error("Choose a .json recipe. Install .js scripts from ripdl Settings into the folder loaded in your browser, then reload the extension.");
        }
        await importText(await f.text());
    } catch (error) {
        $("import-status").className = "bad";
        $("import-status").textContent = String(error.message || error);
    } finally {
        $("file").value = "";
    }
});

$("import-btn").addEventListener("click", () => importText($("paste").value));

async function importText(text) {
    const st = $("import-status");
    st.className = "";
    st.textContent = "Importing...";
    try {
        if (!text.trim()) throw new Error("Choose a .json recipe file or paste its JSON first.");
        try {
            JSON.parse(text);
        } catch {
            throw new Error("This is not valid recipe JSON. JavaScript plugins must be installed from ripdl Settings.");
        }
        let resp;
        try {
            resp = await chrome.runtime.sendMessage({ type: "ripdl_recipe_import", text });
        } catch {
            throw new Error("Cannot reach the plugin service. Reload the extension in your browser's Extensions page and try again.");
        }
        if (!resp?.ok) throw new Error(resp?.error || "Plugin service did not respond. Check the extension's Errors panel.");
        st.className = "ok";
        st.textContent = `Imported ${resp.id}`;
        $("paste").value = "";
        await refresh();
    } catch (error) {
        st.className = "bad";
        st.textContent = String(error.message || error);
    }
}

$("reload").addEventListener("click", () => chrome.runtime.reload());

refresh();
loadVersion();
