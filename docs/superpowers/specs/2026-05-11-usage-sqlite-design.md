# Usage SQLite Database

## Problem

The Claude Code statusline payload contains rich per-session telemetry — cost,
token counts, context-window fill, rate-limit pressure, model, project, effort
level — that exists only in process memory for the lifetime of a single
`claudehud render` invocation. There is no way to answer questions like "how
much did I spend this week", "which projects dominate my Opus usage", or
"which sessions came closest to the 7-day rate limit". The data is discarded
the moment the statusline finishes printing.

## Goal

Persist per-session telemetry to a local SQLite database, written exclusively
by the daemon, and surface that history through a new `claudehud usage`
subcommand family. Opt-in via a single environment variable.

## Non-Goals

- **Intra-session time series.** Storage grain is one row per `session_id`,
  upserted on each render. Plotting context fill over time within a session is
  deferred.
- **Statusline integration.** No new rendered segments fed by the database in
  v1. The statusline continues to read only the current stdin payload.
- **Retention or pruning.** The database grows unbounded; sessions accumulate
  forever. A future spec can add `claudehud usage prune --older-than`.
- **Cross-machine sync.** The database is local-only. No export or replication
  in v1 beyond the file living at a known path.
- **Daemon family of subcommands** (`claudehud daemon status` etc.). Out of
  scope.
- **CSV / non-JSON export formats.** `--json` lands in v1; CSV is deferred.

## High-Level Design

```
cc harness
  │  json on stdin
  ▼
claudehud render ──── writes ──▶  data_dir()/usage/{session_id}.json   (atomic rename)
                                                │
                                       notify Modify/Create
                                                ▼
                                  claudehud-daemon: usage ingest thread
                                                │  UPSERT
                                                ▼
                                  data_dir()/usage.db   (sqlite, WAL)
                                                ▲
                                                │  read-only
claudehud usage <sub> ─────────────────────────┘
```

The render path gains one fire-and-forget write: an atomic rename of a small
JSON file under `data_dir()/usage/{session_id}.json`. The write is gated on
`CLAUDEHUD_USAGE` being set; when unset, the render path is byte-for-byte
unchanged.

The daemon gains a fourth thread (`usage`) that watches `data_dir()/usage/`
via `notify` — the same pattern the registrar already uses for
`clhud-watch/`. On each Modify/Create event it reads the file, parses the
payload, and UPSERTs a row into `sessions`. The daemon holds a single
long-lived `rusqlite::Connection` in WAL mode for the lifetime of the
process.

The `claudehud usage` subcommand opens its own read-only connection. WAL
ensures the cli never blocks the writer and vice versa.

## Components

### `common/src/lib.rs` — new `data_dir()`

Mirrors the existing `cache_dir()` but resolves to a *durable* location.
`cache_dir()` continues to point at `/tmp` (Unix) where ephemeral mmap files
live. `data_dir()` is where SQLite, marker files for usage, and any future
durable state belong.

