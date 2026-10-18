# ripdl

A free, ad-free, multi-segment download manager for Windows and Linux, written
in Rust — with an IDM-style desktop app and a CLI.

ripdl exists because the alternatives are **paywalled** (IDM), **ad-supported**
(Free Download Manager ships ads + tracking), **abandoned** (Motrix), or
**CLI-only** (aria2). ripdl makes no network requests except the downloads you
ask for: no analytics, no crash reporting, no upgrade nags, no bundled offers.
Your history stays on your machine. Source code right here.

## What it does

- **Downloads any URL** using multiple parallel segments (or one plain stream
  when the server doesn't support ranges), with **automatic resume** of
  interrupted tasks
- **Torrents** — magnets, `.torrent` files and URLs, with per-file selection,
  sequential ("stream as you download") mode, files/peers/trackers view
- **Video/stream pages** — route a YouTube-like page through the well-known
  `yt-dlp` tool instead of reimplementing site extraction
- **Queue + scheduler** — background daemon with concurrency limits, speed
  cap, and "start after 1h" / "start at 22:30" scheduling
- **GUI app** — live task list with speeds and progress, settings, system tray
- **CLI** — scriptable, no daemon needed for plain downloads
- **NIC bonding** — combine Wi-Fi + Ethernet + tethering into one faster pipe
  for a single download

See [docs/ROADMAP.md](docs/ROADMAP.md) for the full status and roadmap.

## Install

Grab an installer from the [latest release](https://github.com/ShiniM0d0ri/ripdl/releases):

- **Windows (recommended)**: download `ripdl-v*-x86_64-windows-setup.exe` and
  run it. It installs to `C:\Program Files\ripdl` and launches the GUI.
- **Windows (portable)**: unzip `ripdl-v*-x86_64-windows-portable.zip`
  anywhere (the pinned browser extension is under `ext/` inside).
- **Linux**: install the shipped `.deb` / `.rpm` package — or extract
  `ripdl-v*-x86_64-linux.tar.gz` and copy `ripdl`, `ripdl-daemon` and
  `ripdl-gui` to a `PATH` folder:
  `tar -xzf ripdl-v*-x86_64-linux.tar.gz && sudo cp ripdl-v*/ripdl ripdl-v*/ripdl-daemon ripdl-v*/ripdl-gui /usr/local/bin/`.

The Windows installer includes the browser extension under
`C:\Program Files\ripdl\ext`; the desktop app and daemon are installed with it.

### First steps in the GUI

1. Click **Add URL**, paste a link, pick a folder, **OK**.
2. Watch it go — right-click a row to pause, resume or open the file.
3. Settings (gear icon) controls the speed cap, folder defaults,
   download/segment counts and updates.

### Command line

```sh
ripdl download https://example.com/big-file.zip         # direct download, resumable
ripdl info https://example.com/big-file.zip             # inspect size/ranges first
ripdl download -o D:/Downloads -s 16 <url>              # folder + segment count
ripdl add https://example.com/big-file.zip              # queue via the daemon
ripdl add --after 1h <url>                              # schedule
ripdl add <magnet-link-or-.torrent> --sequential        # torrents
ripdl add --video "https://youtube.com/watch?v=..."     # video/streams (needs yt-dlp)
ripdl ls                                                # live queue + progress
ripdl pause <id> | ripdl resume <id> | ripdl rm <id> [--files]
ripdl config                                            # see/set daemon settings
ripdl update                                            # check for a new release
```

Notes:
- `download` runs standalone; `add` uses the background daemon (auto-spawned
  if you just installed).
- Video pages need [yt-dlp](https://github.com/yt-dlp/yt-dlp) installed
  (`winget install yt-dlp.yt-dlp`, or set `ytdlp_path` in the settings).
- Interrupted downloads keep a `<file>.ripdl.json` sidecar — rerun the same
  command and it continues where it stopped.
- NIC bonding (combine all your uplinks into one download): the GUI settings
  checkbox, `ripdl config bond_interfaces=true`, or `ripdl download --bond`.

## Update & uninstall

- **GUI**: `Help → Check for Updates` (or just launch normally — it checks
  once per session by default). When a new release exists you get a banner
  with **Download & Install**: ripdl downloads the installer, verifies its
  SHA-256 against GitHub's checksum file, and runs it over the old install.
  Turn on *Download & install updates automatically* in Settings for a fully
  hands-off experience (UAC prompt is the only thing you'll see).
- **CLI**: `ripdl update` prints the installed/latest versions and the release
  URL; `ripdl update --install` performs the same verified upgrade.
- **Windows uninstall**: Settings → Apps → *ripdl* → *Uninstall*. It removes
  `C:\Program Files\ripdl` and the browser-host registrations. Your
  **downloads are kept**, the daemon's data dir `%LOCALAPPDATA%\ripdl` is
  **kept** (queue, settings — delete it by hand for a spotless machine), and
  the browser extension is removed from the browser itself
  (`chrome://extensions` → Remove).
- **Portable**: delete the unzipped folder. **Linux** (`deb`/`rpm`): remove
  the package (`apt remove ripdl` / `dnf remove ripdl`). **Linux** (tarball):
  `sudo rm /usr/local/bin/ripdl*` and delete the queue folder
  (`~/.local/share/ripdl`) if you want the data gone too.

Updating a portable copy: drop the new zip's files over the old one while no
download is running.

## Browser extension (experimental)

The MV3 extension hands browser downloads to ripdl, and from its popup can
also offer the current page's video/stream URL via yt-dlp.

1. `ripdl register-host` (registers the native messaging host, per-user)
2. Chrome/Brave/Edge → `chrome://extensions` → enable Developer mode →
   **Load unpacked** → pick `ext/` from the install folder
3. Right-click a download link → **Download link with ripdl** (there's also
   *this page URL*, *selected links* and *all page links*), or open the
   toolbar popup on a media page.

Privacy posture: it only acts when you click, only reads the page URL while
the popup is open, never touches history or runs page scripts, and shares
cookies only if you opt in.

## For developers

```sh
cargo build --release
cargo test        # fully offline; no real networks
```

Rule of the codebase: features land in `ripdl-core` (pure lib) / the daemon
once; CLI, GUI and the browser host are thin shells on top. Torrent support
runs on a vendored, minimally-patched
[librqbit](https://github.com/ikatson/rqbit) (Apache-2.0) under `vendor/`.

## License

MIT OR Apache-2.0 — see
[LICENSE-MIT](LICENSE-MIT) / [LICENSE-APACHE](LICENSE-APACHE).
