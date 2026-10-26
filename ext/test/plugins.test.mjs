// Plugin-layer tests: recipe validation/engine + script shape + built-ins.
// Pure node (no browser, no network): node ext/test/plugins.test.mjs
"use strict";
import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import vm from "node:vm";

import generic from "../hosts/generic.js";
import { BUILTINS } from "../hosts/index.js";
import { pluginMatches, validateRecipe, recipeToPlugin, scriptProblem } from "../plugins.js";

let pass = 0;
const ok = (name) => { pass++; console.log(`  ok  ${name}`); };
const resp = (status, headers, body) => ({
    ok: status >= 200 && status < 300,
    status,
    headers: new Map(headers.map(([k, v]) => [k.toLowerCase(), v])),
    json: async () => body ?? {},
});

/* ---------------- built-ins ---------------- */

assert.ok(existsSync(new URL("../hosts/local/registry.js", import.meta.url)), "shipping extension must include its static script registry");
ok("package: static local registry exists");

assert.equal(BUILTINS.length, 1, "only generic ships built-in");
assert.ok(BUILTINS.includes(generic));
ok("registry: generic is the only built-in");

for (const url of [
    "https://host.example/Game.part1.rar",
    "https://host.example/a/b/movie.mkv#frag",
    "https://host.example/arc.zip?x=1",
]) assert.equal(generic.match(url), true, url);
for (const url of [
    "https://share.example/abc123#x.rar",
    "https://host.example/page",
    "https://host.example/",
]) assert.equal(generic.match(url), false, url);
ok("generic match: archives/media yes, pages no");

assert.equal(generic.name("https://host.example/a/My%20File.part2.rar"), "My File.part2.rar");
assert.equal(generic.name("https://host.example/"), null);
ok("generic name: decoded tail");

/* ---------------- enabled-plugin claims (node has no storage: builtins only) ---------------- */

assert.equal(await pluginMatches("https://host.example/Game.part1.rar"), true, "builtin file types are claimed");
assert.equal(await pluginMatches("https://host.example/page"), false);
assert.equal(await pluginMatches("https://host.example/abc123", true), false, "share links need a resolving plugin");
ok("pluginMatches: file types claimed; resolve-only filter skips them");

/* ---------------- script shape ---------------- */

assert.equal(scriptProblem({ default: { id: "x", match: () => true } }), null);
assert.match(scriptProblem({ default: { id: "x" } }), /match/);
assert.match(scriptProblem({ default: { id: "x", match: () => true, resolve: "yes" } }), /resolve/);
assert.match(scriptProblem({}), /default/);
ok("scriptProblem: shape enforced");

/* ---------------- recipe validation ---------------- */

const good = {
    id: "share.example",
    label: "Share link fixture",
    match: { host: ["share.example", "www.share.example"], path: "^[A-Za-z0-9]{12}$" },
    name: { fragment: true },
    resolve: {
        request: { method: "POST", url: "https://share.example/f/{path}/go", credentials: "include" },
        direct: { header: "x-download-location", prefix: "https://files.example/" },
    },
};

assert.deepEqual(Object.keys(validateRecipe(good)).sort(), ["id", "label", "match", "name", "resolve"]);
ok("recipe: valid shape normalizes");

const throws = (mutate, why) => {
    const r = structuredClone(good);
    mutate(r);
    assert.throws(() => validateRecipe(r), why);
};
throws((r) => { r.id = "BAD ID!"; }, /: id:/);
throws((r) => { delete r.id; }, /: id:/);
throws((r) => { delete r.match.host; }, /match\.host/);
throws((r) => { r.match.host = []; }, /match\.host/);
throws((r) => { r.match.host = ["a b.com"]; }, /match\.host/);
throws((r) => { r.match.path = "["; }, /match\.path/);
throws((r) => { r.name = { fragment: true, tail: true }; }, /: name:/);
throws((r) => { r.name = { query: 5 }; }, /name\.query/);
throws((r) => { r.resolve.request.method = "PUT"; }, /method/);
throws((r) => { r.resolve.request.url = "ftp://x/{path}"; }, /resolve\.request\.url/);
throws((r) => { r.resolve.request.url = "https://x/{nope}"; }, /placeholder/);
throws((r) => { delete r.resolve.direct.header; }, /resolve\.direct/);
throws((r) => { r.resolve.direct = { header: "a b" }; }, /resolve\.direct\.header/);
throws((r) => { r.resolve.direct = { json: "a..b" }; }, /resolve\.direct\.json/);
throws((r) => { r.resolve.direct.prefix = "ftp://x/"; }, /resolve\.direct\.prefix/);
throws((r) => { r.resolve.request.headers = { "bad header": "v" }; }, /headers/);
assert.doesNotThrow(() => validateRecipe({ ...structuredClone(good), resolve: undefined }), "resolve is optional");
ok("recipe: 16 validation failures carry helpful errors (optional resolve stays valid)");

/* ---------------- recipe engine (fake fetch) ---------------- */

const sharePlugin = recipeToPlugin(validateRecipe(good), async () => resp(200, [["x-download-location", "https://files.example/dl/tok1"]]));

