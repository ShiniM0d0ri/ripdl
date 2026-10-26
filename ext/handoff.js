// Browser download handoff: one decision per downloads.onDeterminingFilename.
//
// Chrome holds the filename until suggest() runs or the item is cancelled, so
// the hold is settled exactly once:
//   {verdict:"self"}             an internal ripdl resolve started this
//                                download (hidden tab) and only wanted the URL
//                                -> cancel the browser's copy, stay silent: no
//                                   suggest unless Chrome would not let go, no
//                                   proposal, no duplicate window
//   {verdict:"new", task}        daemon queued the link now
//                                -> cancel the browser's copy (no suggest);
//                                   if Chrome will not let go: suggest() and
//                                   abandon the new proposal
//   {verdict:"duplicate", task}  daemon already has this download
//                                -> cancel the browser's copy and report it via
//                                   onDuplicate; if Chrome will not let go:
//                                   suggest() WITHOUT abandoning (the existing
//                                   task is not ours to drop)
//   {verdict:"keep"} or null     refused, daemon down, or late -> suggest()
// The listener returns the literal true (the hold), never a Promise.

/** Longest the browser waits for a decision before it keeps its own copy. */
export const HANDOFF_DEADLINE_MS = 3000;

/**
 * @param {object} o
 * @param {(item: object, ctx: {beginProposal(id: string): boolean}) => Promise<{verdict: string, task?: object}|null>} o.decide
 *   Calls ctx.beginProposal(id) before sending anything; false means already settled.
 * @param {(item: object) => Promise<boolean>} o.takeItem Cancels and erases the browser's copy.
 * @param {(proposal: string) => void} o.abandon Tells the daemon the browser kept its copy.
 * @param {(item: object, verdict: object, taken: boolean) => void} [o.onDuplicate]
 *   Called once for a `duplicate` verdict; `taken` says whether the browser copy was cancelled.
 * @param {number} [o.deadlineMs]
 */
export function filenameListener({
    decide,
    takeItem,
    abandon,
    onDuplicate = () => {},
    deadlineMs = HANDOFF_DEADLINE_MS,
}) {
    return (item, suggest) => {
        let settled = false;
        let proposal = null;
        const keepInBrowser = () => {
            if (settled) return;
            settled = true;
            clearTimeout(timer);
            try { suggest(); } catch { /* the item was already cancelled */ }
            if (proposal !== null) abandon(proposal);
        };
        const timer = setTimeout(keepInBrowser, deadlineMs);
        const ctx = {
            beginProposal(id) {
                if (settled) return false;
                proposal = id;
                return true;
            },
        };
        Promise.resolve()
            .then(() => decide(item, ctx))
            .then(async (answer) => {
                if (settled) return;
                settled = true;
                clearTimeout(timer);
                const verdict = normalize(answer);
                if (verdict.verdict === "self") {
                    // The URL is already captured by the caller; the browser
                    // copy is disposable. Nothing abandons, no duplicate window.
                    const taken = await tryTake(takeItem, item);
                    if (!taken) { try { suggest(); } catch { /* already cancelled */ } }
                    return;
                }
                if (verdict.verdict === "keep") {
                    try { suggest(); } catch { /* the item was already cancelled */ }
                    return;
                }
                const taken = await tryTake(takeItem, item);
                if (verdict.verdict === "duplicate") onDuplicate(item, verdict, taken);
                if (taken) return;
                try { suggest(); } catch { /* the item was already cancelled */ }
                if (verdict.verdict === "new" && proposal !== null) abandon(proposal);
            }, keepInBrowser);
        return true;
    };
}

/** Accepts only the documented shape; anything else keeps the browser's copy. */
function normalize(answer) {
    if (!answer || typeof answer !== "object") return { verdict: "keep" };
    if (answer.verdict !== "new" && answer.verdict !== "duplicate" && answer.verdict !== "self") return { verdict: "keep" };
    const verdict = { verdict: answer.verdict, task: answer.task ?? null };
    // The request headers the browser used travel with the verdict, so the
    // duplicate window can offer "download again" with the same access.
    if (answer.headers) verdict.headers = answer.headers;
    return verdict;
}

async function tryTake(takeItem, item) {
    try {
        return await takeItem(item);
    } catch {
        return false;
    }
}
