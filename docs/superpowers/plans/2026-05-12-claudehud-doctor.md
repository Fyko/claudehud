# claudehud doctor Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement `claudehud doctor` — a diagnostics subcommand that runs 5 health checks and prints a checklist, exiting nonzero if any check fails.

**Architecture:** New `claudehud/src/doctor.rs` module with one public `run(args)` fn and all check logic. Wire into `claudehud/src/main.rs` dispatch + HELP string. Add `pub mod doctor` to `lib.rs`. No new external deps — uses `serde_json` (already present) and `std`.

**Tech Stack:** Rust (workspace), `pico_args` (already used), `serde_json` (already in `claudehud/Cargo.toml`), `common` crate.

**Spec:** `docs/superpowers/specs/2026-05-12-claudehud-doctor.md`

---

## File Structure

**Create:**
- `claudehud/src/doctor.rs`

**Modify:**
- `claudehud/src/lib.rs` — add `pub mod doctor;`
- `claudehud/src/main.rs` — dispatch `"doctor"` + HELP string entry

---

## Task 1: Scaffold `doctor.rs` with data structures and rendering

**Files:**
- Create: `claudehud/src/doctor.rs`

- [ ] **Step 1: Write `CheckResult` struct + `Report` struct**

```rust
pub struct CheckResult {
    pub id: &'static str,
    pub ok: bool,
    pub detail: String,
}

pub struct Report {
    pub checks: Vec<CheckResult>,
}

impl Report {
    pub fn all_ok(&self) -> bool {
        self.checks.iter().all(|c| c.ok)
    }
}
```

- [ ] **Step 2: Write `print_human(report, use_color)`**

ANSI: green `\x1b[32m✓\x1b[0m` for ok, red `\x1b[31m✗\x1b[0m` for fail. Plain: `[ok]` / `[FAIL]`. Format: `<marker> <id_label> (<detail>)\n`.

Map `id` to a human label:
- `daemon_running` → `"daemon running"`
- `service_registered` → `"service registered"`
- `cache_fresh` → `"cache fresh"`
- `versions_match` → `"versions match"`
- `incidents_cache` → `"incidents cache"`

- [ ] **Step 3: Write `print_json(report)`**

Output:
```json
{"version":1,"ok":<bool>,"checks":[{"id":"...","ok":<bool>,"detail":"..."},...]}
```

Use `serde_json::json!` macro. Print with `println!("{}", ...)`.

- [ ] **Step 4: Verify it compiles**

```bash
cargo check -p claudehud --locked
```

---

## Task 2: Implement check functions

**Files:**
- Modify: `claudehud/src/doctor.rs`

Each function signature: `fn check_<name>(ctx: &Ctx) -> CheckResult` where `Ctx` holds cwd, cache_dir override (for tests), daemon binary path override (for tests).

```rust
struct Ctx {
    cwd: std::path::PathBuf,
    // test seams
    cache_dir_override: Option<std::path::PathBuf>,
    daemon_bin_override: Option<std::path::PathBuf>,
}
```

- [ ] **Step 1: `check_daemon_running`**

Logic:
1. Compute mmap path for cwd's git root (via `common::find_git_root` + `common::mmap_path` or overridden cache dir).
2. If file mtime < 60s ago → `ok=true`, detail `"cache updated Xs ago"`.
3. Else fall through to platform service query:
   - macOS: `launchctl list com.claudehud.daemon` — parse stdout for `"PID"` key. If present and non-`"-"` → `ok=true`, detail `"pid <N>"`. If service found but no PID → `ok=false`, detail `"service registered but not running"`. If command fails → `ok=false`, detail `"not found via launchctl"`.
   - Linux: `systemctl --user is-active claudehud-daemon` — exit 0 → `ok=true`, detail `"systemd unit active"`. Else `ok=false`, detail `"unit not active"`.
   - Windows: `tasklist /FI "IMAGENAME eq claudehud-daemon.exe" /FO CSV /NH` — any non-empty CSV line → `ok=true`, detail `"process found"`. Else `ok=false`.

Compile-time cfg gates for platform branches.

- [ ] **Step 2: `check_service_registered`**

