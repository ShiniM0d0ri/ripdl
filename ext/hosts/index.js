/**
 * Built-in plugin registry. Deliberately tiny: the decision (locked
 * 2026-10-24) is that NO hoster ships built-in - hosters are user-loadable
 * recipes or scripts, so the shipped extension never grows site-specific
 * code. `generic` is the only entry.
 */

import generic from "./generic.js";

export const BUILTINS = [generic];
