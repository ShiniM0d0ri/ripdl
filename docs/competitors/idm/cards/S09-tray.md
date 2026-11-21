# S09 - Tray and mini panel

Status: `spec ready` (Tauri tray exists). Read `SHARED.md`, `S01-main.md`.

## Goal

Keep transfers visible without keeping the full window open; low memory while
hidden.

## IDM behavior

- Tray icon with context menu.
- Mini progress window with compact rows and speed.
- Start/Stop all, scheduler and options reachable from tray.

## ripdl target

- Tray icon: Show, Pause all, Resume all, Settings, Send & Receive, Exit.
- Close-to-tray preference from Settings (already in config).
- Mini panel: 3-5 latest active rows, total speed, Pause/Resume.
- Native spike: suspend webview equivalent not needed; egui window can be
  hidden and repainted on demand.

## State model

```text
window_visible: bool
mini_open: bool
hidden_at: Instant
last_snapshot: TransferSnapshot
```

## Interactions

- Tray click -> reveal main window; second instance forwards to it.
- Double-click tray -> mini panel.
- Pause all/Resume all -> bulk RPC.

## Acceptance

- [ ] Hidden window CPU ~0; memory drops after 30-90 s (measure).
- [ ] Second launch focuses existing window and exits.
- [ ] Mini panel numbers equal daemon snapshot.
- [ ] Screenshots: tray menu, mini panel.

## Work IDs

FEATURES.csv: IDM-029, IDM-030. GUI-011, UXD-005.

## Do not

- Do not reintroduce Tauri WebView2 into the native spike.
