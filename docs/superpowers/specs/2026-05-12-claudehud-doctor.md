# claudehud doctor — Design

**Status:** Approved
**Date:** 2026-05-12

## Problem

When `claudehud` is misconfigured or the daemon is not running, the statusline silently degrades — no branch name, no git dirty indicator, no incident banner. Users have no quick way to verify whether the tool is healthy or to diagnose why it isn't.

## Goal

Add `claudehud doctor`: a diagnostics subcommand that runs a fixed set of health checks, prints a human-readable checklist, and exits nonzero if any check fails. Machine-readable output is available via `--json`.

## Non-Goals

- Auto-remediation (e.g., restarting the daemon). Diagnose only; fix is left to the user.
- Exhaustive process introspection beyond "is the daemon alive and keeping the cache fresh".
- Checking network reachability of `status.claude.com`. That's the daemon's job; its absence is covered by cache freshness.
- Cross-user daemon inspection (only the current user's service is checked).
- Interactive mode (no prompts, no colors other than ANSI pass/fail markers).

## Output Format

Human mode (stdout is a TTY or `--color` flag used):

```
✓ daemon running (pid 4821)
✓ launchd service registered
✗ cache stale (last update 12m ago)
✓ versions match (0.4.2)
✓ no active incidents
```

Non-TTY (piped) or `--no-color`:

```
[ok] daemon running (pid 4821)
[ok] launchd service registered
[FAIL] cache stale (last update 12m ago)
[ok] versions match (0.4.2)
[ok] no active incidents
```

`--json`:

```json
{
  "version": 1,
  "ok": false,
  "checks": [
    { "id": "daemon_running",       "ok": true,  "detail": "pid 4821" },
    { "id": "service_registered",   "ok": true,  "detail": "launchd plist present" },
    { "id": "cache_fresh",          "ok": false, "detail": "last update 12m ago" },
    { "id": "versions_match",       "ok": true,  "detail": "0.4.2" },
    { "id": "incidents_cache",      "ok": true,  "detail": "no active incidents" }
  ]
}
```

Exit code: 0 if all checks pass, 1 if any fail.

## Checks (locked)

### 1. `daemon_running`

**Goal:** Is the daemon process alive?

Platform approaches (ordered by cheapness):

- **Cache freshness as proxy** (all platforms): if the cache file for cwd's git root was modified within the last 60 seconds, the daemon is almost certainly running and healthy. Return `ok` without shelling out. Rationale: a recently-written cache file cannot exist without a live daemon.
- **macOS fallback:** `launchctl list com.claudehud.daemon`. Parse the output: if the `PID` field is present and non-zero, daemon is running. If the service is registered but `PID` is absent, it crashed and launchd hasn't restarted it yet — `ok=false`, detail "service registered but not running".
- **Linux fallback:** `systemctl --user is-active claudehud-daemon`. Exit code 0 → running. Exit code 3 → inactive/dead.
- **Windows fallback:** query the `claudehud-daemon` process by name via `tasklist /FI "IMAGENAME eq claudehud-daemon.exe" /FO CSV /NH`. Non-empty CSV output (excluding header) → running.

Edge cases:
- **Cache file exists but is stale (>5 min), daemon appears "running" via launchctl/systemctl:** report daemon running but note cache stale separately in the `cache_fresh` check. Don't conflate.
- **No service registered, daemon launched manually:** cache freshness proxy still catches "running"; service-registered check is separate.
- **User installed via `cargo install` (no service):** daemon may be running as a manual process. Cache freshness catches "running". Service check reports "not registered" as a warning (ok=false).

### 2. `service_registered`

**Goal:** Is there a platform-appropriate service registration for autostart?

- **macOS:** plist exists at `~/Library/LaunchAgents/com.claudehud.daemon.plist`.
- **Linux:** unit file exists at `~/.config/systemd/user/claudehud-daemon.service`.
- **Windows:** query `schtasks /Query /TN claudehud-daemon /FO LIST` — exit 0 → registered.

Rationale: reuse the path constants already implicit in `install.rs` and the README. No need to actually parse file contents — existence (or schtasks query success) is sufficient.

**Soft failure:** This check reports `ok=false` but the detail string should be "not registered (daemon may still run manually)" to avoid alarming users who know what they're doing.

### 3. `cache_fresh`

**Goal:** Is the daemon keeping the cache current?

Logic:
1. Find the git root of `cwd` (reuse `common::find_git_root`). If no git root, skip — detail "not in a git repo".
2. Compute the mmap path (`common::mmap_path(hash_path(root))`).
3. If file absent: `ok=false`, detail "cache file missing".
4. If file present: read `mtime`. Age = `now - mtime`. If age > 5 min: `ok=false`, detail "last update Xm ago". Otherwise `ok=true`, detail "updated Xs ago".

Edge case: repo is clean and untouched for hours, so mtime is legitimately old. We can't distinguish this from a crashed daemon without actually reading the seqlock. Accept this as a false positive — the check is a heuristic, not a guarantee.

### 4. `versions_match`

**Goal:** Client and daemon are the same release.

Client version: `env!("CARGO_PKG_VERSION")` at compile time.

Daemon version: shell out to `claudehud-daemon --version`. The daemon already handles `--version` (see `claudehud-daemon/src/main.rs:29`). Locate the daemon binary by:
1. `which claudehud-daemon` / `where claudehud-daemon` equivalent: try `std::env::var("PATH")` lookup. Simplest: just invoke it as `claudehud-daemon --version` and let the shell resolve it.
2. If the daemon binary isn't on PATH, skip — detail "claudehud-daemon not found on PATH".

Parse output: `claudehud-daemon X.Y.Z` — strip the prefix and compare.

Edge case: **user installed via install script, daemon is in `~/.local/bin/claudehud-daemon`** but shell isn't sourced. `claudehud --version` from the same install dir would work, but `claudehud-daemon --version` might not find it. Resolution: also try `exe_dir().join("claudehud-daemon")` (sibling to the running `claudehud` binary) as a fallback before giving up.

### 5. `incidents_cache`

**Goal:** Is the incidents cache present and reasonably fresh?

Logic:
1. Get `incidents_path()` from `common::incidents`.
2. If absent: `ok=true`, detail "no cache (no incidents reported)". Absent = daemon hasn't seen any incidents, which is fine.
3. If present: read mtime. If age > 15 min: `ok=false`, detail "incidents cache stale (Xm ago)". Otherwise read file and check `total_count`. If `total_count > 0`: `ok=true`, detail "N active incident(s)". If `total_count == 0`: `ok=true`, detail "no active incidents".

Rationale: 15 min threshold because the daemon polls every 5 min; 15 min allows for 2 missed polls before alarming.

## Architecture

### New files

- `claudehud/src/doctor.rs` — all check logic + rendering + JSON serialization.

### Modified files

- `claudehud/src/lib.rs` — add `pub mod doctor`.
- `claudehud/src/main.rs` — dispatch `"doctor"` subcommand; add to HELP string.

### No new external dependencies

All five checks use `std::fs`, `std::process::Command`, and existing workspace crates (`common`). JSON output uses `serde_json` which is already in the dependency tree (via `claudehud`'s `Cargo.toml`).

## Data Structures

```rust
pub struct CheckResult {
    pub id: &'static str,
    pub ok: bool,
    pub detail: String,
}

pub struct Report {
    pub checks: Vec<CheckResult>,
}
```

`Report::all_ok()` — `checks.iter().all(|c| c.ok)`.

## TTY / Color Detection

Use `std::io::IsTerminal` (already imported in `main.rs`) on stdout. `--json` implies no ANSI. `--no-color` forces plain text. Default: ANSI when stdout is a TTY.

## Flags

```
claudehud doctor [OPTIONS]

OPTIONS:
  --json        Emit machine-readable JSON to stdout.
  --no-color    Plain text (suppress ANSI even on TTY).
  -h, --help    Print this help.
```

No `--verbose` or `--quiet` for v1. Detail strings are always emitted.

## Locked Decisions

| Decision | Choice |
|----------|--------|
| Daemon liveness primary signal | Cache freshness (no shell out required for happy path) |
| Daemon liveness fallback | Platform service query (launchctl / systemctl / tasklist) |
| Version comparison | Shell out to `claudehud-daemon --version` |
| Missing incidents cache | `ok=true` (absence = no incidents, not a failure) |
| Service-not-registered severity | `ok=false` with "may still run manually" note |
| Exit code | 0 = all pass, 1 = any fail |
| Stale cache threshold | >5 min |
| Stale incidents threshold | >15 min |
| JSON schema version | `"version": 1` |

## Compatibility

No existing code is modified beyond `lib.rs` (module declaration) and `main.rs` (dispatch + HELP string). The hot render path is untouched.
