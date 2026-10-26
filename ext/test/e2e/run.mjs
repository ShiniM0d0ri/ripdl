// End-to-end run for ripdl: the CLI, the daemon, the browser extension and the
// GUI, one pass. Builds ripdl.exe, ripdl-daemon.exe and ripdl-gui.exe into
// target\e2e, then runs against a private daemon (own pipe, own TCP port, own
// data folder), a loopback fixture server, a native-messaging relay that starts
// the real bridge, and headless Edge with a copy of ext/. Nothing touches the
// installed ripdl, the user's queue, or the user's Downloads folder.
//
//   node ext/test/e2e/run.mjs                 build target\e2e, run every phase
//   node ext/test/e2e/run.mjs --no-build      reuse the existing target\e2e binaries
//   node ext/test/e2e/run.mjs --only=cli,ext  run a subset (phases: cli, ext, gui)
//
// Output: target\e2e\runs\<run>\report.json, logs, and screenshots in shots\.
// Network: fixtures, the relay and daemon RPC stay on loopback. The LAN check is
// the one exception: it opens the private daemon's drop box on all interfaces
// for a few seconds (free port, pairing code required, lan_open off) and turns
// it off again. Nothing calls out to the internet: the daemon's update check is
// off and Edge is pointed at a dead proxy.
import { execFile, spawn, spawnSync } from "node:child_process";
import crypto from "node:crypto";
import fs from "node:fs";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { collectErrors, connect, evaluate, getJson, sleep, until } from "./cdp.mjs";
import { startFixtures } from "./fixtures.mjs";
import { startRelay } from "./relay.mjs";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const ROOT = path.resolve(HERE, "..", "..", "..");
const EXT_SRC = path.join(ROOT, "ext");
const EXT_ID = "cimcjenbnacepkgebkmpkggjoahdmggc";
const KiB = 1024;
const MiB = 1024 * KiB;

const argv = process.argv.slice(2);
const flag = (name) => argv.includes(name);
const onlyArg = argv.find((a) => a.startsWith("--only=")) ?? "--only=cli,ext,gui";
const PHASES = new Set(onlyArg.slice("--only=".length).split(",").map((s) => s.trim()));

const BIN = path.join(ROOT, "target", "e2e", "debug");
const RIPDL = path.join(BIN, "ripdl.exe");
const DAEMON = path.join(BIN, "ripdl-daemon.exe");
const GUI = path.join(BIN, "ripdl-gui.exe");
const EDGE = [
    "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe",
    "C:\\Program Files\\Microsoft\\Edge\\Application\\msedge.exe",
].find((p) => fs.existsSync(p));

const RUN = crypto.randomBytes(3).toString("hex");
const RUN_DIR = path.join(ROOT, "target", "e2e", "runs", RUN);
const DIR = {
    data: path.join(RUN_DIR, "data"),
    downloads: path.join(RUN_DIR, "downloads"),
    popupDest: path.join(RUN_DIR, "popup-dest"),
    browserDl: path.join(RUN_DIR, "browser-downloads"),
    inbox: path.join(RUN_DIR, "inbox"),
    src: path.join(RUN_DIR, "src"),
    shots: path.join(RUN_DIR, "shots"),
    profile: path.join(RUN_DIR, "edge-profile"),
    guiProfile: path.join(RUN_DIR, "gui-profile"),
    ext: path.join(RUN_DIR, "ext"),
};
const PIPE = `\\\\.\\pipe\\ripdl-rpc-e2e-${RUN}`;
const GUI_PIPE = `\\\\.\\pipe\\ripdl-gui-e2e-${RUN}`;

// Run state, set by main().
let ENV = {};
let RPC_PORT = 0;
let LAN_PORT = 0;
let ORIGIN = "";
let FIXTURES = null;
let RELAY = null;
let daemon = null;
let NR_TASK = null;
let BIG_TASK = null;
const procs = [];
const shots = [];
const evidence = {};
const results = [];
const pageErrors = [];

const fx = (p) => ORIGIN + p;
const home = () => `${ORIGIN}/`;

function sha256(buf) {
    return crypto.createHash("sha256").update(buf).digest("hex");
}
function shaOf(file) {
    return sha256(fs.readFileSync(file));
}

// Deterministic-size, random-content fixtures. Served by name from every route.
const FILES = new Map();
function addFile(name, size, extra = {}) {
    const buf = crypto.randomBytes(size);
    FILES.set(name, { buf, size, sha: sha256(buf), ...extra });
}
addFile("big.bin", 8 * MiB);
addFile("nr.bin", 2 * MiB);
addFile("slow.bin", 6 * MiB, { throttleBps: 128 * KiB });
addFile("direct.bin", 512 * KiB);
for (const n of [1, 2, 3]) addFile(`bnd.part${n}.rar`, 512 * KiB);
addFile("pop.bin", 2 * MiB);
addFile("cap.bin", 1 * MiB);
addFile("fallback.bin", 1 * MiB);
addFile("ext-slow.bin", 6 * MiB, { throttleBps: 128 * KiB });
addFile("single.zip", 1 * MiB);
for (const n of [1, 2, 3]) addFile(`rel.part${n}.rar`, 1 * MiB);
for (const n of [1, 2, 3]) addFile(`named.part${n}.rar`, 1 * MiB);
for (const n of [1, 2]) addFile(`box.part${n}.rar`, 1 * MiB);

function pageHtml(origin) {
    const link = (p) => `${origin}${p}`;
    return `<!doctype html>
<html><head><meta charset="utf-8"><title>ripdl e2e ${RUN}</title></head>
<body style="font: 16px/1.6 sans-serif; margin: 24px">
<h1>ripdl e2e ${RUN}</h1>
<p id="prose">Release notes: this paragraph has no links, so copying it must not produce an offer.</p>
<p id="plain">${[1, 2, 3].map((n) => link(`/files/rel.part${n}.rar`)).join("\n")}</p>
<p id="named">Parts: <a href="/files/named.part1.rar">named.part1.rar</a> <a href="/files/named.part2.rar">named.part2.rar</a> <a href="/files/named.part3.rar">Download</a> <a href="/article">Read more</a></p>
<p id="single"><a href="/files/single.zip">single.zip</a></p>
<p id="slow"><a href="/slow/ext-slow.bin">ext-slow.bin</a></p>
<textarea id="box" rows="3" cols="70">${[1, 2].map((n) => link(`/files/box.part${n}.rar`)).join("\n")}</textarea>
</body></html>`;
}

// Rows of the duplicate window, as the page renders them.
const ROWS_JS = `[...document.querySelectorAll('#rows .row')].map((r) => ({
    name: (r.querySelector('.name') || {}).textContent || '',
    state: (r.querySelector('.state') || {}).textContent || '',
    buttons: [...r.querySelectorAll('button[data-action]')].map((b) => b.dataset.action + '|' + b.textContent.trim()),
    result: (r.querySelector('.result') || {}).textContent || '',
}))`;

function section(title) {
    console.log(`\n== ${title}`);
}

function check(name, ok, detail = "") {
    const pass = !!ok;
    results.push({ name, ok: pass, detail: String(detail) });
    const note = detail ? `  (${String(detail).replace(/\s+/g, " ").trim().slice(0, 180)})` : "";
    console.log(`${pass ? "PASS" : "FAIL"}  ${name}${note}`);
}