assert.equal(sharePlugin.match("https://share.example/abc123def456#x.rar"), true);
assert.equal(sharePlugin.match("https://www.share.example/abc123def456#x.rar"), true);
assert.equal(sharePlugin.match("https://share.example/help"), false, "reserved path fails the regex");
assert.equal(sharePlugin.match("https://example.com/abc123"), false);
assert.equal(sharePlugin.match("https://files.example/dl/tok"), false, "direct host is not a share link");
ok("recipe match mirrors the script plugin");

assert.equal(sharePlugin.name("https://share.example/abc123def456#My%20File.part1.rar"), "My File.part1.rar");
assert.equal(sharePlugin.name("https://share.example/abc123def456"), null);
assert.equal(sharePlugin.name("https://example.com/abc#x.rar"), null);
ok("recipe name: fragment decoded, off-host null");

const calls = [];
const countingPlugin = recipeToPlugin(validateRecipe(good), async (url, init) => {
    calls.push({ url, init });
    return resp(200, [["x-download-location", "https://files.example/dl/tok1"]]);
});
let out = await countingPlugin.resolve({ url: "https://share.example/abc123def456#A.part1.rar" });
assert.equal(out.url, "https://files.example/dl/tok1");
assert.equal(out.name, "A.part1.rar");
assert.equal(calls[0].url, "https://share.example/f/abc123def456/go");
assert.equal(calls[0].init.method, "POST");
assert.equal(calls[0].init.credentials, "include");
ok("recipe resolve: POST template filled, header extracted, name kept");

const failing = recipeToPlugin(validateRecipe(good), async () => { throw new Error("net"); });
assert.equal(await failing.resolve({ url: "https://share.example/abc123def456" }), null, "network error");
assert.equal(await sharePlugin.resolve({ url: "https://example.com/abc123" }), null, "off-host");
const notOk = recipeToPlugin(validateRecipe(good), async () => resp(403, []));
assert.equal(await notOk.resolve({ url: "https://share.example/abc123def456" }), null, "non-success -> null");
const offPrefix = recipeToPlugin(validateRecipe(good), async () => resp(200, [["x-download-location", "https://evil.example/x"]]));
assert.equal(await offPrefix.resolve({ url: "https://share.example/abc123def456" }), null, "prefix guard");
const noHeader = recipeToPlugin(validateRecipe(good), async () => resp(200, []));
assert.equal(await noHeader.resolve({ url: "https://share.example/abc123def456" }), null, "missing header");
ok("recipe resolve: every failure degrades to null");

// json extraction variant
const jsonRecipe = recipeToPlugin(validateRecipe({
    ...good, id: "json.host", match: { host: "json.host" },
    resolve: { request: { method: "GET", url: "https://json.host/api/{path}" }, direct: { json: "download.url" } },
}), async () => resp(200, [], { download: { url: "https://cdn.json.host/f/file.bin" } }));
out = await jsonRecipe.resolve({ url: "https://json.host/abc123" });
assert.equal(out.url, "https://cdn.json.host/f/file.bin");
ok("recipe resolve: json dot-path extraction");

// tail + query name kinds
const tailRecipe = recipeToPlugin(validateRecipe({
    ...good, id: "tail.host", match: { host: "tail.host" }, name: { tail: true }, resolve: null,
}), async () => { throw new Error("none"); });
assert.equal(tailRecipe.name("https://tail.host/a/file%20name.zip"), "file name.zip");
assert.equal(await tailRecipe.resolve({ url: "https://tail.host/a/file.zip" }), null, "no resolve op -> null");
const queryRecipe = recipeToPlugin(validateRecipe({
    ...good, id: "q.host", match: { host: "q.host" }, name: { query: "f" }, resolve: null,
}), async () => { throw new Error("none"); });
assert.equal(queryRecipe.name("https://q.host/abc?f=part2.rar"), "part2.rar");
ok("recipe name kinds: tail and query");

const elements = new Map();
const element = (id) => {
    if (!elements.has(id)) elements.set(id, { textContent: "", value: "", className: "", addEventListener() {} });
    return elements.get(id);
};
const context = vm.createContext({
    document: { getElementById: element },
    chrome: {
        runtime: {
            getManifest: () => ({ version: "0.4.2" }),
            sendMessage: async () => { throw new Error("Receiving end does not exist."); },
        },
    },
});
vm.runInContext(readFileSync(new URL("../options.js", import.meta.url), "utf8"), context);
await vm.runInContext('importText(\'\')', context);
assert.match(element("import-status").textContent, /paste its JSON/);
await vm.runInContext('importText(\'export default {}\')', context);
assert.match(element("import-status").textContent, /JavaScript plugins/);
await vm.runInContext('importText(\'{"id":"fixture"}\')', context);
assert.match(element("import-status").textContent, /Cannot reach the plugin service/);
assert.equal(element("import-status").className, "bad");
assert.match(element("list").textContent, /Plugin service unavailable/);
ok("options: invalid input and disconnected worker show actionable errors");

console.log(`\n${pass} checks passed`);
