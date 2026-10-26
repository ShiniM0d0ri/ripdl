// Duplicate window model contract: node ext/test/duplicate-model.test.mjs
import { test } from "node:test";
import assert from "node:assert/strict";
import {
    actionsFor, baseName, bucketOf, browserNote, displayName, formatBytes,
    headline, nameFromUrl, percentOf, stateLine,
} from "../duplicate-model.js";

const ids = (list) => list.map((a) => a.id);
const primaries = (list) => list.filter((a) => a.primary).map((a) => a.id);
const direct = (state, extra = {}) => ({
    id: "t1", url: "https://files.example/a.zip", kind: "http", state,
    dest_file: "C:\\Users\\me\\Downloads\\a.zip", downloaded: 100, total: 100, ...extra,
});
const magnet = (state, extra = {}) => ({
    id: "t2", url: "magnet:?xt=urn:btih:abc", kind: "torrent", state,
    dest_file: "C:\\Users\\me\\Downloads\\show", downloaded: 0, total: 0, ...extra,
});

test("every daemon state falls in one bucket, unknown states count as active", () => {
    assert.equal(bucketOf({ state: "completed" }), "done");
    assert.equal(bucketOf({ state: "paused" }), "paused");
    assert.equal(bucketOf({ state: "failed" }), "failed");
    assert.equal(bucketOf({ state: "cancelled" }), "cancelled");
    assert.equal(bucketOf({ state: "running" }), "active");
    assert.equal(bucketOf({ state: "queued" }), "active");
    assert.equal(bucketOf({ state: "from-a-newer-daemon" }), "active");
    assert.equal(bucketOf(null), "active");
});

test("a finished direct download offers open, reveal, copy, browser with open primary", () => {
    const list = actionsFor(direct("completed"));
    assert.deepEqual(ids(list), ["open", "reveal", "copy", "browser"]);
    assert.deepEqual(primaries(list), ["open"]);
    assert.equal(list[2].label, "Download again");
});

test("a finished download with no file on disk offers only the browser", () => {
    assert.deepEqual(ids(actionsFor(direct("completed", { dest_file: null }))), ["browser"]);
});

test("a finished torrent offers open and reveal, never copy or browser", () => {
    assert.deepEqual(ids(actionsFor(magnet("completed"))), ["open", "reveal"]);
});

test("paused resumes first, and failed says Retry", () => {
    const paused = actionsFor(direct("paused"));
    assert.deepEqual(ids(paused), ["resume", "reveal", "browser"]);
    assert.equal(paused[0].label, "Resume");
    const failed = actionsFor(direct("failed", { error: "disk full" }));
    assert.deepEqual(ids(failed), ["resume", "reveal", "browser"]);
    assert.equal(failed[0].label, "Retry");
    assert.deepEqual(primaries(failed), ["resume"]);
});

test("the daemon resumes only paused or failed tasks, so a cancelled one offers a second copy", () => {
    assert.deepEqual(ids(actionsFor(direct("cancelled"))), ["copy", "browser", "reveal"]);
    assert.deepEqual(primaries(actionsFor(direct("cancelled"))), ["copy"]);
    assert.deepEqual(ids(actionsFor(direct("cancelled", { dest_file: null }))), ["browser"]);
    assert.deepEqual(ids(actionsFor(magnet("cancelled"))), ["reveal"]);
    assert.deepEqual(ids(actionsFor(magnet("cancelled", { dest_file: null }))), []);
});

test("an active download keeps ripdl's copy first and never offers a second copy", () => {
    const running = actionsFor(direct("running", { dest_file: null }));
    assert.deepEqual(ids(running), ["keep", "browser"]);
    assert.equal(running[0].label, "Keep ripdl's download");
    assert.deepEqual(ids(actionsFor(direct("queued"))), ["keep", "reveal", "browser"]);
    assert.deepEqual(primaries(actionsFor(direct("queued"))), ["keep"]);
});

test("a torrent in progress has keep and reveal, no browser", () => {
    assert.deepEqual(ids(actionsFor(magnet("running", { dest_file: null }))), ["keep"]);
});

