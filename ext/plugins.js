/**
 * Hoster plugin host: built-ins + user-loadable recipes and scripts.
 *
 * Trust model (locked, see docs/plan-bundles-and-plugins.md):
 *   - Recipes (.json) are inert data: JSON.parse + this file's declarative
 *     engine. Never executed, importable from anywhere.
 *   - Scripts (.js) run with the extension's powers, so they load ONLY from
 *     the extension's own `hosts/local/` folder, listed in the generated
 *     `hosts/local/registry.js` (static imports - dynamic import() is
 *     forbidden on ServiceWorkerGlobalScope). Never fetched from the network.
 *     New scripts start disabled; the options page shows the full code before
 *     the toggle unlocks.
 *   - No eval, no remote plugin code, no captcha/paywall logic, no stored
 *     credentials. `resolve()` may only call the host's own endpoints.
 *
 * Storage keys (chrome.storage.local):
 *   plugin_recipes   [recipe]              imported recipes
 *   plugins_enabled  {id: bool}            per-host toggles
 *   plugin_seen      {filename: bool}      scripts whose code was viewed
 */

import { BUILTINS } from "./hosts/index.js";
import { SCRIPTS } from "./hosts/local/registry.js";

/** The one pure registry instance per SW lifetime; invalidated on change. */
let cache = null;

/* ============================ recipes (data only) ======================= */

/**
 * Validate one imported recipe object, normalizing what it can. Throws
 * `Error("recipe <id>: <where>: <what>")` with text meant for the options
 * page. Every field is data; nothing here ever runs user-controlled code -
 * even regexes only ever test the URL string of a link the user selected.
 */
