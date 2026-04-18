# `claudehud install` Subcommand Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a `claudehud install` subcommand that writes `.statusLine.command` into `~/.claude/settings.json` safely, replacing the fragile sed/jq/python3 logic in `install.sh`.

**Architecture:** New `claudehud/src/install.rs` module with pure helpers (path resolution, JSON load, set-statusline, atomic write, TTY prompt) behind one `run(args) -> ExitCode`. Routed from `main.rs` by pico-args subcommand dispatch. Hot statusline-render path is untouched. `install.sh` shrinks to a single invocation.

**Tech Stack:** Rust 2021, `pico-args` 0.5 (already present), `serde_json` with `preserve_order` feature, `std::io::IsTerminal`, `std::env::current_exe`. `tempfile` crate as dev-dep for test scratch dirs.

**Spec:** `docs/superpowers/specs/2026-04-17-claudehud-install-subcommand-design.md`

---

## File Structure

**Create:**
- `claudehud/src/install.rs` — all subcommand logic + unit tests.

**Modify:**
- `claudehud/Cargo.toml` — add `preserve_order` feature to `serde_json`; add `tempfile` dev-dep.
- `claudehud/src/lib.rs` — export the new `install` module.
- `claudehud/src/main.rs` — subcommand dispatch, HELP update.
- `install.sh` — remove `configure_claude`, `set_statusline`, `warn`; replace with one `claudehud install` invocation.

---

## Task 1: Cargo dependencies

**Files:**
- Modify: `claudehud/Cargo.toml`

- [ ] **Step 1: Update serde_json feature + add tempfile dev-dep**

Edit `claudehud/Cargo.toml`. Replace the serde_json line and add a `[dev-dependencies]` section:

```toml
[dependencies]
common = { workspace = true }
memmap2 = { workspace = true }
pico-args = "0.5"
serde = { version = "1", features = ["derive"] }
serde_json = { version = "1", features = ["preserve_order"] }
time = { version = "0.3", features = ["local-offset"] }

[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 2: Verify build**

Run: `cargo build -p claudehud`
Expected: build succeeds. `indexmap` shows up in the dependency graph (pulled in by `preserve_order`).

- [ ] **Step 3: Commit**

```bash
git add claudehud/Cargo.toml Cargo.lock
git commit -m "chore(claudehud): enable serde_json preserve_order + add tempfile dev-dep"
```

---

## Task 2: Scaffold `install` module + subcommand dispatch

Goal: `claudehud install` parses, does nothing useful yet, exits 0. `claudehud install -h` prints usage. The existing `claudehud` (no subcommand) render path is untouched.

**Files:**
- Create: `claudehud/src/install.rs`
- Modify: `claudehud/src/lib.rs`
- Modify: `claudehud/src/main.rs`

- [ ] **Step 1: Create `claudehud/src/install.rs` with a stub `run`**

```rust
//! `claudehud install` — write statusLine config into ~/.claude/settings.json.

use std::process::ExitCode;

use pico_args::Arguments;

const HELP: &str = "\
claudehud install

USAGE:
  claudehud install [OPTIONS]

OPTIONS:
  --force                  Overwrite an existing statusLine without prompting.
  --settings <PATH>        Path to settings.json. Overrides $CLAUDE_CONFIG_DIR
                           and the default of ~/.claude/settings.json.
  --dry-run                Print resulting JSON to stdout without writing.
  -h, --help               Print this help.
";

pub fn run(mut args: Arguments) -> ExitCode {
    if args.contains(["-h", "--help"]) {
        print!("{HELP}");
        return ExitCode::SUCCESS;
    }
    // TODO: parse flags, resolve path, edit settings.
    ExitCode::SUCCESS
}
```

- [ ] **Step 2: Export the module from `lib.rs`**

Edit `claudehud/src/lib.rs` to add:

```rust
pub mod fmt;
pub mod git;
pub mod incidents;
pub mod input;
pub mod install;
pub mod render;
pub mod time;
```

- [ ] **Step 3: Dispatch from `main.rs`**

Edit `claudehud/src/main.rs`. Replace imports + top of `main` as follows:

```rust
use std::io::{self, Read};
use std::path::Path;
use std::process::ExitCode;

use claudehud::render::RoundingMode;
use claudehud::{git, incidents, input, install, render};

const HELP: &str = "\
claudehud

USAGE:
  claudehud [OPTIONS]
  claudehud install [OPTIONS]

OPTIONS:
  --usage-rounding-mode <MODE>   How to round usage percentages.
                                 Values: floor (default), ceiling, nearest
  -V, --version                  Print version and exit
  -h, --help                     Print this help

SUBCOMMANDS:
  install                        Configure Claude Code to use this binary
                                 as its statusLine. See `claudehud install -h`.
";