async function step(name, fn) {
    try {
        await fn();
    } catch (err) {
        check(name, false, `error: ${err.message}`);
    }
}

function sameFile(file, name) {
    return fs.existsSync(file) && shaOf(file) === FILES.get(name).sha;
}

function freePort() {
    return new Promise((resolve, reject) => {
        const srv = net.createServer();
        srv.unref();
        srv.on("error", reject);
        srv.listen(0, "127.0.0.1", () => {
            const { port } = srv.address();
            srv.close(() => resolve(port));
        });
    });
}

function alive(child) {
    return child.exitCode === null && child.signalCode === null;
}

function killTree(pid) {
    if (!pid) return;
    execFile("taskkill", ["/PID", String(pid), "/T", "/F"], { windowsHide: true }, () => {});
}

function spawnTracked(exe, args, env, logName) {
    const log = fs.openSync(path.join(RUN_DIR, logName), "a");
    const child = spawn(exe, args, { env, stdio: ["ignore", log, log], windowsHide: true });
    procs.push(child);
    return child;
}

function build() {
    console.log("building ripdl-cli, ripdl-daemon and ripdl-gui into target\\e2e ...");
    const r = spawnSync(
        "cargo",
        ["build", "-p", "ripdl-cli", "-p", "ripdl-daemon", "-p", "ripdl-gui", "--target-dir", "target/e2e"],
        { cwd: ROOT, stdio: "inherit" },
    );
    if (r.status !== 0) throw new Error(`cargo build failed (exit ${r.status})`);
}

// Appended to the private background.js only: records the request-record hook and the
// capture decision's inputs and outcome. No product branch is changed.
const TRACE_TAIL = `
function __ripdlTraceWebRequest(details) {
    const e = globalThis.__ripdlE2E;
    e.webRequests = e.webRequests || [];
    e.webRequests.push({ at: Date.now(), url: details.url, tabId: details.tabId, type: details.type });
    if (e.webRequests.length > 200) e.webRequests.shift();
}
function __ripdlTraceFilename(listener) {
    return (item, suggest) => {
        const e = globalThis.__ripdlE2E;
        e.filenameEvents = e.filenameEvents || [];
        e.filenameEvents.push({ at: Date.now(), id: item.id, url: item.url, finalUrl: item.finalUrl });
        if (e.filenameEvents.length > 200) e.filenameEvents.shift();
        return listener(item, suggest);
    };
}
async function __ripdlTraceDecide(item, ctx) {
    const e = globalThis.__ripdlE2E;
    e.decisions = e.decisions || [];
    const rec = {
        at: Date.now(), url: item.url, finalUrl: item.finalUrl, totalBytes: item.totalBytes,
        hostAlive, hasPort: !!port, owned: browserOwned.has(item.url) || browserOwned.has(item.finalUrl),
        pre: !!findRecord(item, false), reqHas: reqByUrl.has(item.url) || reqByUrl.has(item.finalUrl),
        stages: [],
    };
    settings().then((s) => { rec.capture = s.capture; });
    e.decisions.push(rec);
    const wrapped = {
        beginProposal(id) {
            const ok = ctx.beginProposal(id);
            rec.stages.push({ begin: id, ok, at: Date.now() });
            return ok;
        },
    };
    try {
        const out = await decideHandoff(item, wrapped);
        rec.result = out ? out.verdict : null;
        return out;
    } catch (err) {
        rec.error = String((err && err.message) || err);
        throw err;
    } finally {
        rec.doneAt = Date.now();
    }
}
`;

// Private copy of ext/ with the relay shim imported first. The shipped tree is untouched.
function prepareExtensionCopy(relayUrl) {
    fs.rmSync(DIR.ext, { recursive: true, force: true });
    fs.cpSync(EXT_SRC, DIR.ext, {
        recursive: true,
        filter: (src) => path.relative(EXT_SRC, src).split(path.sep)[0] !== "test",
    });
    const shim = fs.readFileSync(path.join(HERE, "shim.js"), "utf8").replace("__RELAY_URL__", relayUrl);
    fs.writeFileSync(path.join(DIR.ext, "e2e-shim.js"), shim);
    const bg = path.join(DIR.ext, "background.js");
    // Diagnostics only: the capture snapshot reads this module state (private copy, never shipped).
    const exposed = "\nglobalThis.__ripdlE2E.internals = { reqByUrl, reqByRequestId, browserOwned, findRecord, headersFor, get hostAlive() { return hostAlive; }, get port() { return port; } };\n" + TRACE_TAIL;
    let src = fs.readFileSync(bg, "utf8");
    const anchors = [
        ["function remember(details) {", "function remember(details) {\n    __ripdlTraceWebRequest(details);"],
        ["        decide: decideHandoff,", "        decide: __ripdlTraceDecide,"],
        ["chrome.downloads.onDeterminingFilename.addListener(\n    filenameListener({", "chrome.downloads.onDeterminingFilename.addListener(__ripdlTraceFilename(\n    filenameListener({"],
        ["        onDuplicate: onCaptureDuplicate,\n    }),\n);", "        onDuplicate: onCaptureDuplicate,\n    })),\n);"],
    ];
    for (const [anchor, to] of anchors) {
        if (src.split(anchor).length !== 2) throw new Error(`e2e trace anchor is not unique: ${anchor}`);
        src = src.replace(anchor, to);
    }
    fs.writeFileSync(bg, `import "./e2e-shim.js";\n${src}${exposed}`);
}

async function shotOf(conn, name) {
    const r = await conn.send("Page.captureScreenshot", { format: "png" });
    fs.writeFileSync(path.join(DIR.shots, name), Buffer.from(r.data, "base64"));
    shots.push(name);
    return name;
}

// The installed ripdl is observed, never driven: its daemon PIDs must survive the run and its queue must not change.
async function userDaemonState() {
    const out = await new Promise((resolve) =>
        execFile(
            "tasklist",
            ["/FI", "IMAGENAME eq ripdl-daemon.exe", "/FO", "CSV", "/NH"],
            { windowsHide: true },
            (_err, stdout) => resolve(stdout || ""),
        ),
    );
    const pids = [...out.matchAll(/"ripdl-daemon\.exe","(\d+)"/gi)].map((m) => m[1]).sort();
    const queue = path.join(process.env.LOCALAPPDATA || "", "ripdl", "queue.json");
    let stamp = "absent";
    try {
        const s = fs.statSync(queue);
        stamp = `${s.size}:${s.mtimeMs}`;
    } catch {
        /* no queue file */
    }
    return { pids, queue: stamp };
}

// Names, sizes and times in the user's Downloads folder. A run must leave it exactly as it found it.
function downloadsSnapshot() {
    const dir = path.join(os.homedir(), "Downloads");
    const seen = new Map();
    if (!fs.existsSync(dir)) return seen;
    for (const name of fs.readdirSync(dir)) {
        try {
            const s = fs.statSync(path.join(dir, name));
            seen.set(name, `${s.size}:${s.mtimeMs}`);
        } catch {
            seen.set(name, "unreadable");
        }
    }
    return seen;
}

