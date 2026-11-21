# S14 - First-run onboarding

Status: `spec ready`. Read `SHARED.md`, `S02-add-url.md`, `S03-settings.md`.
Related: S08 (browser-integration welcome).

## Goal

A brand-new user reaches a useful state in under a minute with three skippable
screens. The browser integration prompt is handled by S08, not duplicated here.

## IDM behavior

- No app wizard; only browser integration popup after install.
- First launch goes straight to the task manager.

## ripdl target

1. Welcome: "Where should downloads go?" prefilled folder + Start.
2. Browser (optional): "Catch downloads from your browser?" register host,
   copyable command, prominent Skip. Links to S08 details.
3. Network (optional): only when >=2 uplinks exist; speed cap + bonding toggle.
4. Done -> main window with S13 empty state.

Rules:

- No account, no email, no upgrade/registration/telemetry prompt.
- Every step skippable; replayable from Help.
- State stored in config (`onboarding_done`), not a hidden file.

## State model

```text
step: 0 | 1 | 2 | 3
download_dir: PathBuf
browser_choice: register | skip
network_choice: configure | skip
done: bool
```

## Interactions

- Enter advances; Escape skips current step.
- Back button returns without losing earlier choices.
- Finish writes config and opens S01.

## Acceptance

- [ ] Three screens max; all skippable.
- [ ] No nag on later launches.
- [ ] Replay from Help works.
- [ ] Screenshot each step.

## Work IDs

FEATURES.csv: IDM-037, IDM-038 (excluded). UI-REHAUL UXD-001.

## Do not

- Do not merge this with S08; separate concerns (app setup vs browser handshake).
