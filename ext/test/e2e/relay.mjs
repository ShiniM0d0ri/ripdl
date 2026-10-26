// Native-messaging relay for the e2e runner. The shim (shim.js, injected into
// the runner's private copy of ext/) calls chrome.runtime.connectNative, which
// lands here. Each session starts the real ripdl.exe bridge with Chrome's argv
// and frames its stdio the way Chrome does (u32 little-endian length, then
// UTF-8 JSON). The extension side speaks plain HTTP instead: POST to send,
// long-poll GET to receive. No native host registration is involved.
import { spawn } from "node:child_process";
import crypto from "node:crypto";
import fs from "node:fs";
import http from "node:http";
import path from "node:path";

const POLL_MS = 20000;
const IDLE_MS = 90000;

export async function startRelay({ bridgeExe, origin, env, logDir }) {
    const sessions = new Map();
    const log = fs.openSync(path.join(logDir, "relay.log"), "a");
    const note = (line) => fs.writeSync(log, `${new Date().toISOString()} ${line}\n`);
    const wake = (s) => {
        for (const fn of s.waiters.splice(0)) fn();
    };

    function open() {
        const id = crypto.randomBytes(6).toString("hex");
        const child = spawn(bridgeExe, [origin], { env, stdio: ["pipe", "pipe", "pipe"], windowsHide: true });
        const s = { id, child, outbox: [], waiters: [], closed: false, lastSeen: Date.now(), frames: 0 };
        sessions.set(id, s);
        note(`${id} open pid=${child.pid} argv=${origin}`);
        let buf = Buffer.alloc(0);
        child.stdout.on("data", (chunk) => {
            buf = Buffer.concat([buf, chunk]);
            while (buf.length >= 4) {
                const len = buf.readUInt32LE(0);
                if (buf.length < 4 + len) break;
                const body = buf.subarray(4, 4 + len).toString("utf8");
                buf = buf.subarray(4 + len);
                s.frames++;
                note(`${id} <- ${body.slice(0, 400)}`);
                let msg;
                try {
                    msg = JSON.parse(body);
                } catch {
                    msg = { raw: body };
                }
                s.outbox.push(msg);
                wake(s);
            }
        });
        child.stderr.on("data", (chunk) => note(`${id} stderr ${chunk.toString("utf8").trim().slice(0, 400)}`));
        child.stdin.on("error", () => {});
        child.on("exit", (code) => {
            note(`${id} exit ${code}`);
            s.closed = true;
            wake(s);
        });
        return s;
    }

    function frame(s, text) {
        const payload = Buffer.from(text, "utf8");
        const head = Buffer.alloc(4);
        head.writeUInt32LE(payload.length, 0);
        note(`${s.id} -> ${text.slice(0, 400)}`);
        s.child.stdin.write(Buffer.concat([head, payload]));
    }

    function stop(s) {
        if (s.closed) return;
        s.closed = true;
        note(`${s.id} closing`);
        try {
            s.child.stdin.end();
        } catch {
            /* stdin already gone */
        }
        setTimeout(() => {
            if (s.child.exitCode === null) s.child.kill();
        }, 1500).unref();
        wake(s);
    }

    const cors = {
        "Access-Control-Allow-Origin": "*",
        "Access-Control-Allow-Headers": "content-type",
        "Access-Control-Allow-Methods": "GET, POST, OPTIONS",
    };

    const server = http.createServer((req, res) => {
        const send = (status, obj) => {
            try {
                res.writeHead(status, { ...cors, "Content-Type": "application/json" });
                res.end(JSON.stringify(obj));
            } catch {
                /* client went away */
            }
        };
        if (req.method === "OPTIONS") {
            res.writeHead(204, cors);
            res.end();
            return;
        }
        let body = "";
        req.setEncoding("utf8");
        req.on("data", (chunk) => {
            body += chunk;
        });
        req.on("end", () => {
            const parts = new URL(req.url, "http://relay").pathname.split("/").filter(Boolean);
            if (parts[0] !== "s") return send(404, { error: "no route" });
            if (req.method === "POST" && parts[1] === "new") return send(200, { id: open().id });
            const s = sessions.get(parts[1]);
            if (!s) return send(404, { error: "no session" });
            s.lastSeen = Date.now();
            if (req.method === "POST" && parts[2] === "send") {
                if (s.closed) return send(410, { ok: false });
                frame(s, body);
                return send(200, { ok: true });
            }
            if (req.method === "GET" && parts[2] === "poll") {
                const reply = () => {
                    if (s.outbox.length) return send(200, { messages: s.outbox.splice(0) });
                    if (s.closed) return send(200, { closed: true, messages: [] });
                    return send(200, { messages: [] });
                };
                if (s.outbox.length || s.closed) return reply();
                let timer = null;
                const done = () => {
                    clearTimeout(timer);
                    reply();
                };
                timer = setTimeout(() => {
                    s.waiters = s.waiters.filter((w) => w !== done);
                    reply();
                }, POLL_MS);
                s.waiters.push(done);
                return undefined;
            }
            if (req.method === "POST" && parts[2] === "close") {
                stop(s);
                return send(200, { ok: true });
            }
            return send(404, { error: "no route" });
        });
    });

    const reaper = setInterval(() => {
        for (const s of sessions.values()) {
            if (!s.closed && Date.now() - s.lastSeen > IDLE_MS) {
                note(`${s.id} idle, closing`);
                stop(s);
            }
        }
    }, 5000);
    reaper.unref();

    await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
    const url = `http://127.0.0.1:${server.address().port}`;
    return {
        url,
        close() {
            clearInterval(reaper);
            for (const s of sessions.values()) {
                s.closed = true;
                try {
                    s.child.kill();
                } catch {
                    /* already exited */
                }
            }
            server.close();
            server.closeAllConnections?.();
        },
    };
}
