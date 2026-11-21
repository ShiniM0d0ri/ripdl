# UI rehaul and onboarding

Goal: a calm, fast, trustworthy download manager that a new user can use in
under a minute without reading docs. Clutter is a bug, not a style choice.

This is a capability track in `FEATURE-ROADMAP.md` (UXD ids). It does not wait
for v0.5 truth-layer work to finish: onboarding and empty states can land
early, but no screen is called done until it passes `UI-QA.md`.

## Principles

1. **Progressive disclosure.** Show the next useful thing, not every possible
   thing. Advanced controls live behind an explicit "Advanced" expander.
2. **One primary action per screen.** Every dialog has one visually dominant
   button; everything else is secondary or text.
3. **Honest states.** `connecting`, `verifying`, `metadata`, `stale`, `seeding`,
   and `0 B/s` are distinct and labelled. Never a spinner that means "stuck".
4. **No math in the UI.** The frontend renders `TransferSnapshot`; it never
   derives speed, totals, or ETA.
5. **Keyboard first.** Every action reachable without a mouse, with visible
   focus and sensible tab order.
6. **Accessible by construction.** AccessKit names, roles, and actions on
   every interactive element; WCAG AA contrast; 100/150/200% DPI correct.
7. **Density without noise.** Default columns: name, size, progress, speed,
   status, kind. Optional columns off by default.

## First-run onboarding (three screens, all skippable)

A new user sees a short flow, then a clean empty state. Nothing blocks them.

| Step | Screen | Content |
|---|---|---|
| 1 | Welcome | "Where should downloads go?" prefilled with the user's Downloads folder. One field, one **Start** button. A small "I'll choose later" text link. |
| 2 | Browser (optional) | "Catch downloads from your browser?" One button to register the host, with the exact CLI command shown for copy/paste. **Skip** is prominent. |
| 3 | Network (optional) | Shown only when at least two working uplinks exist. Simple speed-limit field and bonding toggle, both optional. **Done**. |

Rules:

- No account, no email, no telemetry opt-in, no wizard pages beyond these.
- The flow is idempotent and replayable from Help > Run setup again.
- Skipping step 2/3 uses safe defaults; nothing later nags.
- After step 3 the main window shows an empty state with one CTA:
  "Paste a link or drop a .torrent file here".

## Screen inventory

Every screen below has a QA fixture and screenshots for each listed state.

| ID | Screen | States to verify |
|---|---|---|
| UXD-001 | Main task list | empty, 1 running, mixed states, 200 tasks, multi-selection, filtered |
| UXD-002 | Add URL | simple, advanced expanded, validation error, duplicate found |
| UXD-003 | Add torrent | file picker, metadata waiting, file checklist, sequential toggle |
| UXD-004 | Add bundle | pasted links, preview, duplicate links, grouped package |
| UXD-005 | HTTP details | running, paused, stale/expired, refresh dialog, open file/folder |
| UXD-006 | Torrent details | files, peers, trackers, seeding, verifying, empty peers |
| UXD-007 | Settings | six tabs, generated schema, advanced filter, restart badge, save error |
| UXD-008 | Send & Receive | code shown/hidden, received files, partial rows, discovered devices, send progress |
| UXD-009 | Update | current, available, downloading, install failed, auto-install opt-in |
| UXD-010 | About / Help | version, links, run setup again, diagnostics |
| UXD-011 | Tray + mini panel | hidden, revealed, single-instance forward, context entries |
| UXD-012 | Refresh address | pre-filled URL, dry-run verdict, kept X of Y, swap + resume |
| UXD-013 | Extension popup | host alive, host missing, capture offer, duplicate window |
| UXD-014 | Extension options | connection status, plugins, generated settings, diagnostics |
| UXD-015 | Error/empty states | no daemon, permission denied, disk full, dead link, wrong URL |

## Component system

One shared set of tokens and components, used by Tauri UI and the native spike:

- **Tokens**: color, spacing, radius, typography, z-index, motion - one source
  shared with the extension where practical.
- **Icons**: one inline sprite; every icon has an accessible name and tooltip.
- **Row**: name with file-type glyph, size, progress bar, speed, ETA, status
  badge, kind icon. Stable cell keys so a volatile update cannot shift columns.
- **Chips/badges**: seeding, verifying, connecting, stale, partial, excluded.
- **Dialogs**: one primary action, Escape closes, focus returns to opener.
- **Inputs**: size/rate fields parse and validate inline with unit examples.
- **Toasts**: transient status only; never the only place an error is shown.
- **Tables**: sortable, resizable, persisted widths, virtualized for 200 rows.

Each component has a fixture in the QA harness: a small screen that renders
every state (default, hover, focus, disabled, error, loading) for screenshots.

## Anti-clutter rules

- Advanced columns (peers, full path, ETA) are opt-in via View menu.
- Settings shows common fields first; "Advanced" reveals the rest.
- Context menus show only actions valid for the current selection.
- No banner unless it needs a decision. Update banner hides when current.
- Errors explain the next action, not just the status code.
- Progress bars never animate without real progress; use indeterminate only
  with a text label such as `connecting`.

## Definition of a perfect screen

A screen is done only when all of these pass:

- [ ] One primary action is obvious within 2 seconds of looking at a screenshot.
- [ ] No clipped text or overflow at 100/150/200% DPI, 1280x720 minimum.
- [ ] WCAG AA contrast; focus visible; AccessKit/ARIA names on all controls.
- [ ] Keyboard-only flow completes the screen's main task.
- [ ] Empty, loading, error, and one-result states all have screenshots.
- [ ] 200-row state scrolls without layout shift or frame drops.
- [ ] All text is honest: no stale rate, no false 100%, no unexplained dash.
- [ ] The screenshot is reviewed against `UI-QA.md` and stored with the run.

## Relationship to other tracks

- v0.5: onboarding, empty states, and the generated-settings form land with
  the Tauri GUI adapter (M4b).
- v0.6: full rehaul of all screens, component system, native-spike parity.
- v0.7: accessibility polish, themes, localization-ready layout.

The native spike is not a second design. It consumes the same screen inventory
and component rules; only the renderer changes.