export function validateRecipe(obj) {
    const fail = (where, what) => {
        const id = obj && typeof obj === "object" && typeof obj.id === "string" ? obj.id : "(no id)";
        throw new Error(`recipe ${id}: ${where}: ${what}`);
    };
    if (!obj || typeof obj !== "object" || Array.isArray(obj)) fail("root", "expected a JSON object");
    const r = { match: {}, name: null, resolve: null };

    if (typeof obj.id !== "string" || !/^[a-z0-9][a-z0-9._-]{2,63}$/i.test(obj.id.trim()))
        fail("id", "2-64 characters: letters, digits, dot, dash, underscore");
    r.id = obj.id.trim();
    r.label = typeof obj.label === "string" && obj.label.trim() ? obj.label.trim() : r.id;

    const m = obj.match ?? {};
    if (typeof m !== "object" || Array.isArray(m)) fail("match", "expected an object");
    const hosts = Array.isArray(m.host) ? m.host : [m.host];
    if (!hosts.length || hosts.some((h) => typeof h !== "string" || !h.trim()))
        fail("match.host", "a hostname string or a non-empty array of them");
    const hostList = hosts.map((h) => h.trim().toLowerCase().replace(/^www\./, "")).filter(Boolean);
    if (!hostList.length) fail("match.host", "at least one hostname");
    if (hostList.some((h) => /[^\w.-]|\.\./.test(h))) fail("match.host", "hostnames only (letters, digits, dots, dashes)");
    r.match.hosts = hostList;
    if (m.path != null) {
        if (typeof m.path !== "string") fail("match.path", "a regular-expression string");
        try { r.match.path = new RegExp(m.path); } catch (e) { fail("match.path", `not a valid regex: ${e.message}`); }
    }

    if (obj.name != null) {
        const n = obj.name;
        if (typeof n !== "object" || Array.isArray(n)) fail("name", "expected an object");
        const kinds = ["fragment", "query", "tail"].filter((k) => n[k] != null);
        if (kinds.length !== 1) fail("name", "exactly one of fragment / query / tail");
        if (kinds[0] === "query" && typeof n.query !== "string") fail("name.query", "a query-parameter name string");
        r.name = { kind: kinds[0], param: n.query };
    }

    if (obj.resolve != null) {
        const res = obj.resolve;
        if (typeof res !== "object" || Array.isArray(res)) fail("resolve", "expected an object");
        const q = res.request;
        if (!q || typeof q !== "object") fail("resolve.request", "expected an object");
        if (q.method !== "GET" && q.method !== "POST") fail("resolve.request.method", '"GET" or "POST"');
        if (typeof q.url !== "string" || !/^https?:\/\//.test(q.url)) fail("resolve.request.url", "an http(s) URL template");
        const used = [...String(q.url).matchAll(/\{(\w+)\}/g)].map((x) => x[1]);
        const known = new Set(["host", "path", "fragment"]);
        const bad = used.filter((k) => !known.has(k));
        if (bad.length) fail("resolve.request.url", `unknown placeholder {${bad[0]}} (known: host, path, fragment)`);
        if (q.credentials != null && q.credentials !== "include" && q.credentials !== "omit")
            fail("resolve.request.credentials", '"include" or "omit"');
        if (q.headers != null) {
            if (typeof q.headers !== "object" || Array.isArray(q.headers)) fail("resolve.request.headers", "an object of name: value");
            for (const [k, v] of Object.entries(q.headers))
                if (typeof v !== "string" || /[^\w!#$%&'*+.^`|~-]/.test(k)) fail(`resolve.request.headers.${k}`, "header name/value strings");
        }
        const d = res.direct;
        if (!d || typeof d !== "object") fail("resolve.direct", "expected an object");
        const extractors = ["header", "json"].filter((k) => d[k] != null);
        if (extractors.length !== 1) fail("resolve.direct", "exactly one of header / json");
        if (extractors[0] === "header" && (typeof d.header !== "string" || /[^\w-]/.test(d.header)))
            fail("resolve.direct.header", "a response-header name");
        if (extractors[0] === "json" && (typeof d.json !== "string" || !/^[\w]+(\.[\w]+)*$/.test(d.json)))
            fail("resolve.direct.json", "a dot path like download.url");
        if (d.prefix != null && (typeof d.prefix !== "string" || !/^https?:\/\//.test(d.prefix)))
            fail("resolve.direct.prefix", "an http(s) prefix the direct URL must start with");
        r.resolve = {
            method: q.method,
            url: q.url,
            credentials: q.credentials ?? "include",
            headers: q.headers ?? {},
            extract: extractors[0],
            key: extractors[0] === "header" ? d.header.toLowerCase() : d.json,
            prefix: d.prefix ?? null,
        };
    }
    return r;
}

/** Wrap a validated recipe in the plugin API - pure data interpretation. */
export function recipeToPlugin(recipe, fetchImpl) {
    const hostOf = (url) => {
        try {
            const u = new URL(url);
            if (u.protocol !== "http:" && u.protocol !== "https:") return null;
            return { host: u.hostname.toLowerCase().replace(/^www\./, ""), u };
        } catch { return null; }
    };
    const decoded = (s) => { try { return decodeURIComponent(s); } catch { return s; } };
    const matches = (url) => {
        const h = hostOf(url);
        if (!h || !recipe.match.hosts.includes(h.host)) return false;
        if (recipe.match.path && !recipe.match.path.test(decoded(h.u.pathname.replace(/^\/+/, "")))) return false;
        return true;
    };
    const nameFrom = (url) => {
        if (!matches(url)) return null;
        if (!recipe.name) return null;
        const h = hostOf(url);
        if (recipe.name.kind === "fragment") { const v = decoded(h.u.hash.slice(1)).trim(); return v || null; }
        if (recipe.name.kind === "query") { const v = h.u.searchParams.get(recipe.name.param); return v && v.trim() ? v.trim() : null; }
        const tail = decoded(h.u.pathname.split("/").pop() || "").trim();
        return tail || null;
    };
    const fill = (tpl, u) => tpl
        .replaceAll("{host}", u.hostname.replace(/^www\./, ""))
        .replaceAll("{path}", u.pathname.replace(/^\/+/, "").replace(/\/+$/, ""))
        .replaceAll("{fragment}", u.hash.slice(1));
    return {
        id: recipe.id,
        label: recipe.label,
        match(url) { return matches(url); },
        name(url) { return nameFrom(url); },
        async resolve(link) {
            if (!recipe.resolve) return null;
            const url = typeof link === "string" ? link : link?.url;
            if (!matches(url)) return null;
            const h = hostOf(url);
            let resp;
            try {
                resp = await fetchImpl(fill(recipe.resolve.url, h.u), {
                    method: recipe.resolve.method,
                    credentials: recipe.resolve.credentials,
                    headers: recipe.resolve.headers,
                    redirect: "manual",
                });
            } catch { return null; }
            if (!resp.ok) return null;
            let direct = null;
            if (recipe.resolve.extract === "header") {
                direct = resp.headers.get(recipe.resolve.key);
            } else {
                try {
                    const body = await resp.json();
                    direct = String(recipe.resolve.key).split(".").reduce((o, k) => (o == null ? o : o[k]), body);
                    direct = direct == null ? null : String(direct);
                } catch { return null; }
            }
            if (!direct || !/^https?:\/\//i.test(direct)) return null;
            if (recipe.resolve.prefix && !direct.startsWith(recipe.resolve.prefix)) return null;
            return { url: direct, name: nameFrom(url), headers: {} };
        },
    };
}

/* ============================ scripts (files on disk) =================== */

/** Shape check for an imported script module - the only contract it must
 * satisfy. Bad shapes never load; the options page shows the reason. */
export function scriptProblem(mod) {
    if (!mod?.default || typeof mod.default !== "object") return "no default export object";
    const p = mod.default;
    if (typeof p.id !== "string" || !p.id.trim()) return "missing id";
    if (typeof p.match !== "function") return "missing match(url)";
    if (p.resolve != null && typeof p.resolve !== "function") return "resolve must be a function";
    if (p.name != null && typeof p.name !== "function") return "name must be a function";
    if (p.links != null && typeof p.links !== "function") return "links must be a function";
    return null;
}

/* ============================ registry ================================= */

async function storageGet(key) {
    try { return (await chrome.storage.local.get(key))[key]; } catch { return null; }
}

async function registryInner() {
    if (cache) return cache;
    const recipes = Array.isArray(await storageGet("plugin_recipes")) ? await storageGet("plugin_recipes") : [];
    const enabled = (await storageGet("plugins_enabled")) || {};
    const seen = (await storageGet("plugin_seen")) || {};

    const entries = [];
    const add = (entry) => {
        const i = entries.findIndex((e) => e.id === entry.id);
        if (i >= 0) entries.splice(i, 1);
        entries.push(entry);
    };

    for (const b of BUILTINS) {
        add({ id: b.id, label: b.label, kind: "builtin", plugin: b, enabled: enabled[b.id] !== false });
    }
    for (const raw of recipes) {
        try {
            const recipe = validateRecipe(raw);
            add({
                id: recipe.id, label: recipe.label, kind: "recipe",
                plugin: recipeToPlugin(recipe, (u, o) => fetch(u, o)),
                enabled: enabled[recipe.id] !== false,
            });
        } catch (e) {
            add({ id: raw?.id || "(broken recipe)", label: raw?.label || "(broken recipe)", kind: "recipe", error: String(e.message || e), enabled: false });
        }
    }
    for (const { file, mod } of SCRIPTS) {
        try {
            const problem = scriptProblem(mod);
            if (problem) { add({ id: file, label: file, kind: "script", file, error: problem, enabled: false }); continue; }
            add({
                id: mod.default.id.trim(), label: mod.default.label || mod.default.id.trim(),
                kind: "script", file, plugin: mod.default,
                enabled: enabled[mod.default.id.trim()] === true && seen[file] === true,
            });
        } catch (e) {
            add({ id: file, label: file, kind: "script", file, error: String(e.message || e), enabled: false });
        }
    }
    cache = entries;
    return entries;
}

export function invalidatePlugins() { cache = null; }

/** Full registry read (options page listing, per-plugin test). */
export async function registry() {
    return registryInner();
}

if (globalThis.chrome?.storage?.onChanged) {
    chrome.storage.onChanged.addListener((changes, area) => {
        if (area === "local" && (changes.plugin_recipes || changes.plugins_enabled || changes.plugin_seen)) invalidatePlugins();
    });
}

/** Cheap pre-group name: first enabled plugin that knows this URL. */
export async function pluginNameFor(url) {
    for (const e of await registry()) {
        if (!e.enabled || !e.plugin?.name) continue;
        try {
            if (!e.plugin.match(url)) continue;
            const name = await e.plugin.name(url);
            if (name) return String(name).slice(0, 200);
        } catch { /* a broken plugin must not break the offer */ }
    }
    return null;
}

/** Send-time resolution. Returns {url, name?, headers?} or null (leave the
 * link untouched). Only plugins whose match(url) is true get asked. */
export async function pluginResolve(part) {
    const url = typeof part === "string" ? part : part?.url;
    if (!url) return null;
    for (const e of await registry()) {
        if (!e.enabled || !e.plugin?.resolve) continue;
        try {
            if (!e.plugin.match(url)) continue;
            const out = await e.plugin.resolve({ url, filename: (part && part.filename) || null }, { fetch: (u, o) => fetch(u, o) });
            if (out && typeof out.url === "string" && /^https?:\/\//i.test(out.url)) return out;
        } catch { /* fall through: the link queues as-is */ }
    }
    return null;
}