function newDownloads(before, after) {
    return [...after.keys()].filter((name) => before.get(name) !== after.get(name));
}

// ---- CLI and RPC helpers ----------------------------------------------------

function runRipdl(args, timeout = 120000) {
    return new Promise((resolve) => {
        execFile(RIPDL, args, { env: ENV, windowsHide: true, timeout, maxBuffer: 16 * MiB }, (err, stdout, stderr) => {
            const code = err ? (typeof err.code === "number" ? err.code : 1) : 0;
            resolve({ code, out: `${stdout || ""}${stderr || ""}` });
        });
    });
}

// One request over the TCP fallback, authenticated with the run's token file.
function rpc(cmd) {
    return new Promise((resolve, reject) => {
        const token = fs.readFileSync(path.join(DIR.data, "rpc.token"), "utf8").trim();
        const sock = net.connect({ host: "127.0.0.1", port: RPC_PORT });
        let buf = "";
        const timer = setTimeout(() => {
            sock.destroy();
            reject(new Error("rpc timed out"));
        }, 60000);
        sock.setEncoding("utf8");
        sock.on("connect", () => {
            sock.write(`${JSON.stringify({ auth: token })}\n${JSON.stringify({ id: 1, cmd })}\n`);
        });
        sock.on("data", (chunk) => {
            buf += chunk;
            const nl = buf.indexOf("\n");
            if (nl < 0) return;
            clearTimeout(timer);
            sock.destroy();
            try {
                resolve(JSON.parse(buf.slice(0, nl)).reply);
            } catch (err) {
                reject(err);
            }
        });
        sock.on("error", (err) => {
            clearTimeout(timer);
            reject(err);
        });
        sock.on("close", () => {
            clearTimeout(timer);
            reject(new Error("rpc closed before a reply"));
        });
    });
}

// Raw lines without a token step, to check what the daemon says to a stranger.
function rawRpc(lines, ms = 2500) {
    return new Promise((resolve) => {
        const sock = net.connect({ host: "127.0.0.1", port: RPC_PORT });
        let buf = "";
        const timer = setTimeout(() => sock.destroy(), ms);
        sock.setEncoding("utf8");
        sock.on("connect", () => sock.write(lines.map((l) => `${l}\n`).join("")));
        sock.on("data", (chunk) => {
            buf += chunk;
        });
        sock.on("close", () => {
            clearTimeout(timer);
            resolve(buf);
        });
        sock.on("error", () => {
            clearTimeout(timer);
            resolve(buf);
        });
    });
}

async function tasks() {
    return (await rpc({ type: "list" })).tasks || [];
}

async function taskById(id) {
    return (await tasks()).find((t) => t.id === id);
}

function waitTask(id, pred, ms = 90000) {
    return until(
        `task ${id}`,
        async () => {
            const t = await taskById(id);
            return t && pred(t) ? t : null;
        },
        ms,
        250,
    );
}

