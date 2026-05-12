# Usage SQLite Database Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Persist per-session Claude Code telemetry to a daemon-owned SQLite database and expose it via a new `claudehud usage` read-only CLI, gated behind the `CLAUDEHUD_USAGE` env var.

**Architecture:** Client `render` atomically drops the raw stdin JSON to `data_dir()/usage/{session_id}.json`. A new daemon thread (`usage::start`) watches that directory via `notify`, UPSERTs each payload into a `sessions` table in `data_dir()/usage.db` (WAL mode, single long-lived connection). The `claudehud usage` subcommand family opens a read-only connection for summaries.

**Tech Stack:** Rust 2021, `rusqlite` (bundled SQLite), `notify` (already present in daemon), `serde` / `serde_json`, `pico-args` (already present in client), `time` (already present in client).

**Reference spec:** `docs/superpowers/specs/2026-05-11-usage-sqlite-design.md`

---

## File Structure

**New files:**
- `claudehud-daemon/src/usage.rs` — ingest module (open DB, migrations, watcher, UPSERT)
- `claudehud/src/usage.rs` — client-side: payload drop + read-only CLI

**Modified files:**
- `Cargo.toml` (workspace) — add `rusqlite`, `serde`, `serde_json` to `[workspace.dependencies]`
- `claudehud-daemon/Cargo.toml` — add deps
- `claudehud/Cargo.toml` — add `rusqlite`
- `common/src/lib.rs` — add `data_dir()` + `data_dir_in()`
- `claudehud-daemon/src/main.rs` — spawn `usage::start()` thread
- `claudehud/src/lib.rs` — re-export `usage` module
- `claudehud/src/main.rs` — wire `usage` subcommand, drop payload after render, document env var in `--help`
- `README.md` — document `CLAUDEHUD_USAGE` under Configuration

**Test surfaces:** every new file gets `#[cfg(test)] mod tests` inline (matches existing project pattern — see `claudehud-daemon/src/status.rs` and `common/src/lib.rs`).

---

## Task 1: Add `data_dir()` to `common`

**Files:**
- Modify: `common/src/lib.rs`

The daemon writes to a durable location (not `/tmp`). `data_dir()` mirrors the existing `cache_dir()` shape — same env-override + same per-OS defaults, but pointing at a persistent path.

- [ ] **Step 1: Write the failing tests**

Append to the `#[cfg(test)] mod tests` block at the bottom of `common/src/lib.rs`:

```rust
    #[test]
    fn test_data_dir_respects_env_override() {
        let key = "CLAUDEHUD_DATA_DIR";
        let prev = std::env::var_os(key);
        std::env::set_var(key, "/tmp/claudehud-data-override");
        let got = data_dir();
        if let Some(p) = prev {
            std::env::set_var(key, p);
        } else {
            std::env::remove_var(key);
        }
        assert_eq!(got, Path::new("/tmp/claudehud-data-override"));
    }

    #[test]
    fn test_data_dir_in_format() {
        let p = data_dir_in(Path::new("/srv"));
        assert_eq!(p, Path::new("/srv"));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p common test_data_dir`
Expected: FAIL with `cannot find function 'data_dir' in this scope` and `cannot find function 'data_dir_in' in this scope`.

- [ ] **Step 3: Implement `data_dir` + `data_dir_in`**

Add to `common/src/lib.rs` immediately after the existing `cache_dir()` function (around line 27):

```rust
/// Durable user-data directory. Honored env override: `CLAUDEHUD_DATA_DIR`.
/// macOS default: `~/Library/Application Support/claudehud`.
/// Linux default: `$XDG_DATA_HOME/claudehud` (fallback `~/.local/share/claudehud`).
/// Windows default: `%LOCALAPPDATA%\claudehud\data`.
pub fn data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("CLAUDEHUD_DATA_DIR") {
        return PathBuf::from(dir);
    }
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/"));
        home.join("Library")
            .join("Application Support")
            .join("claudehud")
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
            return PathBuf::from(xdg).join("claudehud");
        }
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/"));
        home.join(".local").join("share").join("claudehud")
    }
    #[cfg(windows)]
    {
        let local = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Users\Default\AppData\Local"));
        local.join("claudehud").join("data")
    }
}

/// Test seam: identity function so callers can inject a tempdir root and the
/// rest of the path-building helpers in this module compose with it.
pub fn data_dir_in(root: &Path) -> PathBuf {
    root.to_path_buf()
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p common test_data_dir`
Expected: PASS, 2 tests.

- [ ] **Step 5: Commit**

```bash
git add common/src/lib.rs
git commit -m "feat(common): add data_dir() for durable user data"
```

---

## Task 2: Add SQLite + serde deps to workspace

**Files:**
- Modify: `Cargo.toml` (workspace root)
- Modify: `claudehud-daemon/Cargo.toml`
- Modify: `claudehud/Cargo.toml`

No test; this is infrastructure that the next task's test will exercise.

- [ ] **Step 1: Add to `[workspace.dependencies]` in `Cargo.toml`**

Replace the entire `[workspace.dependencies]` block in `Cargo.toml` with:

```toml
[workspace.dependencies]
common = { path = "common" }
memmap2 = "0.9"
rusqlite = { version = "0.31", features = ["bundled"] }
serde = { version = "1", features = ["derive"] }
serde_json = { version = "1", features = ["preserve_order"] }
```

- [ ] **Step 2: Add rusqlite + serde_json + serde to daemon**

Replace `claudehud-daemon/Cargo.toml`'s `[dependencies]` block with:

```toml
[dependencies]
common = { workspace = true }
memmap2 = { workspace = true }
rusqlite = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
notify = "6"
crossbeam-channel = "0.5"
ureq = { version = "2", default-features = false, features = ["tls"] }
roxmltree = "0.20"

[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 3: Switch client deps to workspace + add rusqlite**

Replace `claudehud/Cargo.toml`'s `[dependencies]` block with:

```toml
[dependencies]
common = { workspace = true }
memmap2 = { workspace = true }
rusqlite = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
pico-args = "0.5"
time = { version = "0.3", features = ["local-offset"] }
```

- [ ] **Step 4: Verify the workspace builds**

Run: `cargo build --workspace`
Expected: builds successfully. The first build pulls and compiles `rusqlite` with its bundled SQLite C source — takes 1-2 minutes on a fresh checkout. Subsequent builds are fast.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml claudehud-daemon/Cargo.toml claudehud/Cargo.toml Cargo.lock
git commit -m "build: add rusqlite (bundled) + serde to workspace for usage db"
```

---

## Task 3: Daemon — define `UsagePayload` struct + `parse_payload`

**Files:**
- Create: `claudehud-daemon/src/usage.rs`
- Modify: `claudehud-daemon/src/main.rs` (register the module)

This task introduces the file and the deserialization shape only. The daemon doesn't import `claudehud::input` — keeps `common` lean and the daemon free to evolve its parsing independently.

- [ ] **Step 1: Register the module in daemon main.rs**

In `claudehud-daemon/src/main.rs`, after the existing `mod watcher;` line (around line 11), add:

```rust
mod usage;
```

- [ ] **Step 2: Write the failing test**

Create `claudehud-daemon/src/usage.rs` with this initial content (test only, no impl yet):

