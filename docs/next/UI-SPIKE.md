# Native UI spike

Status: **active**. Branch: `spike/native-ui`. Timebox: two weeks. This branch
is never merged into `next` as a second UI; the measurements and the view-model
crate (if it earns its place) come back. The spike is now the primary UI work,
not a side experiment, because the backend contract is complete.

## Why spike now

The Tauri shell is not the source of the wrong speed numbers, so a migration
must not block the truth-layer work. But the new Rust download-manager field
shows that native UI is viable:

- **Mister Download Manager** is a large, feature-complete app on **egui**.
- **oxdm** is a native **iced 0.14** app.
- **SSDownload** is a **Win32** app.
- **NDM-Next** and **motrix-gpui** use **GPUI**.

At the same time, **Prism**, **swiftdl**, **LibreGet**, and **Firelink** ship on
Tauri. Tauri is still the default. The spike is a measurement, not a verdict.

## Toolkit choice

Decision: **egui/eframe**. It does **not** use WebView2 - eframe renders through
`winit` + `wgpu`/`glow` directly, so there is no Chromium/Edge runtime, no
WebView2 install dependency, and no CDP. This matches the requirement that the
native spike not require WebView2.

Start with **egui/eframe**:

- fastest path to a table-heavy task list (`egui_extras::TableBuilder`);
- one binary, no WebView2, no JS bridge;
- `accesskit` feature exists (needs real testing);
- a proven reference in this exact product category (MDM);
- easy 2 Hz repaint without a DOM diff.

Keep the UI model independent so a later **iced** port is possible if egui's
accessibility or text/IME behavior is not good enough. Do not start with GPUI
or Slint: GPUI is young, and Slint's licensing needs a deliberate decision
before it can enter an MIT/Apache project.

## Scope

Week 1 - read-only:

- window shell with sidebar (All / Downloading / Completed / Categories);
- task table: name, size, progress, speed, ETA, status, kind icon;
- status bar: total speed, 60 s graph, per-NIC chips;
- Add URL dialog (no submit yet);
- Settings > Network (limit + bonding readout);
- tray icon, single-instance hand-off, remembered window size.

Week 2 - control:

- pause/resume/remove selected;
- Add URL submit;
- Settings save through the existing RPC;
- details dialog for one HTTP and one torrent task.

Data comes from the same daemon RPC. The spike may use a small mock snapshot
first, but the second half must talk to the real daemon.

## Hard rules

- No business logic in the UI. No rate derivation, no byte math, no ETA math.
- Render `TransferSnapshot` and `LinkSnapshot` from the daemon.
- Do not fork the engine or the daemon.
- Do not change the RPC protocol for the spike; use the v1 commands plus a
  temporary `GetSnapshot` adapter if needed.
- No feature that the Tauri UI does not already have.

## Debugging and UI automation

**No, CDP cannot be reused.** Chrome DevTools Protocol speaks to a
Chromium/WebView2 renderer. An egui/iced app renders to a native window with
wgpu/glow; there is no DOM, no JS runtime, and no DevTools endpoint. The
existing `research/ext-e2e-cdp.js` and the C4 GUI checks in
`ext/test/e2e/run.mjs` are Tauri-WebView-specific and will not transfer.

What replaces it:

- **egui inspection port.** eframe has an `inspection` feature: set
  `EGUI_INSPECTION` (or `EGUI_INSPECTION_ADDR`) and eframe opens a TCP port that
  lets an external tool read the **AccessKit tree**, inject input, and capture
  screenshots. That is the closest native equivalent to CDP's DOM/inject/
  screenshot surface, and it is off by default. This is a concrete reason to
  prototype on egui first; nothing equivalent was found in iced's feature set.
- **AccessKit tree in-process.** With eframe's `accesskit` feature, the same
  tree is available to Rust code, so we can write assertions like "the row with
  this name has state `seeding` and speed `1.2 MiB/s`" without pixels. On
  Windows, `accesskit_windows` maps that tree to UI Automation, so screen
  readers and OS-level automation tools see the app too.
- **Native debugger.** WinDbg/Visual Studio/gdb with symbols, breakpoints, and
  watch windows. This is better than CDP for logic bugs in Rust code.
- **In-app debug panel.** A hidden panel (shortcut) that dumps the last RPC
  snapshot, view-model state, frame time, and link rates. Cheap to build and
  the fastest way to answer "is the UI wrong or is the daemon wrong?"
- **tracing.** The daemon already logs; the spike should log to a file and show
  a small log window rather than depending on a console.
- **Frame/GPU tools.** egui shows frame time/CPU/GPU stats; RenderDoc/PIX can
  capture a frame when needed. Not required for the decision.

Consequence for the schedule: a native UI needs a **new automation harness**.
Budget it explicitly; do not assume the C4 GUI checks can be ported by changing
a URL. The first milestone of the spike is therefore "can a test read the
AccessKit tree and assert a row", not "does the window look nice".

Add one more go/no-go item:

- the inspection/AccessKit harness can assert the task list and drive
  pause/resume without pixel matching, and it runs in CI on Windows.

## Metrics

Measure the same scenarios on the current Tauri build and the spike:

| Metric | Target | How |
|---|---|---|
| Cold start to first usable frame | < 1 s | Process start to table painted |
| Visible working set | < 150 MB | Task Manager / `GetProcessMemoryInfo` |
| Hidden-to-tray working set | < 60 MB | Hide, wait 60 s, measure |
| Idle CPU | < 1% | 60 s with no tasks |
| Active CPU (10 tasks, 2 Hz) | < 3% | Same task set both UIs |
| Binary size | <= Tauri bundle | Release build |
| Keyboard-only flow | complete | Add, pause, resume, remove, settings |
| Narrator/NVDA | usable | Read the task table and dialogs |
| DPI 100/150/200% | no clipping | Windows display settings |
| IME (Japanese/Chinese) | works | Add URL field |
| Tray + single instance | works | Second launch focuses the first |
| Crash-free 30 min soak | yes | 10 tasks, pause/resume churn |

## Go/no-go

Adopt native only if **all** of these hold:

- visible RSS and hidden RSS beat Tauri by at least 30%;
- cold start beats Tauri;
- the keyboard and Narrator flows are usable without a regression;
- the AccessKit/inspection harness proves the UI can be tested without CDP;
- the table remains smooth at 200 tasks and 2 Hz;
- the spike reaches feature parity for the scope above in two weeks.

If it misses, keep Tauri and spend the time on the traffic/link core. A native
UI does not fix a wrong number; it only renders it faster.

## Deliverable

`spike/native-ui/README.md` with:

- screenshots of the five screens;
- a table of measured numbers, Tauri vs native, same machine;
- a list of missing widgets/behaviors and their cost;
- a recommendation: keep Tauri, adopt native, or run a second spike.

No code from the spike merges unless the recommendation is "adopt native" and
the view-model crate is independently useful.
