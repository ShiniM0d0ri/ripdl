# IDM clean-room UI/onboarding analysis

Goal: understand **behavior and information architecture** of a mature download
manager, then map it to ripdl's next UI. This is not a code or asset port. No
IDM code, images, strings, or icons are copied into ripdl; only observations and
feature intent are recorded here.

Artifacts (installer, portable copy, screenshots, extracted extension files)
live in `research/idm-portable/` and are gitignored.

## Sources

- IDM 6.43 build 18 installer, downloaded from the official site.
- Installed files copied to `research/idm-portable/` (portable-style copy).
- Extension packages shipped with IDM: `IDMGCExt.crx`, `IDMEdgeExt.crx`,
  `idmmzcc*.xpi`; native host manifests `IDMMsgHost.json`,
  `IDMMsgHostMoz.json`.
- Screenshots captured from the real UI in the interactive desktop session:
  `research/idm-portable/shots/idm-main.png`, `idm-onboarding-window.png`.
- Public product pages/feature lists where behavior was not observable offline.

## Documents

- `UI-MAP.md` — every screen and its controls, one by one.
- `FEATURES.csv` — machine-readable feature mapping to ripdl work items.
- `ONBOARDING.md` — how the post-install browser-integration popup works and
  how ripdl should replicate the pattern.

## Reading rule for future agents

Open `UI-MAP.md` for the screen you are implementing, then filter `FEATURES.csv`
by that screen id. Mark ripdl status honestly in the CSV when work lands; do
not mark a row `shipped` from a design doc alone.