```rust
// claudehud-daemon/src/usage.rs

use serde::Deserialize;

#[derive(Deserialize, Default, Debug)]
pub struct UsagePayload {
    pub session_id: Option<String>,
    pub session_name: Option<String>,
    pub version: Option<String>,
    pub cwd: Option<String>,
    pub model: Option<Model>,
    pub workspace: Option<Workspace>,
    pub output_style: Option<OutputStyle>,
    pub effort: Option<Effort>,
    pub thinking: Option<Thinking>,
    pub fast_mode: Option<bool>,
    pub exceeds_200k_tokens: Option<bool>,
    pub cost: Option<Cost>,
    pub context_window: Option<ContextWindow>,
    pub rate_limits: Option<RateLimits>,
}

#[derive(Deserialize, Default, Debug)]
pub struct Model {
    pub id: Option<String>,
    pub display_name: Option<String>,
}

#[derive(Deserialize, Default, Debug)]
pub struct Workspace {
    pub project_dir: Option<String>,
}

#[derive(Deserialize, Default, Debug)]
pub struct OutputStyle {
    pub name: Option<String>,
}

#[derive(Deserialize, Default, Debug)]
pub struct Effort {
    pub level: Option<String>,
}

#[derive(Deserialize, Default, Debug)]
pub struct Thinking {
    pub enabled: Option<bool>,
}

#[derive(Deserialize, Default, Debug)]
pub struct Cost {
    pub total_cost_usd: Option<f64>,
    pub total_duration_ms: Option<u64>,
    pub total_api_duration_ms: Option<u64>,
    pub total_lines_added: Option<u64>,
    pub total_lines_removed: Option<u64>,
}

#[derive(Deserialize, Default, Debug)]
pub struct ContextWindow {
    pub context_window_size: Option<u64>,
    pub total_input_tokens: Option<u64>,
    pub total_output_tokens: Option<u64>,
    pub used_percentage: Option<f64>,
    pub current_usage: Option<TokenUsage>,
}

#[derive(Deserialize, Default, Debug)]
pub struct TokenUsage {
    pub cache_creation_input_tokens: Option<u64>,
    pub cache_read_input_tokens: Option<u64>,
}

#[derive(Deserialize, Default, Debug)]
pub struct RateLimits {
    pub five_hour: Option<RateWindow>,
    pub seven_day: Option<RateWindow>,
}

#[derive(Deserialize, Default, Debug)]
pub struct RateWindow {
    pub used_percentage: Option<f64>,
    pub resets_at: Option<u64>,
}

/// `'plan'` when the payload includes a `rate_limits` block, `'api'` otherwise.
pub fn billing_type(p: &UsagePayload) -> &'static str {
    if p.rate_limits.is_some() {
        "plan"
    } else {
        "api"
    }
}

pub fn parse_payload(raw: &str) -> Result<UsagePayload, serde_json::Error> {
    serde_json::from_str(raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAN_FIXTURE: &str = r#"{
        "session_id": "00000000-0000-0000-0000-000000000000",
        "version": "2.1.139",
        "cwd": "/home/user/project",
        "model": {"id": "claude-opus-4-7", "display_name": "Opus 4.7"},
        "workspace": {"project_dir": "/home/user/project"},
        "output_style": {"name": "Gen-Z"},
        "cost": {"total_cost_usd": 0.75, "total_api_duration_ms": 83074},
        "context_window": {
            "context_window_size": 200000,
            "used_percentage": 22,
            "current_usage": {"cache_creation_input_tokens": 330, "cache_read_input_tokens": 43617}
        },
        "rate_limits": {
            "five_hour": {"used_percentage": 10, "resets_at": 1776567600},
            "seven_day": {"used_percentage": 22, "resets_at": 1776974400}
        }
    }"#;

    const API_FIXTURE: &str = r#"{
        "session_id": "11111111-1111-1111-1111-111111111111",
        "version": "2.1.139",
        "model": {"id": "claude-opus-4-7[1m]", "display_name": "Opus 4.7 (1M context)"},
        "cost": {"total_cost_usd": 0.10}
    }"#;

    #[test]
    fn test_parse_plan_fixture() {
        let p = parse_payload(PLAN_FIXTURE).unwrap();
        assert_eq!(p.session_id.as_deref(), Some("00000000-0000-0000-0000-000000000000"));
        assert_eq!(p.model.as_ref().and_then(|m| m.id.as_deref()), Some("claude-opus-4-7"));
        assert!(p.rate_limits.is_some());
        assert_eq!(billing_type(&p), "plan");
    }

    #[test]
    fn test_parse_api_fixture() {
        let p = parse_payload(API_FIXTURE).unwrap();
        assert!(p.rate_limits.is_none());
        assert_eq!(billing_type(&p), "api");
    }

    #[test]
    fn test_parse_empty_object() {
        let p = parse_payload("{}").unwrap();
        assert!(p.session_id.is_none());
        assert_eq!(billing_type(&p), "api");
    }

    #[test]
    fn test_parse_invalid_json() {
        assert!(parse_payload("not json").is_err());
    }
}
```

- [ ] **Step 3: Run tests to verify they pass**

Run: `cargo test -p claudehud-daemon usage::tests`
Expected: PASS, 4 tests.

(No "fails first" step needed here — the impl and tests land together because this task is purely declarative type/parse glue and the next tasks all depend on the types existing.)

- [ ] **Step 4: Commit**

```bash
git add claudehud-daemon/src/usage.rs claudehud-daemon/src/main.rs
git commit -m "feat(daemon): UsagePayload + parse_payload for usage ingest"
```

---

## Task 4: Daemon — `open_db` + migrations

**Files:**
- Modify: `claudehud-daemon/src/usage.rs`

Opens a SQLite connection in WAL mode and applies all pending schema migrations. v1 holds the initial `sessions` and `schema_meta` tables.

- [ ] **Step 1: Add failing tests**

Append to the `#[cfg(test)] mod tests` block in `claudehud-daemon/src/usage.rs`:

```rust
    #[test]
    fn test_open_db_creates_schema() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("usage.db");
        let conn = open_db(&path).unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='sessions'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
        let version: String = conn
            .query_row("SELECT value FROM schema_meta WHERE key='version'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(version, "1");
    }

    #[test]
    fn test_open_db_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("usage.db");
        let _conn1 = open_db(&path).unwrap();
        drop(_conn1);
        let conn2 = open_db(&path).unwrap();
        let version: String = conn2
            .query_row("SELECT value FROM schema_meta WHERE key='version'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(version, "1");
    }

    #[test]
    fn test_open_db_rejects_future_schema() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("usage.db");
        let conn = open_db(&path).unwrap();
        conn.execute(
            "UPDATE schema_meta SET value='99' WHERE key='version'",
            [],
        )
        .unwrap();
        drop(conn);
        let result = open_db(&path);
        assert!(result.is_err(), "expected error opening future-schema db");
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p claudehud-daemon usage::tests::test_open_db`
Expected: FAIL with `cannot find function 'open_db' in this scope`.

- [ ] **Step 3: Implement `open_db` + migrations**

Append to `claudehud-daemon/src/usage.rs` (above the `#[cfg(test)] mod tests` block):

```rust
use rusqlite::{params, Connection};
use std::path::Path;

const SUPPORTED_SCHEMA_VERSION: u32 = 1;

/// Ordered list of migrations. Each entry is (target_version, sql_to_apply).
/// Apply in order; each step bumps `schema_meta.version` atomically.
const MIGRATIONS: &[(u32, &str)] = &[(
    1,
    r#"
    CREATE TABLE IF NOT EXISTS sessions (
      session_id              TEXT PRIMARY KEY,
      first_seen_at           INTEGER NOT NULL,
      last_seen_at            INTEGER NOT NULL,
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
      billing_type            TEXT,
      total_cost_usd          REAL,
      total_duration_ms       INTEGER,
      total_api_duration_ms   INTEGER,
      total_lines_added       INTEGER,
      total_lines_removed     INTEGER,
      context_window_size     INTEGER,
      total_input_tokens      INTEGER,
      total_output_tokens     INTEGER,
      cache_creation_tokens   INTEGER,
      cache_read_tokens       INTEGER,
      context_used_pct        REAL,
      five_hour_used_pct      REAL,
      five_hour_resets_at     INTEGER,
      seven_day_used_pct      REAL,
      seven_day_resets_at     INTEGER
    );
    CREATE INDEX IF NOT EXISTS idx_sessions_last_seen ON sessions(last_seen_at);
    CREATE INDEX IF NOT EXISTS idx_sessions_project   ON sessions(project_dir);
    CREATE TABLE IF NOT EXISTS schema_meta (
      key   TEXT PRIMARY KEY,
      value TEXT NOT NULL
    );
    "#,
)];

/// Opens (or creates) the usage database at `path`, configures WAL, and
/// runs any pending migrations. Refuses to open a database whose schema
/// version is newer than this binary supports.
pub fn open_db(path: &Path) -> Result<Connection, rusqlite::Error> {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let conn = Connection::open(path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    run_migrations(&conn)?;
    Ok(conn)
}

fn run_migrations(conn: &Connection) -> Result<(), rusqlite::Error> {
    // schema_meta must exist before we can read the version. Apply the first
    // migration unconditionally on a fresh database; it's idempotent (IF NOT
    // EXISTS everywhere).
    conn.execute_batch(MIGRATIONS[0].1)?;
    conn.execute(
        "INSERT OR IGNORE INTO schema_meta(key, value) VALUES ('version', '1')",
        [],
    )?;
    let current: String = conn.query_row(
        "SELECT value FROM schema_meta WHERE key='version'",
        [],
        |r| r.get(0),
    )?;
    let current: u32 = current.parse().unwrap_or(0);

    if current > SUPPORTED_SCHEMA_VERSION {
        return Err(rusqlite::Error::InvalidQuery);
    }

    for &(target, sql) in MIGRATIONS.iter().skip(1) {
        if target > current {
            conn.execute_batch(sql)?;
            conn.execute(
                "UPDATE schema_meta SET value=?1 WHERE key='version'",
                params![target.to_string()],
            )?;
        }
    }
    Ok(())
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p claudehud-daemon usage::tests::test_open_db`
Expected: PASS, 3 tests.

- [ ] **Step 5: Commit**

```bash
git add claudehud-daemon/src/usage.rs
git commit -m "feat(daemon): usage::open_db with WAL + v1 schema migrations"
```

---

## Task 5: Daemon — `upsert_session`

**Files:**
- Modify: `claudehud-daemon/src/usage.rs`

Inserts or updates a row in `sessions` from a `UsagePayload`. `first_seen_at` is preserved on conflict; every other column is overwritten with the latest payload's values.

- [ ] **Step 1: Add failing tests**

Append to the `#[cfg(test)] mod tests` block in `claudehud-daemon/src/usage.rs`:

