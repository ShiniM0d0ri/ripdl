// Minimal Chrome DevTools Protocol client on Node's built-in WebSocket, plus
// the polling helpers the e2e runner uses. Node only, no dependencies.

export function connect(wsUrl) {
    const ws = new WebSocket(wsUrl);
    const pending = new Map();
    const listeners = [];
    let seq = 0;
    ws.onmessage = (event) => {
        const msg = JSON.parse(String(event.data));
        if (msg.id !== undefined && pending.has(msg.id)) {
            const { resolve, reject } = pending.get(msg.id);
            pending.delete(msg.id);
            if (msg.error) reject(new Error(`${msg.error.message} (${msg.error.code})`));
            else resolve(msg.result || {});
        } else if (msg.method) {
            for (const fn of listeners) fn(msg);
        }
    };
    const ready = new Promise((resolve, reject) => {
        ws.onopen = () => resolve();
        ws.onerror = () => reject(new Error(`websocket failed: ${wsUrl}`));
    });
    ready.catch(() => {});
    return {
        ready,
        send(method, params = {}) {
            const id = ++seq;
            return new Promise((resolve, reject) => {
                pending.set(id, { resolve, reject });
                ws.send(JSON.stringify({ id, method, params }));
            });
        },
        on(fn) {
            listeners.push(fn);
        },
        close() {
            try {
                ws.close();
            } catch {
                /* already closed */
            }
        },
    };
}

/** Runs an expression in the page or worker and returns its JSON value. */
export async function evaluate(conn, expression) {
    const r = await conn.send("Runtime.evaluate", {
        expression,
        returnByValue: true,
        awaitPromise: true,
        userGesture: true,
    });
    if (r.exceptionDetails) {
        const d = r.exceptionDetails;
        throw new Error((d.exception && d.exception.description) || d.text || "evaluate failed");
    }
    return r.result ? r.result.value : undefined;
}

/** Records uncaught exceptions and console errors from a connection. */
export function collectErrors(conn, where, sink) {
    conn.on((m) => {
        if (m.method === "Runtime.exceptionThrown") {
            const d = m.params.exceptionDetails || {};
            sink.push(`${where}: ${(d.exception && d.exception.description) || d.text || "exception"}`);
        } else if (m.method === "Runtime.consoleAPICalled" && m.params.type === "error") {
            const text = (m.params.args || []).map((a) => a.value ?? a.description ?? "").join(" ");
            sink.push(`${where} console.error: ${text}`);
        }
    });
}

export async function getJson(url) {
    const res = await fetch(url);
    if (!res.ok) throw new Error(`HTTP ${res.status} from ${url}`);
    return res.json();
}

export const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

/** Polls fn until it returns a truthy value; throws after `ms`. Errors inside fn count as not yet. */
export async function until(label, fn, ms = 15000, every = 200) {
    const end = Date.now() + ms;
    for (;;) {
        let value = null;
        try {
            value = await fn();
        } catch {
            value = null;
        }
        if (value) return value;
        if (Date.now() > end) throw new Error(`timed out waiting for ${label}`);
        await sleep(every);
    }
}
