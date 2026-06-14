# Daemon autoupdate — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The `claudehud-daemon` periodically checks GitHub for a newer release, natively downloads + sha256-verifies + swaps both binaries in place, then restarts itself; the client shows a short-lived "updated to vX" notice.

**Architecture:** A new poll thread in the daemon (sibling to `status::start()`) does a conditional GET against the GitHub releases API, compares versions, and on a newer release downloads/verifies/swaps via `ureq` + `sha2`, then `exit(0)`s so launchd/systemd relaunch the new binary. Pure logic (semver compare, config parse, notice format) lives in `common` and is unit-tested without IO. The client reads a small notice file each render.

**Tech Stack:** Rust (Cargo workspace), `ureq` (HTTP, already a daemon dep), `sha2` (new daemon dep), `serde_json` (new common dep, already a client dep). No new runtime tools.

Spec: `docs/superpowers/specs/2026-06-13-daemon-autoupdate-design.md`.

---

## File Structure

**Created:**
- `common/src/version.rs` — `parse_semver`, `compare`, `VersionState`, `parse_tag` (moved from client).
- `common/src/config.rs` — `Config { autoupdate, pin }` + `parse` + `config_path()`.
- `common/src/notice.rs` — `Notice { version, show_until }` + `parse_notice`/`format_notice` + `update_notice_path()`.
- `claudehud/src/notice.rs` — client-side reader: returns the active notice version (or `None`).
- `claudehud-daemon/src/update.rs` — poll thread + `perform_update` + swap.

**Modified:**
- `common/Cargo.toml` — add `serde_json`.
- `common/src/lib.rs` — `pub mod version; pub mod config; pub mod notice;`
- `claudehud/Cargo.toml` — (no change; `serde_json` already present).
- `claudehud/src/update.rs` — use `common::version::*`; drop the moved fns + their tests.
- `claudehud/src/lib.rs` — `pub mod notice;`
- `claudehud/src/render.rs` — `render()` + both layout fns gain an `update_notice: Option<&str>` param; new `push_update_notice`.
- `claudehud/src/main.rs` — read the notice, thread it into `render::render`.
- `claudehud-daemon/Cargo.toml` — add `sha2`.
- `claudehud-daemon/src/main.rs` — spawn `update::start()`.
- `install.sh` — write the config file; harden the systemd unit.
- `README.md` — document autoupdate + the new env var/config file.

---

## Task 1: Move version logic into `common::version`

**Files:**
- Create: `common/src/version.rs`
- Modify: `common/Cargo.toml`, `common/src/lib.rs`, `claudehud/src/update.rs`

- [ ] **Step 1: Add `serde_json` to common**

In `common/Cargo.toml`, add a `[dependencies]` section (the file currently has none):

```toml
[dependencies]
serde_json = "1"
```

- [ ] **Step 2: Create `common/src/version.rs` with the moved code + tests**

This is the verbatim logic from `claudehud/src/update.rs` (the `compare`/`parse_semver`/`parse_tag`/`VersionState` items and their tests), now public:

