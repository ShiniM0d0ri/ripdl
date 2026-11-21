# Settings model (v2)

Settings are a **domain**, not a bag of `Option` fields. Today `DaemonSettings`
is a flat struct, validated only in `SetConfig`, with ad-hoc live behavior and
one field (`extension_dir`) that cannot be applied by `apply`. That is the shape
that produces "the setting exists but does nothing".

v2 replaces it with a versioned, typed, scoped, self-describing schema.

## Design principles

1. **Every setting has metadata**: id, type, default, range, apply policy, UI
   hint, description, and the version that introduced it.
2. **Every setting is validated at the boundary** - config load, RPC, CLI, and
   GUI all use the same validator.
3. **Every setting declares how it applies** - live, next task, restart, or
   read-only. The UI shows this; the daemon enforces it.
4. **Scopes are explicit** - global, category, host, and task. Precedence is
   defined, not implicit.
5. **The schema is the UI contract** - frontends generate forms from it instead
   of hand-maintaining a second copy.
6. **Unknown fields are never silently dropped** - warn, preserve, and upcast.

## Scopes and precedence

```
default  <  global  <  category  <  host  <  task
```

- `global`: daemon-wide defaults.
- `category`: per-category overrides (folder, speed, segments, seeding).
- `host`: per-host rules (connection cap, rate cap, retry policy), matched by
  host or suffix.
- `task`: stored on `TaskRecord` as a partial override.

Composition rules are explicit:

- **Aggregate caps** (global bandwidth) are shared budgets across all tasks.
- **Per-task caps** (task/category/host) are composed with `min`, so a host cap
  of 2 connections cannot be overridden by a task asking for 8.
- **Enums/flags** use the most specific scope that sets them.
- **Paths/identities** (download folder, yt-dlp path) are inherited from the
  most specific scope that provides one.

## Schema groups

| Group | Example settings |
|---|---|
| `transfer` | `max_concurrent`, `max_segments`, `adaptive_segments`, `min_segment_size`, `connection_stagger`, `work_stealing` |
| `bandwidth` | `global_down_bps`, `global_up_bps`, `task_down_bps`, `host_down_bps`, `bucket_burst_ms`, `off_peak_schedule` |
| `network` | `bond_interfaces`, `bond_exclude`, `probe_enabled`, `probe_targets`, `proxy_url`, `dns_mode`, `ipv6_policy`, `http2_enabled` |
| `content` | `default_download_dir`, `filename_collision_policy`, `checksum_policy`, `verify_after_download`, `sidecar_cadence` |
| `torrent` | `seed_enabled`, `seed_ratio`, `seed_time_min`, `listen_port_range`, `upnp`, `dht_persistence`, `fastresume`, `sequential_default` |
| `video` | `ytdlp_path`, `default_format_policy`, `subtitle_policy`, `media_offer_policy` |
| `browser` | `extension_dir`, `capture_policy`, `referer_policy`, `max_offer_links` |
| `ui` | `hide_to_tray`, `theme`, `density`, `graph_window`, `show_per_nic`, `language` |
| `updates` | `update_check`, `update_auto_install`, `update_channel`, `require_checksum` |
| `lan` | `lan_enabled`, `lan_port`, `lan_dir`, `lan_open`, `code_policy`, `lan_rate_bps` |
| `diagnostics` | `log_level`, `diagnostics_opt_in`, `retain_logs_days` |
| `privacy` | `telemetry` (fixed `off`), `uplink_probes` (explicit opt-in when bonding is off) |

`telemetry = off` is not a toggle in the product sense - it is a constant in
the schema so no future feature can turn it on without an ADR and a release
note.

## Setting definition

```rust
SettingDef {
    id: SettingId,                 // "bandwidth.task_down_bps"
    group: SettingGroup,
    value_type: ValueType,         // Bool | Int | Size | Rate | Duration | Path | Enum
    default: Value,
    range: Option<Range>,          // min/max/allowed values
    apply: ApplyPolicy,
    ui: UiHint,                    // Toggle | Number | Size | Rate | Path | Select | Schedule
    description: &'static str,
    introduced_in: u32,
}
```

`ApplyPolicy`:

| Policy | Meaning | Example |
|---|---|---|
| `Live` | Takes effect on the next tick without restarting anything | rate limits, concurrency, log level |
| `NextTask` | Applies to tasks started after the change | segments, proxy, naming policy |
| `Restart` | Needs a daemon restart (shown before saving) | torrent listen port, DHT persistence |
| `ReadOnly` | Derived or fixed; cannot be edited | version, telemetry |

## API

```
GetSettings  { scope: Option<Scope> }        -> SettingsSnapshot { version, schema_version, values }
GetSchema                                    -> Vec<SettingDef>
SetSettings  { scope, patch, task_id?, host? } -> SettingsChanged { changed: Vec<SettingId>, restart_required, warnings }
PreviewSettings { patch }                    -> { effective, warnings, restart_required }
ResetSettings { scope, keys }                -> SettingsChanged
```

- `SetSettings` validates the **whole** document before committing.
- The commit is atomic (file write then in-memory swap).
- After commit, the daemon emits `SettingsChanged` with explicit ids; only the
  owners listed below react:
  - `TrafficController`: bandwidth.*, transfer.max_concurrent
  - `LinkSet`: network.bond_*, network.probe_*
  - `Queue`: transfer.*, content.filename_collision_policy
  - `TorrentSession`: torrent.* (apply or mark restart)
  - `ExtensionHost`: browser.*
  - `UI`: ui.* via snapshot

## Validation rules

- Type + range checks from the schema.
- Cross-field constraints, e.g.:
  - `min_segment_size > 0`
  - `1 <= max_segments <= 64`
  - `task_down_bps <= global_down_bps` when global is finite
  - `seed_ratio` and `seed_time_min` are finite and >= 0
  - `lan_code` length/charset within the configured code policy
  - schedule expressions parse
- Invalid values return a typed error with the setting id and the offending
  value, not a generic string.
- On load, invalid values fall back to the schema default **with a warning**
  that appears in diagnostics. They are never silently accepted.

## Migration

- `config.json` carries `schema_version`.
- v1 -> v2 upcaster maps old flat fields to the new grouped ids.
- Unknown future keys are preserved verbatim and warned about; they are not
  dropped by a rewrite.
- A backup (`.bak`) is kept before the first v2 write.
- Additive fields always have schema defaults, so an older frontend can still
  call `GetSettings` and ignore what it does not know.

## UI consequences

- GUI Settings tabs are generated from `GetSchema`, grouped by `SettingGroup`,
  with the `apply` badge shown next to each row.
- Search and "advanced only" filters come from metadata.
- CLI: `ripdl config <id> <value>` validates against the same schema;
  `ripdl config --schema` lists every id, type, default, and apply policy.
- Extension options uses the same generated schema. No second hand-written
  settings form.
- A "Presets" feature (LAN only, slow link, off-peak) becomes a named bundle of
  partial overrides - trivial once scopes and schema exist.

## Tests

- `schema_complete`: every definition has a default, validator, UI hint, and
  apply policy.
- `config_round_trip`: every setting survives load/save.
- `config_v1_upcast`: fixture v1 files migrate correctly.
- `setting_precedence`: default < global < category < host < task.
- `setting_applies`: each `Live` setting has a test proving it changes behavior
  within one tick.
- `setting_restart_flag`: each `Restart` setting is reported by `Preview`.
- `no_orphan_setting`: no CLI/GUI/ext field exists without a schema id.

This is how "granular" stays manageable: the settings grow, but the rules, the
UI, and the tests are generated from one model.
