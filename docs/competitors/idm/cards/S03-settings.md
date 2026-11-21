# S03 - Settings / Options

Status: `spec ready` (schema and codegen landed; UI to build). Read `SHARED.md`
and `SETTINGS.csv` for the individual 50 options; this card is only the screen
shell.

Screenshot: IDM Settings dialog not captured; group order from IDM help + UI-MAP S03.

## Goal

One generated settings surface shared by GUI, CLI and extension. Familiar order,
no orphan fields, apply policy visible.

## IDM behavior

Groups: General, Connection, Download, Scheduler, Site Grabber/Monitoring,
Video, Advanced. Per-group apply on OK (no live preview).

## ripdl target group order

1. General: download folder, startup, tray, language.
2. Connection: global/task/host caps, burst, segments, proxy, HTTP/2, bonding,
   probe targets, uplinks/exclusions.
3. Content: filename collision policy, checksum, sidecar cadence, categories.
4. Torrents: seed ratio/time, listen port, UPnP, DHT, fastresume, per-task cap.
5. Bundles: auto-extract, part-set policy.
6. Browser: extension status, capture policy, referer rules, welcome page.
7. Updates: check/auto-install, verification (no upgrade nag).
8. Diagnostics: log level, Copy diagnostics, retained logs.

## State model

```text
schema: Vec<SettingDef>          // from GetSchema
draft: Map<SettingId, Value>     // edited values
baseline: SettingsSnapshot       // from GetSettings
changed: Set<SettingId>
warnings: Vec<String>
restart_required: bool
```

## Interactions

- Search box filters settings by id/description.
- Each row shows an apply badge: Live / Next task / Restart.
- Save sends `SetSettings` patch; unchanged fields omitted.
- Invalid value -> inline error with id and allowed range.
- Preview button shows effective value and restart warnings.

## Acceptance

- [ ] Every row has a schema id, default, validator, UI hint, apply badge.
- [ ] Live settings reach daemon within one tick (test from SETTINGS.md).
- [ ] No hand-written field outside the schema.
- [ ] Generated types current (`xtask --check`).
- [ ] Screenshot per tab and one save-error state.

## Work IDs

FEATURES.csv: IDM-010..017, RIP-043..046.
Docs: `docs/next/SETTINGS.md`, `CFG-*`.

## Do not

- Do not add registration, trial or upgrade prompts.
- Do not duplicate a setting in CLI/GUI/extension by hand.
