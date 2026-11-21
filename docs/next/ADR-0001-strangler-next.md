# ADR-0001: Strangler next, not a greenfield rewrite

- Status: accepted
- Date: 2026-11-21
- Branch: `next`

## Context

ripdl works: multi-segment HTTP, sidecar resume, torrents, browser capture, LAN
transfer, signed updates, and an installer all ship. The defects are
cross-cutting: speed limits are per task, torrent/video traffic bypasses them,
per-NIC rates come from two unrelated systems, and frontends compute their own
rates.

A full rewrite would replace the parts that already work and would re-create the
same seams. The new Rust download-manager field (Mister Download Manager, Prism,
swiftdl, Firelink, SSDownload, NDM-Next, KGet) confirms that the engine shape is
not the problem: the winners have one global limiter, per-host limits, one link
model, and a crash-safe state file.

## Decision

Use a strangler approach:

1. Keep `ripdl-core`'s engine, resume format, torrent session, refresh, bundle,
   LAN, update, and the daemon's queue semantics.
2. Introduce two new modules behind traits: `traffic` and `links`.
3. Wire them into the daemon behind a `unified-traffic` feature flag.
4. Add a protocol next snapshot while keeping v1 commands as adapters.
5. Flip the default only after the conformance suite is green.
6. Prototype a native UI in parallel, but do not let it block the truth layer.

## Consequences

Positive:

- working features keep shipping on `main`;
- each step has a failing test and a rollback point;
- the browser extension and old CLI keep working through v1 adapters;
- the native UI decision is based on measurements, not a rewrite deadline.

Negative:

- there are two code paths during the transition;
- the feature flag and protocol adapters add temporary complexity;
- discipline is required: no new features until the truth layer is green.

## Alternatives considered

- **Full greenfield rewrite.** Rejected: highest cost, loses working resume,
  torrent, installer, browser host, and LAN code, and repeats the same
  accounting mistakes.
- **Keep patching the current rate code.** Rejected: every patch adds another
  owner; the bugs are structural.
- **Migrate the UI first.** Rejected: the UI is not the source of the wrong
  numbers; it would hide daemon bugs behind a new shell.
