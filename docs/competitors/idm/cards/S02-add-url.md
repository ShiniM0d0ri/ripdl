# S02 - Add URL

Status: `spec ready` (native spike has a shell only). Read `SHARED.md`, `S01-main.md`.

Screenshot: IDM add dialog is a separate window; ripdl spike: `research/idm-portable/../spike/native-ui/qa/shots/add.png`

## Goal

Paste a link and start. Advanced choices must never clutter the first screen.

## IDM behavior

- One URL field.
- Optional save-to folder.
- Auth/headers/cookies in an options sub-dialog.
- Primary action: Download. Secondary: Cancel.

## ripdl target

Simple view:

- URL (single line, autofocus).
- Save to (text + Browse).
- Primary `Download`, secondary `Cancel`.

Advanced expander (collapsed by default):

- Category.
- Video/stream (yt-dlp).
- Torrent file / magnet.
- Sequential download (torrent).
- Custom headers (name/value rows).
- Start after / start at (links to S04).
- Bundle import link (links to S05).

## State model

```text
url: String
dest: Option<PathBuf>
advanced_open: bool
headers: Vec<(String, String)>
category: Option<CategoryId>
mode: direct | video | torrent
start_at: Option<DateTime>
validation_error: Option<String>
duplicate: Option<TaskInfo>
```

## Interactions

- Enter in URL field submits when valid.
- Duplicate URL -> inline answer offering "Download again" / "Open existing".
- Invalid URL -> inline error, no modal.
- Submit -> RPC `Add`, dialog closes on `Added`.

## Acceptance

- [ ] Simple view has exactly one primary action.
- [ ] Advanced fields are hidden until expanded.
- [ ] Validation errors inline and redacted.
- [ ] Duplicate flow reuses daemon `DuplicatePolicy::Report`.
- [ ] Screenshots: simple, advanced, validation error, duplicate.

## Work IDs

FEATURES.csv: IDM-007..009, RIP-041, RIP-042.
UI-REHAUL: UXD-002.

## Do not

- Do not show speed/bonding/torrent settings here; those are S03.
