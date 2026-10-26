// Browser handoff contract (ext/handoff.js): one onDeterminingFilename decision.
// Pure node (no browser, no network): node ext/test/handoff.test.mjs
import { test } from "node:test";
import assert from "node:assert/strict";
import { filenameListener } from "../handoff.js";

const DEADLINE_MS = 40;
const wait = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

/** Fires the listener once for download 7 and records what it did. */
function run({ decide, takeItem = async () => true }) {
    const calls = { suggest: 0, take: 0, abandon: [], duplicates: [] };
    const listener = filenameListener({
        decide,
        takeItem: async (item) => {
            calls.take++;
            return takeItem(item);
        },
        abandon: (proposal) => calls.abandon.push(proposal),
        onDuplicate: (item, verdict, taken) => calls.duplicates.push({ verdict, taken }),
        deadlineMs: DEADLINE_MS,
    });
    const returned = listener({ id: 7, url: "https://files.example/a.zip" }, () => {
        calls.suggest++;
    });
    return { returned, calls };
}

test("returns the literal true before any decision", () => {
    const { returned } = run({ decide: () => new Promise(() => {}) });
    assert.equal(returned, true);
});

test("keep: the browser keeps the download, nothing taken or abandoned", async () => {
    const { calls } = run({ decide: async () => ({ verdict: "keep" }) });
    await wait(5);
    assert.equal(calls.suggest, 1);
    assert.equal(calls.take, 0);
    assert.deepEqual(calls.abandon, []);
});

test("new: the browser copy is taken and never suggested", async () => {
    const { calls } = run({
        decide: async (item, ctx) => {
            ctx.beginProposal("p1");
            return { verdict: "new" };
        },
    });
    await wait(5);
    assert.equal(calls.suggest, 0);
    assert.equal(calls.take, 1);
    assert.deepEqual(calls.abandon, []);
});

test("new but the browser would not let go: browser keeps it, daemon drops its task", async () => {
    const { calls } = run({
        decide: async (item, ctx) => {
            ctx.beginProposal("p2");
            return { verdict: "new" };
        },
        takeItem: async () => false,
    });
    await wait(5);
    assert.equal(calls.suggest, 1);
    assert.deepEqual(calls.abandon, ["p2"]);
});

test("no answer by the deadline: browser keeps it, nothing to abandon", async () => {
    const { calls } = run({ decide: () => new Promise(() => {}) });
    await wait(DEADLINE_MS + 20);
    assert.equal(calls.suggest, 1);
    assert.equal(calls.take, 0);
    assert.deepEqual(calls.abandon, []);
});

test("deadline after the propose: browser keeps it and the claimed task is abandoned", async () => {
    const { calls } = run({
        decide: (item, ctx) => {
            ctx.beginProposal("p3");
            return new Promise(() => {});
        },
    });
    await wait(DEADLINE_MS + 20);
    assert.equal(calls.suggest, 1);
    assert.equal(calls.take, 0);
    assert.deepEqual(calls.abandon, ["p3"]);
});

test("an answer that arrives after the deadline is ignored", async () => {
    let answer;
    const { calls } = run({
        decide: (item, ctx) => {
            ctx.beginProposal("p4");
            return new Promise((resolve) => {
                answer = resolve;
            });
        },
    });
    await wait(DEADLINE_MS + 20);
    answer({ verdict: "new" });
    await wait(5);
    assert.equal(calls.suggest, 1);
    assert.equal(calls.take, 0);
    assert.deepEqual(calls.abandon, ["p4"], "abandoned once, by the deadline");
});

test("no proposal can start once the browser has kept its copy", async () => {
    let began;
    const { calls } = run({
        decide: async (item, ctx) => {
            await wait(DEADLINE_MS + 10);
            began = ctx.beginProposal("p5");
            return null;
        },
    });
    await wait(DEADLINE_MS + 30);
    assert.equal(began, false);
    assert.equal(calls.suggest, 1);
    assert.deepEqual(calls.abandon, []);
});

test("a rejected decision keeps the browser's copy", async () => {
    const { calls } = run({
        decide: async () => {
            throw new Error("boom");
        },
    });
    await wait(5);
    assert.equal(calls.suggest, 1);
    assert.equal(calls.take, 0);
});

test("a rejected decision after a proposal abandons it", async () => {
    const { calls } = run({
        decide: async (item, ctx) => {
            ctx.beginProposal("p6");
            throw new Error("boom");
        },
    });
    await wait(5);
    assert.equal(calls.suggest, 1);
    assert.deepEqual(calls.abandon, ["p6"]);
});

test("duplicate: the browser copy is taken, reported, and the existing task is left alone", async () => {
    const { calls } = run({
        decide: async () => ({ verdict: "duplicate", task: { id: "t9" } }),
    });
    await wait(5);
    assert.equal(calls.suggest, 0);
    assert.equal(calls.take, 1);
    assert.deepEqual(calls.abandon, []);
    assert.deepEqual(calls.duplicates, [{ verdict: { verdict: "duplicate", task: { id: "t9" } }, taken: true }]);
});

test("duplicate but the browser would not let go: browser keeps it and nothing is abandoned", async () => {
    const { calls } = run({
        decide: async (item, ctx) => {
            ctx.beginProposal("p7");
            return { verdict: "duplicate", task: { id: "t9" } };
        },
        takeItem: async () => false,
    });
    await wait(5);
    assert.equal(calls.suggest, 1);
    assert.deepEqual(calls.abandon, [], "the existing task must survive");
    assert.deepEqual(calls.duplicates.map((d) => d.taken), [false]);
});

test("an unknown verdict is treated as keep", async () => {
    const { calls } = run({ decide: async () => ({ verdict: "maybe" }) });
    await wait(5);
    assert.equal(calls.suggest, 1);
    assert.equal(calls.take, 0);
    assert.deepEqual(calls.duplicates, []);
});

test("self: an internal resolve's copy is taken and stays entirely quiet", async () => {
    const { calls } = run({ decide: async () => ({ verdict: "self" }) });
    await wait(5);
    assert.equal(calls.take, 1);
    assert.equal(calls.suggest, 0, "the resolve is done; nothing waits on suggest");
    assert.deepEqual(calls.abandon, [], "no proposal exists to abandon");
    assert.deepEqual(calls.duplicates, [], "no duplicate window for a download ripdl itself opened");
});

test("self but the browser would not let go: browser keeps the copy", async () => {
    const { calls } = run({
        decide: async () => ({ verdict: "self" }),
        takeItem: async () => false,
    });
    await wait(5);
    assert.equal(calls.suggest, 1, "the download must not hang on the filename hold");
    assert.deepEqual(calls.duplicates, []);
});

test("duplicate: the request headers ride along to the duplicate window", async () => {
    const headers = { Referer: "https://files.example/", "User-Agent": "test" };
    const { calls } = run({
        decide: async () => ({ verdict: "duplicate", task: { id: "t9" }, headers }),
    });
    await wait(5);
    assert.equal(calls.duplicates.length, 1);
    assert.deepEqual(calls.duplicates[0].verdict.headers, headers);
});

test("duplicate without headers: the verdict carries no headers field", async () => {
    const { calls } = run({ decide: async () => ({ verdict: "duplicate", task: { id: "t11" } }) });
    await wait(5);
    assert.equal(calls.duplicates.length, 1);
    assert.equal("headers" in calls.duplicates[0].verdict, false);
});