test("a missing task offers only the browser, and only for a web link", () => {
    assert.deepEqual(ids(actionsFor(null, "https://files.example/a.zip")), ["browser"]);
    assert.deepEqual(actionsFor(null, "magnet:?xt=urn:btih:abc"), []);
    assert.deepEqual(actionsFor(null, ""), []);
});

test("every non-empty choice list has exactly one primary, and it comes first", () => {
    const states = ["completed", "paused", "failed", "cancelled", "running", "queued", "weird"];
    for (const state of states) {
        for (const task of [direct(state), direct(state, { dest_file: null }), magnet(state)]) {
            const list = actionsFor(task);
            if (!list.length) continue;
            assert.equal(primaries(list).length, 1, `${state}: one primary`);
            assert.equal(list[0].primary, true, `${state}: primary is first`);
        }
    }
});

test("stateLine words each bucket", () => {
    assert.equal(stateLine(direct("completed")), "Completed \u00b7 100 B");
    assert.equal(stateLine(direct("completed", { total: 0, downloaded: 0 })), "Completed");
    assert.equal(stateLine(direct("paused", { downloaded: 400, total: 1000 })), "Paused \u00b7 40% \u00b7 400 B of 1000 B");
    assert.equal(stateLine(direct("failed", { error: "disk full" })), "Failed \u00b7 disk full");
    assert.equal(stateLine(direct("failed", { error: null })), "Failed");
    assert.equal(stateLine(direct("cancelled")), "Cancelled");
    assert.equal(stateLine(direct("queued")), "Queued");
    assert.equal(stateLine(direct("running", { probing: true, downloaded: 0 })), "Connecting\u2026");
    assert.equal(stateLine(direct("running", { downloaded: 340, total: 1000 })), "Downloading \u00b7 34% \u00b7 340 B of 1000 B");
    assert.equal(stateLine(direct("running", { downloaded: 1536, total: 0 })), "Downloading \u00b7 1.5 KB");
    assert.equal(stateLine(null), "Already in ripdl");
});

test("formatBytes uses binary units and one decimal under ten", () => {
    assert.equal(formatBytes(0), "0 B");
    assert.equal(formatBytes(-5), "0 B");
    assert.equal(formatBytes(NaN), "0 B");
    assert.equal(formatBytes(100), "100 B");
    assert.equal(formatBytes(1536), "1.5 KB");
    assert.equal(formatBytes(5 * 1024 * 1024), "5.0 MB");
    assert.equal(formatBytes(12 * 1024 * 1024), "12 MB");
    assert.equal(formatBytes(2.5 * 1024 ** 3), "2.5 GB");
});

test("percentOf is null without a size and clamps to 0..100", () => {
    assert.equal(percentOf({ downloaded: 5, total: 0 }), null);
    assert.equal(percentOf({ downloaded: 0, total: 100 }), 0);
    assert.equal(percentOf({ downloaded: 250, total: 100 }), 100);
    assert.equal(percentOf({ downloaded: 99, total: 100 }), 99);
});

test("displayName prefers the file on disk, then the torrent name, then the browser name, then the URL", () => {
    assert.equal(displayName(direct("completed"), {}), "a.zip");
    assert.equal(displayName({ dest_file: null, name: "ubuntu.iso" }, {}), "ubuntu.iso");
    assert.equal(displayName(null, { filename: "suggested.bin", url: "https://x.example/y" }), "suggested.bin");
    assert.equal(displayName(null, { url: "https://x.example/files/my%20movie.mkv?sig=1" }), "my movie.mkv");
});

test("baseName handles both separators and a bare name", () => {
    assert.equal(baseName("C:\\Users\\me\\a.zip"), "a.zip");
    assert.equal(baseName("/home/me/a.zip"), "a.zip");
    assert.equal(baseName("a.zip"), "a.zip");
});

test("nameFromUrl falls back to the raw text when the URL does not parse", () => {
    assert.equal(nameFromUrl("not a url"), "not a url");
    assert.equal(nameFromUrl(""), "download");
});

test("headline and browserNote word the single and batch cases", () => {
    assert.equal(headline(1), "ripdl already has this download");
    assert.equal(headline(3), "ripdl already has 3 of these downloads");
    assert.equal(browserNote(null), "");
    assert.match(browserNote({ taken: false }), /still downloading/);
    assert.match(browserNote({ taken: true }), /was stopped/);
});