```rust
//! Shared version comparison + GitHub release-tag parsing.
//! Used by the client `update` subcommand and the daemon autoupdater.

use std::io;

#[derive(Debug, PartialEq, Eq)]
pub enum VersionState {
    UpToDate,
    Newer(String),
    Ahead(String),
}

/// Parse the `tag_name` field out of a GitHub release JSON body.
pub fn parse_tag(body: &[u8]) -> io::Result<String> {
    let v: serde_json::Value = serde_json::from_slice(body).map_err(|e| {
        io::Error::new(io::ErrorKind::InvalidData, format!("bad JSON from GitHub: {e}"))
    })?;
    v.get("tag_name")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "GitHub response had no tag_name")
        })
}

/// Compare an installed version (`0.1.0`) against a release tag (`v0.2.0`).
pub fn compare(installed: &str, tag: &str) -> VersionState {
    let latest = tag.trim_start_matches('v').to_string();
    let installed_parts = parse_semver(installed);
    let latest_parts = parse_semver(&latest);
    match (installed_parts, latest_parts) {
        (Some(i), Some(l)) if i == l => VersionState::UpToDate,
        (Some(i), Some(l)) if i < l => VersionState::Newer(latest),
        (Some(_), Some(_)) => VersionState::Ahead(latest),
        _ if installed == latest => VersionState::UpToDate,
        _ => VersionState::Newer(latest),
    }
}

/// Parse `MAJOR.MINOR.PATCH[-pre]` into a comparable tuple. Pre-release suffixes
/// sort *before* the bare release.
pub fn parse_semver(s: &str) -> Option<(u64, u64, u64, Option<String>)> {
    let (core, pre) = match s.split_once('-') {
        Some((c, p)) => (c, Some(p.to_string())),
        None => (s, None),
    };
    let mut it = core.split('.');
    let major: u64 = it.next()?.parse().ok()?;
    let minor: u64 = it.next()?.parse().ok()?;
    let patch: u64 = it.next()?.parse().ok()?;
    if it.next().is_some() {
        return None;
    }
    let pre_key = pre.unwrap_or_else(|| "~".to_string());
    Some((major, minor, patch, Some(pre_key)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_tag_extracts_tag_name() {
        let body = br#"{"tag_name":"v0.1.0","name":"Release 0.1.0"}"#;
        assert_eq!(parse_tag(body).unwrap(), "v0.1.0");
    }

    #[test]
    fn parse_tag_errors_on_missing_field() {
        assert!(parse_tag(br#"{"name":"x"}"#).is_err());
    }

    #[test]
    fn parse_tag_errors_on_bad_json() {
        assert!(parse_tag(b"not json at all").is_err());
    }

    #[test]
    fn compare_equal_versions() {
        assert_eq!(compare("0.1.0", "v0.1.0"), VersionState::UpToDate);
        assert_eq!(compare("0.1.0", "0.1.0"), VersionState::UpToDate);
    }

    #[test]
    fn compare_installed_older() {
        assert_eq!(compare("0.1.0", "v0.2.0"), VersionState::Newer("0.2.0".into()));
        assert_eq!(compare("0.1.9", "v0.1.10"), VersionState::Newer("0.1.10".into()));
    }

    #[test]
    fn compare_installed_ahead() {
        assert_eq!(compare("0.2.0", "v0.1.0"), VersionState::Ahead("0.1.0".into()));
    }

    #[test]
    fn compare_prerelease_is_less_than_release() {
        assert_eq!(compare("0.1.0-alpha.4", "v0.1.0"), VersionState::Newer("0.1.0".into()));
        assert_eq!(compare("0.1.0", "v0.1.0-alpha.4"), VersionState::Ahead("0.1.0-alpha.4".into()));
    }

    #[test]
    fn compare_unparseable_falls_back_to_string_eq() {
        assert_eq!(compare("weird", "weird"), VersionState::UpToDate);
        assert_eq!(compare("weird", "other"), VersionState::Newer("other".into()));
    }
}
```

- [ ] **Step 3: Register the module**

In `common/src/lib.rs`, below `pub mod incidents;` add:

```rust
pub mod version;
```

- [ ] **Step 4: Point the client at the shared code**

In `claudehud/src/update.rs`:
1. Delete the local `VersionState` enum, `compare`, `parse_semver`, `parse_tag` fns **and** the `tests` module entries for `parse_tag_*`, `compare_*` (they now live in common).
2. Add `use common::version::{compare, VersionState};` near the top. (Do **not** import `parse_tag` — `latest_tag` calls it fully-qualified below, and an unused import trips `-D warnings`.)
3. `latest_tag` keeps its curl/wget `fetch`, but its body becomes:

```rust
fn latest_tag() -> io::Result<String> {
    let body = fetch(RELEASES_API)?;
    common::version::parse_tag(&body)
}
```

(Leave `fetch`, `run_check`, `run_install_sh`, `RELEASES_API`, `INSTALL_URL` untouched.)

- [ ] **Step 5: Verify the workspace builds + tests pass**

Run: `cargo test -p common version`
Expected: the 8 `version::tests` pass.
Run: `cargo build -p claudehud`
Expected: builds clean (no unused-import / missing-symbol errors).

- [ ] **Step 6: Commit**

```bash
git add common/Cargo.toml common/src/version.rs common/src/lib.rs claudehud/src/update.rs
git commit -m "refactor(common): extract version compare/parse into common::version"
```

---

## Task 2: `common::config`

**Files:**
- Create: `common/src/config.rs`
- Modify: `common/src/lib.rs`

- [ ] **Step 1: Write the failing tests**

Create `common/src/config.rs`:

```rust
//! Daemon-readable config: `${XDG_CONFIG_HOME:-$HOME/.config}/claudehud/config`.
//! Dead-simple `key=value`, one per line. `#` comments + blank lines ignored.
//! Absent file → defaults (autoupdate on, no pin).

use std::path::PathBuf;

#[derive(Debug, PartialEq, Eq)]
pub struct Config {
    pub autoupdate: bool,
    pub pin: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config { autoupdate: true, pin: None }
    }
}

/// Resolve the config file path. Honors `XDG_CONFIG_HOME`, else `$HOME/.config`.
/// Windows path is a seam (unimplemented in v1) — returns an empty path there so
/// callers treat it as "absent → defaults".
pub fn config_path() -> PathBuf {
    #[cfg(unix)]
    {
        if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
            return PathBuf::from(xdg).join("claudehud").join("config");
        }
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(".config").join("claudehud").join("config");
        }
        PathBuf::new()
    }
    #[cfg(not(unix))]
    {
        PathBuf::new()
    }
}

