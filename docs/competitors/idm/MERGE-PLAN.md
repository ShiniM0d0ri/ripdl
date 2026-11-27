# IDM -> ripdl merge plan

Decision source: `FEATURES.csv` and `SETTINGS.csv`. This is the single list of
what to adopt, adapt or exclude when making ripdl's UI look and behave like IDM
while keeping ripdl-exclusive features.

Legend: **adopt** = same behavior; **adapt** = same place, ripdl semantics;
**exclude** = never implement.

## Shell and navigation

| IDM feature | Decision | ripdl target |
|---|---|---|
| Menu bar Tasks/File/Downloads/View/Help | adapt | Add Tools/Help; no Registration |
| Toolbar with icon+label actions | adopt | Add URL, Pause, Stop, Delete, Options, Scheduler, Start/Stop Queue |
| Refresh address | ripdl-only | Add beside Resume for unfinished rows |
| Add torrent / Add bundle / Send & Receive | ripdl-only | Add after Add URL via toolbar dropdown |
| Categories sidebar | adopt | Add Downloads with type folders, Unfinished, Finished, Grabber, Queues |
| Torrents / Bundles categories | ripdl-only | First-class nodes beside Video |
| Wide table with Last Try column | adopt | File Name, Kind, Size, Status, Time left, Transfer rate, Last Try, Description |
| Status strip with aggregate rate + graph | adopt | Feed from daemon snapshot, not derived |

## Dialogs

| IDM feature | Decision | ripdl target |
|---|---|---|
| Add URL simple view | adopt | URL + Save to + primary Download |
| Advanced options in Add | adapt | Category, video/torrent, sequential, headers, start-at behind Advanced |
| Options nine-tabs | adapt | Same order, plus Torrents/Bundles/Browser/Updates/Diagnostics |
| Scheduler dialog | adapt | Queue list, start/stop, schedule, retries, concurrent files |
| Progress/details window | adapt | HTTP details with segments + links; torrent files/peers/trackers |
| Grabber review list | adapt | Bundle preview with weak links and duplicates |
| Registration/trial/upgrade popups | exclude | Never; About only |

## Settings to merge now (P0/P1)

- global speed limit, max segments, per-host exceptions (adapt to host limits);
- Save To per category, temp dir;
- silent dialogs / no modal storms;
- connection speed selection -> replace with measured link capacity;
- file types / site capture exclusions;
- proxy (P2), quota windows (P3), scheduler (P2).

## Settings to exclude

- dial-up, event sounds, site credential vault, modem hangup, force-terminate,
  registration/trial, browser click-monitor for IE.

## ripdl-exclusive things to keep visible

- **Torrents**: sidebar category, seeding state, details tabs, per-task cap,
  bonding rates in Settings > Connection.
- **Bundles**: sidebar category, parts count, preview/review, auto-extract policy.
- **Refresh address**: toolbar + context + details; expired/stale state.
- **LAN drop box**: Send & Receive in toolbar/Settings/tray.
- **Verified updates**: Settings > Updates with honest state, no nag.
- **Diagnostics**: log level + Copy diagnostics, no telemetry.
- **No telemetry / no ads / no registration**: fixed product rules.

## UI implementation order

1. S01 main shell (menu, toolbar, categories, table, status) - from `cards/S01-main.md`.
2. S02 Add URL - `cards/S02-add-url.md`.
3. S03 Settings with IDM tab order + ripdl groups - `cards/S03-settings.md` + `SETTINGS.csv`.
4. S12 speed display from snapshot - `cards/S12-speed.md`.
5. S07 details, S04 scheduler, S05 grabber, S08 welcome, S09 tray.
6. Screenshot QA each card with `UI-QA.md`.

## Rule for future agents

Do not load all cards at once. Use `MAP-INDEX.md`, open one card, implement it,
capture its screenshots, then move to the next. `FEATURES.csv` and
`SETTINGS.csv` are the master trackers; update them in the same commit.