```rust
    #[test]
    fn test_upsert_inserts_new_session() {
        let tmp = tempfile::tempdir().unwrap();
        let conn = open_db(&tmp.path().join("usage.db")).unwrap();
        let payload = parse_payload(PLAN_FIXTURE).unwrap();
        upsert_session(&conn, &payload, 1_700_000_000).unwrap();
        let (first, last, cost, billing): (i64, i64, f64, String) = conn
            .query_row(
                "SELECT first_seen_at, last_seen_at, total_cost_usd, billing_type FROM sessions WHERE session_id=?1",
                params!["00000000-0000-0000-0000-000000000000"],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(first, 1_700_000_000);
        assert_eq!(last, 1_700_000_000);
        assert!((cost - 0.75).abs() < 1e-9);
        assert_eq!(billing, "plan");
    }

    #[test]
    fn test_upsert_preserves_first_seen_on_update() {
        let tmp = tempfile::tempdir().unwrap();
        let conn = open_db(&tmp.path().join("usage.db")).unwrap();
        let payload = parse_payload(PLAN_FIXTURE).unwrap();
        upsert_session(&conn, &payload, 1_700_000_000).unwrap();
        upsert_session(&conn, &payload, 1_700_000_999).unwrap();
        let (first, last): (i64, i64) = conn
            .query_row(
                "SELECT first_seen_at, last_seen_at FROM sessions WHERE session_id=?1",
                params!["00000000-0000-0000-0000-000000000000"],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(first, 1_700_000_000, "first_seen_at must not move");
        assert_eq!(last, 1_700_000_999);
    }

    #[test]
    fn test_upsert_skips_payload_without_session_id() {
        let tmp = tempfile::tempdir().unwrap();
        let conn = open_db(&tmp.path().join("usage.db")).unwrap();
        let payload = parse_payload(r#"{"cwd": "/x"}"#).unwrap();
        let result = upsert_session(&conn, &payload, 1_700_000_000);
        assert!(matches!(result, Ok(UpsertOutcome::SkippedNoSessionId)));
        let count: i64 = conn
            .query_row("SELECT count(*) FROM sessions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p claudehud-daemon usage::tests::test_upsert`
Expected: FAIL with `cannot find function 'upsert_session' in this scope`.

- [ ] **Step 3: Implement `upsert_session`**

Append to `claudehud-daemon/src/usage.rs` (above the `#[cfg(test)] mod tests` block):

```rust
#[derive(Debug, PartialEq, Eq)]
pub enum UpsertOutcome {
    Inserted,
    Updated,
    SkippedNoSessionId,
}

/// UPSERT a session row from `payload`. `now_secs` is the unix timestamp to
/// record as `last_seen_at` (and `first_seen_at` on insert). Returns
/// `SkippedNoSessionId` when the payload has no session id.
pub fn upsert_session(
    conn: &Connection,
    payload: &UsagePayload,
    now_secs: i64,
) -> Result<UpsertOutcome, rusqlite::Error> {
    let Some(sid) = payload.session_id.as_deref().filter(|s| !s.is_empty()) else {
        return Ok(UpsertOutcome::SkippedNoSessionId);
    };

    let model = payload.model.as_ref();
    let workspace = payload.workspace.as_ref();
    let cost = payload.cost.as_ref();
    let cw = payload.context_window.as_ref();
    let cu = cw.and_then(|c| c.current_usage.as_ref());
    let rl = payload.rate_limits.as_ref();
    let five = rl.and_then(|r| r.five_hour.as_ref());
    let seven = rl.and_then(|r| r.seven_day.as_ref());

    let changes_before = conn.changes();
    let sql = "
        INSERT INTO sessions (
            session_id, first_seen_at, last_seen_at,
            cc_version, session_name, cwd, project_dir,
            model_id, model_display, output_style, effort_level,
            thinking_enabled, fast_mode, exceeds_200k_tokens, billing_type,
            total_cost_usd, total_duration_ms, total_api_duration_ms,
            total_lines_added, total_lines_removed,
            context_window_size, total_input_tokens, total_output_tokens,
            cache_creation_tokens, cache_read_tokens, context_used_pct,
            five_hour_used_pct, five_hour_resets_at,
            seven_day_used_pct, seven_day_resets_at
        ) VALUES (
            ?1, ?2, ?2,
            ?3, ?4, ?5, ?6,
            ?7, ?8, ?9, ?10,
            ?11, ?12, ?13, ?14,
            ?15, ?16, ?17,
            ?18, ?19,
            ?20, ?21, ?22,
            ?23, ?24, ?25,
            ?26, ?27,
            ?28, ?29
        )
        ON CONFLICT(session_id) DO UPDATE SET
            last_seen_at = excluded.last_seen_at,
            cc_version = excluded.cc_version,
            session_name = excluded.session_name,
            cwd = excluded.cwd,
            project_dir = excluded.project_dir,
            model_id = excluded.model_id,
            model_display = excluded.model_display,
            output_style = excluded.output_style,
            effort_level = excluded.effort_level,
            thinking_enabled = excluded.thinking_enabled,
            fast_mode = excluded.fast_mode,
            exceeds_200k_tokens = excluded.exceeds_200k_tokens,
            billing_type = excluded.billing_type,
            total_cost_usd = excluded.total_cost_usd,
            total_duration_ms = excluded.total_duration_ms,
            total_api_duration_ms = excluded.total_api_duration_ms,
            total_lines_added = excluded.total_lines_added,
            total_lines_removed = excluded.total_lines_removed,
            context_window_size = excluded.context_window_size,
            total_input_tokens = excluded.total_input_tokens,
            total_output_tokens = excluded.total_output_tokens,
            cache_creation_tokens = excluded.cache_creation_tokens,
            cache_read_tokens = excluded.cache_read_tokens,
            context_used_pct = excluded.context_used_pct,
            five_hour_used_pct = excluded.five_hour_used_pct,
            five_hour_resets_at = excluded.five_hour_resets_at,
            seven_day_used_pct = excluded.seven_day_used_pct,
            seven_day_resets_at = excluded.seven_day_resets_at
    ";

    conn.execute(
        sql,
        params![
            sid,
            now_secs,
            payload.version,
            payload.session_name,
            payload.cwd,
            workspace.and_then(|w| w.project_dir.as_deref()),
            model.and_then(|m| m.id.as_deref()),
            model.and_then(|m| m.display_name.as_deref()),
            payload
                .output_style
                .as_ref()
                .and_then(|s| s.name.as_deref()),
            payload.effort.as_ref().and_then(|e| e.level.as_deref()),
            payload.thinking.as_ref().and_then(|t| t.enabled).map(|b| b as i64),
            payload.fast_mode.map(|b| b as i64),
            payload.exceeds_200k_tokens.map(|b| b as i64),
            billing_type(payload),
            cost.and_then(|c| c.total_cost_usd),
            cost.and_then(|c| c.total_duration_ms).map(|n| n as i64),
            cost.and_then(|c| c.total_api_duration_ms).map(|n| n as i64),
            cost.and_then(|c| c.total_lines_added).map(|n| n as i64),
            cost.and_then(|c| c.total_lines_removed).map(|n| n as i64),
            cw.and_then(|c| c.context_window_size).map(|n| n as i64),
            cw.and_then(|c| c.total_input_tokens).map(|n| n as i64),
            cw.and_then(|c| c.total_output_tokens).map(|n| n as i64),
            cu.and_then(|u| u.cache_creation_input_tokens).map(|n| n as i64),
            cu.and_then(|u| u.cache_read_input_tokens).map(|n| n as i64),
            cw.and_then(|c| c.used_percentage),
            five.and_then(|w| w.used_percentage),
            five.and_then(|w| w.resets_at).map(|n| n as i64),
            seven.and_then(|w| w.used_percentage),
            seven.and_then(|w| w.resets_at).map(|n| n as i64),
        ],
    )?;

    // SQLite reports `changes()` as 1 for both INSERT and ON-CONFLICT UPDATE.
    // Distinguish by comparing first_seen_at to now_secs after the write.
    let first: i64 = conn.query_row(
        "SELECT first_seen_at FROM sessions WHERE session_id=?1",
        params![sid],
        |r| r.get(0),
    )?;
    let _ = changes_before;
    Ok(if first == now_secs {
        UpsertOutcome::Inserted
    } else {
        UpsertOutcome::Updated
    })
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p claudehud-daemon usage::tests::test_upsert`
Expected: PASS, 3 tests.

- [ ] **Step 5: Commit**

```bash
git add claudehud-daemon/src/usage.rs
git commit -m "feat(daemon): UPSERT a session row from a UsagePayload"
```

---

## Task 6: Daemon — `ingest_file` with debounce

**Files:**
- Modify: `claudehud-daemon/src/usage.rs`

Reads a payload file from disk, parses, and UPSERTs — with a per-session 500ms debounce so burst writes from one CC session don't thrash SQLite.

- [ ] **Step 1: Add failing tests**

Append to the `#[cfg(test)] mod tests` block in `claudehud-daemon/src/usage.rs`:

