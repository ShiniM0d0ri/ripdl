# Professional engineering rules

This document is the answer to "how do we stop writing bug-driven software".
Bugs are evidence that a rule was missing. We add the rule and its test; we do
not build a roadmap out of individual defects.

## Method

1. **Domain first.** Model the concepts (Task, Transfer, Link, Setting, Policy,
   Event) and their invariants before writing adapters.
2. **Contracts before code.** Every cross-module interaction is a trait or a
   versioned message with a contract test.
3. **Architecture decisions are recorded.** An ADR is required for a new
   dependency, a protocol change, a storage format change, or a new frontend.
4. **Bugs are triaged into invariants.** `ISSUES.csv` is evidence, not a
   backlog. A defect is closed when the invariant that prevents its whole class
   exists and passes.
5. **No feature without a matrix row.** Before code: id, owner, target wave,
   test name. Before `shipped`: the test passes.

## Layering (hexagonal)

```
domain/         pure types, invariants, policy decisions; no I/O
application/    daemon services: queue, scheduler, updater, event bus
ports/          traits: Clock, Meter, Traffic, Links, Settings, EventSink
adapters/
  inbound/      rpc, cli, gui, extension host, native messaging
  outbound/     engine, torrent, hunt, lan, update, disk, network
```

Dependency rule: **dependencies point inward**. `domain` depends on nothing in
this crate tree. `application` depends on `domain` and `ports`. Adapters depend
on `application` and `ports`. No adapter imports another adapter's internals.

Architecture fitness tests in CI:

- `core_does_not_depend_on_daemon_or_ui` (parse `Cargo.toml` + `use` graph);
- `domain_has_no_io` (no `tokio`, `std::fs`, `reqwest` in `domain/`);
- `every_port_has_contract_test`;
- `no_adapter_to_adapter_use`.

## Error model

- Domain/application errors are typed enums with stable codes.
- RPC errors carry `code`, `message`, and optional `details`. Human strings are
  for display only, never for control flow.
- Secrets (tokens, auth headers, codes) are redacted at the boundary; a `Debug`
  impl must not print them.
- Errors carry a cause chain and a trace id so a diagnostics bundle is useful.
- No `panic!`/`unwrap` in production paths where a user input can reach the
  value. Parsers return `Result`.

Contract tests:

- `error_codes_stable`: codes are additive and documented.
- `errors_are_redacted`: no secret appears in serialized errors.
- `rpc_error_shape`: v2 clients can switch on `code`.

## Observability

- Structured `tracing` with `trace_id` on every RPC request and download run.
- Metrics: active tasks, bytes by task/link, limiter wait time, retry counts,
  snapshot seq, queue depth, host probes.
- Diagnostics bundle (opt-in): redacted config, last N errors, protocol
  versions, snapshot seq, uplink probe results, recent logs.
- Logs rotate and are pruned by age/count.
- **No telemetry.** Every new outbound request needs an ADR and a setting; the
  uplink probe and update check are documented exceptions with opt-outs.

## Testing pyramid

| Layer | Scope | Location |
|---|---|---|
| Pure unit | parsers, planners, rate windows, limiter math, settings validation | crate `#[cfg(test)]` |
| Property/fuzz | sidecar parsing, filename parsing, percent/size parsing, ranges | `proptest` |
| Contract | each port trait has a fake implementing it | `tests/contracts/` |
| Integration | hand-rolled mock HTTP server (gzip, 429, stalls, no-range, ETag) | `tests/` |
| Daemon | in-process daemon + RPC, two-task and bundle scenarios | `daemon/tests/` |
| Frontend | node tests for extension, generated types drift | `ext/test/` |
| E2E | manual isolated pass of CLI/daemon/extension/GUI | `ext/test/e2e/` |

Never hit the real network in CI. Smoke URLs are manual only.

## CI gates

- `cargo fmt --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo check --workspace --all-targets`
- `cargo test --workspace` (offline)
- `cargo deny check` - enforce MIT/Apache-only for first-party deps; GPL stays
  out of the tree (librqbit is Apache-2.0)
- `cargo audit`
- node tests: `plugins`, `handoff`, `duplicate-model`
- `xtask codegen-protocol --check` - generated TS types are current
- architecture fitness tests
- scalability gates from `SCALABILITY.md`
- e2e gate is manual, documented, and run before every release
- visual QA gate from `UI-QA.md`: every changed screen has a passing screenshot
  with tree assertions; a failed or missing screenshot blocks the PR

## Review checklist

Every PR must answer:

- Which matrix feature id does this implement, and what is its new status?
- Which invariant does it add or protect? Which test encodes it?
- Does it change a port or protocol? If so, where is the ADR?
- Does it add a setting? If so, is it in `SETTINGS.md` with metadata and tests?
- Does it add an outbound network call? If so, why is it not telemetry?
- Are errors typed, redacted, and traceable?
- Are docs updated in the same PR?

## Versioning and compatibility

- Workspace SemVer. Protocol version is independent and additive when possible.
- A v0.5 daemon answers v1 commands for at least one full minor cycle.
- Storage formats carry a version and an upcaster; never a silent rewrite.
- Deprecations are announced in release notes one release before removal.
- Release notes are hand-written, user-facing, and short.

## Definition of "professional" here

Not "no bugs". It is:

- the rule is written before the fix;
- the test fails for the whole bug class, not one repro;
- the frontend cannot lie because it does not compute the number;
- a new feature cannot be added without its contract, setting metadata, and
  test;
- the next person can find all of that in `docs/v2/` without asking us.