fn main() -> ExitCode {
    let mut args = pico_args::Arguments::from_env();

    if matches!(args.subcommand().ok().flatten().as_deref(), Some("install")) {
        return install::run(args);
    }

    if args.contains(["-h", "--help"]) {
        print!("{HELP}");
        return ExitCode::SUCCESS;
    }

    if args.contains(["-V", "--version"]) {
        println!("claudehud {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }

    let rounding = match args.opt_value_from_str::<_, String>("--usage-rounding-mode") {
        Ok(Some(s)) => match RoundingMode::parse(&s) {
            Some(m) => m,
            None => {
                eprintln!("claudehud: unknown --usage-rounding-mode '{s}' (want: floor|ceiling|nearest)");
                return ExitCode::from(2);
            }
        },
        Ok(None) => RoundingMode::default(),
        Err(e) => {
            eprintln!("claudehud: {e}");
            return ExitCode::from(2);
        }
    };

    let mut raw = String::new();
    io::stdin().read_to_string(&mut raw).unwrap_or(0);

    if raw.trim().is_empty() {
        print!("Claude");
        return ExitCode::SUCCESS;
    }

    let input: input::Input = serde_json::from_str(&raw).unwrap_or_default();
    let git = input
        .cwd
        .as_deref()
        .filter(|s| !s.is_empty())
        .and_then(|cwd| git::branch_and_dirty(Path::new(cwd)));

    let incident = incidents::read_incident();
    print!("{}", render::render(&input, git, incident.as_ref(), rounding));
    ExitCode::SUCCESS
}
```

`pico_args::Arguments::subcommand()` pops the first non-flag positional. If it equals `"install"`, we dispatch and return. Otherwise the normal render path runs.

- [ ] **Step 4: Verify it compiles and runs**

Run: `cargo build -p claudehud`
Expected: success.

Run: `./target/debug/claudehud install -h`
Expected: prints the install-subcommand HELP.

Run: `./target/debug/claudehud --help`
Expected: prints top-level HELP showing `install` under `SUBCOMMANDS`. Confirms subcommand dispatch didn't swallow `--help`.

Run: `printf '' | ./target/debug/claudehud`
Expected: prints `Claude` (empty stdin fallback). Confirms the non-install render path still works.

- [ ] **Step 5: Commit**

```bash
git add claudehud/src/install.rs claudehud/src/lib.rs claudehud/src/main.rs
git commit -m "feat(claudehud): scaffold install subcommand dispatch"
```

---

## Task 3: `resolve_settings_path` — precedence logic

Goal: pure function resolving `--settings` > `$CLAUDE_CONFIG_DIR/settings.json` > `$HOME/.claude/settings.json`, returning `None` if no home is discoverable and nothing else was provided.

**Files:**
- Modify: `claudehud/src/install.rs`

- [ ] **Step 1: Write failing tests**

Append to `claudehud/src/install.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn explicit_path_wins() {
        let got = resolve_settings_path(
            Some(PathBuf::from("/tmp/explicit.json")),
            Some(PathBuf::from("/tmp/config-dir")),
            Some(PathBuf::from("/tmp/home")),
        );
        assert_eq!(got, Some(PathBuf::from("/tmp/explicit.json")));
    }

    #[test]
    fn config_dir_beats_home() {
        let got = resolve_settings_path(
            None,
            Some(PathBuf::from("/tmp/config-dir")),
            Some(PathBuf::from("/tmp/home")),
        );
        assert_eq!(got, Some(PathBuf::from("/tmp/config-dir/settings.json")));
    }

    #[test]
    fn home_is_fallback() {
        let got = resolve_settings_path(
            None,
            None,
            Some(PathBuf::from("/tmp/home")),
        );
        assert_eq!(got, Some(PathBuf::from("/tmp/home/.claude/settings.json")));
    }

    #[test]
    fn none_when_nothing_provided() {
        assert_eq!(resolve_settings_path(None, None, None), None);
    }
}
```

- [ ] **Step 2: Run tests to verify failure**

Run: `cargo test -p claudehud install::tests`
Expected: FAIL — `resolve_settings_path` not defined.

- [ ] **Step 3: Implement `resolve_settings_path`**

Add (above the `#[cfg(test)]` block):

```rust
use std::path::{Path, PathBuf};

fn resolve_settings_path(
    explicit: Option<PathBuf>,
    config_dir: Option<PathBuf>,
    home: Option<PathBuf>,
) -> Option<PathBuf> {
    if let Some(p) = explicit {
        return Some(p);
    }
    if let Some(dir) = config_dir {
        return Some(dir.join("settings.json"));
    }
    home.map(|h| h.join(".claude").join("settings.json"))
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p claudehud install::tests`
Expected: 4 tests pass.

- [ ] **Step 5: Commit**

```bash
git add claudehud/src/install.rs
git commit -m "feat(claudehud): install settings-path precedence"
```

---

## Task 4: `load_settings` — read JSON or recognize missing

Goal: given a path, return `Ok(None)` if the file is missing, `Ok(Some(Value))` if valid JSON, `Err` if present but invalid.

**Files:**
- Modify: `claudehud/src/install.rs`

- [ ] **Step 1: Write failing tests**

Add inside the `mod tests` block:

```rust
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn load_missing_is_none() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("missing.json");
        let got = load_settings(&path).unwrap();
        assert!(got.is_none());
    }

    #[test]
    fn load_existing_parses() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, r#"{"theme":"dark"}"#).unwrap();
        let got = load_settings(&path).unwrap().unwrap();
        assert_eq!(got["theme"], "dark");
    }

    #[test]
    fn load_invalid_is_err() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("broken.json");
        fs::write(&path, "not json at all").unwrap();
        assert!(load_settings(&path).is_err());
    }
```

- [ ] **Step 2: Run tests to verify failure**

Run: `cargo test -p claudehud install::tests::load_`
Expected: FAIL — `load_settings` not defined.

- [ ] **Step 3: Implement `load_settings`**

Add to `install.rs` (above `#[cfg(test)]`):

```rust
use std::fs;
use std::io;

use serde_json::Value;

fn load_settings(path: &Path) -> io::Result<Option<Value>> {
    match fs::read_to_string(path) {
        Ok(s) => {
            let v = serde_json::from_str(&s).map_err(|e| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{} is not valid JSON: {e}", path.display()),
                )
            })?;
            Ok(Some(v))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p claudehud install::tests::load_`
Expected: 3 tests pass.

- [ ] **Step 5: Commit**

```bash
git add claudehud/src/install.rs
git commit -m "feat(claudehud): install load_settings with missing/invalid handling"
```

---

## Task 5: `set_statusline_command` — mutate JSON, preserve order

Goal: given a `Value` (expected to be an object, but may be missing/non-object), set `.statusLine = {"command": <cmd>}`. Return the mutated value. Key order of sibling keys must survive.

**Files:**
- Modify: `claudehud/src/install.rs`

- [ ] **Step 1: Write failing tests**

Add inside `mod tests`:

```rust
    #[test]
    fn set_on_empty_object() {
        let v = serde_json::json!({});
        let got = set_statusline_command(v, "/bin/claudehud");
        assert_eq!(got["statusLine"]["command"], "/bin/claudehud");
    }

    #[test]
    fn set_preserves_sibling_order() {
        let v: Value = serde_json::from_str(
            r#"{"theme":"dark","hooks":{"PreCompact":[]},"env":{"FOO":"1"}}"#,
        )
        .unwrap();
        let got = set_statusline_command(v, "/bin/claudehud");

        let keys: Vec<&str> = got.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys, vec!["theme", "hooks", "env", "statusLine"]);
    }

    #[test]
    fn set_overwrites_existing_statusline() {
        let v: Value = serde_json::from_str(
            r#"{"statusLine":{"command":"/old/path"},"theme":"dark"}"#,
        )
        .unwrap();
        let got = set_statusline_command(v, "/new/path");
        assert_eq!(got["statusLine"]["command"], "/new/path");
        // preserve_order means statusLine stays at position 0
        let keys: Vec<&str> = got.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys, vec!["statusLine", "theme"]);
    }

    #[test]
    fn set_on_non_object_replaces_with_object() {
        // Pathological input: someone has a root-level array or scalar.
        // We replace with a fresh object rather than corrupt further.
        let v = serde_json::json!([1, 2, 3]);
        let got = set_statusline_command(v, "/bin/claudehud");
        assert_eq!(got["statusLine"]["command"], "/bin/claudehud");
        assert_eq!(got.as_object().unwrap().len(), 1);
    }
```

- [ ] **Step 2: Run tests to verify failure**

Run: `cargo test -p claudehud install::tests::set_`
Expected: FAIL — `set_statusline_command` not defined.

- [ ] **Step 3: Implement `set_statusline_command`**

Add to `install.rs`:

```rust
fn set_statusline_command(value: Value, command: &str) -> Value {
    let mut obj = match value {
        Value::Object(m) => m,
        _ => serde_json::Map::new(),
    };
    let mut sl = serde_json::Map::new();
    sl.insert("command".to_string(), Value::String(command.to_string()));
    obj.insert("statusLine".to_string(), Value::Object(sl));
    Value::Object(obj)
}
```

Note: `.insert()` on an already-present key replaces the value *without moving* it in an `indexmap`-backed map (which `preserve_order` gives us). That is what the `set_overwrites_existing_statusline` test verifies.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p claudehud install::tests::set_`
Expected: 4 tests pass.

- [ ] **Step 5: Commit**

```bash
git add claudehud/src/install.rs
git commit -m "feat(claudehud): install set_statusline_command preserves key order"
```

---

## Task 6: `atomic_write` — tempfile + rename, cleanup on error

Goal: write a string to a path atomically by writing to `<path>.claudehud-tmp` then renaming. On any error, remove the tempfile before returning.

**Files:**
- Modify: `claudehud/src/install.rs`

- [ ] **Step 1: Write failing tests**

Add inside `mod tests`:

```rust
    #[test]
    fn atomic_write_creates_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.json");
        atomic_write(&path, "hello").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "hello");
    }

    #[test]
    fn atomic_write_replaces_existing() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, "old").unwrap();
        atomic_write(&path, "new").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "new");
    }

    #[test]
    fn atomic_write_no_tempfile_left_behind_on_success() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.json");
        atomic_write(&path, "x").unwrap();

        let entries: Vec<_> = fs::read_dir(dir.path()).unwrap().collect();
        assert_eq!(entries.len(), 1, "only the settings.json should remain");
    }

    #[test]
    fn atomic_write_errors_when_parent_missing() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("no-such-subdir").join("settings.json");
        let err = atomic_write(&path, "x").unwrap_err();
        // Doesn't matter which exact kind; just that we surfaced an error.
        assert!(matches!(
            err.kind(),
            io::ErrorKind::NotFound | io::ErrorKind::Other
        ));
    }