/// Read + parse the config file, falling back to defaults on any error.
pub fn load() -> Config {
    let path = config_path();
    match std::fs::read_to_string(&path) {
        Ok(s) => parse(&s),
        Err(_) => Config::default(),
    }
}

/// Parse config text. Unknown keys ignored. Missing keys keep their defaults.
pub fn parse(text: &str) -> Config {
    let mut cfg = Config::default();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else { continue };
        let (k, v) = (k.trim(), v.trim());
        match k {
            "autoupdate" => cfg.autoupdate = !matches!(v, "false" | "0" | "no" | "off"),
            "pin" if !v.is_empty() => cfg.pin = Some(v.to_string()),
            _ => {}
        }
    }
    cfg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_text_is_default() {
        assert_eq!(parse(""), Config::default());
    }

    #[test]
    fn autoupdate_false_disables() {
        assert!(!parse("autoupdate=false").autoupdate);
        assert!(!parse("autoupdate = off").autoupdate);
        assert!(parse("autoupdate=true").autoupdate);
    }

    #[test]
    fn pin_is_captured() {
        assert_eq!(parse("pin=v0.2.0").pin, Some("v0.2.0".to_string()));
        assert_eq!(parse("pin=").pin, None);
    }

    #[test]
    fn comments_and_blanks_ignored() {
        let text = "# a comment\n\n  autoupdate=false  \npin=v1.0.0\n";
        let c = parse(text);
        assert!(!c.autoupdate);
        assert_eq!(c.pin, Some("v1.0.0".to_string()));
    }

    #[test]
    fn unknown_keys_ignored() {
        let c = parse("wat=1\nautoupdate=false");
        assert!(!c.autoupdate);
    }
}
```

- [ ] **Step 2: Register the module + run tests to confirm they pass**

In `common/src/lib.rs` add `pub mod config;`.
Run: `cargo test -p common config`
Expected: 5 `config::tests` pass.

- [ ] **Step 3: Commit**

```bash
git add common/src/config.rs common/src/lib.rs
git commit -m "feat(common): add daemon config file parser"
```

---

## Task 3: `common::notice`

**Files:**
- Create: `common/src/notice.rs`
- Modify: `common/src/lib.rs`

- [ ] **Step 1: Write the failing tests**

Create `common/src/notice.rs`:

```rust
//! The one-shot "updated to vX" notice shared between daemon (writer) and
//! client (reader). Plain file: line 1 = version, line 2 = show-until epoch.

use std::path::PathBuf;

use crate::cache_dir;

#[derive(Debug, PartialEq, Eq)]
pub struct Notice {
    pub version: String,
    pub show_until: u64,
}

/// Path to the notice file in the cache dir.
pub fn update_notice_path() -> PathBuf {
    cache_dir().join("clhud-update-notice")
}

/// Serialize to file contents (`version\nshow_until\n`).
pub fn format_notice(n: &Notice) -> String {
    format!("{}\n{}\n", n.version, n.show_until)
}

/// Parse file contents. `None` on malformed input.
pub fn parse_notice(text: &str) -> Option<Notice> {
    let mut lines = text.lines();
    let version = lines.next()?.trim().to_string();
    if version.is_empty() {
        return None;
    }
    let show_until = lines.next()?.trim().parse::<u64>().ok()?;
    Some(Notice { version, show_until })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let n = Notice { version: "0.2.0".into(), show_until: 1_700_000_300 };
        assert_eq!(parse_notice(&format_notice(&n)), Some(n));
    }

    #[test]
    fn garbled_is_none() {
        assert_eq!(parse_notice(""), None);
        assert_eq!(parse_notice("0.2.0"), None); // missing epoch line
        assert_eq!(parse_notice("0.2.0\nnotnum"), None);
        assert_eq!(parse_notice("\n123"), None); // empty version
    }
}
```

- [ ] **Step 2: Register the module + run tests**

In `common/src/lib.rs` add `pub mod notice;`.
Run: `cargo test -p common notice`
Expected: 2 `notice::tests` pass.

- [ ] **Step 3: Commit**

```bash
git add common/src/notice.rs common/src/lib.rs
git commit -m "feat(common): add update-notice format shared by daemon + client"
```

---

## Task 4: Client renders the notice

**Files:**
- Create: `claudehud/src/notice.rs`
- Modify: `claudehud/src/lib.rs`, `claudehud/src/render.rs`, `claudehud/src/main.rs`

- [ ] **Step 1: Write the failing test for the client reader**

Create `claudehud/src/notice.rs`:

```rust
//! Client-side reader for the one-shot update notice. Degrades silently like
//! the git cache: any error → no notice.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use common::notice::{parse_notice, update_notice_path};