Logic per platform:
- macOS: `fs::metadata(home.join("Library/LaunchAgents/com.claudehud.daemon.plist"))`.ok()` → `ok=true`.
- Linux: `fs::metadata(home.join(".config/systemd/user/claudehud-daemon.service")).ok()` → `ok=true`.
- Windows: `Command::new("schtasks").args(["/Query", "/TN", "claudehud-daemon", "/FO", "LIST"]).status().map(|s| s.success()).unwrap_or(false)` → `ok=true`.

On `ok=false`: detail `"not registered (daemon may still run manually)"`.

Home dir: `std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))`.

- [ ] **Step 3: `check_cache_fresh`**

Logic:
1. Find git root of cwd (use `ctx.cwd`). If none → skip with `ok=true`, detail `"not in a git repo"`.
2. Compute hash + mmap path (respecting `ctx.cache_dir_override`).
3. If no file → `ok=false`, detail `"cache file missing"`.
4. Read mtime. Age = `SystemTime::now() - mtime`. If age > 300s → `ok=false`, detail `"last update Xm ago"`. Else `ok=true`, detail `"updated Xs ago"`.

- [ ] **Step 4: `check_versions_match`**

Logic:
1. Locate daemon binary: first try `ctx.daemon_bin_override`; else try sibling to `std::env::current_exe()` (same dir, filename `claudehud-daemon` / `claudehud-daemon.exe`); else fall back to plain `"claudehud-daemon"` on PATH.
2. Run `<bin> --version`, capture stdout. Parse `"claudehud-daemon X.Y.Z"` → extract version string.
3. Compare against `env!("CARGO_PKG_VERSION")`. Match → `ok=true`, detail `"<version>"`. Mismatch → `ok=false`, detail `"client <A>, daemon <B>"`. Binary not found → `ok=false`, detail `"claudehud-daemon not found"`.

- [ ] **Step 5: `check_incidents_cache`**

Logic:
1. Get path from `common::incidents::incidents_path()` (or `ctx.cache_dir_override.join("clhud-incidents.bin")`).
2. If absent → `ok=true`, detail `"no cache (no incidents reported)"`.
3. Read mtime. If age > 900s → `ok=false`, detail `"stale (Xm ago)"`.
4. Else read and parse via `claudehud::incidents::read_incidents_from`. If `total_count > 0` → `ok=true`, detail `"N active incident(s)"`. Else → `ok=true`, detail `"no active incidents"`.

- [ ] **Step 6: `cargo check -p claudehud --locked` — must pass**

---

## Task 3: Wire into `run()` entry point

**Files:**
- Modify: `claudehud/src/doctor.rs`

- [ ] **Step 1: Write `pub fn run(mut args: pico_args::Arguments) -> std::process::ExitCode`**

Parse flags: `--json`, `--no-color`, `-h`/`--help`. Call `args.finish()` and reject unknown args.

Build `Ctx` from `std::env::current_dir()`.

Run all 5 checks. Build `Report`.

Print (json vs human, color vs plain). Return `ExitCode::SUCCESS` if `report.all_ok()`, else `ExitCode::from(1)`.

- [ ] **Step 2: Add HELP const at top of doctor.rs**

```
claudehud doctor

USAGE:
  claudehud doctor [OPTIONS]

OPTIONS:
  --json        Emit machine-readable JSON.
  --no-color    Suppress ANSI colors.
  -h, --help    Print this help.
```

---

## Task 4: Wire subcommand into main dispatch

**Files:**
- Modify: `claudehud/src/lib.rs`
- Modify: `claudehud/src/main.rs`

- [ ] **Step 1: Add `pub mod doctor;` to `claudehud/src/lib.rs`**

- [ ] **Step 2: Add `doctor` to the import line in `main.rs`**

Current: `use claudehud::{git, incidents, input, install, render, update};`
New: `use claudehud::{doctor, git, incidents, input, install, render, update};`

- [ ] **Step 3: Add dispatch arm in `main()`**

```rust
Some("doctor") => return doctor::run(args),
```

- [ ] **Step 4: Add to HELP string**

Add line:
```
  claudehud doctor [OPTIONS]     Run health checks on the daemon + cache.
                                 See `claudehud doctor -h`.
```

- [ ] **Step 5: `cargo build -p claudehud --locked` — must pass**

---

## Task 5: Write unit tests

**Files:**
- Modify: `claudehud/src/doctor.rs` (add `#[cfg(test)] mod tests` block)

All tests use `Ctx` with overridden `cache_dir_override` and `daemon_bin_override` to avoid touching real system state.

- [ ] **Test 1: `cache_fresh_missing_file` — `ok=false`, detail contains "missing"**