```

- [ ] **Step 2: Run tests to verify failure**

Run: `cargo test -p claudehud install::tests::atomic_`
Expected: FAIL — `atomic_write` not defined.

- [ ] **Step 3: Implement `atomic_write`**

Add to `install.rs`:

```rust
fn atomic_write(path: &Path, contents: &str) -> io::Result<()> {
    let tmp = tempfile_path(path);
    let write_result = fs::write(&tmp, contents);
    if let Err(e) = write_result {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

fn tempfile_path(target: &Path) -> PathBuf {
    let mut name = target
        .file_name()
        .map(|s| s.to_os_string())
        .unwrap_or_else(|| "settings.json".into());
    name.push(".claudehud-tmp");
    target.with_file_name(name)
}
```

Using a sibling tempfile (same dir) keeps the `rename` on one filesystem, which is what makes it atomic on POSIX.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p claudehud install::tests::atomic_`
Expected: 4 tests pass.

- [ ] **Step 5: Commit**

```bash
git add claudehud/src/install.rs
git commit -m "feat(claudehud): install atomic_write helper"
```

---

## Task 7: Wire `run()` — resolve, load, set, write

Goal: implement the non-interactive `install::run` path: parse args, resolve path, skip silently if parent missing, load → set → write. No TTY prompt or `--force` yet (those come in Task 8). `--dry-run` supported minimally in a later task.

**Files:**
- Modify: `claudehud/src/install.rs`

- [ ] **Step 1: Write failing integration test**

Add inside `mod tests`:

```rust
    #[test]
    fn install_adds_statusline_to_existing_file() {
        let dir = tempdir().unwrap();
        let settings = dir.path().join("settings.json");
        fs::write(
            &settings,
            r#"{"theme":"dark","hooks":{"PreCompact":[{"matcher":"*"}]}}"#,
        )
        .unwrap();

        apply(&Config {
            settings_path: settings.clone(),
            command: "/bin/claudehud".into(),
            force: false,
            dry_run: false,
        })
        .unwrap();

        let got: Value =
            serde_json::from_str(&fs::read_to_string(&settings).unwrap()).unwrap();
        assert_eq!(got["statusLine"]["command"], "/bin/claudehud");
        assert_eq!(got["theme"], "dark");
        assert_eq!(got["hooks"]["PreCompact"][0]["matcher"], "*");
    }

    #[test]
    fn install_creates_file_when_missing() {
        let dir = tempdir().unwrap();
        let settings = dir.path().join("settings.json");

        apply(&Config {
            settings_path: settings.clone(),
            command: "/bin/claudehud".into(),
            force: false,
            dry_run: false,
        })
        .unwrap();

        let got: Value =
            serde_json::from_str(&fs::read_to_string(&settings).unwrap()).unwrap();
        assert_eq!(got["statusLine"]["command"], "/bin/claudehud");
        assert_eq!(got.as_object().unwrap().len(), 1);
    }

    #[test]
    fn install_silent_skip_when_parent_missing() {
        let dir = tempdir().unwrap();
        let settings = dir.path().join("nonexistent").join("settings.json");

        let outcome = apply(&Config {
            settings_path: settings.clone(),
            command: "/bin/claudehud".into(),
            force: false,
            dry_run: false,
        })
        .unwrap();

        assert!(matches!(outcome, Outcome::SkippedMissingParent));
        assert!(!settings.exists());
    }
```

- [ ] **Step 2: Run tests to verify failure**

Run: `cargo test -p claudehud install::tests::install_`
Expected: FAIL — `Config`, `apply`, `Outcome` not defined.

- [ ] **Step 3: Implement `Config`, `Outcome`, `apply`**

Add to `install.rs`:

```rust
pub struct Config {
    pub settings_path: PathBuf,
    pub command: String,
    pub force: bool,
    pub dry_run: bool,
}

#[derive(Debug)]
pub enum Outcome {
    /// Parent directory of settings.json does not exist; nothing written.
    SkippedMissingParent,
    /// settings.json did not exist; created with only .statusLine.
    Created,
    /// settings.json existed without statusLine; added.
    Added,
    /// settings.json existed with statusLine; replaced.
    Overwrote,
    /// User declined the overwrite prompt, or non-TTY collision without --force.
    SkippedCollision,
    /// --dry-run: resulting JSON was printed to stdout.
    DryRan,
}

fn apply(cfg: &Config) -> io::Result<Outcome> {
    if let Some(p) = cfg.settings_path.parent() {
        if !p.as_os_str().is_empty() && !p.exists() {
            return Ok(Outcome::SkippedMissingParent);
        }
    }

    let existing = load_settings(&cfg.settings_path)?;
    let file_existed = existing.is_some();
    let had_statusline = existing
        .as_ref()
        .and_then(Value::as_object)
        .map(|o| o.contains_key("statusLine"))
        .unwrap_or(false);

    let base = existing.unwrap_or_else(|| Value::Object(serde_json::Map::new()));
    let updated = set_statusline_command(base, &cfg.command);

    let mut rendered = serde_json::to_string_pretty(&updated)
        .expect("serialize settings json");
    rendered.push('\n');

    if cfg.dry_run {
        print!("{rendered}");
        return Ok(Outcome::DryRan);
    }

    atomic_write(&cfg.settings_path, &rendered)?;

    Ok(if !file_existed {
        Outcome::Created
    } else if had_statusline {
        // Task 7 doesn't gate on --force yet — that's Task 8. For now, an
        // existing statusLine is unconditionally overwritten.
        Outcome::Overwrote
    } else {
        Outcome::Added
    })
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p claudehud install::tests::install_`
Expected: 3 tests pass.

- [ ] **Step 5: Verify preserve_order still holds in integration**

Run: `cargo test -p claudehud install::tests`
Expected: all tests still pass (10 total at this point).

- [ ] **Step 6: Commit**

```bash
git add claudehud/src/install.rs
git commit -m "feat(claudehud): install apply() wires resolve+load+set+write"
```

---

## Task 8: Collision gating — `--force` + TTY prompt + non-TTY fail

Goal: when the settings file already has a `statusLine` and `--force` is not set:
- If stdin is a TTY: prompt `[y/N]`, overwrite on `y`/`yes`, else skip.
- If stdin is not a TTY: return `SkippedCollision` with an error message. The CLI layer (Task 9) will map that to exit 1.

**Files:**
- Modify: `claudehud/src/install.rs`

- [ ] **Step 1: Write failing tests**

Collision + non-TTY and collision + `--force` are pure-enough to unit test by injecting the "is tty" and "confirm" callbacks. Add a struct field for the prompt strategy so tests can stub it.

Add inside `mod tests`:

```rust
    #[test]
    fn collision_force_overwrites() {
        let dir = tempdir().unwrap();
        let settings = dir.path().join("settings.json");
        fs::write(&settings, r#"{"statusLine":{"command":"/old"}}"#).unwrap();

        let outcome = apply_with_prompt(
            &Config {
                settings_path: settings.clone(),
                command: "/new".into(),
                force: true,
                dry_run: false,
            },
            &mut PromptFn::NonInteractive,
        )
        .unwrap();

        assert!(matches!(outcome, Outcome::Overwrote));
        let got: Value =
            serde_json::from_str(&fs::read_to_string(&settings).unwrap()).unwrap();
        assert_eq!(got["statusLine"]["command"], "/new");
    }

    #[test]
    fn collision_non_tty_skips() {
        let dir = tempdir().unwrap();
        let settings = dir.path().join("settings.json");
        fs::write(&settings, r#"{"statusLine":{"command":"/old"}}"#).unwrap();

        let outcome = apply_with_prompt(
            &Config {
                settings_path: settings.clone(),
                command: "/new".into(),
                force: false,
                dry_run: false,
            },
            &mut PromptFn::NonInteractive,
        )
        .unwrap();

        assert!(matches!(outcome, Outcome::SkippedCollision));
        let got: Value =
            serde_json::from_str(&fs::read_to_string(&settings).unwrap()).unwrap();
        assert_eq!(got["statusLine"]["command"], "/old", "file should be unchanged");
    }

    #[test]
    fn collision_tty_yes_overwrites() {
        let dir = tempdir().unwrap();
        let settings = dir.path().join("settings.json");
        fs::write(&settings, r#"{"statusLine":{"command":"/old"}}"#).unwrap();

        let outcome = apply_with_prompt(
            &Config {
                settings_path: settings.clone(),
                command: "/new".into(),
                force: false,
                dry_run: false,
            },
            &mut PromptFn::Canned("y".into()),
        )
        .unwrap();

        assert!(matches!(outcome, Outcome::Overwrote));
    }

    #[test]
    fn collision_tty_no_skips() {
        let dir = tempdir().unwrap();
        let settings = dir.path().join("settings.json");
        fs::write(&settings, r#"{"statusLine":{"command":"/old"}}"#).unwrap();

        let outcome = apply_with_prompt(
            &Config {
                settings_path: settings.clone(),
                command: "/new".into(),
                force: false,
                dry_run: false,
            },
            &mut PromptFn::Canned("n".into()),
        )
        .unwrap();

        assert!(matches!(outcome, Outcome::SkippedCollision));
    }
```

- [ ] **Step 2: Run tests to verify failure**

Run: `cargo test -p claudehud install::tests::collision_`
Expected: FAIL — `apply_with_prompt`, `PromptFn` not defined.

- [ ] **Step 3: Introduce the prompt strategy + refactor**

Add to `install.rs`:

```rust
pub enum PromptFn {
    /// Never prompts; treated as non-TTY / user-said-no.
    NonInteractive,
    /// Return a canned response (for tests).
    Canned(String),
    /// Read one line from stdin (production path).
    Stdin,
}

impl PromptFn {
    fn ask(&mut self, path: &Path) -> io::Result<bool> {
        match self {
            PromptFn::NonInteractive => Ok(false),
            PromptFn::Canned(s) => Ok(is_yes(s)),
            PromptFn::Stdin => {
                use std::io::Write;
                let prompt = format!(
                    "{} already has a statusLine. Overwrite? [y/N] ",
                    path.display()
                );
                io::stderr().write_all(prompt.as_bytes())?;
                io::stderr().flush()?;
                let mut line = String::new();
                io::stdin().read_line(&mut line)?;
                Ok(is_yes(line.trim()))
            }
        }
    }
}

fn is_yes(s: &str) -> bool {
    matches!(s.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}
```

Refactor `apply` to take a `&mut PromptFn` and gate collision behavior:

```rust
fn apply_with_prompt(cfg: &Config, prompt: &mut PromptFn) -> io::Result<Outcome> {
    if let Some(p) = cfg.settings_path.parent() {
        if !p.as_os_str().is_empty() && !p.exists() {
            return Ok(Outcome::SkippedMissingParent);
        }
    }

    let existing = load_settings(&cfg.settings_path)?;
    let file_existed = existing.is_some();
    let had_statusline = existing
        .as_ref()
        .and_then(Value::as_object)
        .map(|o| o.contains_key("statusLine"))
        .unwrap_or(false);

    // collision gate
    if had_statusline && !cfg.force {
        let yes = prompt.ask(&cfg.settings_path)?;
        if !yes {
            return Ok(Outcome::SkippedCollision);
        }
    }

    let base = existing.unwrap_or_else(|| Value::Object(serde_json::Map::new()));
    let updated = set_statusline_command(base, &cfg.command);

    let mut rendered = serde_json::to_string_pretty(&updated)
        .expect("serialize settings json");
    rendered.push('\n');

    if cfg.dry_run {
        print!("{rendered}");
        return Ok(Outcome::DryRan);
    }

    atomic_write(&cfg.settings_path, &rendered)?;

    Ok(if !file_existed {
        Outcome::Created
    } else if had_statusline {
        Outcome::Overwrote
    } else {
        Outcome::Added
    })
}

fn apply(cfg: &Config) -> io::Result<Outcome> {
    let mut prompt = if io::stdin().is_terminal() {
        PromptFn::Stdin
    } else {
        PromptFn::NonInteractive
    };
    apply_with_prompt(cfg, &mut prompt)
}
```

Add the necessary import at the top of the file:

```rust
use std::io::IsTerminal;
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p claudehud install::tests`
Expected: all tests pass (14 total now).

- [ ] **Step 5: Commit**

```bash
git add claudehud/src/install.rs
git commit -m "feat(claudehud): install collision gating with --force + TTY prompt"
```

---

## Task 9: CLI argument parsing in `run()`

Goal: replace the stub `run` with real flag parsing, path resolution, `current_exe` lookup, dispatch to `apply`, and map `Outcome` + errors to stdout/stderr/exit codes.

**Files:**
- Modify: `claudehud/src/install.rs`

- [ ] **Step 1: Implement `run`**

Replace the stub `pub fn run` in `install.rs` with:

```rust
pub fn run(mut args: Arguments) -> ExitCode {
    if args.contains(["-h", "--help"]) {
        print!("{HELP}");
        return ExitCode::SUCCESS;
    }

    let force = args.contains("--force");
    let dry_run = args.contains("--dry-run");

    let explicit: Option<PathBuf> = match args.opt_value_from_str("--settings") {
        Ok(v) => v,
        Err(e) => {
            eprintln!("claudehud install: {e}");
            return ExitCode::from(2);
        }
    };

    let remaining = args.finish();
    if !remaining.is_empty() {
        eprintln!(
            "claudehud install: unexpected arguments: {}",
            remaining
                .iter()
                .map(|s| s.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join(" ")
        );
        return ExitCode::from(2);
    }

    let config_dir = std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from);
    let home = std::env::var_os("HOME").map(PathBuf::from);

    let settings_path = match resolve_settings_path(explicit, config_dir, home) {
        Some(p) => p,
        None => {
            eprintln!("claudehud install: cannot locate settings.json (no --settings, $CLAUDE_CONFIG_DIR, or $HOME)");
            return ExitCode::from(1);
        }
    };

    let command = match std::env::current_exe() {
        Ok(p) => p.display().to_string(),
        Err(e) => {
            eprintln!("claudehud install: cannot determine own path: {e}");
            return ExitCode::from(1);
        }
    };

    let cfg = Config {
        settings_path: settings_path.clone(),
        command,
        force,
        dry_run,
    };

    match apply(&cfg) {
        Ok(Outcome::SkippedMissingParent) => {
            // Silent skip: no message, exit 0. The user's ~/.claude dir
            // doesn't exist yet, so Claude Code isn't installed. They'll
            // re-run claudehud install via install.sh on next upgrade.
            ExitCode::SUCCESS
        }
        Ok(Outcome::Created) => {
            println!("==> created {} with statusLine", settings_path.display());
            ExitCode::SUCCESS
        }
        Ok(Outcome::Added) => {
            println!("==> added statusLine to {}", settings_path.display());
            ExitCode::SUCCESS
        }
        Ok(Outcome::Overwrote) => {
            println!("==> updated statusLine in {}", settings_path.display());
            ExitCode::SUCCESS
        }
        Ok(Outcome::SkippedCollision) => {
            if cfg.force {
                // shouldn't happen — force skips the prompt
                ExitCode::SUCCESS
            } else if io::stdin().is_terminal() {
                println!("==> skipping statusLine configuration");
                ExitCode::SUCCESS
            } else {
                eprintln!(
                    "claudehud install: {} already has a statusLine",
                    settings_path.display()
                );
                eprintln!("hint: pass --force to overwrite, or run interactively");
                ExitCode::from(1)
            }
        }
        Ok(Outcome::DryRan) => ExitCode::SUCCESS,
        Err(e) if e.kind() == io::ErrorKind::InvalidData => {
            eprintln!("claudehud install: {e}");
            ExitCode::from(2)
        }
        Err(e) => {
            eprintln!("claudehud install: {e}");
            ExitCode::from(1)
        }
    }
}
```

- [ ] **Step 2: Verify it builds**

Run: `cargo build -p claudehud`
Expected: success.

- [ ] **Step 3: Manual smoke test — dry-run against a scratch settings.json**

```bash
mkdir -p /tmp/clhud-smoke/.claude
cat > /tmp/clhud-smoke/.claude/settings.json <<'EOF'
{"theme":"dark","hooks":{"PreCompact":[{"matcher":"*"}]}}
EOF

./target/debug/claudehud install \
    --settings /tmp/clhud-smoke/.claude/settings.json \
    --dry-run
```

Expected stdout:
```json
{
  "theme": "dark",
  "hooks": {
    "PreCompact": [
      {
        "matcher": "*"
      }
    ]
  },
  "statusLine": {
    "command": "/path/to/target/debug/claudehud"
  }
}
```

(Exact `command` path reflects where you invoked it from.)

- [ ] **Step 4: Manual smoke test — real write**

```bash
./target/debug/claudehud install --settings /tmp/clhud-smoke/.claude/settings.json
cat /tmp/clhud-smoke/.claude/settings.json
```

Expected: `==> added statusLine to ...`, file now contains `statusLine`, original keys preserved in order, `hooks` structure intact.

- [ ] **Step 5: Manual smoke test — collision + non-TTY fails**

```bash
echo "" | ./target/debug/claudehud install --settings /tmp/clhud-smoke/.claude/settings.json
echo "exit: $?"
```

Expected: `claudehud install: /tmp/clhud-smoke/.claude/settings.json already has a statusLine` on stderr, exit 1.

- [ ] **Step 6: Manual smoke test — collision + `--force`**

```bash
./target/debug/claudehud install --settings /tmp/clhud-smoke/.claude/settings.json --force
```

Expected: `==> updated statusLine in ...`, exit 0.

- [ ] **Step 7: Commit**

```bash
git add claudehud/src/install.rs
git commit -m "feat(claudehud): install run() wires CLI args to apply()"
```

---

## Task 10: `install.sh` — replace json editing with subcommand call

Goal: delete `configure_claude`, `set_statusline`, `warn` helpers; invoke `claudehud install` instead.

**Files:**
- Modify: `install.sh`

- [ ] **Step 1: Remove the `warn` helper**

In `install.sh`, delete the `warn()` line (added in the earlier stopgap commit):

```sh
say() { printf '\033[1m==> %s\033[0m\n' "$*"; }
warn() { printf '\033[33mwarn:\033[0m %s\n' "$*" >&2; }   # DELETE THIS LINE
err() { printf '\033[31merror:\033[0m %s\n' "$*" >&2; exit 1; }
```

- [ ] **Step 2: Remove `set_statusline` and the body of `configure_claude`**

Delete the entire `set_statusline()` function (defined around line 99) and the entire `configure_claude()` function (following it). Replace both with a single new `configure_claude()`:

```sh
# ---------------------------------------------------------------------------
# configure claude code statusline
# ---------------------------------------------------------------------------

configure_claude() {
    [ -n "${CLAUDEHUD_SKIP_CONFIG:-}" ] && {
        say "skipping Claude Code configuration (CLAUDEHUD_SKIP_CONFIG is set)"
        return 0
    }

    install_args=""
    [ -n "${CLAUDEHUD_FORCE_CONFIG:-}" ] && install_args="--force"

    # claudehud install is a no-op if ~/.claude doesn't exist yet, and exits
    # non-zero on collision without --force. Don't let a non-force collision
    # abort the rest of the install — the subcommand already printed a hint
    # and the binary is on disk.
    "$INSTALL_DIR/claudehud" install $install_args || true
}
```

(The section header comment stays.)

- [ ] **Step 3: Verify the script parses**

Run: `sh -n install.sh`
Expected: no output, exit 0.

- [ ] **Step 4: Manual end-to-end test**

From the repo root, simulate a clean install against a scratch HOME. Build the release binary first so we're testing real paths:

```bash
cargo build --release -p claudehud
mkdir -p /tmp/clhud-e2e/.local/bin /tmp/clhud-e2e/.claude
cp target/release/claudehud /tmp/clhud-e2e/.local/bin/
cat > /tmp/clhud-e2e/.claude/settings.json <<'EOF'
{"theme":"dark","hooks":{"PreCompact":[{"matcher":"*","hooks":[]}]}}
EOF

# exercise just the configure_claude function with tweaked globals
HOME=/tmp/clhud-e2e \
INSTALL_DIR=/tmp/clhud-e2e/.local/bin \
sh -c '
    . <(sed -n "/^say()/,/^configure_claude() {/p; /^}$/p" install.sh)
    configure_claude
'
cat /tmp/clhud-e2e/.claude/settings.json
```

Expected: `==> added statusLine to ...`, and the file contains `statusLine` plus the original keys intact and in order.

(If sourcing the script function is awkward in your shell, just invoke the binary directly: `HOME=/tmp/clhud-e2e /tmp/clhud-e2e/.local/bin/claudehud install`.)

- [ ] **Step 5: Commit**

```bash
git add install.sh
git commit -m "refactor(install): delegate statusLine config to claudehud install"
```

---

## Task 11: README + help output update

Goal: document the new subcommand briefly in README so the feature is discoverable.

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Check current README for install/config section**

Run: `grep -n -E 'install|configure|settings.json' README.md`
Expected: identifies the existing install/config section. (If README is silent on this, add a short "Manual configuration" heading to the install section.)

- [ ] **Step 2: Add a note about `claudehud install`**

Insert a short paragraph under the install section:

```markdown
### Manual configuration

If you skipped auto-configuration (e.g. `CLAUDEHUD_SKIP_CONFIG=1`), wire the
statusLine in later with:

```bash
claudehud install
```

Use `--force` to overwrite an existing `statusLine`. `--dry-run` prints the
resulting JSON without writing. `claudehud install --help` lists all flags.
```

- [ ] **Step 3: Commit**

```bash
git add README.md
git commit -m "docs: document claudehud install subcommand"
```

---

## Task 12: Self-review + full test suite

- [ ] **Step 1: Run the full workspace test suite**

Run: `cargo test --workspace`
Expected: all tests pass.

- [ ] **Step 2: Run clippy**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: no warnings. Fix any that appear.

- [ ] **Step 3: Run rustfmt check**

Run: `cargo fmt --all -- --check`
Expected: clean. Run `cargo fmt --all` and commit if not.

- [ ] **Step 4: Verify `install.sh` still parses under both sh and bash**

Run: `sh -n install.sh && bash -n install.sh`
Expected: both clean.

- [ ] **Step 5: Confirm the statusline render hot path is unchanged**

Run: `echo '{}' | ./target/release/claudehud`
Expected: prints `Claude` (same as before this work).

Run with real-ish input:
```bash
echo '{"cwd":"'"$PWD"'","model":{"display_name":"Sonnet"}}' \
  | ./target/release/claudehud
```
Expected: renders a statusline identical to what the pre-change binary would have produced.

- [ ] **Step 6: Final commit (if anything fell out of self-review)**

```bash
git status
# commit any stragglers with a descriptive message
```

---

## Summary of commits (expected)

1. `chore(claudehud): enable serde_json preserve_order + add tempfile dev-dep`
2. `feat(claudehud): scaffold install subcommand dispatch`
3. `feat(claudehud): install settings-path precedence`
4. `feat(claudehud): install load_settings with missing/invalid handling`
5. `feat(claudehud): install set_statusline_command preserves key order`
6. `feat(claudehud): install atomic_write helper`
7. `feat(claudehud): install apply() wires resolve+load+set+write`
8. `feat(claudehud): install collision gating with --force + TTY prompt`
9. `feat(claudehud): install run() wires CLI args to apply()`
10. `refactor(install): delegate statusLine config to claudehud install`
11. `docs: document claudehud install subcommand`
12. (optional) self-review cleanup

Twelve small commits; each leaves the workspace in a buildable, test-passing state.