/// Returns the version string to advertise, or `None` if there's no active
/// notice. Best-effort removes the file once it has expired.
pub fn active_notice() -> Option<String> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    active_notice_at(&update_notice_path(), now)
}

/// Test seam: evaluate against an explicit path + clock.
pub fn active_notice_at(path: &Path, now: u64) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let notice = parse_notice(&text)?;
    if now < notice.show_until {
        Some(notice.version)
    } else {
        let _ = std::fs::remove_file(path);
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::notice::{format_notice, Notice};

    fn tmp(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("clhud-notice-{name}-{}", std::process::id()))
    }

    #[test]
    fn active_when_before_deadline() {
        let p = tmp("active");
        std::fs::write(&p, format_notice(&Notice { version: "0.2.0".into(), show_until: 1000 })).unwrap();
        assert_eq!(active_notice_at(&p, 500), Some("0.2.0".to_string()));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn expired_returns_none_and_removes() {
        let p = tmp("expired");
        std::fs::write(&p, format_notice(&Notice { version: "0.2.0".into(), show_until: 1000 })).unwrap();
        assert_eq!(active_notice_at(&p, 1000), None);
        assert!(!p.exists(), "expired notice file should be removed");
    }

    #[test]
    fn missing_file_is_none() {
        assert_eq!(active_notice_at(&tmp("missing"), 0), None);
    }
}
```

- [ ] **Step 2: Register module + run reader tests**

In `claudehud/src/lib.rs` add `pub mod notice;`.
Run: `cargo test -p claudehud notice`
Expected: 3 `notice::tests` pass.

- [ ] **Step 3: Write the failing render test**

In `claudehud/src/render.rs` `tests` module, add:

```rust
#[test]
fn test_render_update_notice_present() {
    let input = Input::default();
    let out = render(&input, None, &[], 0, Some("0.2.0"), RoundingMode::Floor, Layout::Comfortable);
    let plain = strip_ansi(&out);
    assert!(plain.contains("updated to v0.2.0"), "got: {plain:?}");
}

#[test]
fn test_render_no_update_notice_absent() {
    let input = Input::default();
    let out = render(&input, None, &[], 0, None, RoundingMode::Floor, Layout::Comfortable);
    assert!(!strip_ansi(&out).contains("updated to"));
}
```

> Note: use the same ANSI-stripping helper the existing incident tests use (search the `tests` module for how `plain` is produced, e.g. `strip_ansi`, and match it). If existing tests inline the stripping, inline it the same way here.

- [ ] **Step 4: Run the render test to verify it fails**

Run: `cargo test -p claudehud test_render_update_notice_present`
Expected: FAIL — `render` takes 6 args, not 7 (arity mismatch / does not compile).

- [ ] **Step 5: Thread the param through `render` + both layouts**

In `claudehud/src/render.rs`:

1. `render` signature + dispatch:

```rust
pub fn render(
    input: &Input,
    git: Option<(String, bool)>,
    incidents: &[Incident],
    total_active: u8,
    update_notice: Option<&str>,
    rounding: RoundingMode,
    layout: Layout,
) -> String {
    match layout {
        Layout::Comfortable => render_comfortable(input, git, incidents, total_active, update_notice, rounding),
        Layout::Condensed => render_condensed(input, git, incidents, total_active, update_notice, rounding),
    }
}
```

2. Add `update_notice: Option<&str>` to `render_comfortable` and `render_condensed` signatures (place it right after `total_active`, matching `render`'s order).

3. In **`render_comfortable`**, immediately after the `push_incidents(...)` call:

```rust
    push_update_notice(update_notice, &mut out);
