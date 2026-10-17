# ripdl

A free, ad-free, multi-segment download manager written in Rust.

ripdl exists because the alternatives are either **paywalled** (IDM), **ad-supported**
(Free Download Manager now ships ads + tracking), **abandoned** (Motrix), or
**CLI-only** (aria2). ripdl is the missing piece: a polished, native, offline-first
downloader with zero telemetry and zero monetization.

Use it instead of cracking IDM - it is free, it is honest, and its source
code is right here.

## Download

Prebuilt binaries for **Windows x64** and **Linux x64** are attached to each
[GitHub Release](https://github.com/ShiniM0d0ri/ripdl/releases) (tagged
releases; latest build → latest tag).

- Windows: unzip `ripdl-v*-x86_64-pc-windows-msvc.zip`, put `ripdl.exe` /
  `ripdl-daemon.exe` on your `PATH`. The MV3 browser extension is included
  in the same archive under `ext/`.
- Linux: extract `ripdl-v*-x86_64-unknown-linux-gnu.tar.gz`
  (`tar -xzf ripdl-*.tar.gz -C ~/.local/bin`).

Then:

```sh
ripdl download https://example.com/big-file.zip
```

The daemon is spawned automatically on demand. No installation steps beyond
that, no services, no telemetry.

Platform note: the local RPC uses a named pipe on Windows, a unix socket
on Linux, with TCP loopback as a fallback everywhere.

Build from source instead (Rust 1.98+):

```sh
cargo build --release
```

## Status

v0.1 - engine + CLI. See [docs/ROADMAP.md](docs/ROADMAP.md) for the plan.

| Feature | Status |
| --- | --- |
| Multi-segment HTTP(S) acceleration | ✅ core |
| Resume (sidecar state file) | ✅ core |
| Per-segment retry + backoff | ✅ core |
| Cancellation | ✅ core |
| Headers / cookies / referer / UA pass-through | ✅ core + CLI |
| Content-Disposition filename (RFC 5987) | ✅ core |
| Duplicate-URL collision guard (daemon) | ✅ daemon |
| Failure-injection integration tests (offline) | ✅ core |
| Scheduler (`--after` / `--at` HH:MM) | ✅ daemon + CLI |
| Browser extension click-capture (MV3) | ✅ `ext/` + native host |
| CLI (`ripdl download`, `ripdl info`) | ✅ |
| Queue with concurrency limits | ✅ core + daemon-persisted |
| Auto-categorization by file type | ✅ core |
| Daemon + RPC (TCP localhost + named pipe) | ✅ v1 |
| Video/stream hunt (`yt-dlp` sidecar, `add --video`) | ✅ daemon + CLI |
| Tauri GUI | 🚧 P2 |
| Torrent (via `librqbit`) | 🚧 P3 |

## Architecture

```
ripdl-core (pure library, no UI deps)
   events + CancellationToken
        │
   ┌────┴─────┬──────────┬──────────────┐
   │          │          │              │
 ripdl     daemon      Tauri GUI    browser ext
 (CLI)      (RPC)       (P2)      + native host (P4)
```

New features are implemented in `ripdl-core`/daemon once; every frontend gets
them automatically. That is the rule that keeps this codebase sane as it grows.

## Build

```sh
cargo build --release
cargo test
```

Tests are fully offline and safe behind any proxy/air gap: `crates/ripdl-core/tests/`
runs a hand-rolled mock HTTP server (drop injection, no-range mode, etag
switching) - the suite never hits real networks.

## Usage

```sh
ripdl info https://example.com/big-file.zip      # inspect before downloading
ripdl download https://example.com/big-file.zip  # 8 segments, resumable
ripdl download -o D:/Downloads -s 16 <url>       # custom dir + segment count
```

### Hunt mode (video/stream pages)

```sh
ripdl add --video "https://www.youtube.com/watch?v=..."        # or any stream page
```

Video tasks are handed to a `yt-dlp` sidecar (ripdl does **not** reimplement
site extraction). Progress, pause/resume/cancel behave like every other task;
resume continues partial `.part` files. yt-dlp must be installed — e.g.
`winget install yt-dlp.yt-dlp` — or point the daemon at it in
`%LOCALAPPDATA%\ripdl\config.json`.

Interrupted downloads keep a `<file>.ripdl.json` sidecar; rerun the same
command and it resumes from where it stopped.

### Daemon + background queue

```sh
ripdl-daemon                                     # queue server (auto-spawned if absent)
ripdl add https://example.com/big-file.zip      # queue it, return immediately
ripdl ls                                        # live progress + speeds
ripdl add --after 6s <url>                      # schedule (also: --at 22:30)
ripdl pause <id> | resume <id> | rm <id>
```

### Browser capture (experimental)

The MV3 extension in `ext/` hands intercepted downloads to the ripdl daemon.
Minimal setup:

```sh
ripdl register-host                              # registers the native host
```

…then load `ext/` unpacked (chrome://extensions). Privacy posture: the
extension offers a daemon download *before* cancelling the browser item,
never touches your history, and shares cookies only if you opt in.

## Anti-adware position

The tools this replaces phone home and monetize you. For the record:

- ripdl makes **no network requests** other than the downloads you ask for
- no analytics, no crash reporting, no "upgrade" nags, no bundled offers
- everything runs locally; your download history stays on your machine

## License

MIT OR Apache-2.0 (dual-licensed, standard for Rust): see
[LICENSE-MIT](LICENSE-MIT) and [LICENSE-APACHE](LICENSE-APACHE).