function parseId(out) {
    const m = /[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/i.exec(out || "");
    return m ? m[0] : null;
}

async function startDaemon() {
    daemon = spawnTracked(DAEMON, [], ENV, "daemon.log");
    await until("the private daemon to answer", async () => (await runRipdl(["ls"], 5000)).code === 0, 30000, 300);
}

async function stopDaemon() {
    if (!daemon) return;
    await runRipdl(["stop"], 15000);
    await until("the private daemon to exit", async () => (await runRipdl(["ls"], 3000)).code !== 0, 20000, 300).catch(
        () => {},
    );
    if (alive(daemon)) killTree(daemon.pid);
    daemon = null;
}

// Every phase runs against the run's own folders. Extension and GUI adds leave dest unset, so the daemon
// default must point at the run folder before any phase starts, or those adds land in the user's Downloads.
async function isolateDaemon() {
    const set = await runRipdl([
        "config",
        `default_download_dir=${DIR.downloads}`,
        `lan_dir=${DIR.inbox}`,
        `lan_port=${LAN_PORT}`,
        "lan_open=false",
        "update_check=false",
    ]);
    check("the private daemon is isolated to the run folders", set.code === 0, set.out.trim());
    if (set.code !== 0) throw new Error("the private daemon could not be isolated; no phase was started");
}

// ---- phases -----------------------------------------------------------------

async function cliPhase() {
    section("CLI and private daemon");

    await step("A0 private daemon answers on its own pipe", async () => {
        const r = await runRipdl(["ls"]);
        check("A0 private daemon answers on its own pipe", r.code === 0, r.out.trim());
    });

    await step("A1 settings applied", async () => {
        // isolateDaemon() applied these before any phase ran; A1 reads them back.
        const shown = (await runRipdl(["config"])).out;
        check("A1 download folder is the run folder", shown.includes(DIR.downloads));
        check("A1 update check is off (no network from the daemon)", /update_check:\s+false/.test(shown));
    });

    await step("A2 multi-segment HTTP download", async () => {
        const r = await runRipdl(["add", fx("/files/big.bin"), "--dest", DIR.downloads]);
        const id = parseId(r.out);
        check("A2 add returns a task id", !!id, r.out.trim());
        BIG_TASK = id;
        await waitTask(id, (t) => t.state === "completed");
        check("A2 big.bin matches its SHA-256", sameFile(path.join(DIR.downloads, "big.bin"), "big.bin"));
        const ranged = FIXTURES.hits().filter((h) => h.path === "/files/big.bin" && h.range).length;
        check("A2 the engine used byte-range requests", ranged > 1, `${ranged} ranged requests`);
    });

    await step("A3 server without range support: one stream", async () => {
        const r = await runRipdl(["add", fx("/norange/nr.bin"), "--dest", DIR.downloads]);
        NR_TASK = parseId(r.out);
        await waitTask(NR_TASK, (t) => t.state === "completed");
        check("A3 nr.bin matches its SHA-256", sameFile(path.join(DIR.downloads, "nr.bin"), "nr.bin"));
    });

    await step("A4 RPC refuses a wrong token and serves the right one", async () => {
        const raw = await rawRpc([JSON.stringify({ auth: "not-the-token" }), JSON.stringify({ id: 9, cmd: { type: "list" } })]);
        check("A4 a wrong token gets no task list", !raw.includes('"tasks"'), raw.trim().slice(0, 120));
        const listed = await rpc({ type: "list" });
        check("A4 the right token gets the task list", Array.isArray(listed.tasks), listed.type);
    });

    await step("A5 pause parks a running download; refresh and resume complete it", async () => {
        const r = await runRipdl(["add", fx("/slow/slow.bin"), "--dest", DIR.downloads]);
        const id = parseId(r.out);
        await waitTask(id, (t) => (t.downloaded || 0) > 64 * KiB, 60000);
        await runRipdl(["pause", id]);
        const parked = await waitTask(id, (t) => t.state === "paused", 30000);
        check("A5 paused task keeps partial bytes", parked.downloaded > 0 && parked.downloaded < parked.total, `${parked.downloaded} of ${parked.total}`);
        await sleep(1500);
        const still = await taskById(id);
        check("A5 paused task does not move", still.state === "paused" && still.downloaded === parked.downloaded);
        const rf = await runRipdl(["refresh", id, fx("/slow/slow.bin?sig=2"), "--resume"]);
        check("A5 refresh accepts the same file at a new address", rf.code === 0, rf.out.trim().slice(0, 140));
        await waitTask(id, (t) => t.state === "completed", 90000);
        check("A5 resumed download matches its SHA-256", sameFile(path.join(DIR.downloads, "slow.bin"), "slow.bin"));
    });

    await step("A6 the same URL again returns the existing task, not a second one", async () => {
        const before = (await tasks()).length;
        const r = await runRipdl(["add", fx("/files/big.bin"), "--dest", DIR.downloads]);
        check("A6 the default share policy answers with the existing id", !!BIG_TASK && parseId(r.out) === BIG_TASK, r.out.trim());
        check("A6 the queue did not grow", (await tasks()).length === before);
    });

    await step("A7 rm --files removes the finished file and its task", async () => {
        const r = await runRipdl(["rm", NR_TASK, "--files"]);
        check("A7 rm accepted", r.code === 0, r.out.trim());
        check("A7 the file is gone", !fs.existsSync(path.join(DIR.downloads, "nr.bin")));
        check("A7 the task is gone", !(await taskById(NR_TASK)));
    });

    await step("A8 invalid settings are rejected", async () => {
        const num = await runRipdl(["config", "max_segments=abc"]);
        check("A8 a non-number is refused", num.code !== 0 && /needs a number/.test(num.out), num.out.trim().slice(0, 140));
        const key = await runRipdl(["config", "bogus_key=1"]);
        check("A8 an unknown setting is refused", key.code !== 0 && /unknown setting/.test(key.out), key.out.trim().slice(0, 140));
    });

    await step("A9 bundle groups parts into one folder task", async () => {
        const urls = [1, 2, 3].map((n) => fx(`/files/bnd.part${n}.rar`));
        const pv = await runRipdl(["bundle", ...urls, "--preview"]);
        check("A9 preview groups the three parts", pv.code === 0 && /bnd/.test(pv.out), pv.out.trim().slice(0, 160));
        const before = (await tasks()).length;
        const r = await runRipdl(["bundle", ...urls, "--name", "bnd-set", "--dest", DIR.downloads]);
        const id = parseId(r.out);
        check("A9 the bundle is one new task", !!id && (await tasks()).length === before + 1, r.out.trim().slice(0, 120));
        await waitTask(id, (t) => t.state === "completed");
        const parts = [1, 2, 3].map((n) => path.join(DIR.downloads, "bnd-set", `bnd.part${n}.rar`));
        check(
            "A9 all three parts land in bnd-set with matching SHA-256",
            parts.every((p, i) => sameFile(p, `bnd.part${i + 1}.rar`)),
        );
    });

    await step("A11 direct download and info need no queue", async () => {
        const out = path.join(RUN_DIR, "direct");
        fs.mkdirSync(out, { recursive: true });
        const before = (await tasks()).length;
        const info = await runRipdl(["info", fx("/files/direct.bin")]);
        check("A11 info plans the URL", info.code === 0 && info.out.includes("direct.bin"));
        const dl = await runRipdl(["download", fx("/files/direct.bin"), "--out", out]);
        check("A11 direct download matches its SHA-256", dl.code === 0 && sameFile(path.join(out, "direct.bin"), "direct.bin"), dl.out.trim().slice(-120));
        check("A11 neither added a task", (await tasks()).length === before);
    });

    await step("A12 LAN drop box: wrong code refused, right code delivers", async () => {
        await runRipdl(["config", "lan_enabled=true"]);
        const info = await until(
            "the LAN receiver to listen",
            async () => {
                const r = await runRipdl(["lan", "info"]);
                return /^receive:\s+on$/m.test(r.out) && /^code:\s+[A-Za-z0-9]{4,}\s*$/m.test(r.out) ? r.out : null;
            },
            20000,
            300,
        );
        const code = /^code:\s+([A-Za-z0-9]{4,})\s*$/m.exec(info)[1];
        check("A12 receiver is on and shows a pairing code", true, `code ${code}`);
        const html = await fetch(`http://127.0.0.1:${LAN_PORT}/`).then((r) => r.text()).catch(() => "");
        check("A12 the receive page does not reveal the pairing code", html.length > 0 && !html.includes(code), `${html.length} bytes`);
        const src = path.join(DIR.src, "lan.bin");
        const srcBuf = crypto.randomBytes(512 * KiB);
        fs.writeFileSync(src, srcBuf);
        const target = `http://127.0.0.1:${LAN_PORT}`;
        const bad = await runRipdl(["lan", "send", src, "--to", target, "--code", "ZZZZZZ"]);
        check("A12 a wrong pairing code is refused", bad.code !== 0, bad.out.trim().slice(0, 140));
        check("A12 a refused copy leaves nothing in the inbox", !fs.existsSync(path.join(DIR.inbox, "lan.bin")));
        const good = await runRipdl(["lan", "send", src, "--to", target, "--code", code], 120000);
        check("A12 the right pairing code delivers", good.code === 0, good.out.trim().split("\n").pop());
        const landed = path.join(DIR.inbox, "lan.bin");
        await until("the LAN file in the inbox", () => fs.existsSync(landed) && fs.statSync(landed).size === srcBuf.length, 30000, 300);
        check("A12 the received file matches the sent SHA-256", shaOf(landed) === sha256(srcBuf));
        await runRipdl(["config", "lan_enabled=false"]);
    });
}

async function extPhase() {
    section("Extension in headless Edge (copy of ext/, real bridge, private daemon)");
    // Browser downloads go to the run folder via the per-run profile. CDP's Browser.setDownloadBehavior
    // would route them through DevTools' download delegate, which skips the extension's download events.
    fs.mkdirSync(DIR.browserDl, { recursive: true });
    fs.mkdirSync(path.join(DIR.profile, "Default"), { recursive: true });
    fs.writeFileSync(
        path.join(DIR.profile, "Default", "Preferences"),
        JSON.stringify({
            download: { default_directory: DIR.browserDl, prompt_for_download: false, directory_upgrade: true },
            savefile: { default_directory: DIR.browserDl },
        }),
    );
    const cdpPort = await freePort();
    spawnTracked(
        EDGE,
        [
            "--headless=new",
            `--remote-debugging-port=${cdpPort}`,
            "--remote-allow-origins=*",
            `--user-data-dir=${DIR.profile}`,
            `--load-extension=${DIR.ext}`,
            "--no-first-run",
            "--no-default-browser-check",
            "--disable-background-networking",
            "--disable-component-update",
            "--disable-sync",
            "--proxy-server=127.0.0.1:9",
            "--proxy-bypass-list=127.0.0.1;localhost",
            "about:blank",
        ],
        process.env,
        "edge.log",
    );
    const cdp = `http://127.0.0.1:${cdpPort}`;
    const pages = () => getJson(`${cdp}/json/list`).catch(() => []);
    const version = await until("Edge DevTools", () => getJson(`${cdp}/json/version`), 90000, 300);
    const browser = connect(version.webSocketDebuggerUrl);
    await browser.ready;
    const conns = [browser];
    try {

        // Edge ships its own extension workers, so probe each one for the pinned ID.
        const swTarget = await until(
            "the ripdl service worker",
            async () => {
                for (const t of (await pages()).filter((x) => x.type === "service_worker" && x.url.includes("background.js"))) {
                    const probe = connect(t.webSocketDebuggerUrl);
                    try {
                        await probe.ready;
                        if ((await evaluate(probe, "chrome.runtime.id")) === EXT_ID) return t;
                    } catch {
                        /* another extension's worker */
                    } finally {
                        probe.close();
                    }
                }
                return null;
            },
            90000,
            500,
        ).catch(async (err) => {
            const seen = (await pages()).map((x) => `${x.type} ${x.url}`).join(" | ");
            throw new Error(`${err.message}; targets seen: ${seen || "none"}`);
        });
        const sw = connect(swTarget.webSocketDebuggerUrl);
        await sw.ready;
        conns.push(sw);
        collectErrors(sw, "service worker", pageErrors);
        await sw.send("Runtime.enable");
        const shimState = await evaluate(sw, "typeof globalThis.__ripdlE2E");
        const shimErrors = shimState === "object" ? await evaluate(sw, "JSON.stringify(globalThis.__ripdlE2E.errors)") : "n/a";
        check("B0 the ripdl service worker runs with the e2e shim", shimState === "object", `typeof __ripdlE2E = ${shimState}; observer errors ${shimErrors}`);
        const hello = await until(
            "extension_hello at the daemon",
            async () => {
                const cfg = await rpc({ type: "get_config" });
                return cfg.extension && cfg.extension.last_seen > 0 ? cfg.extension : null;
            },
            60000,
            500,
        ).catch(() => null);
        check("B0 the extension's hello reached the daemon", !!hello, JSON.stringify(hello));
        evidence.hello = hello;

        const pageTarget = await until(
            "the first page target",
            async () => (await pages()).find((t) => t.type === "page" && t.url.startsWith("about:")),
            30000,
            200,
        );
        const pg = connect(pageTarget.webSocketDebuggerUrl);
        await pg.ready;
        conns.push(pg);
        collectErrors(pg, "page", pageErrors);
        await pg.send("Page.enable");
        await pg.send("Runtime.enable");
        await pg.send("Emulation.setDeviceMetricsOverride", { width: 1000, height: 800, deviceScaleFactor: 1, mobile: false });
        await pg.send("Emulation.setFocusEmulationEnabled", { enabled: true });
        await pg.send("Page.bringToFront");

        const gotoHome = async () => {
            await pg.send("Page.navigate", { url: home() });
            await until("the fixture page", async () => (await evaluate(pg, "location.href + ' ' + document.readyState")) === `${home()} complete`, 30000, 200);
            await sleep(500);
            await evaluate(pg, "window.__copies = 0; document.addEventListener('copy', () => { window.__copies++; }, true); true");
        };
        await gotoHome();

        const squash = (s) => String(s || "").replace(/\s+/g, " ").trim();
        const chip = () =>
            evaluate(
                pg,
                `(() => {
                    const h = document.querySelector('ripdl-offer-chip');
                    if (!h || !h.shadowRoot) return null;
                    return {
                        text: h.shadowRoot.textContent,
                        buttons: [...h.shadowRoot.querySelectorAll('button')].map((b) => {
                            const q = b.getBoundingClientRect();
                            return { label: b.textContent.trim(), x: q.left + q.width / 2, y: q.top + q.height / 2 };
                        }),
                    };
                })()`,
            );
        const offerReady = async () => {
            const c = await chip();
            return c && /Send \d+ links? to ripdl/.test(c.text) ? c : null;
        };
        const queuedReady = async () => {
            const c = await chip();
            return c && /queued \d/.test(c.text) ? c : null;
        };
        const gone = async () => (await chip()) === null;
        const selectNode = (id) =>
            evaluate(
                pg,
                `(() => {
                    const active = document.activeElement;
                    if (active && active !== document.body) active.blur();
                    const r = document.createRange();
                    r.selectNodeContents(document.getElementById(${JSON.stringify(id)}));
                    const s = getSelection();
                    s.removeAllRanges();
                    s.addRange(r);
                    return s.toString().length;
                })()`,
            );
        const selectBox = () =>
            evaluate(
                pg,
                `(() => {
                    const t = document.getElementById('box');
                    t.focus();
                    t.select();
                    return t.selectionEnd - t.selectionStart;
                })()`,
            );
        const copyKeys = async () => {
            const before = await evaluate(pg, "window.__copies");
            const key = { modifiers: 2, key: "c", code: "KeyC", windowsVirtualKeyCode: 67, nativeVirtualKeyCode: 67 };
            await pg.send("Input.dispatchKeyEvent", { type: "keyDown", commands: ["copy"], ...key });
            await pg.send("Input.dispatchKeyEvent", { type: "keyUp", ...key });
            if ((await evaluate(pg, "window.__copies")) > before) return { fired: true, via: "ctrl+c" };
            await evaluate(pg, "document.execCommand('copy')");
            const after = await evaluate(pg, "window.__copies");
            return { fired: after > before, via: "execCommand fallback" };
        };
        const clickChip = async (label) => {
            const c = await chip();
            const b = c && c.buttons.find((x) => x.label === label);
            if (!b) throw new Error(`no "${label}" button in the chip`);
            await pg.send("Input.dispatchMouseEvent", { type: "mouseMoved", x: b.x, y: b.y });
            await pg.send("Input.dispatchMouseEvent", { type: "mousePressed", x: b.x, y: b.y, button: "left", buttons: 1, clickCount: 1 });
            await pg.send("Input.dispatchMouseEvent", { type: "mouseReleased", x: b.x, y: b.y, button: "left", buttons: 0, clickCount: 1 });
        };
        const pressEscape = async () => {
            const key = { key: "Escape", code: "Escape", windowsVirtualKeyCode: 27, nativeVirtualKeyCode: 27 };
            await pg.send("Input.dispatchKeyEvent", { type: "keyDown", ...key });
            await pg.send("Input.dispatchKeyEvent", { type: "keyUp", ...key });
        };

        // Extension pages (popup, options) opened as real tabs through the browser endpoint.
        const openExtPage = async (relPath) => {
            const { targetId } = await browser.send("Target.createTarget", { url: `chrome-extension://${EXT_ID}/${relPath}` });
            const target = await until(`${relPath} target`, async () => (await pages()).find((t) => t.id === targetId), 20000, 200);
            const conn = connect(target.webSocketDebuggerUrl);
            await conn.ready;
            conns.push(conn);
            await conn.send("Page.enable");
            await conn.send("Runtime.enable");
            // The target opens on about:blank and then navigates to the extension page.
            // Nothing may touch chrome.* until the extension document is in place.
            await until(
                `${relPath} to load`,
                () => evaluate(conn, "location.protocol === 'chrome-extension:' && !!(chrome.runtime && chrome.runtime.id)"),
                15000,
                200,
            );
            collectErrors(conn, relPath, pageErrors);
            return {
                conn,
                close: async () => {
                    conn.close();
                    await browser.send("Target.closeTarget", { targetId }).catch(() => {});
                },
            };
        };
        const popupAdd = async (url, destDir) => {
            const popup = await openExtPage("popup.html");
            const reply = await evaluate(
                popup.conn,
                `chrome.runtime.sendMessage(${JSON.stringify({ type: "ripdl_send", cmd: { type: "add", url, dest_dir: destDir, headers: {} } })})`,
            );
            await popup.close();
            return reply;
        };

        // The duplicate window is a normal extension page: find it, read its rows, press its buttons.
        const dupWindow = () =>
            until("the duplicate window", async () => (await pages()).find((t) => t.type === "page" && t.url.includes("/duplicate.html")), 20000, 250).catch(() => null);
        const closeDupWindows = async () => {
            for (const t of (await pages()).filter((x) => x.type === "page" && x.url.includes("/duplicate.html"))) {
                await getJson(`${cdp}/json/close/${t.id}`).catch(() => {});
            }
            await sleep(300);
        };
        const openPanel = async (target) => {
            const conn = connect(target.webSocketDebuggerUrl);
            await conn.ready;
            conns.push(conn);
            await conn.send("Runtime.enable");
            collectErrors(conn, "duplicate window", pageErrors);
            return {
                conn,
                rows: () => evaluate(conn, ROWS_JS),
                waitRows: () => until("the duplicate rows", async () => {
                    const r = await evaluate(conn, ROWS_JS);
                    return r.length ? r : null;
                }, 10000),
                click: (action, index = 0) =>
                    evaluate(
                        conn,
                        `(() => { const row = document.querySelectorAll('#rows .row')[${index}]; const b = row && row.querySelector('button[data-action="${action}"]'); if (!b) return false; b.click(); return true; })()`,
                    ),
            };
        };

        // B2 to B7: the copy-capture flows, as the verified harness had them.
        await step("B2 prose copy: no offer", async () => {
            await selectNode("prose");
            const k = await copyKeys();
            check("B2 copy event fired on the prose selection", k.fired, k.via);
            await sleep(1500);
            check("B2 prose copy shows no chip", (await chip()) === null);
        });

        await step("B3 pasted URL list -> one bundle of 3", async () => {
            await selectNode("plain");
            const k = await copyKeys();
            check("B3 copy event fired", k.fired, k.via);
            const c = await until("the B3 offer", offerReady, 10000);
            evidence.b3Offer = squash(c.text);
            check("B3 chip offers 3 links", c.text.includes("Send 3 links to ripdl"), squash(c.text));
            check("B3 chip groups the parts as one set", c.text.includes("rel - 3 of 3 parts"));
            await shotOf(pg, "b3-offer.png");
            await clickChip("Send");
            const q = await until("the B3 queued status", queuedReady, 15000);
            evidence.b3Status = squash(q.text);
            check("B3 Send reports one bundle", q.text.includes("queued 1 bundle"), squash(q.text));
            await shotOf(pg, "b3-queued.png");
            check("B3 chip closes after the send", await until("the B3 chip to close", gone, 6000));
        });

        await step("B4 named anchors and a Download label -> one bundle", async () => {
            await selectNode("named");
            const k = await copyKeys();
            check("B4 copy event fired", k.fired, k.via);
            const c = await until("the B4 offer", offerReady, 10000);
            check("B4 chip offers 3 links (prose link excluded)", c.text.includes("Send 3 links to ripdl"), squash(c.text));
            check("B4 chip groups the named parts as one set", c.text.includes("named - 3 of 3 parts"));
            await clickChip("Send");
            const q = await until("the B4 queued status", queuedReady, 15000);
            check("B4 Send reports one bundle", q.text.includes("queued 1 bundle"), squash(q.text));
            check("B4 chip closes after the send", await until("the B4 chip to close", gone, 6000));
        });

        await step("B5 textarea selection offers; Escape dismisses without sending", async () => {
            await selectBox();
            const k = await copyKeys();
            check("B5 copy event fired in the textarea", k.fired, k.via);
            const c = await until("the B5 offer", offerReady, 10000);
            check("B5 textarea offers 2 links", c.text.includes("Send 2 links to ripdl") && c.text.includes("box - 2 of 2 parts"), squash(c.text));
            await pressEscape();
            check("B5 Escape closes the chip", await until("the B5 chip to close", gone, 3000));
        });

        await step("B6 Dismiss button closes the offer without sending", async () => {
            await selectNode("plain");
            await copyKeys();
            await until("the B6 offer", offerReady, 10000);
            await clickChip("Dismiss");
            check("B6 Dismiss closes the chip", await until("the B6 chip to close", gone, 3000));
        });

        await step("B7 one link -> one download", async () => {
            await selectNode("single");
            const k = await copyKeys();
            check("B7 copy event fired", k.fired, k.via);
            const c = await until("the B7 offer", offerReady, 10000);
            check("B7 chip offers 1 link as a single download", c.text.includes("Send 1 link to ripdl") && c.text.includes("1 single download"), squash(c.text));
            await clickChip("Send");
            const q = await until("the B7 queued status", queuedReady, 15000);
            check("B7 Send reports one download", q.text.includes("queued 1 download"), squash(q.text));
            check("B7 chip closes after the send", await until("the B7 chip to close", gone, 6000));
        });

        await step("B8 popup page: ripdl_send add with dest_dir", async () => {
            const reply = await popupAdd(fx("/files/pop.bin"), DIR.popupDest);
            check("B8 popup message accepted by the native host", !!(reply && reply.ok), JSON.stringify(reply));
            const landed = await until("the popup download", () => sameFile(path.join(DIR.popupDest, "pop.bin"), "pop.bin"), 60000, 300).then(
                () => true,
                () => false,
            );
            check("B8 popup download landed in dest_dir and matches", landed);
        });

        await step("B9 copying a finished link opens the duplicate window", async () => {
            await closeDupWindows();
            await selectNode("single");
            await copyKeys();
            await until("the B9 offer", offerReady, 10000);
            await clickChip("Send");
            const dup = await dupWindow();
            check("B9 duplicate window opened for the existing download", !!dup, dup ? dup.url : "none");
            if (!dup) return;
            const panel = await openPanel(dup);
            const rows = await panel.waitRows();
            evidence.b9Rows = rows;
            check(
                "B9 the window names the file and offers Download again",
                rows.length === 1 && rows[0].buttons.some((b) => b.startsWith("copy|Download again")),
                JSON.stringify(rows[0] && rows[0].buttons),
            );
            check("B9 the window shows the finished state", /Completed/.test(rows[0].state), rows[0].state);
            await shotOf(panel.conn, "b9-duplicate-completed.png");
            check("B9 Download again pressed", await panel.click("copy"));
            const copy = path.join(DIR.downloads, "single (1).zip");
            const copied = await until("the B9 second copy", () => sameFile(copy, "single.zip"), 60000, 300).then(
                () => true,
                () => false,
            );
            check("B9 the second copy gets a fresh name and matches", copied, path.basename(copy));
            check("B9 the original file is untouched", sameFile(path.join(DIR.downloads, "single.zip"), "single.zip"));
            panel.conn.close();
        });

        await step("B10 paused download: the duplicate window offers Resume", async () => {
            const reply = await popupAdd(fx("/slow/ext-slow.bin"), DIR.downloads);
            check("B10 popup queued the slow download", !!(reply && reply.ok), JSON.stringify(reply));
            const running = await until(
                "the B10 task",
                async () => (await tasks()).find((t) => t.url === fx("/slow/ext-slow.bin") && (t.downloaded || 0) > 64 * KiB),
                60000,
                250,
            );
            await runRipdl(["pause", running.id]);
            await waitTask(running.id, (t) => t.state === "paused", 30000);
            await closeDupWindows();
            await selectNode("slow");
            await copyKeys();
            await until("the B10 offer", offerReady, 10000);
            await clickChip("Send");
            const dup = await dupWindow();
            check("B10 duplicate window opened for the paused download", !!dup);
            if (!dup) return;
            const panel = await openPanel(dup);
            const rows = await panel.waitRows();
            evidence.b10Rows = rows;
            check(
                "B10 the window says Paused and offers Resume",
                /Paused/.test(rows[0].state) && rows[0].buttons.some((b) => b.startsWith("resume|")),
                `${rows[0].state} | ${rows[0].buttons.join(", ")}`,
            );
            await shotOf(panel.conn, "b10-duplicate-paused.png");
            check("B10 Resume pressed", await panel.click("resume"));
            await waitTask(running.id, (t) => t.state === "completed", 90000);
            check("B10 the resumed download matches its SHA-256", sameFile(path.join(DIR.downloads, "ext-slow.bin"), "ext-slow.bin"));
            panel.conn.close();
        });

        // Diagnostics for a failed capture check: what the service worker knows about a URL.
        // Read-only; uses the private copy's internals and the shim's download events.
        const captureState = (urlPath) =>
            evaluate(
                sw,
                `(async () => {
                    const e = globalThis.__ripdlE2E || {};
                    const x = e.internals || {};
                    const want = ${JSON.stringify(urlPath)};
                    const hit = (u) => (u || "").includes(want);
                    const items = (await chrome.downloads.search({})).filter((d) => hit(d.url) || hit(d.finalUrl));
                    const rows = [];
                    for (const d of items) {
                        let record = null;
                        let headers = null;
                        try {
                            record = x.findRecord ? !!x.findRecord(d, true) : null;
                            const rec = record && x.findRecord(d, true);
                            headers = rec ? await x.headersFor(d, rec) : null;
                        } catch (err) {
                            headers = "throws: " + err.message;
                        }
                        rows.push({ id: d.id, state: d.state, url: d.url, finalUrl: d.finalUrl, totalBytes: d.totalBytes, error: d.error || null, record, headers });
                    }
                    return JSON.stringify({
                        hostAlive: x.hostAlive ?? null,
                        port: !!x.port,
                        stored: await chrome.storage.local.get(["capture", "hostAlive"]),
                        requests: x.reqByUrl ? [...x.reqByUrl.keys()].filter(hit) : null,
                        owned: x.browserOwned ? [...x.browserOwned.keys()].filter(hit) : null,
                        events: (e.events || []).filter((ev) => ev.kind === "determining" || hit(ev.url) || hit(ev.finalUrl)).slice(-8),
                        filenameEvents: (e.filenameEvents || []).filter((f) => hit(f.url) || hit(f.finalUrl)).slice(-6),
                        decisions: (e.decisions || []).slice(-6),
                        webRequests: (e.webRequests || []).filter((w) => hit(w.url)).slice(-6),
                        items: rows,
                    });
                })()`,
            ).catch((err) => `snapshot failed: ${err.message}`);

        await step("B11 a browser download is captured by ripdl", async () => {
            await closeDupWindows();
            await pg.send("Page.navigate", { url: fx("/dl/cap.bin") });
            const got = await until("the captured file", () => sameFile(path.join(DIR.downloads, "cap.bin"), "cap.bin"), 60000, 300).then(
                () => true,
                () => false,
            );
            check("B11 the captured download lands in ripdl's folder and matches", got, got ? "" : await captureState("/dl/cap.bin"));
            await sleep(2000);
            check("B11 the browser kept no copy", !fs.existsSync(path.join(DIR.browserDl, "cap.bin")));
            await gotoHome();
        });

        await step("B12 a browser download of a finished file opens the duplicate window", async () => {
            await closeDupWindows();
            await pg.send("Page.navigate", { url: fx("/dl/cap.bin") });
            const dup = await dupWindow();
            check("B12 duplicate window opened for the captured file", !!dup, dup ? "" : await captureState("/dl/cap.bin"));
            if (dup) {
                const panel = await openPanel(dup);
                const rows = await panel.waitRows();
                evidence.b12Rows = rows;
                check("B12 the window offers Download again", rows.some((r) => r.buttons.some((b) => b.startsWith("copy|"))), JSON.stringify(rows.map((r) => r.buttons)));
                await shotOf(panel.conn, "b12-duplicate-capture.png");
                panel.conn.close();
            }
            await sleep(1500);
            check("B12 the browser copy was stopped", !fs.existsSync(path.join(DIR.browserDl, "cap.bin")));
            await closeDupWindows();
            await gotoHome();
        });

        await step("B13 options page renders", async () => {
            const opt = await openExtPage("options.html");
            await sleep(1000);
            const info = await evaluate(opt.conn, "document.title + ' | ' + document.body.innerText.length");
            check("B13 options page has content", /\| \d+$/.test(info) && !/\| 0$/.test(info), info);
            await shotOf(opt.conn, "b13-options.png");
            await opt.close();
        });

        await step("B14 daemon down: the browser keeps its own copy", async () => {
            await stopDaemon();
            check("B14 the private daemon is stopped", (await runRipdl(["ls"], 4000)).code !== 0);
            await pg.send("Page.navigate", { url: fx("/dl/fallback.bin") });
            // The native host respawns a sibling daemon when none is listening (bridge.rs), so the
            // handoff can still be accepted. Either outcome is correct; the check records which one ran.
            let route = null;
            await until(
                "the fallback is kept by the browser or taken by the bridge-spawned daemon",
                () => {
                    if (sameFile(path.join(DIR.browserDl, "fallback.bin"), "fallback.bin")) return (route = "browser") && true;
                    if (sameFile(path.join(DIR.downloads, "fallback.bin"), "fallback.bin")) return (route = "daemon") && true;
                    return false;
                },
                90000,
                500,
            ).catch(() => {});
            check(`B14 fallback saved byte for byte (route: ${route ?? "none"})`, route !== null);
            await gotoHome();
            await startDaemon();
            check("B14 the private daemon is back", true);
        });

        await step("B15 every queued file matches its SHA-256", async () => {
            const expect = [
                [path.join(DIR.downloads, "rel", "rel.part1.rar"), "rel.part1.rar"],
                [path.join(DIR.downloads, "rel", "rel.part2.rar"), "rel.part2.rar"],
                [path.join(DIR.downloads, "rel", "rel.part3.rar"), "rel.part3.rar"],
                [path.join(DIR.downloads, "named", "named.part1.rar"), "named.part1.rar"],
                [path.join(DIR.downloads, "named", "named.part2.rar"), "named.part2.rar"],
                [path.join(DIR.downloads, "named", "named.part3.rar"), "named.part3.rar"],
                [path.join(DIR.downloads, "single.zip"), "single.zip"],
                [path.join(DIR.downloads, "ext-slow.bin"), "ext-slow.bin"],
                [path.join(DIR.downloads, "cap.bin"), "cap.bin"],
                [path.join(DIR.popupDest, "pop.bin"), "pop.bin"],
            ];
            for (const [file, name] of expect) check(`B15 SHA-256 ${name}`, sameFile(file, name), file);
            check("B15 box links were never downloaded", !fs.existsSync(path.join(DIR.downloads, "box")));
        });

        await step("B16 server-side request checks", async () => {
            const seen = FIXTURES.hits();
            const pageFiles = seen.filter((h) => /^\/files\/(rel|named)\.|^\/files\/single\.zip$/.test(h.path));
            check("B16 the box links were never requested", !seen.some((h) => h.path.includes("/files/box.part")));
            check("B16 the prose link /article was never requested", !seen.some((h) => h.path === "/article"));
            check(
                "B16 page-initiated file requests carry the page as Referer",
                pageFiles.length > 0 && pageFiles.every((h) => h.referer === home()),
                `${pageFiles.length} requests`,
            );
            const popupHits = seen.filter((h) => h.path === "/files/pop.bin");
            check("B16 the popup add sent no Referer", popupHits.length > 0 && popupHits.every((h) => h.referer === ""), `${popupHits.length} requests`);
            check("B16 the daemon used byte-range requests", seen.some((h) => h.range !== ""));
        });

        check("B17 no uncaught exceptions or console errors in the worker or the pages", pageErrors.length === 0, pageErrors.slice(0, 3).join(" | "));
    } finally {
        for (const c of conns) c.close();
        evidence.pageErrors = pageErrors.slice();
    }
}

async function guiPhase() {
    section("GUI (single instance and screenshots)");
    if (!fs.existsSync(GUI)) {
        check("C0 GUI binary is built", false, GUI);
        return;
    }
    const dbg = await freePort();
    const gui1 = spawnTracked(GUI, [], { ...ENV, WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${dbg}` }, "gui1.log");
    const page = await until(
        "the GUI page target",
        async () => {
            const list = await getJson(`http://127.0.0.1:${dbg}/json/list`).catch(() => []);
            return list.find((t) => t.type === "page") ?? null;
        },
        90000,
        500,
    ).catch(() => null);
    check("C0 first GUI window is up (WebView2 page target)", !!page, page ? page.url : "no page target");
    if (!page) return;
    await sleep(3000);

    const gui2 = spawnTracked(GUI, [], ENV, "gui2.log");
    const code2 = await new Promise((resolve) => {
        const t = setTimeout(() => resolve("still running"), 25000);
        gui2.once("exit", (code) => {
            clearTimeout(t);
            resolve(code);
        });
    });
    check("C1 second GUI launch hands off to the first and exits", code2 === 0, `exit ${code2}`);
    check("C1 first GUI instance stays up", alive(gui1));

    const ws = connect(page.webSocketDebuggerUrl);
    await ws.ready;
    await ws.send("Page.enable");
    await ws.send("Runtime.enable");
    await sleep(2500);
    await shotOf(ws, "c1-gui-main.png");
    const opened = await evaluate(
        ws,
        `(() => { const b = document.querySelector('button[title^="Settings"]'); if (b) { b.click(); return true; } return false; })()`,
    );
    check("C2 Settings button found and pressed", opened === true);
    await sleep(1000);
    await shotOf(ws, "c2-gui-settings.png");
    await evaluate(ws, `(() => { document.querySelector('#set-close').click(); return true; })()`);
    await sleep(500);

    // C3: the Add URL dialog opens and is cancelled without a request.
    // Send and receive stays closed: opening it runs a LAN discovery
    // broadcast, which this gate does not do.
    await evaluate(ws, `(() => { document.querySelector('#btn-add').click(); return true; })()`);
    await sleep(800);
    const addOpen = await evaluate(
        ws,
        `(() => { const d = document.querySelector('#dlg-backdrop'); return !!d && !d.classList.contains('hidden'); })()`,
    );
    check("C3 Add URL dialog opens", addOpen === true);
    await shotOf(ws, "c3-gui-add-url.png");
    await evaluate(ws, `(() => { document.querySelector('#dlg-cancel').click(); return true; })()`);
    await sleep(400);
    ws.close();
    killTree(gui1.pid);
}

// ---- orchestration ----------------------------------------------------------

async function main() {
    if (!flag("--no-build")) build();
    for (const exe of [RIPDL, DAEMON]) {
        if (!fs.existsSync(exe)) throw new Error(`missing ${exe}; run without --no-build first`);
    }
    if (PHASES.has("ext") && !EDGE) throw new Error("Microsoft Edge not found");
    for (const [key, dir] of Object.entries(DIR)) {
        if (key !== "ext") fs.mkdirSync(dir, { recursive: true });
    }
    RPC_PORT = await freePort();
    LAN_PORT = await freePort();
    ENV = {
        ...process.env,
        RIPDL_DATA_DIR: DIR.data,
        RIPDL_RPC_PIPE: PIPE,
        RIPDL_RPC_PORT: String(RPC_PORT),
        RIPDL_GUI_PIPE: GUI_PIPE,
        // The GUI gets its own WebView2 profile, never the installed app's.
        WEBVIEW2_USER_DATA_FOLDER: DIR.guiProfile,
    };
    console.log(`run ${RUN}: ${RUN_DIR}`);
    const before = await userDaemonState();
    const downloadsBefore = downloadsSnapshot();

    try {
        FIXTURES = startFixtures({ files: FILES, page: pageHtml });
        ORIGIN = await FIXTURES.start();
        RELAY = await startRelay({ bridgeExe: RIPDL, origin: `chrome-extension://${EXT_ID}/`, env: ENV, logDir: RUN_DIR });
        prepareExtensionCopy(RELAY.url);
        await startDaemon();
        await isolateDaemon();
        if (PHASES.has("cli")) await cliPhase();
        if (PHASES.has("ext")) await extPhase();
        if (PHASES.has("gui")) await guiPhase();
    } catch (err) {
        check("run completed without an abort", false, err.message);
    } finally {
        await stopDaemon().catch(() => {});
        for (const p of procs) if (alive(p)) killTree(p.pid);
        RELAY?.close();
        await FIXTURES?.close();
    }

    const after = await userDaemonState();
    check(
        "the installed daemon and its queue were not touched",
        before.pids.every((p) => after.pids.includes(p)) && before.queue === after.queue,
        `daemon pids ${before.pids.join(",") || "none"}; queue ${before.queue}`,
    );
    const leaked = newDownloads(downloadsBefore, downloadsSnapshot());
    check(
        "the user's Downloads folder was not written",
        leaked.length === 0,
        leaked.length ? `new or changed: ${leaked.join(", ")}` : "no entries added or changed",
    );

    const passed = results.filter((r) => r.ok).length;
    const report = {
        run: RUN,
        runDir: RUN_DIR,
        phases: [...PHASES],
        passed,
        total: results.length,
        checks: results,
        evidence,
        shots: shots.map((s) => path.join(DIR.shots, s)),
    };
    const reportFile = path.join(RUN_DIR, "report.json");
    fs.writeFileSync(reportFile, JSON.stringify(report, null, 2));
    console.log(`\n${passed}/${results.length} checks passed`);
    console.log(`report: ${reportFile}`);
    process.exitCode = passed === results.length ? 0 : 1;
}

main().catch((err) => {
    console.error(err);
    process.exitCode = 2;
});