```

4. In **`render_condensed`**, immediately after its `push_incidents(...)` call, add the same line.

5. Add the helper (near `push_incident_line`):

```rust
/// One-shot "updated to vX" line, shown under line 1 for a few minutes after a
/// daemon self-update. Its own line, like an incident.
fn push_update_notice(version: Option<&str>, out: &mut String) {
    let Some(v) = version else { return };
    out.push('\n');
    out.push_str(DIM);
    out.push_str("updated to v");
    out.push_str(v);
    out.push_str(RESET);
}
```

(`DIM`/`RESET` are already imported via `use crate::fmt::*;`.)

- [ ] **Step 6: Fix the other `render::render` callers**

The only production caller is `claudehud/src/main.rs` (Task 4 Step 8). Within `render.rs` tests, update every existing `render(...)` / direct `render_comfortable`/`render_condensed` call site to pass the new arg (`None` for notice, unless the test is the one asserting the notice). Search the tests module for `render(` and `render_comfortable(`/`render_condensed(` and insert the arg in the correct position.

- [ ] **Step 7: Run render tests**

Run: `cargo test -p claudehud render`
Expected: the two new tests pass; all pre-existing render tests still pass.

- [ ] **Step 8: Wire the reader into `main.rs`**

In `claudehud/src/main.rs`:
1. Add `notice` to the import: `use claudehud::{git, incidents, input, install, notice, render, update};`
2. In `render(...)`, after the `let (incidents, total_active) = incidents::read_incidents();` line, add:

```rust
    let update_notice = notice::active_notice();
```

3. Update the `render::render(...)` call to pass it:

```rust
    print!(
        "{}",
        render::render(&input, git, &incidents, total_active, update_notice.as_deref(), rounding, layout)
    );
```

- [ ] **Step 9: Build + full client test run**

Run: `cargo test -p claudehud`
Expected: all pass, including the new notice + render tests.

- [ ] **Step 10: Commit**

```bash
git add claudehud/src/notice.rs claudehud/src/lib.rs claudehud/src/render.rs claudehud/src/main.rs
git commit -m "feat(client): render one-shot update notice under line 1"
```

---

## Task 5: Daemon update module — version check + decision (no swap yet)

**Files:**
- Create: `claudehud-daemon/src/update.rs`
- Modify: `claudehud-daemon/Cargo.toml`, `claudehud-daemon/src/main.rs`

- [ ] **Step 1: Add `sha2` to the daemon (used in Task 6; add now to avoid a second Cargo edit)**

In `claudehud-daemon/Cargo.toml` `[dependencies]`, add:

```toml
sha2 = "0.10"
```

- [ ] **Step 2: Write the failing tests for the pure helpers**

Create `claudehud-daemon/src/update.rs` with the pure decision + target-resolution logic and tests:

```rust
//! Daemon self-update: poll GitHub releases, then download/verify/swap binaries.
//! Pure helpers (target triple, update decision) are unit-tested; the IO-heavy
//! `perform_update` is integration-tested for its swap step.

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::version::{compare, VersionState};

const RELEASES_API: &str = "https://api.github.com/repos/fyko/claudehud/releases/latest";
const USER_AGENT: &str = concat!("claudehud-daemon/", env!("CARGO_PKG_VERSION"));
const FIRST_DELAY: Duration = Duration::from_secs(60);
const POLL_INTERVAL: Duration = Duration::from_secs(300);

/// The release-asset target triple for the current platform. `None` on
/// unsupported / not-yet-implemented platforms (e.g. Windows in v1).
pub fn target_triple() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Some("aarch64-apple-darwin"),
        ("macos", "x86_64") => Some("x86_64-apple-darwin"),
        ("linux", "x86_64") => Some("x86_64-unknown-linux-musl"),
        ("linux", "aarch64") => Some("aarch64-unknown-linux-musl"),
        _ => None,
    }
}

/// Decide which tag (if any) to update to.
/// - `pin` present → target the pinned tag directly (cap behavior: a pinned
///   install that's already at the pin won't move; one pinned ahead moves once).
/// - `pin` absent → target `latest`.
/// Returns `Some(tag)` only when the target is strictly newer than `installed`.
pub fn decide_target(installed: &str, latest: &str, pin: Option<&str>) -> Option<String> {
    let target = pin.unwrap_or(latest);
    match compare(installed, target) {
        VersionState::Newer(_) => Some(target.to_string()),
        _ => None,
    }
}

/// True if `exe` looks like a real install (not a `cargo` build dir). Guards
/// against a dev binary self-updating.
pub fn is_installed_path(exe: &Path) -> bool {
    !exe.components().any(|c| c.as_os_str() == "target")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decide_unpinned_newer() {
        assert_eq!(decide_target("0.1.0", "v0.2.0", None), Some("v0.2.0".into()));
    }

    #[test]
    fn decide_unpinned_uptodate() {
        assert_eq!(decide_target("0.2.0", "v0.2.0", None), None);
    }

    #[test]
    fn decide_pinned_targets_pin_not_latest() {
        // pinned to 0.2.0 while latest is 0.3.0: move only to the pin.
        assert_eq!(decide_target("0.1.0", "v0.3.0", Some("v0.2.0")), Some("v0.2.0".into()));
    }

    #[test]
    fn decide_pinned_already_at_pin_is_noop() {
        assert_eq!(decide_target("0.2.0", "v0.3.0", Some("v0.2.0")), None);
    }

    #[test]
    fn installed_path_rejects_target_dir() {
        assert!(!is_installed_path(Path::new("/home/u/claudehud/target/release/claudehud-daemon")));
        assert!(is_installed_path(Path::new("/home/u/.local/bin/claudehud-daemon")));
    }
}
```

- [ ] **Step 3: Run the pure tests**

Run: `cargo test -p claudehud-daemon update::tests`
Expected: 5 tests pass. (The module isn't wired into `main` yet — add `mod update;` next so it compiles into the test binary.)

If the test run reports `update` is not part of the crate, add `mod update;` to `claudehud-daemon/src/main.rs` (alongside `mod status;`) first, then re-run.

- [ ] **Step 4: Add the poll loop (network, no swap)**

Append to `claudehud-daemon/src/update.rs`:

```rust
/// Entry point for the autoupdate thread. Returns (thread exits) when
/// autoupdate is disabled, on a dev build, or for an unsupported platform.
pub fn start() {
    // Never self-update a debug/dev build.
    if cfg!(debug_assertions) {
        return;
    }
    let cfg = common::config::load();
    if !cfg.autoupdate {
        return;
    }
    let Some(target) = target_triple() else { return };
    let Ok(exe) = std::env::current_exe() else { return };
    if !is_installed_path(&exe) {
        return;
    }
    let install_dir = match exe.parent() {
        Some(d) => d.to_path_buf(),
        None => return,
    };

    let agent = ureq::AgentBuilder::new()
        .user_agent(USER_AGENT)
        .timeout_connect(Duration::from_secs(5))
        .timeout_read(Duration::from_secs(15))
        .build();

    let mut etag: Option<String> = None;
    std::thread::sleep(FIRST_DELAY);
    loop {
        match fetch_latest_tag(&agent, etag.as_deref()) {
            Ok(Some((tag, new_etag))) => {
                etag = new_etag;
                let installed = env!("CARGO_PKG_VERSION");
                if let Some(target_tag) = decide_target(installed, &tag, cfg.pin.as_deref()) {
                    if let Err(e) = perform_update(&agent, &install_dir, target, &target_tag) {
                        eprintln!("WARN autoupdate: {e}");
                    }
                    // perform_update exits the process on success; if we're still
                    // here it failed — keep polling.
                }
            }
            Ok(None) => {} // 304 Not Modified
            Err(e) => eprintln!("WARN autoupdate fetch: {e}"),
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// Conditional GET of the latest release. `Ok(None)` on 304.
fn fetch_latest_tag(
    agent: &ureq::Agent,
    etag: Option<&str>,
) -> Result<Option<(String, Option<String>)>, String> {
    let mut req = agent.get(RELEASES_API);
    if let Some(tag) = etag {
        req = req.set("If-None-Match", tag);
    }
    match req.call() {
        Ok(resp) => {
            let new_etag = resp.header("ETag").map(str::to_string);
            let body = resp.into_string().map_err(|e| e.to_string())?;
            let tag = common::version::parse_tag(body.as_bytes()).map_err(|e| e.to_string())?;
            Ok(Some((tag, new_etag)))
        }
        Err(ureq::Error::Status(304, _)) => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}
```

Note: `perform_update` is implemented in Task 6. To keep this task compiling on its own, add a temporary stub at the bottom of the file:

```rust
fn perform_update(
    _agent: &ureq::Agent,
    _install_dir: &Path,
    _target: &str,
    _tag: &str,
) -> Result<(), String> {
    Err("perform_update not yet implemented".into())
}
```

- [ ] **Step 5: Build the daemon**

Run: `cargo build -p claudehud-daemon`
Expected: builds clean (warnings about the unused `PathBuf` import are fine to resolve now or in Task 6).

- [ ] **Step 6: Commit**

```bash
git add claudehud-daemon/Cargo.toml claudehud-daemon/src/update.rs claudehud-daemon/src/main.rs
git commit -m "feat(daemon): autoupdate poll loop + update-decision logic"
```

---

## Task 6: `perform_update` — download, verify, swap, notice, restart

**Files:**
- Modify: `claudehud-daemon/src/update.rs`

- [ ] **Step 1: Write the failing integration test for the swap step**

Factor the swap so it's testable without network. Add to the `tests` module in `claudehud-daemon/src/update.rs`:

```rust
#[test]
fn swap_binary_replaces_in_place() {
    let dir = std::env::temp_dir().join(format!("clhud-swap-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let dest = dir.join("claudehud-daemon");
    std::fs::write(&dest, b"OLD").unwrap();

    swap_binary(&dir, "claudehud-daemon", b"NEW").unwrap();

    assert_eq!(std::fs::read(&dest).unwrap(), b"NEW");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn verify_sha256_matches_known_vector() {
    // sha256("abc")
    let expected = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    assert!(verify_sha256(b"abc", expected));
    assert!(!verify_sha256(b"abc", "deadbeef"));
}
```

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p claudehud-daemon swap_binary_replaces_in_place`
Expected: FAIL — `swap_binary` / `verify_sha256` not defined.

- [ ] **Step 3: Implement the swap + verify helpers + real `perform_update`**

In `claudehud-daemon/src/update.rs`, **replace the temporary `perform_update` stub** with:

```rust
use sha2::{Digest, Sha256};

const BASE_URL: &str = "https://github.com/fyko/claudehud/releases/download";
const BINARIES: [&str; 2] = ["claudehud", "claudehud-daemon"];

/// Download both binaries + sidecars, verify, swap in place, write the notice,
/// then exit so the service manager relaunches the new daemon. On any failure,
/// returns `Err` having mutated nothing on disk (verify-before-swap).
fn perform_update(
    agent: &ureq::Agent,
    install_dir: &Path,
    target: &str,
    tag: &str,
) -> Result<(), String> {
    // 1. download + verify BOTH before swapping EITHER.
    let mut payloads: Vec<(&str, Vec<u8>)> = Vec::with_capacity(2);
    for bin in BINARIES {
        let bin_url = format!("{BASE_URL}/{tag}/{bin}-{target}");
        let bytes = download(agent, &bin_url)?;
        let sidecar = download_text(agent, &format!("{bin_url}.sha256"))?;
        let expected = sidecar.split_whitespace().next().unwrap_or("");
        if !verify_sha256(&bytes, expected) {
            return Err(format!("checksum mismatch for {bin}-{target}"));
        }
        payloads.push((bin, bytes));
    }

    // 2. swap both into place.
    for (bin, bytes) in &payloads {
        swap_binary(install_dir, bin, bytes).map_err(|e| format!("swap {bin}: {e}"))?;
    }

    // 3. write the one-shot notice (now + 5 min).
    write_notice(tag);

    // 4. restart: exit so launchd/systemd relaunch the new binary.
    eprintln!("==> autoupdate: installed {tag}, restarting");
    std::process::exit(0);
}

fn download(agent: &ureq::Agent, url: &str) -> Result<Vec<u8>, String> {
    let resp = agent.get(url).call().map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    std::io::copy(&mut resp.into_reader(), &mut buf).map_err(|e| e.to_string())?;
    Ok(buf)
}

fn download_text(agent: &ureq::Agent, url: &str) -> Result<String, String> {
    agent.get(url).call().map_err(|e| e.to_string())?.into_string().map_err(|e| e.to_string())
}

fn verify_sha256(bytes: &[u8], expected_hex: &str) -> bool {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let actual: String = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
    !expected_hex.is_empty() && actual.eq_ignore_ascii_case(expected_hex)
}

/// Atomically replace `install_dir/name` with `bytes`. Writes a sibling temp in
/// the SAME directory (guarantees a same-filesystem, atomic `rename`), sets the
/// exec bit on unix, then renames over the target. Replacing a running binary's
/// path is safe on unix — the live process keeps its open inode.
fn swap_binary(install_dir: &Path, name: &str, bytes: &[u8]) -> std::io::Result<()> {
    let dest = install_dir.join(name);
    let tmp = install_dir.join(format!("{name}.new"));
    std::fs::write(&tmp, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
    }
    std::fs::rename(&tmp, &dest)
}

fn write_notice(tag: &str) {
    use std::time::{SystemTime, UNIX_EPOCH};
    let Ok(now) = SystemTime::now().duration_since(UNIX_EPOCH) else { return };
    let notice = common::notice::Notice {
        version: tag.trim_start_matches('v').to_string(),
        show_until: now.as_secs() + 300,
    };
    let _ = std::fs::write(
        common::notice::update_notice_path(),
        common::notice::format_notice(&notice),
    );
}
```

Remove the now-unused `PathBuf` import if the compiler flags it (`Path` is still used).

- [ ] **Step 4: Run the swap + verify tests**

Run: `cargo test -p claudehud-daemon`
Expected: `swap_binary_replaces_in_place` + `verify_sha256_matches_known_vector` pass; the Task 5 pure tests still pass.

- [ ] **Step 5: Commit**

```bash
git add claudehud-daemon/src/update.rs
git commit -m "feat(daemon): download/verify/swap binaries + write update notice"
```

---

## Task 7: Spawn the autoupdate thread

**Files:**
- Modify: `claudehud-daemon/src/main.rs`

- [ ] **Step 1: Spawn `update::start()` alongside the other threads**

In `claudehud-daemon/src/main.rs`:
1. Ensure `mod update;` is declared with the other `mod` lines (added in Task 5 Step 3).
2. After the existing `std::thread::spawn(|| { status::start(); });` block, add:

```rust
    std::thread::spawn(|| {
        update::start();
    });
```

- [ ] **Step 2: Build**

Run: `cargo build -p claudehud-daemon`
Expected: clean build.

- [ ] **Step 3: Full workspace check**

Run: `cargo test`
Expected: all crates' tests pass.
Run: `cargo clippy --workspace -- -D warnings` (if clippy is configured for the repo)
Expected: no new warnings from the added modules. (Pre-existing workspace clippy drift is out of scope — see memory.)

- [ ] **Step 4: Commit**

```bash
git add claudehud-daemon/src/main.rs
git commit -m "feat(daemon): start autoupdate thread"
```

---

## Task 8: Installer config + hardened systemd unit + docs

**Files:**
- Modify: `install.sh`, `README.md`

- [ ] **Step 1: Persist config from install-time env vars**

In `install.sh`, add a `write_config` function (place it near `configure_claude`):

```sh
# ---------------------------------------------------------------------------
# write daemon config (autoupdate opt-out + version pin)
# ---------------------------------------------------------------------------
# The daemon runs under launchd/systemd and never sees the installing shell's
# env. Persist the relevant knobs to a file it reads on startup.
write_config() {
    # only write when the user actually set something — otherwise defaults apply
    [ -z "${CLAUDEHUD_VERSION:-}" ] && [ -z "${CLAUDEHUD_NO_AUTOUPDATE:-}" ] && return 0

    cfg_dir="${XDG_CONFIG_HOME:-$HOME/.config}/claudehud"
    cfg="$cfg_dir/config"
    mkdir -p "$cfg_dir"
    : > "$cfg"
    [ -n "${CLAUDEHUD_NO_AUTOUPDATE:-}" ] && printf 'autoupdate=false\n' >> "$cfg"
    [ -n "${CLAUDEHUD_VERSION:-}" ]       && printf 'pin=%s\n' "$CLAUDEHUD_VERSION" >> "$cfg"
    say "wrote daemon config to $cfg"
}
```

Call it from `main()` just before the daemon-start switch (after `configure_claude`):

```sh
    configure_claude

    write_config

    case "$(uname -s)" in
```

- [ ] **Step 2: Harden the systemd unit against restart rate-limiting**

In `install.sh` `start_daemon_linux`, change the heredoc unit to:

```sh
    cat > "$svc" <<EOF
[Unit]
Description=claudehud git cache daemon
# autoupdate restarts the unit once per release; don't let that trip the limiter
StartLimitIntervalSec=0

[Service]
ExecStart=${INSTALL_DIR}/claudehud-daemon
Restart=always
RestartSec=2

[Install]
WantedBy=default.target
EOF
```

(launchd's `KeepAlive=true` already relaunches on any exit; its 10s `ThrottleInterval` is fine for one restart per release — no plist change needed.)

- [ ] **Step 3: Document the env var in the install.sh header**

In the `install.sh` options comment block, add:

```sh
#   CLAUDEHUD_NO_AUTOUPDATE=1  Disable daemon self-update (persisted to config)
```

- [ ] **Step 4: Document autoupdate in the README**

Add a section to `README.md` (after "Status incidents") describing: the daemon checks for new releases every 5 min, verifies sha256, swaps in place, and restarts; the statusline shows `updated to vX.Y.Z` for ~5 min; opt out with `CLAUDEHUD_NO_AUTOUPDATE=1` at install time or `autoupdate=false` in `~/.config/claudehud/config`; pin with `CLAUDEHUD_VERSION=vX.Y.Z`. Note Windows autoupdate is not yet implemented.

- [ ] **Step 5: Lint the shell script**

Run: `shellcheck install.sh` (if available)
Expected: no new errors from the added function.

- [ ] **Step 6: Commit**

```bash
git add install.sh README.md
git commit -m "feat(install): persist autoupdate config + harden systemd unit"
```

---

## Final verification

- [ ] **Run the whole suite**

Run: `cargo test`
Expected: all green.

- [ ] **Sanity-check the daemon binary builds in release mode** (matches CI / real installs)

Run: `cargo build --release -p claudehud-daemon -p claudehud`
Expected: clean.

- [ ] **Manual smoke (optional, requires a real release newer than local):** temporarily build with a lower `workspace.version`, run the daemon binary from an install-dir-like path (not under `target/`) in a release build, and confirm it logs `installed vX` and writes `clhud-update-notice`. Revert the version bump afterward.

---

## Notes for the implementer

- **Why pin = exact target, not a cap with latest:** simplest correct behavior. A pinned install sits at its pin (no movement = effective opt-out); a pin set ahead of the installed version moves exactly once. No need to enumerate releases.
- **Why exit(0) instead of re-exec:** launchd `KeepAlive` / systemd `Restart=always` already supervise the process; letting them relaunch avoids re-implementing exec semantics and picks up the freshly-swapped binary cleanly. Version converges after one update, so there's no restart loop.
- **Windows seams:** `target_triple()` returns `None`, `config_path()` returns an empty path, and `swap_binary` uses plain `rename` (Windows can't overwrite a *running* exe — the self-swap needs the rename-then-place trick). All are isolated; implementing Windows later means filling these three spots + an `install.ps1` config write, no structural change.
- **Pre-existing workspace clippy/fmt drift** is documented in memory — don't fix unrelated lints on this branch.
```
