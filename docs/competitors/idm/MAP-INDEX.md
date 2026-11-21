# IDM UI map - implementation index

Read **only this file**, then open the one card for the screen you are building.
Each card is self-contained. Shared rules live in `SHARED.md` (read once).

Upgrade/registration/trial popups are **excluded** from this map.

| Card | Screen | ripdl status | Depends on |
|---|---|---|---|
| S01 | Main window + task table | spec ready, native spike partial | SHARED |
| S02 | Add URL | spec ready, spike shell only | S01 |
| S03 | Settings / Options | spec ready, generated schema landed | SHARED, S01 |
| S04 | Scheduler | spec ready, daemon support exists | S02, S03 |
| S05 | Link grabber / bundles | spec ready, bundle engine exists | S02, S01 |
| S06 | Queues | spec ready, single queue today | S01 |
| S07 | Task details / progress | spec ready, Tauri dialog exists | S01 |
| S08 | Browser-integration welcome | spec ready in ONBOARDING.md | extension R1-R6 |
| S09 | Tray + mini panel | spec ready, Tauri tray exists | S01 |
| S10 | Categories + filters | spec ready, categories exist | S01 |
| S11 | Context menus + selection | spec ready | S01 |
| S12 | Speed display + graph | spec ready, daemon truth landed | S01 |
| S13 | Empty / error states | spec ready | S01 |
| S14 | First-run onboarding | spec ready, 3-screen flow planned | S02, S03 |

## How to use

1. Pick the card for your screen.
2. Read `SHARED.md` once for invariants and conventions.
3. Implement only that card; do not pre-load other cards.
4. Update the card's status line and `FEATURES.csv` in the same commit.

## Source artifacts

Screenshots and extracted extension files are in `research/idm-portable/`
(gitignored). Do not commit them or copy their assets into ripdl.