Create a temp dir (no mmap file). Provide as `cache_dir_override`. Use any non-git path for cwd (so git-root check skips to hash step — actually: use the real cwd which is a git repo, so we get the real hash, but point cache_dir at temp dir).

- [ ] **Test 2: `cache_fresh_recent_file` — `ok=true`**

Create a temp mmap file (138 bytes), set mtime to now. Override cache dir. Assert `ok=true`.

- [ ] **Test 3: `cache_fresh_stale_file` — `ok=false`, detail contains "ago"**

Create temp mmap file, set mtime to `now - 600s`. Assert `ok=false`.

- [ ] **Test 4: `cache_fresh_not_in_git_repo` — `ok=true`, detail "not in a git repo"**

Pass cwd = `/tmp` (no git root there). Assert `ok=true`, detail contains "not in a git repo".

- [ ] **Test 5: `versions_match_same` — `ok=true`**

Override daemon bin with a temp script / binary that prints `"claudehud-daemon <current_version>\n"`. On Unix: write a shell script, `chmod +x`. On Windows: skip or write a batch file. Use `daemon_bin_override`. Assert `ok=true`.

- [ ] **Test 6: `versions_match_mismatch` — `ok=false`**

Daemon bin prints `"claudehud-daemon 0.0.0-fake"`. Assert `ok=false`, detail contains "client" and "daemon".

- [ ] **Test 7: `versions_match_not_found` — `ok=false`, detail contains "not found"**

Pass a nonexistent path as `daemon_bin_override`. Assert `ok=false`.

- [ ] **Test 8: `incidents_cache_absent` — `ok=true`, detail "no cache"**

Temp dir, no incidents file. Assert `ok=true`, detail contains "no cache".

- [ ] **Test 9: `incidents_cache_fresh_no_incidents` — `ok=true`, detail "no active incidents"**

Write a valid zeroed INCIDENTS_MMAP_SIZE buffer. Assert `ok=true`.

- [ ] **Test 10: `print_human_color` — ANSI markers present**

Build a `Report` with one ok + one fail check. Call `print_human` with `use_color=true`, capture output (need a string buffer). Assert `✓` present, `✗` present.

- [ ] **Test 11: `print_human_no_color` — plain markers**

Same but `use_color=false`. Assert `[ok]` and `[FAIL]` present.

- [ ] **Test 12: `print_json_structure` — valid JSON, correct shape**

Parse output with `serde_json::from_str`. Assert `version == 1`, `ok == false` when any check fails.

- [ ] **Step: `cargo test -p claudehud --locked` — all tests pass**

---

## Task 6: Integration test

**Files:**
- Modify: `claudehud/src/doctor.rs` (or add `tests/doctor_integration.rs`)

- [ ] **Test: `doctor_runs_and_produces_output`**

Build the binary (`cargo build -p claudehud`). Run `claudehud doctor --json`. Parse output. Assert `checks` has 5 entries, each with `id`, `ok`, `detail`. Assert exit code is 0 or 1 (both valid depending on system state). This test is *structural*, not asserting pass/fail.

Alternatively (simpler — inline in doctor.rs tests): call `doctor::run` directly by extracting a `run_with_ctx(ctx, flags)` helper that returns `(Report, ExitCode)` so we can assert without spawning a subprocess.

Preferred: extract `run_inner(ctx, json, no_color) -> (Report, ExitCode)` private function; `run()` is a thin wrapper. The integration test calls `run_inner` with real `Ctx`.

- [ ] **Step: `cargo test -p claudehud -- doctor` — passes**

---

## Task 7: Verify quality gates

- [ ] **`cargo fmt --check`** — no formatting issues
- [ ] **`cargo clippy --workspace --all-targets -- -D warnings`** — zero warnings
- [ ] **`cargo test --workspace`** — all tests pass

Fix any issues before proceeding.

---

## Task 8: Commit and open PR

- [ ] **Stage changed files:** `git add claudehud/src/doctor.rs claudehud/src/lib.rs claudehud/src/main.rs docs/superpowers/specs/2026-05-12-claudehud-doctor.md docs/superpowers/plans/2026-05-12-claudehud-doctor.md`
- [ ] **Commit** with message `feat: claudehud doctor subcommand`
- [ ] **Push** `git push -u origin HEAD`
- [ ] **Open PR** with `gh pr create`
