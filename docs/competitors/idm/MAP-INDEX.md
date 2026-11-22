# IDM UI map - implementation index

Read **only this file**, then open the one card for the screen you are building.
Each card is self-contained. Shared rules live in `SHARED.md` (read once).

Upgrade/registration/trial popups are **excluded** from this map.

| Card | Screen | ripdl status | Depends on |
|---|---|---|---|
| S01 | Main window + task table | spike implemented (light Segoe/Fluent shell, table, actions, status, search; sort/resize/multi-select open) | SHARED |
| S02 | Add URL | spike implemented (form + advanced + Add RPC; folder picker and duplicate review open) | S01 |
| S03 | Settings / Options | spike partial (all 20 daemon-backed settings exposed incl. LAN; IDM-only options tracked in UI-OPTIONS.csv) | SHARED, S01, SETTINGS.csv |
| S04 | Scheduler | spike shell (queue list + schedule fields static) | S02, S03 |
| S05 | Link grabber / bundles | spec ready, bundle engine exists | S02, S01 |
| S06 | Queues | spec ready, single queue today | S01 |
| S07 | Task details / progress | spike implemented (tabs, part/file progress, open file/folder) | S01 |
| S08 | Browser-integration welcome | spec ready in ONBOARDING.md | extension R1-R6 |
| S09 | Tray + mini panel | spec ready, Tauri tray exists | S01 |
| S10 | Categories + filters | spike partial (categories, counts, local search) | S01 |
| S11 | Context menus + selection | spike partial (row selection + actions; context menu open) | S01 |
| S12 | Speed display + graph | spike partial (status-strip rate; graph open) | S01 |
| S13 | Empty / error states | spike partial (empty + offline defaults; loading/errors open) | S01 |
| S14 | First-run onboarding | spec ready, 3-screen flow planned | S02, S03 |

## UI review ledger

`UI-REVIEW.csv` is the grouped, screenshot-backed review ledger. It contains
implemented-feature rows (`F-*`) and findings (`R-*`) with severity, status
(`fixed/verified`, `open`) and evidence. The spike screenshots live in
`spike/native-ui/qa/shots/`.

`UI-OPTIONS.csv` is the per-option coverage ledger: all 20 daemon-backed
settings plus the 50 mapped IDM options, each with `ui_status`
(`exposed`, `extension`, `planned`, `excluded`, `not_applicable`).

The design source is `spike/native-ui/design/` (UICanvas 1.3.0, MIT), with
exported reference shots in `spike/native-ui/qa/design/`.

## How to use

1. Pick the card for your screen.
2. Read `SHARED.md` once for invariants and conventions.
3. Implement only that card; do not pre-load other cards.
4. Update the card's status line and `FEATURES.csv` in the same commit.

## Source artifacts

Screenshots and extracted extension files are in `research/idm-portable/`
(gitignored). Do not commit them or copy their assets into ripdl.