```rust
    #[test]
    fn test_ingest_file_reads_and_upserts() {
        let tmp = tempfile::tempdir().unwrap();
        let conn = open_db(&tmp.path().join("usage.db")).unwrap();
        let drops = tmp.path().join("usage");
        std::fs::create_dir_all(&drops).unwrap();
        let file = drops.join("00000000-0000-0000-0000-000000000000.json");
        std::fs::write(&file, PLAN_FIXTURE).unwrap();

        let mut state = IngestState::new();
        ingest_file(&conn, &file, &mut state, 1_700_000_000);

        let count: i64 = conn
            .query_row(
                "SELECT count(*) FROM sessions WHERE session_id=?1",
                params!["00000000-0000-0000-0000-000000000000"],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_ingest_file_debounces_same_session() {
        let tmp = tempfile::tempdir().unwrap();
        let conn = open_db(&tmp.path().join("usage.db")).unwrap();
        let drops = tmp.path().join("usage");
        std::fs::create_dir_all(&drops).unwrap();
        let file = drops.join("00000000-0000-0000-0000-000000000000.json");
        std::fs::write(&file, PLAN_FIXTURE).unwrap();

        let mut state = IngestState::new();
        ingest_file(&conn, &file, &mut state, 1_700_000_000);
        // Re-write with a later timestamp; debounce should drop this.
        ingest_file(&conn, &file, &mut state, 1_700_000_000 + 1);

        let last: i64 = conn
            .query_row(
                "SELECT last_seen_at FROM sessions WHERE session_id=?1",
                params!["00000000-0000-0000-0000-000000000000"],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(last, 1_700_000_000, "debounced second call must not have updated row");
    }

    #[test]
    fn test_ingest_file_ignores_bad_json() {
        let tmp = tempfile::tempdir().unwrap();
        let conn = open_db(&tmp.path().join("usage.db")).unwrap();
        let drops = tmp.path().join("usage");
        std::fs::create_dir_all(&drops).unwrap();
        let file = drops.join("garbage.json");
        std::fs::write(&file, "not json").unwrap();

        let mut state = IngestState::new();
        ingest_file(&conn, &file, &mut state, 1_700_000_000);
        let count: i64 = conn
            .query_row("SELECT count(*) FROM sessions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p claudehud-daemon usage::tests::test_ingest`
Expected: FAIL with `cannot find type 'IngestState'` / `cannot find function 'ingest_file'`.

- [ ] **Step 3: Implement `IngestState` + `ingest_file`**

Append to `claudehud-daemon/src/usage.rs` (above the `#[cfg(test)] mod tests` block):

```rust
use std::collections::HashMap;
use std::time::Instant;

const DEBOUNCE_MS: u128 = 500;

/// Per-thread mutable state for the ingest loop. Records the last time
/// each session was UPSERTed so burst renders coalesce into one write.
pub struct IngestState {
    last_seen: HashMap<String, Instant>,
}

impl IngestState {
    pub fn new() -> Self {
        Self {
            last_seen: HashMap::new(),
        }
    }
}

impl Default for IngestState {
    fn default() -> Self {
        Self::new()
    }
}

/// Read `path` as JSON, UPSERT into `conn`. Errors are logged + swallowed.
/// Debounces by session_id: skips if the same session was processed within
/// the last `DEBOUNCE_MS`.
pub fn ingest_file(conn: &Connection, path: &Path, state: &mut IngestState, now_secs: i64) {
    let raw = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("WARN usage read {}: {e}", path.display());
            return;
        }
    };
    let payload = match parse_payload(&raw) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("WARN usage parse {}: {e}", path.display());
            return;
        }
    };
    let Some(sid) = payload.session_id.clone().filter(|s| !s.is_empty()) else {
        return;
    };
    let now = Instant::now();
    if let Some(prev) = state.last_seen.get(&sid) {
        if now.duration_since(*prev).as_millis() < DEBOUNCE_MS {
            return;
        }
    }
    state.last_seen.insert(sid, now);
    if let Err(e) = upsert_session(conn, &payload, now_secs) {
        eprintln!("WARN usage upsert {}: {e}", path.display());
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p claudehud-daemon usage::tests::test_ingest`
Expected: PASS, 3 tests.

- [ ] **Step 5: Commit**

```bash
git add claudehud-daemon/src/usage.rs
git commit -m "feat(daemon): ingest_file with per-session 500ms debounce"
```

---

## Task 7: Daemon — `usage::start` (notify watcher + drain)

**Files:**
- Modify: `claudehud-daemon/src/usage.rs`

The thread entry point. Watches `data_dir()/usage/` for `Create`/`Modify` events, drains existing files on startup, ingests on each event. Mirrors the registrar pattern in `claudehud-daemon/src/registrar.rs`.

- [ ] **Step 1: Implement `start()`**

Append to `claudehud-daemon/src/usage.rs` (above the `#[cfg(test)] mod tests` block):

```rust
use crossbeam_channel::{unbounded, Receiver};
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::time::{SystemTime, UNIX_EPOCH};

fn now_unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Daemon thread entry: open the usage db, watch `data_dir()/usage/`, drain
/// any files that arrived while the daemon was down, then loop forever
/// ingesting Modify/Create events. Failures during setup log to stderr and
/// the thread exits — the rest of the daemon (registrar/watcher/status)
/// keeps running.
pub fn start() {
    let data_dir = common::data_dir();
    let drops_dir = data_dir.join("usage");
    if let Err(e) = std::fs::create_dir_all(&drops_dir) {
        eprintln!("WARN usage: cannot create {}: {e}", drops_dir.display());
        return;
    }
    let db_path = data_dir.join("usage.db");
    let conn = match open_db(&db_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("WARN usage: cannot open {}: {e}", db_path.display());
            return;
        }
    };

    let (tx, rx) = unbounded::<std::path::PathBuf>();
    let mut watcher = match build_watcher(tx) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("WARN usage: cannot build watcher: {e}");
            return;
        }
    };
    if let Err(e) = watcher.watch(&drops_dir, RecursiveMode::NonRecursive) {
        eprintln!("WARN usage: cannot watch {}: {e}", drops_dir.display());
        return;
    }

    let mut state = IngestState::new();

    // Drain pre-existing files. Tracker is fresh, so debounce won't drop them.
    if let Ok(entries) = std::fs::read_dir(&drops_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|x| x == "json") {
                ingest_file(&conn, &path, &mut state, now_unix_secs());
            }
        }
    }

    drain_events(&conn, rx, &mut state);
    // Keep `_watcher` alive until the receiver returns Err.
    drop(watcher);
}

fn build_watcher(
    tx: crossbeam_channel::Sender<std::path::PathBuf>,
) -> Result<RecommendedWatcher, notify::Error> {
    RecommendedWatcher::new(
        move |res: notify::Result<Event>| {
            if let Ok(event) = res {
                if matches!(event.kind, EventKind::Create(_) | EventKind::Modify(_)) {
                    for path in &event.paths {
                        if path.extension().is_some_and(|x| x == "json") {
                            let _ = tx.send(path.clone());
                        }
                    }
                }
            }
        },
        Config::default(),
    )
}

fn drain_events(
    conn: &Connection,
    rx: Receiver<std::path::PathBuf>,
    state: &mut IngestState,
) {
    while let Ok(path) = rx.recv() {
        ingest_file(conn, &path, state, now_unix_secs());
    }
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo build -p claudehud-daemon`
Expected: builds without warnings.

