# S08 - Browser-integration welcome

Status: `spec ready`. Read `SHARED.md`, `ONBOARDING.md` (full mechanism).

## Goal

After install/upgrade, connect the browser with one prompt and one contextual
page. No account, no wizard, no upgrade nag.

## IDM behavior

- Installer registers native messaging host and ships extension packages.
- Finish opens browser add-extension prompt, then local welcome page.
- Page state comes from query flags: installed/updated, not installed, needs
  update, incognito, consent. One state shown at a time; one primary action.
- Page closes itself through native host.

## ripdl target

- `ripdl register-host --open-welcome` opens `chrome-extension://<pinned>/welcome.html`.
- Installer `[Run]` calls it after register-host (already has the hook).
- Welcome page states:
  - `connected` -> "Open ripdl" (primary).
  - `needs-permission` -> "Allow in private windows" + steps.
  - `error` -> "Retry" / "Done".
  - `needs-update` -> "Update ripdl" (opens our own updater, no nag).
  - `done` -> "Done" (closes page).
- Host commands: `status`, `close_page`, `close_window`.

## State model

```text
state: connected | needs-permission | error | needs-update | done
browser: chrome | edge | firefox
host_alive: bool
```

## Acceptance

- [ ] Install -> welcome -> connected -> page closes (e2e).
- [ ] No telemetry/consent prompt.
- [ ] Firefox wording differs for private windows.
- [ ] Page never ships IDM text or art.

## Work IDs

ONBOARDING.md R1..R6. FEATURES.csv: IDM-025..028, IDM-038 (excluded).

## Do not

- Do not copy IDM's page, wording, icons, or extension code.
