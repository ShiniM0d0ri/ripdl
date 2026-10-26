// Loopback fixture server for the e2e runner. Listens on 127.0.0.1 only.
//   /                    fixture page (built by the runner from the origin)
//   /article             a prose link that must never be downloaded
//   /files/<name>        byte ranges supported
//   /dl/<name>           same bytes with Content-Disposition: attachment
//   /norange/<name>      no range support (the single-stream path)
//   /slow/<name>         byte ranges, throttled per connection (entry.throttleBps)
// Every request is logged; the runner reads the log through hits().
import http from "node:http";

const RANGE = /^bytes=(\d*)-(\d*)$/;

export function startFixtures({ files, page }) {
    const hits = [];
    let origin = "";

    const server = http.createServer((req, res) => {
        const url = new URL(req.url, "http://127.0.0.1");
        if (url.pathname === "/__hits") {
            res.writeHead(200, { "Content-Type": "application/json" });
            res.end(JSON.stringify(hits));
            return;
        }
        const hit = { path: url.pathname, range: req.headers.range || "", referer: req.headers.referer || "", status: 0 };
        hits.push(hit);
        const text = (status, type, body) => {
            hit.status = status;
            res.writeHead(status, { "Content-Type": type, "Content-Length": Buffer.byteLength(body) });
            res.end(body);
        };
        if (url.pathname === "/") return text(200, "text/html; charset=utf-8", page(origin));
        if (url.pathname === "/article") return text(200, "text/html; charset=utf-8", "<p>article</p>");
        const m = /^\/(files|dl|norange|slow)\/(.+)$/.exec(url.pathname);
        const name = m ? decodeURIComponent(m[2]) : "";
        const entry = m ? files.get(name) : undefined;
        if (!entry) return text(404, "text/plain", "not found");
        serve(m[1], name, entry, req, res, hit);
    });

    return {
        start() {
            return new Promise((resolve) => {
                server.listen(0, "127.0.0.1", () => {
                    origin = `http://127.0.0.1:${server.address().port}`;
                    resolve(origin);
                });
            });
        },
        origin: () => origin,
        hits: () => hits.slice(),
        close() {
            return new Promise((resolve) => {
                server.close(() => resolve());
                server.closeAllConnections?.();
            });
        },
    };
}

function serve(kind, name, entry, req, res, hit) {
    const buf = entry.buf;
    if (kind === "norange") {
        hit.status = 200;
        res.writeHead(200, { "Content-Type": "application/octet-stream", "Content-Length": buf.length });
        res.end(buf);
        return;
    }
    const headers = { "Content-Type": "application/octet-stream", "Accept-Ranges": "bytes" };
    if (kind === "dl") headers["Content-Disposition"] = `attachment; filename="${name}"`;
    let start = 0;
    let end = buf.length - 1;
    let status = 200;
    const m = RANGE.exec(req.headers.range || "");
    if (m && (m[1] !== "" || m[2] !== "")) {
        if (m[1] === "") {
            start = Math.max(0, buf.length - Number(m[2]));
        } else {
            start = Number(m[1]);
            if (m[2] !== "") end = Math.min(Number(m[2]), buf.length - 1);
        }
        if (start > end || start >= buf.length) {
            hit.status = 416;
            res.writeHead(416, { "Content-Range": `bytes */${buf.length}` });
            res.end();
            return;
        }
        status = 206;
        headers["Content-Range"] = `bytes ${start}-${end}/${buf.length}`;
    }
    const body = buf.subarray(start, end + 1);
    headers["Content-Length"] = body.length;
    hit.status = status;
    res.writeHead(status, headers);
    if (kind !== "slow" || !entry.throttleBps) {
        res.end(body);
        return;
    }
    const chunk = Math.max(4096, Math.floor(entry.throttleBps / 10));
    let offset = 0;
    const timer = setInterval(() => {
        if (res.destroyed) {
            clearInterval(timer);
            return;
        }
        if (offset >= body.length) {
            clearInterval(timer);
            res.end();
            return;
        }
        res.write(body.subarray(offset, offset + chunk));
        offset += chunk;
    }, 100);
    res.on("close", () => clearInterval(timer));
}
