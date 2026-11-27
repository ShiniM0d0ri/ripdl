# Post-install browser-integration onboarding

Observed: after IDM installation, clicking **Finish** opens the browser with an
integration prompt, then a branded welcome page. The user needs no setup
knowledge: the browser asks to add the extension, and the page explains the few
states that can need attention.

## How it works (behavior only)

1. **Installer registers a native messaging host** for each browser family:
   - Chromium manifests (`IDMMsgHost.json`) list the allowed extension origins
     and the host executable.
   - Firefox manifest (`IDMMsgHostMoz.json`) lists the allowed extension id.
   - The host binary ships in the IDM folder.
2. **Extension packages ship with the installer** (`IDMGCExt.crx`,
   `IDMEdgeExt.crx`, `idmmzcc*.xpi`). They are installed during/after setup.
3. **Finish opens the browser** at the extension's own local welcome page
   (`welcome.html`) with query parameters describing the state:
   - `previous` — first install vs update,
   - `msedge` / `mzffox` / `opera` — browser-specific wording and support links,
   - `error` — IDM not installed/detected,
   - `update` — installed IDM is too old,
   - `incognito` — user must allow InPrivate/private windows,
   - `consent` — Firefox data-consent step.
4. The page presents **one situation at a time**:
   - extension installed / updated,
   - "IDM is not installed" with a retry/close action,
   - "IDM needs updating",
   - "allow in private windows" instructions,
   - Firefox data-consent with policy view and accept/refuse.
5. Buttons send small integer commands to the native host to close the tab or
   window, remove the add-on, or record consent. No account, no email, no
   multi-page wizard, no telemetry opt-in.

Screenshot (`research/idm-portable/shots/idm-onboarding-window.png`) shows
Chrome's extension page context; Chrome currently blocks the extension package
on this machine, but the page source and manifest are observable offline.

## Why it is good onboarding

- The **browser** performs the scary permission step, not the app.
- The page is **contextual**: one state, one primary action, browser-specific
  words, no generic "settings" dump.
- It **closes itself** through the native host once the user is done, so there
  is no leftover tab.
- Failure modes are explained in place (not installed, old version, incognito).

## ripdl adaptation plan (clean-room)

ripdl already has the hard parts: native host, `register-host`, and a versioned
extension. Add the missing last mile:

- R1: `ripdl register-host` writes manifests and verifies the installed
  extension id (already done). Add a `--open-welcome` mode that opens the
  browser at `chrome-extension://<pinned-id>/welcome.html?...`.
- R2: Installer's `[Run]` invokes `ripdl.exe register-host --open-welcome`
  after install/upgrade (already runs register-host; extend it).
- R3: Extension ships `welcome.html` with five states: `connected`,
  `needs-permission`, `needs-update`, `error`, `done`. Wording is ripdl's own.
- R4: One primary action per state: *Open ripdl*, *Grant private-window
  access*, *Update ripdl*, *Retry*, *Done*. No account or telemetry prompt.
- R5: Native host answers `close_page`, `close_window`, and `status` commands
  so the page can dismiss itself and report connection health.
- R6: Add an e2e check: install -> welcome page -> connected state -> page
  closes -> copy chip works from a test page.

Do **not** copy IDM's page, wording, icons, or extension code. The pattern is
the deliverable, not the pixels.
