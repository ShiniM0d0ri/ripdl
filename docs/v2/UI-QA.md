# UI QA: debug every screen, screenshot every state

Standing rule: **no screen is done until it has been driven, inspected,
screenshotted, fixed, and re-screenshotted against a checklist.** "Looks fine"
is not a test result. A screenshot plus an AccessKit/DOM assertion is.

This is an iterative loop, not a one-time pass. We keep going until every
entry in the matrix below passes.

## Tools by shell

| Target | Driver | Inspector | Screenshot |
|---|---|---|---|
| Current Tauri GUI | `ext/test/e2e/run-in-session.ps1` (desktop session) + CDP via `research/ext-e2e-cdp.js` | CDP DOM + WebView2 accessibility tree | CDP `Page.captureScreenshot` / harness `shots/` |
| Extension popup/options | headless Edge + CDP (already used in B phase) | CDP DOM | CDP screenshot |
| Native egui spike | in-process harness + egui inspection port (`EGUI_INSPECTION`) | AccessKit tree via inspection TCP | inspection screenshot endpoint / OS screen capture |
| CLI | text snapshots in tests | `--json` snapshot | terminal capture for docs only |

The agent shell is elevated (session 0), where WebView2 cannot open windows.
All Tauri GUI work goes through `run-in-session.ps1`; do not spawn
`target/debug/ripdl-gui.exe` directly. This is documented in AGENTS.md.

## The loop (repeat per screen/state)

1. **Instrument.** Every interactive element has a stable hook: a `data-testid`
   in the web UI, or a stable AccessKit `id`/label in egui. Without a hook a
   component cannot be debugged reliably.
2. **Drive.** Use the harness to put the app into the exact state - empty, one
   task, 200 tasks, error, dialog open, dark mode, 200% DPI.
3. **Inspect.** Assert on the tree, not pixels: the row with name X has state
   `seeding` and speed `1.2 MiB/s`; the dialog has exactly one primary button;
   no element has an empty accessible name.
4. **Capture.** Save to `target/e2e/runs/<RUN>/shots/<screen>-<state>.png` with
   a sidecar JSON containing viewport, DPI, theme, protocol version, and the
   tree assertion results.
5. **Debug.** If anything is wrong, use the tree hook to locate the component,
   fix it, and re-run the same state. Never "fix" by moving pixels blindly.
   For the current Tauri UI, CDP can query the DOM and computed styles; for
   egui, the inspection port can read the AccessKit tree and inject input.
6. **Review.** Compare the screenshot with `UI-REHAUL.md`: primary action,
   density, contrast, honesty of labels, no overflow. Leave a short note in
   the sidecar: pass or the exact reason it failed.
7. **Store.** When a state passes, copy the screenshot into `docs/screenshots/`
   with a stable name for the README. Keep the failing ones inside the run
   folder so regressions are visible.

## Screen/state matrix

| Screen | States | Hooks | Checks |
|---|---|---|---|
| Main task list | empty, 1 running, mixed, 200 tasks, multi-select, filtered | `task-row`, `row-name`, `row-speed`, `status-badge`, `ctx-menu` | column order stable, no layout shift, virtualized scroll, keyboard select |
| Add URL | simple, advanced, error, duplicate | `dlg-url`, `dlg-ok`, `dlg-adv` | one primary action, inline validation, focus trapped |
| Add torrent | picker, metadata wait, checklist, sequential | `torrent-file`, `file-row`, `only-check` | checklist keyboardable, metadata state honest |
| Add bundle | pasted links, preview, duplicates, grouped | `bundle-links`, `bundle-preview` | 100-link cap message, duplicate count shown |
| HTTP details | running, paused, stale, refresh, open | `details-http`, `refresh-url` | copy address, kept X of Y, no stale rate |
| Torrent details | files, peers, trackers, seeding, verifying | `details-torrent`, `tab-files` | 2 s refresh, empty peers state, ratio/peers |
| Settings | six tabs, advanced filter, restart badge, save error | `settings-tab-*`, `setting-<id>` | generated from schema, apply badge, no orphan field |
| Send & Receive | code hidden/shown, partial rows, devices, send progress | `lan-code`, `lan-file`, `send-row` | code not leaked in page, partial badge, progress honest |
| Update | current, available, downloading, failed | `update-banner`, `update-install` | banner hides when current, error inline |
| About/Help | version, links, setup again, diagnostics | `about-version`, `btn-diagnostics` | redacted diagnostics, version matches daemon |
| Tray/mini | hidden, revealed, second launch | `tray-menu`, `mini-panel` | webview suspend/resume, first window restored |
| Extension popup | host alive/missing, offer, duplicate | `ext-status`, `offer-card` | falls back to browser, no 15 s hang |
| Extension options | plugins, generated settings, diagnostics | `opt-host`, `opt-schema` | protocol version visible, no token in diagnostics |

## Additional assertions per screenshot

- `no_empty_accessible_names`: every interactive node has a name.
- `one_primary_action`: exactly one element marked primary per dialog.
- `no_overflow`: `scrollWidth <= clientWidth` for text containers.
- `contrast_aa`: sampled text/background pairs meet 4.5:1.
- `dpi_100_150_200`: same screen captured at three scale factors.
- `tree_matches_snapshot`: the visible numbers equal the daemon snapshot.

These run for every state above; failures block the screen.

## Exit criteria

- Every row in the matrix has at least one passing screenshot per state, with
  a sidecar containing the tree assertions.
- Every failing check has a linked fix commit and a re-capture that passes.
- A curated set lives in `docs/screenshots/` for the README.
- The GUI e2e gate (C0-C3/C4) still passes, and the extension B phase still
  passes, after the rehaul.
- If the native spike is adopted, the same matrix passes there too - no
  screenshot is accepted from a different screen inventory.

We iterate until this list is fully green. "Almost perfect" is a failing
state.