(`start()` itself isn't unit-tested — the integration of notify, threads, and the channel is exercised by the end-to-end smoke test at the end of this plan. The pieces composed inside it — `open_db`, `ingest_file`, the drain loop — are all individually tested.)

- [ ] **Step 3: Run full daemon tests**

Run: `cargo test -p claudehud-daemon`
Expected: all existing + new tests pass, no new warnings.

- [ ] **Step 4: Commit**

```bash
git add claudehud-daemon/src/usage.rs
git commit -m "feat(daemon): usage::start with notify watcher + drain-on-startup"
```

---

## Task 8: Daemon — spawn the usage thread

**Files:**
- Modify: `claudehud-daemon/src/main.rs`

Adds the fourth thread alongside registrar / watcher / status. Unconditional spawn — when `CLAUDEHUD_USAGE` is unset on the client side, no files arrive and the thread idles.

- [ ] **Step 1: Add the spawn**

In `claudehud-daemon/src/main.rs`, locate the existing thread spawns (around line 54-60):

```rust
    std::thread::spawn(move || {
        registrar::start(tx2);
    });

    std::thread::spawn(|| {
        status::start();
    });
```

Insert a third spawn immediately after the `status::start` block, before the `watcher::start(rx)` line:

```rust
    std::thread::spawn(|| {
        usage::start();
    });
```

Also extend `main`'s pre-spawn `create_dir_all` calls to include the data dir. Locate:

```rust
    let _ = std::fs::create_dir_all(common::cache_dir());
    let _ = std::fs::create_dir_all(common::watch_dir());
```

Replace with:

```rust
    let _ = std::fs::create_dir_all(common::cache_dir());
    let _ = std::fs::create_dir_all(common::watch_dir());
    let _ = std::fs::create_dir_all(common::data_dir());
```

- [ ] **Step 2: Verify it builds**

Run: `cargo build -p claudehud-daemon`
Expected: builds without warnings.

- [ ] **Step 3: Verify tests still pass**

Run: `cargo test -p claudehud-daemon`
Expected: all existing tests pass.

- [ ] **Step 4: Commit**

```bash
git add claudehud-daemon/src/main.rs
git commit -m "feat(daemon): spawn usage ingest thread"
```

---

## Task 9: Client — `drop_usage_payload` + env gate

**Files:**
- Create: `claudehud/src/usage.rs`
- Modify: `claudehud/src/lib.rs`
- Modify: `claudehud/src/main.rs`

The client writes the raw stdin JSON atomically (write-then-rename) to `data_dir()/usage/{session_id}.json` when `CLAUDEHUD_USAGE` is set to any non-empty value.

- [ ] **Step 1: Create `claudehud/src/usage.rs` with tests**

Create `claudehud/src/usage.rs`:

```rust
// claudehud/src/usage.rs
//
// Two responsibilities, intentionally co-located in one module:
//   1. Write the raw stdin payload to `data_dir()/usage/{session_id}.json`
//      so the daemon can ingest it (see `drop_payload`).
//   2. Read-side CLI for `claudehud usage <subcommand>` (added in later tasks).

use std::path::Path;

const ENV_GATE: &str = "CLAUDEHUD_USAGE";

/// True when `CLAUDEHUD_USAGE` is set to any non-empty value.
pub fn is_enabled() -> bool {
    std::env::var_os(ENV_GATE).is_some_and(|v| !v.is_empty())
}

/// Atomically write `raw` to `{data_dir}/usage/{session_id}.json`. Best
/// effort — every failure is silently swallowed so the render path never
/// errors because of usage tracking.
pub fn drop_payload(raw: &str, session_id: &str) {
    drop_payload_to(&common::data_dir(), raw, session_id);
}

/// Test seam: same as `drop_payload` but writes under an explicit root.
pub fn drop_payload_to(data_dir: &Path, raw: &str, session_id: &str) {
    if session_id.is_empty() {
        return;
    }
    let dir = data_dir.join("usage");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let tmp = dir.join(format!("{session_id}.json.tmp"));
    let final_ = dir.join(format!("{session_id}.json"));
    if std::fs::write(&tmp, raw).is_err() {
        return;
    }
    let _ = std::fs::rename(&tmp, &final_);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_drop_writes_atomically() {
        let tmp = tempfile::tempdir().unwrap();
        drop_payload_to(tmp.path(), r#"{"k":"v"}"#, "sess-1");
        let content =
            std::fs::read_to_string(tmp.path().join("usage").join("sess-1.json")).unwrap();
        assert_eq!(content, r#"{"k":"v"}"#);
        // .tmp must have been renamed away.
        assert!(!tmp.path().join("usage").join("sess-1.json.tmp").exists());
    }

    #[test]
    fn test_drop_skips_empty_session_id() {
        let tmp = tempfile::tempdir().unwrap();
        drop_payload_to(tmp.path(), r#"{}"#, "");
        assert!(!tmp.path().join("usage").exists());
    }

    #[test]
    fn test_drop_overwrites_existing_file() {
        let tmp = tempfile::tempdir().unwrap();
        drop_payload_to(tmp.path(), "first", "sess-1");
        drop_payload_to(tmp.path(), "second", "sess-1");
        let content =
            std::fs::read_to_string(tmp.path().join("usage").join("sess-1.json")).unwrap();
        assert_eq!(content, "second");
    }

    #[test]
    fn test_is_enabled_unset_is_false() {
        let key = "CLAUDEHUD_USAGE";
        let prev = std::env::var_os(key);
        std::env::remove_var(key);
        assert!(!is_enabled());
        if let Some(p) = prev {
            std::env::set_var(key, p);
        }
    }

    #[test]
    fn test_is_enabled_empty_is_false() {
        let key = "CLAUDEHUD_USAGE";
        let prev = std::env::var_os(key);
        std::env::set_var(key, "");
        assert!(!is_enabled());
        if let Some(p) = prev {
            std::env::set_var(key, p);
        } else {
            std::env::remove_var(key);
        }
    }

    #[test]
    fn test_is_enabled_set_is_true() {
        let key = "CLAUDEHUD_USAGE";
        let prev = std::env::var_os(key);
        std::env::set_var(key, "1");
        assert!(is_enabled());
        if let Some(p) = prev {
            std::env::set_var(key, p);
        } else {
            std::env::remove_var(key);
        }
    }
}
```

- [ ] **Step 2: Re-export the module**

Append to `claudehud/src/lib.rs`:

```rust
pub mod usage;
```

- [ ] **Step 3: Run tests to verify they pass**

Run: `cargo test -p claudehud usage::tests`
Expected: PASS, 6 tests.

(`is_enabled` env tests can race with other env-mutating tests. If the harness flags this, append `-- --test-threads=1` for this run. Existing test in `common/src/lib.rs::test_cache_dir_respects_env_override` already uses raw `set_var`, so this matches project precedent.)

- [ ] **Step 4: Wire into client main**

In `claudehud/src/main.rs`, locate the `render` function. After the existing line:

```rust
    print!(
        "{}",
        render::render(&input, git, &incidents, total_active, rounding, layout)
    );
```

Insert before the trailing `ExitCode::SUCCESS`:

```rust
    if usage::is_enabled() {
        if let Some(sid) = input.session_id.as_deref() {
            usage::drop_payload(&raw, sid);
        }
    }
```

Also add the import. Modify the existing `use claudehud::...` line near the top of `main.rs`:

```rust
use claudehud::{git, incidents, input, install, render, update, usage};
```

- [ ] **Step 5: Verify the client builds**

Run: `cargo build -p claudehud`
Expected: builds without warnings.

- [ ] **Step 6: Verify all tests pass**

Run: `cargo test -p claudehud`
Expected: all existing + new tests pass.

- [ ] **Step 7: Commit**

```bash
git add claudehud/src/usage.rs claudehud/src/lib.rs claudehud/src/main.rs
git commit -m "feat(claudehud): drop usage payload when CLAUDEHUD_USAGE set"
```

---

## Task 10: Client — CLI scaffold + `usage db` subcommand

**Files:**
- Modify: `claudehud/src/usage.rs`
- Modify: `claudehud/src/main.rs`

Adds the `claudehud usage` subcommand entry point and the simplest sub-subcommand (`db` — prints the path). Establishes the dispatch + help structure that later tasks extend.

- [ ] **Step 1: Add the failing test**

Append to the `#[cfg(test)] mod tests` block in `claudehud/src/usage.rs`:

```rust
    #[test]
    fn test_db_path_for() {
        let tmp = tempfile::tempdir().unwrap();
        let got = db_path_for(tmp.path());
        assert_eq!(got, tmp.path().join("usage.db"));
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p claudehud usage::tests::test_db_path_for`
Expected: FAIL with `cannot find function 'db_path_for' in this scope`.

- [ ] **Step 3: Implement the CLI entry point + `db` handler**

Append to `claudehud/src/usage.rs` (above the `#[cfg(test)] mod tests` block):

```rust
use std::path::PathBuf;
use std::process::ExitCode;

const USAGE_HELP: &str = "\
claudehud usage — query the local Claude Code usage database

USAGE:
  claudehud usage [today]                   summary for today
  claudehud usage week                      summary for last 7 days
  claudehud usage sessions [OPTIONS]        list recent sessions
  claudehud usage projects [OPTIONS]        top projects by spend
  claudehud usage db                        print the usage.db path

Requires the daemon to be running and CLAUDEHUD_USAGE=1 set in your shell
so the client drops payloads for the daemon to ingest.

Run `claudehud usage <sub> --help` for subcommand options.
";

/// Build the absolute path to the usage db under `data_dir`.
pub fn db_path_for(data_dir: &Path) -> PathBuf {
    data_dir.join("usage.db")
}

/// CLI dispatcher invoked from `main.rs` when the first arg is `usage`.
pub fn run(mut args: pico_args::Arguments) -> ExitCode {
    if args.contains(["-h", "--help"]) {
        print!("{USAGE_HELP}");
        return ExitCode::SUCCESS;
    }
    let sub = match args.subcommand() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("claudehud usage: {e}");
            return ExitCode::from(2);
        }
    };
    let sub = sub.as_deref().unwrap_or("today");
    match sub {
        "today" | "week" | "sessions" | "projects" => {
            eprintln!("claudehud usage {sub}: not yet implemented");
            ExitCode::from(2)
        }
        "db" => {
            println!("{}", db_path_for(&common::data_dir()).display());
            ExitCode::SUCCESS
        }
        other => {
            eprintln!("claudehud usage: unknown subcommand '{other}'");
            eprintln!("run `claudehud usage --help` for options");
            ExitCode::from(2)
        }
    }
}
```

- [ ] **Step 4: Wire `usage` into main.rs subcommand match**

In `claudehud/src/main.rs`, locate the `match args.subcommand()...` block (around line 45). Add a `usage` arm immediately after the existing `render` arm:

```rust
        Some("render") => return render(args),
        Some("usage") => return usage::run(args),
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p claudehud usage::tests::test_db_path_for`
Expected: PASS.

- [ ] **Step 6: Smoke-test the cli**

Run: `cargo run -p claudehud -- usage db`
Expected: prints a path ending in `claudehud/usage.db` (macOS: `~/Library/Application Support/claudehud/usage.db`, Linux XDG, or Windows `%LOCALAPPDATA%\claudehud\data\usage.db`), exit 0.

Run: `cargo run -p claudehud -- usage --help`
Expected: prints `USAGE_HELP` text, exit 0.

Run: `cargo run -p claudehud -- usage bogus`
Expected: prints `unknown subcommand 'bogus'` to stderr, exit 2.

- [ ] **Step 7: Commit**

```bash
git add claudehud/src/usage.rs claudehud/src/main.rs
git commit -m "feat(claudehud): usage subcommand scaffold + `usage db`"
```

---

## Task 11: Client — `usage today` + `usage week` summaries

**Files:**
- Modify: `claudehud/src/usage.rs`

The two summary subcommands share the same shape: aggregate over a time window, format a one-line summary plus top-3 projects and a top model.

- [ ] **Step 1: Add failing tests**

Append to the `#[cfg(test)] mod tests` block in `claudehud/src/usage.rs`:

```rust
    fn seed_test_db(path: &Path) {
        use rusqlite::{params, Connection};
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (
                session_id TEXT PRIMARY KEY,
                first_seen_at INTEGER NOT NULL,
                last_seen_at INTEGER NOT NULL,
                cc_version TEXT, session_name TEXT, cwd TEXT, project_dir TEXT,
                model_id TEXT, model_display TEXT, output_style TEXT, effort_level TEXT,
                thinking_enabled INTEGER, fast_mode INTEGER, exceeds_200k_tokens INTEGER,
                billing_type TEXT,
                total_cost_usd REAL, total_duration_ms INTEGER, total_api_duration_ms INTEGER,
                total_lines_added INTEGER, total_lines_removed INTEGER,
                context_window_size INTEGER, total_input_tokens INTEGER,
                total_output_tokens INTEGER, cache_creation_tokens INTEGER,
                cache_read_tokens INTEGER, context_used_pct REAL,
                five_hour_used_pct REAL, five_hour_resets_at INTEGER,
                seven_day_used_pct REAL, seven_day_resets_at INTEGER
            );",
        )
        .unwrap();
        // Three sessions; 1 outside the window (1_700_000_000).
        for (sid, last, cost, api_ms, project, model) in [
            ("a", 1_700_000_100, 1.50_f64, 600_000_i64, "/p/alpha", "Opus 4.7"),
            ("b", 1_700_000_200, 0.50, 200_000, "/p/alpha", "Opus 4.7"),
            ("c", 1_700_000_300, 2.50, 400_000, "/p/beta", "Sonnet 4.6"),
            ("d", 1_699_000_000, 9.99, 999_999, "/p/old", "Opus 3.5"),
        ] {
            conn.execute(
                "INSERT INTO sessions (session_id, first_seen_at, last_seen_at,
                 total_cost_usd, total_api_duration_ms, project_dir, model_display, billing_type)
                 VALUES (?1, ?2, ?2, ?3, ?4, ?5, ?6, 'plan')",
                params![sid, last, cost, api_ms, project, model],
            )
            .unwrap();
        }
    }

    #[test]
    fn test_summarize_window_aggregates_correctly() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("usage.db");
        seed_test_db(&db);
        let summary = summarize_window(&db, 1_700_000_000).unwrap();
        assert_eq!(summary.session_count, 3);
        assert!((summary.total_cost - 4.5).abs() < 1e-9);
        assert_eq!(summary.total_api_ms, 1_200_000);
        assert_eq!(summary.top_projects[0].0, "/p/beta");
        assert!((summary.top_projects[0].1 - 2.5).abs() < 1e-9);
        assert_eq!(summary.top_model.as_deref(), Some("Opus 4.7"));
    }

    #[test]
    fn test_summarize_window_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("usage.db");
        seed_test_db(&db);
        let summary = summarize_window(&db, 2_000_000_000).unwrap();
        assert_eq!(summary.session_count, 0);
        assert_eq!(summary.total_cost, 0.0);
        assert!(summary.top_projects.is_empty());
        assert!(summary.top_model.is_none());
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p claudehud usage::tests::test_summarize_window`
Expected: FAIL with `cannot find function 'summarize_window'`.

- [ ] **Step 3: Implement `summarize_window` + handlers**

Append to `claudehud/src/usage.rs` (above the `#[cfg(test)] mod tests` block):

```rust
use rusqlite::{params, Connection, OpenFlags};

#[derive(Debug, Default)]
pub struct Summary {
    pub session_count: i64,
    pub total_cost: f64,
    pub total_api_ms: i64,
    pub top_projects: Vec<(String, f64, i64)>,
    pub top_model: Option<String>,
    pub top_model_share_pct: i64,
}

fn open_readonly(db: &Path) -> Result<Connection, rusqlite::Error> {
    Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY)
}

/// Aggregate sessions with `last_seen_at >= since`.
pub fn summarize_window(db: &Path, since: i64) -> Result<Summary, rusqlite::Error> {
    let conn = open_readonly(db)?;
    let (count, cost, api_ms): (i64, Option<f64>, Option<i64>) = conn.query_row(
        "SELECT count(*), COALESCE(sum(total_cost_usd), 0), COALESCE(sum(total_api_duration_ms), 0)
         FROM sessions WHERE last_seen_at >= ?1",
        params![since],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;

    let mut top_projects: Vec<(String, f64, i64)> = Vec::new();
    let mut stmt = conn.prepare(
        "SELECT COALESCE(project_dir, '(unknown)'), sum(total_cost_usd), count(*)
         FROM sessions WHERE last_seen_at >= ?1
         GROUP BY project_dir ORDER BY 2 DESC LIMIT 3",
    )?;
    for row in stmt.query_map(params![since], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, Option<f64>>(1)?.unwrap_or(0.0), r.get::<_, i64>(2)?))
    })? {
        top_projects.push(row?);
    }

    let mut top_model: Option<String> = None;
    let mut top_model_share_pct: i64 = 0;
    if api_ms.unwrap_or(0) > 0 {
        if let Ok((model, share_ms)) = conn.query_row(
            "SELECT COALESCE(model_display, '(unknown)'), sum(total_api_duration_ms)
             FROM sessions WHERE last_seen_at >= ?1
             GROUP BY model_display ORDER BY 2 DESC LIMIT 1",
            params![since],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<i64>>(1)?.unwrap_or(0))),
        ) {
            top_model = Some(model);
            let total = api_ms.unwrap_or(0);
            top_model_share_pct = if total > 0 { (share_ms * 100) / total } else { 0 };
        }
    }

    Ok(Summary {
        session_count: count,
        total_cost: cost.unwrap_or(0.0),
        total_api_ms: api_ms.unwrap_or(0),
        top_projects,
        top_model,
        top_model_share_pct,
    })
}

fn fmt_duration_ms(ms: i64) -> String {
    let secs = ms / 1000;
    let hours = secs / 3600;
    let mins = (secs % 3600) / 60;
    if hours > 0 {
        format!("{hours}h {mins}m")
    } else {
        format!("{mins}m")
    }
}

fn fmt_project(project_dir: &str) -> String {
    Path::new(project_dir)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(project_dir)
        .to_string()
}

fn print_summary(label: &str, summary: &Summary) {
    println!(
        "{label}   {} sessions   ${:.2} spent   {} total api time",
        summary.session_count,
        summary.total_cost,
        fmt_duration_ms(summary.total_api_ms)
    );
    if !summary.top_projects.is_empty() {
        let parts: Vec<String> = summary
            .top_projects
            .iter()
            .map(|(p, c, n)| format!("{} (${:.2}, {n})", fmt_project(p), c))
            .collect();
        println!("top projects:  {}", parts.join(" · "));
    }
    if let Some(m) = &summary.top_model {
        println!(
            "top model:     {m}   {}% of api time",
            summary.top_model_share_pct
        );
    }
}

fn ensure_db_or_hint(db: &Path) -> bool {
    if db.exists() {
        return true;
    }
    eprintln!(
        "claudehud: no usage database at {} — set CLAUDEHUD_USAGE=1 and run a Claude Code session to populate it",
        db.display()
    );
    false
}

fn since_secs(now: i64, days: i64) -> i64 {
    now - days * 86_400
}
```

Then update the `run()` dispatcher to wire in the new arms. Replace the existing `"today" | "week" | "sessions" | "projects"` arm with:

```rust
        "today" => run_today(),
        "week" => run_week(),
        "sessions" | "projects" => {
            eprintln!("claudehud usage {sub}: not yet implemented");
            ExitCode::from(2)
        }
```

Add the handlers below `run`:

```rust
fn now_unix_secs() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn run_today() -> ExitCode {
    let db = db_path_for(&common::data_dir());
    if !ensure_db_or_hint(&db) {
        return ExitCode::SUCCESS;
    }
    let since = since_secs(now_unix_secs(), 1);
    match summarize_window(&db, since) {
        Ok(s) => {
            let date = today_str();
            print_summary(&date, &s);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("claudehud usage today: {e}");
            ExitCode::from(1)
        }
    }
}

fn run_week() -> ExitCode {
    let db = db_path_for(&common::data_dir());
    if !ensure_db_or_hint(&db) {
        return ExitCode::SUCCESS;
    }
    let since = since_secs(now_unix_secs(), 7);
    match summarize_window(&db, since) {
        Ok(s) => {
            print_summary("last 7 days", &s);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("claudehud usage week: {e}");
            ExitCode::from(1)
        }
    }
}

fn today_str() -> String {
    let now = time::OffsetDateTime::now_local()
        .unwrap_or_else(|_| time::OffsetDateTime::now_utc());
    format!(
        "{:04}-{:02}-{:02}",
        now.year(),
        u8::from(now.month()),
        now.day()
    )
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p claudehud usage::tests::test_summarize_window`
Expected: PASS, 2 tests.

- [ ] **Step 5: Smoke-test against an empty db**

Run: `cargo run -p claudehud -- usage today`
Expected: prints `claudehud: no usage database at <path>...` to stderr, exit 0.

- [ ] **Step 6: Commit**

```bash
git add claudehud/src/usage.rs
git commit -m "feat(claudehud): usage today + week summaries"
```

---

## Task 12: Client — `usage sessions` listing

**Files:**
- Modify: `claudehud/src/usage.rs`

Lists recent sessions in a compact table. Supports `--limit N`, `--project PATH`, `--since YYYY-MM-DD`, `--json`.

- [ ] **Step 1: Add failing tests**

Append to the `#[cfg(test)] mod tests` block in `claudehud/src/usage.rs`:

```rust
    #[test]
    fn test_list_sessions_default_orders_desc() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("usage.db");
        seed_test_db(&db);
        let opts = SessionsOpts {
            limit: 10,
            project: None,
            since: 0,
        };
        let rows = list_sessions(&db, &opts).unwrap();
        let ids: Vec<&str> = rows.iter().map(|r| r.session_id.as_str()).collect();
        assert_eq!(ids, vec!["c", "b", "a", "d"]);
    }

    #[test]
    fn test_list_sessions_limit() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("usage.db");
        seed_test_db(&db);
        let opts = SessionsOpts {
            limit: 2,
            project: None,
            since: 0,
        };
        let rows = list_sessions(&db, &opts).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].session_id, "c");
        assert_eq!(rows[1].session_id, "b");
    }

    #[test]
    fn test_list_sessions_filter_by_project() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("usage.db");
        seed_test_db(&db);
        let opts = SessionsOpts {
            limit: 10,
            project: Some("/p/alpha".to_string()),
            since: 0,
        };
        let rows = list_sessions(&db, &opts).unwrap();
        let ids: Vec<&str> = rows.iter().map(|r| r.session_id.as_str()).collect();
        assert_eq!(ids, vec!["b", "a"]);
    }

    #[test]
    fn test_list_sessions_filter_since() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("usage.db");
        seed_test_db(&db);
        let opts = SessionsOpts {
            limit: 10,
            project: None,
            since: 1_700_000_000,
        };
        let rows = list_sessions(&db, &opts).unwrap();
        assert_eq!(rows.len(), 3, "session d is older than the window");
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p claudehud usage::tests::test_list_sessions`
Expected: FAIL with `cannot find type 'SessionsOpts'` / `cannot find function 'list_sessions'`.

- [ ] **Step 3: Implement `list_sessions` + the cli handler**

Append to `claudehud/src/usage.rs` (above the `#[cfg(test)] mod tests` block):

```rust
pub struct SessionsOpts {
    pub limit: i64,
    pub project: Option<String>,
    pub since: i64,
}

pub struct SessionRow {
    pub session_id: String,
    pub last_seen_at: i64,
    pub project_dir: Option<String>,
    pub total_cost_usd: f64,
    pub context_used_pct: Option<f64>,
    pub model_display: Option<String>,
}

pub fn list_sessions(db: &Path, opts: &SessionsOpts) -> Result<Vec<SessionRow>, rusqlite::Error> {
    let conn = open_readonly(db)?;
    let sql = "SELECT session_id, last_seen_at, project_dir,
                      COALESCE(total_cost_usd, 0), context_used_pct, model_display
               FROM sessions
               WHERE last_seen_at >= ?1
                 AND (?2 IS NULL OR project_dir = ?2)
               ORDER BY last_seen_at DESC
               LIMIT ?3";
    let mut stmt = conn.prepare(sql)?;
    let project = opts.project.clone();
    let rows = stmt.query_map(params![opts.since, project, opts.limit], |r| {
        Ok(SessionRow {
            session_id: r.get(0)?,
            last_seen_at: r.get(1)?,
            project_dir: r.get(2)?,
            total_cost_usd: r.get(3)?,
            context_used_pct: r.get(4)?,
            model_display: r.get(5)?,
        })
    })?;
    rows.collect()
}

fn parse_ymd_to_unix(s: &str) -> Option<i64> {
    let mut parts = s.split('-');
    let y: i32 = parts.next()?.parse().ok()?;
    let m: u8 = parts.next()?.parse().ok()?;
    let d: u8 = parts.next()?.parse().ok()?;
    let date = time::Date::from_calendar_date(y, time::Month::try_from(m).ok()?, d).ok()?;
    let dt = date.with_hms(0, 0, 0).ok()?.assume_utc();
    Some(dt.unix_timestamp())
}

fn run_sessions(mut args: pico_args::Arguments) -> ExitCode {
    let db = db_path_for(&common::data_dir());
    if !ensure_db_or_hint(&db) {
        return ExitCode::SUCCESS;
    }
    let json = args.contains("--json");
    let limit: i64 = args
        .opt_value_from_str("--limit")
        .ok()
        .flatten()
        .unwrap_or(20);
    let project: Option<String> = args.opt_value_from_str("--project").ok().flatten();
    let since_str: Option<String> = args.opt_value_from_str("--since").ok().flatten();
    let since = match since_str {
        Some(s) => match parse_ymd_to_unix(&s) {
            Some(v) => v,
            None => {
                eprintln!("claudehud usage sessions: --since must be YYYY-MM-DD");
                return ExitCode::from(2);
            }
        },
        None => 0,
    };
    let opts = SessionsOpts {
        limit,
        project,
        since,
    };
    let rows = match list_sessions(&db, &opts) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("claudehud usage sessions: {e}");
            return ExitCode::from(1);
        }
    };

    if json {
        for r in &rows {
            println!(
                "{{\"session_id\":\"{}\",\"last_seen_at\":{},\"project_dir\":{},\"total_cost_usd\":{:.4},\"context_used_pct\":{},\"model_display\":{}}}",
                r.session_id,
                r.last_seen_at,
                json_opt_str(r.project_dir.as_deref()),
                r.total_cost_usd,
                json_opt_num(r.context_used_pct),
                json_opt_str(r.model_display.as_deref())
            );
        }
        return ExitCode::SUCCESS;
    }

    println!("session                                  last seen          cost     ctx   model");
    for r in &rows {
        let ts = fmt_unix_ts(r.last_seen_at);
        let project = r
            .project_dir
            .as_deref()
            .map(fmt_project)
            .unwrap_or_else(|| "—".into());
        let ctx = r
            .context_used_pct
            .map(|p| format!("{:>3.0}%", p))
            .unwrap_or_else(|| "  —".into());
        let model = r.model_display.as_deref().unwrap_or("—");
        let sid = if r.session_id.len() > 36 {
            r.session_id[..36].to_string()
        } else {
            r.session_id.clone()
        };
        println!(
            "{sid:<36}  {project:<12}  {ts}  ${:<7.2} {ctx}   {model}",
            r.total_cost_usd
        );
    }
    ExitCode::SUCCESS
}

fn fmt_unix_ts(secs: i64) -> String {
    let dt = time::OffsetDateTime::from_unix_timestamp(secs)
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
    let dt = dt
        .to_offset(time::UtcOffset::current_local_offset().unwrap_or(time::UtcOffset::UTC));
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        dt.year(),
        u8::from(dt.month()),
        dt.day(),
        dt.hour(),
        dt.minute()
    )
}

fn json_opt_str(s: Option<&str>) -> String {
    match s {
        Some(v) => format!("\"{}\"", v.replace('"', "\\\"")),
        None => "null".to_string(),
    }
}

fn json_opt_num(n: Option<f64>) -> String {
    match n {
        Some(v) => format!("{v}"),
        None => "null".to_string(),
    }
}
```

Update the dispatch in `run`. Replace the `"sessions" | "projects"` arm with:

```rust
        "sessions" => run_sessions(args),
        "projects" => {
            eprintln!("claudehud usage projects: not yet implemented");
            ExitCode::from(2)
        }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p claudehud usage::tests::test_list_sessions`
Expected: PASS, 4 tests.

- [ ] **Step 5: Commit**

```bash
git add claudehud/src/usage.rs
git commit -m "feat(claudehud): usage sessions listing with --limit/--project/--since/--json"
```

---

## Task 13: Client — `usage projects` listing

**Files:**
- Modify: `claudehud/src/usage.rs`

Top projects by spend over a window. Supports `--since YYYY-MM-DD` and `--limit N`.

- [ ] **Step 1: Add failing tests**

Append to the `#[cfg(test)] mod tests` block in `claudehud/src/usage.rs`:

```rust
    #[test]
    fn test_top_projects_orders_by_cost_desc() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("usage.db");
        seed_test_db(&db);
        let rows = top_projects(&db, 0, 10).unwrap();
        // /p/old has highest cost ($9.99) but is filtered when since>0.
        // With since=0, expect /p/old, /p/beta, /p/alpha.
        let paths: Vec<&str> = rows.iter().map(|r| r.project_dir.as_str()).collect();
        assert_eq!(paths, vec!["/p/old", "/p/beta", "/p/alpha"]);
    }

    #[test]
    fn test_top_projects_respects_since() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("usage.db");
        seed_test_db(&db);
        let rows = top_projects(&db, 1_700_000_000, 10).unwrap();
        let paths: Vec<&str> = rows.iter().map(|r| r.project_dir.as_str()).collect();
        assert_eq!(paths, vec!["/p/beta", "/p/alpha"]);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p claudehud usage::tests::test_top_projects`
Expected: FAIL with `cannot find function 'top_projects'`.

- [ ] **Step 3: Implement `top_projects` + handler**

Append to `claudehud/src/usage.rs` (above the `#[cfg(test)] mod tests` block):

```rust
pub struct ProjectRow {
    pub project_dir: String,
    pub total_cost_usd: f64,
    pub session_count: i64,
}

pub fn top_projects(
    db: &Path,
    since: i64,
    limit: i64,
) -> Result<Vec<ProjectRow>, rusqlite::Error> {
    let conn = open_readonly(db)?;
    let mut stmt = conn.prepare(
        "SELECT COALESCE(project_dir, '(unknown)'),
                COALESCE(sum(total_cost_usd), 0),
                count(*)
         FROM sessions
         WHERE last_seen_at >= ?1
         GROUP BY project_dir
         ORDER BY 2 DESC
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![since, limit], |r| {
        Ok(ProjectRow {
            project_dir: r.get(0)?,
            total_cost_usd: r.get(1)?,
            session_count: r.get(2)?,
        })
    })?;
    rows.collect()
}

fn run_projects(mut args: pico_args::Arguments) -> ExitCode {
    let db = db_path_for(&common::data_dir());
    if !ensure_db_or_hint(&db) {
        return ExitCode::SUCCESS;
    }
    let limit: i64 = args
        .opt_value_from_str("--limit")
        .ok()
        .flatten()
        .unwrap_or(10);
    let since_str: Option<String> = args.opt_value_from_str("--since").ok().flatten();
    let since = match since_str {
        Some(s) => match parse_ymd_to_unix(&s) {
            Some(v) => v,
            None => {
                eprintln!("claudehud usage projects: --since must be YYYY-MM-DD");
                return ExitCode::from(2);
            }
        },
        // Default to last 30 days when no --since given.
        None => since_secs(now_unix_secs(), 30),
    };
    let rows = match top_projects(&db, since, limit) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("claudehud usage projects: {e}");
            return ExitCode::from(1);
        }
    };
    println!("project                                cost      sessions");
    for r in &rows {
        let p = fmt_project(&r.project_dir);
        println!(
            "{p:<36}   ${:<7.2}  {}",
            r.total_cost_usd, r.session_count
        );
    }
    ExitCode::SUCCESS
}
```

Update the dispatch in `run`. Replace the `"projects"` arm with:

```rust
        "projects" => run_projects(args),
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p claudehud usage::tests::test_top_projects`
Expected: PASS, 2 tests.

- [ ] **Step 5: Commit**

```bash
git add claudehud/src/usage.rs
git commit -m "feat(claudehud): usage projects ranking"
```

---

## Task 14: Docs — README + `claudehud --help`

**Files:**
- Modify: `claudehud/src/main.rs`
- Modify: `README.md`

- [ ] **Step 1: Update `claudehud --help` text**

In `claudehud/src/main.rs`, modify the `const HELP: &str = "..."` block. Add a new line under `USAGE:` (after the `claudehud update` line):

```
  claudehud usage [SUB] [OPTS]   Query the local usage database.
                                 See `claudehud usage --help`.
```

Add a new line to the `ENVIRONMENT:` section, after the existing `CLAUDE_CONFIG_DIR` block:

```
  CLAUDEHUD_USAGE                When set to any non-empty value, the render
                                 path drops the stdin payload to
                                 data_dir()/usage/ for the daemon to ingest.
                                 Unset by default. See README for details.
  CLAUDEHUD_DATA_DIR             Override the durable data directory holding
                                 usage.db and per-session payload drops.
                                 Default: ~/Library/Application Support/claudehud
                                 (macOS), $XDG_DATA_HOME/claudehud (Linux),
                                 %LOCALAPPDATA%\\claudehud\\data (Windows).
```

- [ ] **Step 2: Update README.md**

In `README.md`, locate the `## Configuration` section. After the existing layout / Claude Code / Daemon configuration blocks (but before `## Dependencies`), add:

````markdown
### Usage tracking (opt-in)

When `CLAUDEHUD_USAGE` is set to any non-empty value, `claudehud render`
drops the Claude Code stdin payload as JSON to `data_dir()/usage/{session_id}.json`
after each render. The daemon's `usage` thread watches that directory and
UPSERTs one row per session into a local SQLite database at
`data_dir()/usage.db`.

Enable it by exporting in your shell:

```bash
export CLAUDEHUD_USAGE=1
```

Inspect the database via the read-only CLI:

```bash
claudehud usage              # today's summary
claudehud usage week         # last 7 days
claudehud usage sessions     # recent sessions
claudehud usage projects     # top projects by spend
claudehud usage db           # print path to usage.db
```

The CLI does not require `CLAUDEHUD_USAGE` to be set — if the database
exists from a prior opt-in, reads are always permitted. To stop collecting
new data, unset the env var. To delete history, remove `data_dir()/usage.db`
and `data_dir()/usage/`.

Data location: `~/Library/Application Support/claudehud/` (macOS),
`$XDG_DATA_HOME/claudehud/` or `~/.local/share/claudehud/` (Linux),
`%LOCALAPPDATA%\claudehud\data\` (Windows). Override with `CLAUDEHUD_DATA_DIR`.
````

Also update the `## Dependencies` table by adding three new rows:

```markdown
| `rusqlite` | client + daemon | SQLite bindings (bundled, no system libsqlite3) |
| `serde` + `serde_json` | client + daemon | usage payload (de)serialization |
```

(The client already lists `serde` and `serde_json` indirectly via the existing client-side use; adding the daemon to the row is the substantive change.)

- [ ] **Step 3: Smoke-test help text**

Run: `cargo run -p claudehud -- --help`
Expected: prints `HELP` including the new `usage` line and `CLAUDEHUD_USAGE` env var.

- [ ] **Step 4: Commit**

```bash
git add claudehud/src/main.rs README.md
git commit -m "docs: document CLAUDEHUD_USAGE env var + usage subcommand"
```

---

## Task 15: End-to-end smoke test

**Files:**
- nothing new — manual verification only

A live test of the full pipeline: client drop → daemon ingest → CLI read. Run on the developer's machine; not part of CI.

- [ ] **Step 1: Build release binaries**

Run: `cargo build --release --workspace`
Expected: clean build.

- [ ] **Step 2: Set up an isolated data dir**

```bash
export CLAUDEHUD_DATA_DIR=/tmp/claudehud-e2e
export CLAUDEHUD_USAGE=1
rm -rf "$CLAUDEHUD_DATA_DIR"
```

- [ ] **Step 3: Start the daemon in the foreground**

In one terminal:

```bash
./target/release/claudehud-daemon
```

(Note: on macOS release builds, `windows_subsystem` doesn't apply; daemon stays attached. On Windows, run the debug build for visible logs: `./target/debug/claudehud-daemon.exe`.)

- [ ] **Step 4: Drive the client with a fixture payload**

In a second terminal:

```bash
echo '{
  "session_id": "e2e-test-0001",
  "version": "2.1.139",
  "cwd": "/tmp",
  "model": {"id": "claude-opus-4-7", "display_name": "Opus 4.7"},
  "workspace": {"project_dir": "/tmp/e2e"},
  "cost": {"total_cost_usd": 1.23, "total_api_duration_ms": 60000},
  "rate_limits": {"five_hour": {"used_percentage": 10, "resets_at": 0}}
}' | ./target/release/claudehud render
```

Expected: prints a statusline. No errors.

- [ ] **Step 5: Verify the daemon picked it up**

```bash
ls -la "$CLAUDEHUD_DATA_DIR/usage/"
sqlite3 "$CLAUDEHUD_DATA_DIR/usage.db" "SELECT session_id, total_cost_usd, billing_type FROM sessions;"
```

Expected: `usage/` contains `e2e-test-0001.json`. The query returns
`e2e-test-0001|1.23|plan`.

- [ ] **Step 6: Verify the CLI reads correctly**

```bash
./target/release/claudehud usage today
./target/release/claudehud usage sessions --limit 5
./target/release/claudehud usage db
```

Expected: `today` shows 1 session, $1.23 spent. `sessions` lists `e2e-test-0001`. `db` prints `$CLAUDEHUD_DATA_DIR/usage.db`.

- [ ] **Step 7: Stop the daemon, verify gating**

Stop the daemon (Ctrl-C). Unset the env var:

```bash
unset CLAUDEHUD_USAGE
rm "$CLAUDEHUD_DATA_DIR/usage/e2e-test-0001.json"
echo '{"session_id":"e2e-test-0002","cost":{"total_cost_usd":9.99}}' | ./target/release/claudehud render
ls "$CLAUDEHUD_DATA_DIR/usage/"
```

Expected: `usage/` is empty — the client did not write a drop file because `CLAUDEHUD_USAGE` was unset.

- [ ] **Step 8: Tear down**

```bash
unset CLAUDEHUD_DATA_DIR CLAUDEHUD_USAGE
rm -rf /tmp/claudehud-e2e
```

(No commit — this is a runtime verification.)

---

## Verification

After completing all tasks, run the full test suite:

```bash
cargo test --workspace
```

Expected: all tests pass, no warnings.

```bash
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: clippy clean (note: pre-existing workspace clippy drift may surface — only treat new findings in files touched by this plan as blockers).
