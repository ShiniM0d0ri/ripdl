# Extension plan (v2)

The extension is a first-class frontend, not a capture add-on. It is the only
frontend that runs inside another vendor's process, so it has the tightest
constraints: a 1 MiB native-messaging reply cap, an MV3 service worker that can
be killed at any time, and a browser download that must never be blocked by a
missing daemon.

## Current state (grounded in the code)

- `ext/background.js` connects a persistent native-messaging port
  (`chrome.runtime.connectNative`) and sends newline-free framed JSON through
  `ripdl.exe` native host.
- The host (`bridge.rs`) uses 8 worker threads, a 64-frame queue, a 16 MiB
  inbound frame cap, and a 1 MiB outbound cap (`HOST_TO_BROWSER_MAX`).
- `BridgeConfig::default()` sets `use_pipe: true`, and `connect_daemon_sync`
  tries the named pipe / unix socket before TCP. **The AGENTS backlog note
  "bridge itself still speaks TCP" appears stale** - verify with an e2e
  assertion before scheduling work.
- Liveness is checked by sending `{ type: "list" }` and seeing any reply
  (`background.js:247`). With a large task list this is wasteful and can hit
  the reply cap.
- Capability detection uses `get_config` (`bundles` field), which is the right
  pattern and should be extended to a protocol version.
- Offers are capped at 100 links (`MAX_OFFER_LINKS`), deduped and http(s)-only.
- A 15 s pending map resolves to `null` on timeout; on port disconnect all
  waiters are resolved and the map is cleared. Good failure behavior.
- Duplicate flow uses daemon-side `propose` / `abandon` with a 60 s TTL.
- Browser integration e2e (B0-B17) is the strongest gate in the repo.

## Problems v2 must solve

| ID | Problem | Impact |
|---|---|---|
| XTN-001 | Full `list` polling, including liveness probe | Payload grows with task count; can exceed 1 MiB |
| XTN-002 | No protocol version handshake | A newer/older pair must guess via `get_config` fields |
| XTN-003 | Hand-written protocol shapes in JS | Field drift is a runtime bug, not a compile error |
| XTN-004 | MV3 service worker loses in-memory task state | Popup/offer shows stale or empty data after restart |
| XTN-005 | Bridge retries all errors and can leak reader threads on RPC timeout | ISS-069, ISS-070 |
| XTN-006 | Firefox remains unsupported | BRW-012 |
| XTN-007 | No diagnostics from the extension side | "Nothing happened" is hard to support |

## Workstreams

### E1 - Verify the transport and harden the host

- Add an e2e assertion that the host reaches the daemon over the named pipe on
  Windows (no TCP listener required). If it already passes, close the backlog
  item as stale.
- Fix ISS-069 (timeout thread leak) and ISS-070 (retry only connect failures).
- Add `ping` to the protocol; replace the `list` liveness probe with it.

Exit: `bridge_timeout_no_thread_leak`, `bridge_no_retry_on_auth_failure`, and a
C4-style check proving pipe transport.

### E2 - Protocol handshake

- `extension_hello` carries `version` and `protocol: 2`.
- Daemon replies with supported protocol versions and capability flags;
  `GetConfig` keeps the existing fields for older clients.
- If the daemon is v1, the extension uses the v1 adapters and marks itself
  "limited mode" in options. It must never hang waiting for a v2 command.

Exit: `protocol_v1_compat`, plus an e2e test with a v1-only mock daemon.

### E3 - Delta snapshot store

- Replace task-list polling with `GetSnapshot { since }`.
- Keep a `Map<id, TransferSnapshot>` in `background.js`; apply `changed`,
  `removed`, and `seq` from the snapshot.
- Rehydrate with `since: None` on service-worker startup and after a port
  reconnect.
- Render only changed rows in the popup/options; keep the port open.

Exit: `snapshot_200_tasks_under_1mib`, `delta_snapshot_only_changed`, and an
extension test that kills the service worker and verifies rehydration.

### E4 - Generated types

- Add `xtask codegen-protocol` producing `ext/src/protocol.d.ts` from the Rust
  RPC types.
- CI fails when generated files drift.
- Use the generated `Reply`/`TransferSnapshot` types in `background.js`.

Exit: `generated_types_current`.

### E5 - Capture scalability and plugin capabilities

- Keep the 100-link offer cap; add a per-tab media list cap (there is already
  `MEDIA_PER_TAB`) and document both in the matrix.
- Give hoster plugins a small capability manifest (which hosts, what data they
  read) so `keepRecognized` does not have to run every plugin against every
  weak candidate.
- Never let a plugin call the daemon directly; it returns proposals to the
  background script, which owns all RPC.

Exit: a test with 100 weak anchors that completes within the existing probe
budget.

### E6 - Diagnostics

- Add `ripdl_extension_status` fields: last protocol version, last snapshot
  seq, last error, host alive, daemon version.
- Add "Copy diagnostics" in options: redacted URLs, last error, protocol skew.
  This was already a product-research request (OBS-003).

Exit: an e2e B-phase check that the diagnostics contain no auth tokens.

### E7 - Firefox parity (later wave)

- MV3 vs MV2 differences in `webRequest`, `downloads`, and native messaging.
- Do not start until the Chromium v2 path is green.

## Version skew matrix

| Daemon | Extension | Behavior |
|---|---|---|
| v0.4 (v1 only) | v0.4 (v1 only) | Current shipped behavior |
| v0.5 (v1+v2) | v0.4 (v1 only) | v1 adapters answer; no delta, no limits accuracy claims |
| v0.5 (v1+v2) | v0.5 (v2) | Delta snapshots, version handshake, generated types |
| v0.6+ (v2 only) | v0.4 | Refuse with an explicit "update the extension" status, never hang |

## Gate additions (B phase)

- B18+ v2 handshake and `ping`.
- B19 delta snapshot applies `changed` and `removed` correctly.
- B20 200-task snapshot stays under 1 MiB or paginates cleanly.
- B21 service-worker restart rehydrates from `since: None`.
- B22 generated types match the Rust source in CI.
- B23 pipe transport verified (close the stale AGENTS item if it passes).

## Definition of done for the extension track

- No `list` polling for liveness or state.
- One protocol version source of truth, generated into TypeScript.
- The extension renders daemon snapshots and computes no rates.
- Old extensions keep working against the v0.5 daemon.
- Capture still falls back to the browser when ripdl is absent.
