# ripdl

A free, ad-free, multi-segment download manager written in Rust.

ripdl exists because the alternatives are either **paywalled** (IDM), **ad-supported**
(Free Download Manager now ships ads + tracking), **abandoned** (Motrix), or
**CLI-only** (aria2). ripdl is the missing piece: a polished, native, offline-first
downloader with zero telemetry and zero monetization.

Built for the people who currently pirate IDM: use this instead - it is free,
it is honest, and its source code is right here.

## Status

v0.1 - engine + CLI. See [docs/ROADMAP.md](docs/ROADMAP.md) for the plan.

| Feature | Status |
| --- | --- |
| Multi-segment HTTP(S) acceleration | ✅ core |
| Resume (sidecar state file) | ✅ core |
| Per-segment retry + backoff | ✅ core |
| Cancellation | ✅ core |
| Failure-injection integration tests (offline) | ✅ core |
| CLI (`ripdl download`, `ripdl info`) | ✅ |
| Queue with concurrency limits | ✅ core + daemon-persisted |
| Auto-categorization by file type | ✅ core |
| Daemon + RPC (TCP localhost) | ✅ v1 |
| Tauri GUI | 🚧 P2 |
| Torrent (via `librqbit`) | 🚧 P3 |
| Browser extension + native host | 🚧 P4 |
| Speed limiter, scheduler | 🚧 backlog |

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

## Usage

```sh
ripdl info https://example.com/big-file.zip      # inspect before downloading
ripdl download https://example.com/big-file.zip  # 8 segments, resumable
ripdl download -o D:/Downloads -s 16 <url>       # custom dir + segment count
```

Interrupted downloads keep a `<file>.ripdl.json` sidecar; rerun the same
command and it resumes from where it stopped.

## Anti-adware position

The tools this replaces phone home and monetize you. For the record:

- ripdl makes **no network requests** other than the downloads you ask for
- no analytics, no crash reporting, no "upgrade" nags, no bundled offers
- everything runs locally; your download history stays on your machine

## License

MIT OR Apache-2.0 (dual-licensed, standard for Rust). License texts are added
before the first public release.