- env override: `CLAUDEHUD_DATA_DIR`
- macOS default: `~/Library/Application Support/claudehud/`
- Linux default: `$XDG_DATA_HOME/claudehud/` if set, else `~/.local/share/claudehud/`
- Windows default: `%LOCALAPPDATA%\claudehud\data\`

Test seam: `data_dir_in(root: &Path) -> PathBuf` for unit tests, analogous to
`mmap_path_in`.

### `claudehud-daemon/src/usage.rs` (new)

The ingest module. Owns the SQLite connection, runs migrations on startup,
watches `data_dir()/usage/` for Modify/Create events, and UPSERTs sessions.

Public surface:

- `pub fn start()` — entry point invoked from `main.rs` on its own thread.

Internals:

- `fn open_db(path: &Path) -> Result<Connection>` — opens connection, sets
  `PRAGMA journal_mode = WAL`, `PRAGMA synchronous = NORMAL`, runs migrations.
- `fn run_migrations(conn: &Connection)` — reads `schema_meta.version`,
  applies any pending migration steps from a const array
  `MIGRATIONS: &[(u32, &str)]`. v1 contains a single entry creating
  `sessions` and `schema_meta`.
- `fn ingest_file(conn: &Connection, path: &Path)` — `serde_json::from_str`,
  builds an `UsagePayload` struct mirroring `claudehud::input::Input` for the
  fields we persist, UPSERTs. Errors logged to stderr; never propagated to
  caller.
- Debounce by `session_id`: per-session `last_processed_at` in a
  `HashMap<String, Instant>`. Skip a Modify event if processed within the
  last 500 ms. Burst renders from a single session collapse into one upsert.

Drain-on-startup: after `notify` is armed, walk `data_dir()/usage/` once and
ingest every file present, mirroring the pattern in `registrar::start`.

### `claudehud-daemon/src/main.rs`

One added `std::thread::spawn` for `usage::start()`. Thread is unconditionally
spawned — when no client is dropping payloads, the watcher idles on
`notify` with zero CPU cost, and the SQLite connection sits open but unused.
This avoids forcing users to edit launchd plists / systemd unit
`Environment=` blocks to enable the feature.

### `claudehud/src/main.rs` — drop-on-render

After `render::render` returns and before `print!`, the client checks
`std::env::var_os("CLAUDEHUD_USAGE")`. When set to any non-empty value:

```rust
if let Some(session_id) = input.session_id.as_deref() {
    if !session_id.is_empty() {
        let _ = drop_usage_payload(&raw, session_id);
    }
}
```

`drop_usage_payload` writes to `data_dir()/usage/{session_id}.json.tmp` then
atomically renames to `{session_id}.json`. All errors are silently
swallowed — the render path's exit code is determined solely by the
statusline render itself.

The raw stdin string is written verbatim. This avoids re-serialization cost
in the hot path and lets the daemon evolve its parsing without coordinated
client changes.

### `claudehud/src/usage.rs` (new) — read-side CLI

Wired into the existing `pico_args` subcommand match in `main.rs`. Opens
SQLite with `SQLITE_OPEN_READONLY` so accidental writes are impossible from
the cli.

Subcommands (all read-only):

```
claudehud usage today                                       summary for today
claudehud usage week                                        summary for last 7d
claudehud usage sessions [--limit N] [--project PATH]
                         [--since YYYY-MM-DD] [--json]      list recent sessions
claudehud usage projects [--since YYYY-MM-DD] [--limit N]   top projects by spend
claudehud usage db                                          print path to usage.db
```

Default output is a compact human-readable table; `--json` switches to
newline-delimited JSON for piping.

`today` / `week` output sketch:

```
2026-05-11   12 sessions   $4.83 spent   1h 47m total api time
top projects:  claudehud ($2.10, 6 sessions) · dotfiles ($1.40, 3) · scratch ($1.33, 3)
top model:     Opus 4.7 (1M context)   91% of api time
```

`sessions` output sketch:

```
session                                  last seen          cost     ctx   model
00000000-0000-…-000000000000   claudehud  2026-05-11 14:02   $0.75    22%   Opus 4.7
deadbeef-c0ff-…-1234           dotfiles   2026-05-11 13:11   $0.31     8%   Sonnet 4.6
```

DB-missing fallback: if `data_dir()/usage.db` doesn't exist, print one line
to stderr (`claudehud: no usage database at <path> — set CLAUDEHUD_USAGE=1 and run a Claude Code session to populate it`)
and exit 0. Empty pipelines stay clean.

The `usage` subcommand is *not* gated on `CLAUDEHUD_USAGE`. If the database
exists from a prior opt-in, reads are always permitted — turning off drops
should not lock the user out of inspecting historical data.

## Schema

Single durable table, one row per session, UPSERT semantics. Schema version
recorded in a `schema_meta` table for forward-compatible migrations.

```sql
CREATE TABLE IF NOT EXISTS sessions (
  session_id              TEXT PRIMARY KEY,
  first_seen_at           INTEGER NOT NULL,   -- unix secs, INSERT only
  last_seen_at            INTEGER NOT NULL,   -- unix secs, refreshed each UPSERT
  cc_version              TEXT,
  session_name            TEXT,
  cwd                     TEXT,
  project_dir             TEXT,
  model_id                TEXT,
  model_display           TEXT,
  output_style            TEXT,
  effort_level            TEXT,
  thinking_enabled        INTEGER,
  fast_mode               INTEGER,
  exceeds_200k_tokens     INTEGER,
  billing_type            TEXT,               -- 'plan' | 'api'

  total_cost_usd          REAL,
  total_duration_ms       INTEGER,
  total_api_duration_ms   INTEGER,
  total_lines_added       INTEGER,
  total_lines_removed     INTEGER,

  context_window_size       INTEGER,
  total_input_tokens        INTEGER,
  total_output_tokens       INTEGER,
  cache_creation_tokens     INTEGER,
  cache_read_tokens         INTEGER,
  context_used_pct          REAL,

  five_hour_used_pct        REAL,
  five_hour_resets_at       INTEGER,
  seven_day_used_pct        REAL,
  seven_day_resets_at       INTEGER
);

