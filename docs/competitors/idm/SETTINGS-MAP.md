# IDM individual settings/options map

Source: IDM's shipped help files, decompiled into `research/idm-portable/chm/`
(gitignored). Primary source: `idman/Options.htm`, `scheduler/scheduler.htm`,
`scheduler/IDM_queues.htm`. This is behavior mapping only; no IDM text, code or
images are copied into ripdl.

Machine-readable version: `SETTINGS.csv`. Open the CSV when implementing; this
file is the readable index.

## Options dialog — nine tabs

### General

- Start IDM automatically with Windows.
- Browser integration: basic vs advanced.
- Add Browser / Details per browser.
- Detect New Applications (take over downloads from new apps).
- Run click-monitoring module for IE-based browsers.
- Show download panel on web players for multimedia requests.
- Keys: special-key combinations to force or prevent capture.
- URL clipboard monitoring.

### File Types

- List of file types taken over from browsers.
- "Don't start downloading" site list with `*` wildcards.
- Address-exceptions list (`Edit list...`).

### Connection

- Connection speed selection.
- Max Connection Number (help warns against >4).
- Exceptions table: per-server connection count.
- Download Limits: quota windows (e.g. 40 MB/hour) with automatic resume.

### Save To

- Default directory per category.
- Remember last save path per category.
- Temporary directory for partial files.

### Downloads

- Download Progress dialog: don't show / show minimized / normal.
- Show Download Complete dialog.
- Silent mode: no start/progress/complete dialogs.
- Start download immediately while Download File Info dialog is open.
- Ignore file modification time changes when resuming.
- Antivirus executable path + command-line parameters on completion.

### Proxy

- Proxy server settings, Find (import from browser).
- Use FTP in PASV.
- Proxy login credentials.
- No-proxy address list with wildcards.

### Site Logins (registered-only in IDM)

- Per-site/directory URL + username/password.

### Dial-Up

- Dial-up connection selection, credentials, redial count/interval.

### Sounds

- Sound file per download event.

## Scheduler dialog

- Queue list with per-queue schedule and files.
- Start Now / Stop.
- One-time downloading vs Periodic synchronization.
- Start download at / Stop download at.
- Number of retries per file if download fails.
- Hang up modem when done (obsolete — excluded).
- Turn off computer when done / Force processes to terminate.
- Open a file or run a program when done.
- Files in queue: reorder with arrows/drag-drop, delete.
- Number of files downloaded at the same time per queue.
- Download Limits per queue.
- Changes saved immediately for order/contents; Apply for other fields.

## ripdl disposition

- Most rows map to the typed setting schema (`docs/next/SETTINGS.md`).
- IDM-only rows (dial-up, sounds, modem hangup, site logins as a separate
  credential store) are `excluded` or low priority; ripdl uses headers/cookies
  and the OS for those.
- Upgrade/registration/trial rows are excluded everywhere.
