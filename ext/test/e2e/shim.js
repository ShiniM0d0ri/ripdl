// e2e only. The runner copies ext/ into a private folder, writes this file
// there with the relay address filled in, and prepends an import of it to the
// copied background.js. It is never shipped (ext/test/ is test code).
//
// Chrome's native messaging needs a registered host, which a test run does not
// have. connectNative is routed through the runner's relay instead, which starts
// the real ripdl.exe bridge and frames its stdio the way Chrome does.
const RELAY = "__RELAY_URL__";

function connectNativeViaRelay(name) {
    const onMessage = [];
    const onDisconnect = [];
    const outbound = [];
    let sessionId = null;
    let closed = false;
    let sending = Promise.resolve();

    const fire = (listeners, arg) => {
        for (const fn of listeners) {
            try {
                fn(arg);
            } catch (err) {
                console.error("e2e shim listener failed", err);
            }
        }
    };
    const finish = () => {
        if (closed) return;
        closed = true;
        fire(onDisconnect, port);
    };
    const flush = () => {
        if (!sessionId) return;
        while (outbound.length) {
            const body = JSON.stringify(outbound.shift());
            const id = sessionId;
            sending = sending.then(() =>
                fetch(`${RELAY}/s/${id}/send`, {
                    method: "POST",
                    headers: { "Content-Type": "text/plain" },
                    body,
                }).catch(() => {}),
            );
        }
    };
    const poll = async (id) => {
        while (!closed) {
            let data;
            try {
                data = await (await fetch(`${RELAY}/s/${id}/poll`)).json();
            } catch {
                break;
            }
            if (data.closed) break;
            for (const msg of data.messages || []) fire(onMessage, msg);
        }
        finish();
    };

    const port = {
        name,
        onMessage: { addListener: (fn) => onMessage.push(fn), removeListener: () => {}, hasListener: () => false },
        onDisconnect: { addListener: (fn) => onDisconnect.push(fn), removeListener: () => {}, hasListener: () => false },
        postMessage(message) {
            if (closed) throw new Error("Attempting to use a disconnected port object");
            outbound.push(message);
            flush();
        },
        disconnect() {
            if (closed) return;
            closed = true;
            if (sessionId) fetch(`${RELAY}/s/${sessionId}/close`, { method: "POST" }).catch(() => {});
        },
    };

    fetch(`${RELAY}/s/new`, { method: "POST", headers: { "Content-Type": "text/plain" }, body: name })
        .then((r) => r.json())
        .then((data) => {
            sessionId = data.id;
            flush();
            poll(sessionId);
        })
        .catch(() => finish());
    return port;
}

try {
    chrome.runtime.connectNative = connectNativeViaRelay;
} catch {
    Object.defineProperty(chrome.runtime, "connectNative", {
        value: connectNativeViaRelay,
        configurable: true,
        writable: true,
    });
}

// Download events as the browser announced them, for the report when a capture
// check fails. Observers only: they never call suggest(), so ripdl still decides.
// A registration the browser refuses is recorded in errors instead of stopping
// the worker from starting.
globalThis.__ripdlE2E = { relay: RELAY, events: [], errors: [] };
const noteEvent = (kind, detail) => {
    const events = globalThis.__ripdlE2E.events;
    events.push({ at: Date.now(), kind, ...detail });
    if (events.length > 200) events.shift();
};
const observe = (name, register) => {
    try {
        register();
    } catch (err) {
        globalThis.__ripdlE2E.errors.push(`${name}: ${err && err.message ? err.message : err}`);
    }
};
// Bisect switch: the download observers are off while the service worker start
// failure is isolated. Flip to true once the cause is known.
// Keep false: a test-only chrome.downloads listener registered before the product's
// stops the service worker from starting in Edge (seen as a dead handoff, not an error).
const OBSERVE_DOWNLOADS = false;
if (OBSERVE_DOWNLOADS) {
    observe("onCreated", () =>
        chrome.downloads.onCreated.addListener((item) => noteEvent("created", { id: item.id, url: item.url, state: item.state })),
    );
    observe("onDeterminingFilename", () =>
        chrome.downloads.onDeterminingFilename.addListener((item) =>
            noteEvent("determining", { id: item.id, url: item.url, finalUrl: item.finalUrl, totalBytes: item.totalBytes }),
        ),
    );
    observe("onChanged", () =>
        chrome.downloads.onChanged.addListener((delta) =>
            noteEvent("changed", { id: delta.id, state: delta.state ? delta.state.current : null, error: delta.error ? delta.error.current : null }),
        ),
    );
}