CREATE INDEX IF NOT EXISTS idx_sessions_last_seen ON sessions(last_seen_at);
CREATE INDEX IF NOT EXISTS idx_sessions_project   ON sessions(project_dir);

CREATE TABLE IF NOT EXISTS schema_meta (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
INSERT OR IGNORE INTO schema_meta(key, value) VALUES ('version', '1');
```

`billing_type` is derived at ingest time: `'plan'` when `rate_limits` is
present in the payload, `'api'` otherwise. This matches the existing
heuristic used by `claudehud/src/render.rs` for choosing between rate-limit
bars and the 💰 cost segment.

UPSERT shape:

```sql
INSERT INTO sessions (session_id, first_seen_at, last_seen_at, /* …all cols… */)
VALUES (?, ?, ?, /* …binds… */)
ON CONFLICT(session_id) DO UPDATE SET
  last_seen_at = excluded.last_seen_at,
  /* every column except session_id and first_seen_at gets excluded.<col> */
;
```

All fields except `session_id`, `first_seen_at`, `last_seen_at` allow NULL —
upstream payload fields are individually optional (already modeled as
`Option<T>` in `claudehud::input`).

## Configuration

Single env var: **`CLAUDEHUD_USAGE`**. Any non-empty value enables. Unset or
empty disables.

| surface                          | gated on `CLAUDEHUD_USAGE`? |
|----------------------------------|-----------------------------|
| `claudehud render` drop          | yes                         |
| daemon usage ingest thread       | no (always spawned, idle)   |
| `claudehud usage` cli            | no (gated on db existence)  |

This design lets users flip the feature on/off from `~/.zshrc` alone — no
launchd plist or systemd unit edits required. Claude Code launches the
client from the user's shell, so the env propagates naturally. The daemon
runs under launchd/systemd/Task Scheduler and would otherwise need its
service definition edited to see new env vars; making the daemon's thread
unconditional sidesteps that entirely.

Documented in:

- `claudehud --help` (under `ENVIRONMENT`)
- `README.md` under `## Configuration`

## IPC Details

**File naming:** `{session_id}.json`. Session IDs from Claude Code are UUIDs
— filesystem-safe across macOS/Linux/Windows, no escaping needed. Total file
count is bounded by lifetime distinct sessions, expected to stay in the low
hundreds.

**Atomicity:** client writes to `{session_id}.json.tmp`, then `rename`s to
`{session_id}.json`. On all supported filesystems within the same directory
the rename is atomic, so the daemon never observes a partial JSON.

**Rate limiting (client side):** none in v1. Small (~1 KB) atomic renames
are cheap (~100 µs). If profiling later shows render-path impact, add a
sidecar `{session_id}.json.ts` mtime check.

**Debouncing (daemon side):** per-session `HashMap<String, Instant>` of last
processed time. Skip events <500 ms after last upsert for the same session.
Cumulative payload fields make missed events safe — the next event carries
the latest totals.

**Drain-on-startup:** after `notify::Watcher::watch` returns, walk
`data_dir()/usage/` and process every file. Handles the daemon-was-down
window when client drops accumulated.

**Daemon-down behavior:** files accumulate in `data_dir()/usage/` until the
daemon starts. No data loss; ingest happens on next daemon startup via the
drain.

## Error Handling

- **Client drop failures** (dir create, write, rename, env var lookup): all
  swallowed. Render exit code unaffected.
- **Daemon SQLite open failure**: log to stderr, the usage thread exits
  early. Other daemon threads (registrar, watcher, status) are unaffected.
  No retry — the user will see the WARN on next daemon launch and can
  investigate (likely a permissions / disk issue).
- **Daemon ingest failure** (parse error, SQLite UPSERT failure): log to
  stderr, continue to next event. Bad files stay in the directory — they
  will be retried on the next Modify event for that session.
- **CLI SQLite open failure**: print one-line error to stderr, exit non-zero.
- **CLI db-missing**: print hint, exit 0 (clean pipelines).
- **Schema version newer than this binary supports**: log loud WARN, refuse
  to write/upsert (daemon) or refuse to read (cli) and exit non-zero. Forces
  the user to upgrade rather than silently corrupting.

## Dependencies

New crates:

| crate              | added to                | purpose                              | feature flags          |
|--------------------|-------------------------|--------------------------------------|------------------------|
| `rusqlite`         | daemon, claudehud (cli) | SQLite bindings                      | `bundled` (vendor lib) |
| `serde_json`       | daemon                  | parse dropped JSON payloads          | default                |
| `serde`            | daemon                  | derive `Deserialize` on payload type | `derive`               |

Binary size impact (estimated):

- daemon: +~800 KB (rusqlite bundled SQLite + serde_json + serde)
- client: +~600 KB (rusqlite bundled SQLite, reuses existing serde_json)

This roughly doubles current combined release size (878 KB → ~2.3 MB). Worth
the cost given the new functionality and the no-system-libsqlite3 install
simplicity from `bundled`.

## Testing

Unit tests:

- `common::data_dir_in` respects env override; matches per-OS defaults.
- `usage::open_db` creates the schema on a fresh path; migrations idempotent.
- `usage::ingest_file` correctly UPSERTs from a representative JSON
  (reuse `REAL_STDIN_FIXTURE` and `API_BILLING_FIXTURE` already in
  `claudehud/src/input.rs`).
- `billing_type` resolves to `'plan'` when `rate_limits` present, `'api'`
  otherwise.
- Debounce: rapid successive `ingest_file` calls for the same session
  collapse to one UPSERT (test the debounce logic in isolation; full
  notify+thread integration is out of scope).
- CLI: `claudehud usage db` prints the path. `claudehud usage today` against
  a seeded test db returns expected aggregate.

Integration test (single test, daemon-side):

- Spin up a tempdir as `CLAUDEHUD_DATA_DIR`, call `usage::start()` on a
  thread, write a payload file into `usage/`, wait briefly, assert the
  `sessions` row exists with expected fields.

No-op test:

- With `CLAUDEHUD_USAGE` unset, `claudehud render` does not create
  `data_dir()/usage/`. Verifies the gate.

## Migration & Rollout

- v1 is the only schema entry in `MIGRATIONS`. Future versions append to
  the array; the daemon runs each pending step in order.
- No backfill of pre-existing sessions. The feature starts collecting from
  first opt-in render forward.
- Reverting: users can delete `data_dir()/usage.db` and `data_dir()/usage/`
  to clear all state. Unsetting `CLAUDEHUD_USAGE` halts new drops.

## Open Questions

None at design time.

## Future Work (deferred, not in this spec)

- Statusline segment fed by the database (e.g., "7d burn $12.40 ↑").
- Intra-session snapshot table for rate-limit-over-time plots.
- Retention / pruning subcommand.
- `--csv` and other export formats.
- Optional daemon subcommand family (`claudehud daemon status`).
- Auto-injection of `CLAUDEHUD_USAGE` into launchd/systemd service
  definitions by `claudehud install` (would also enable the daemon-side
  thread to gate on env if desired).
