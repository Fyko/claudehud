//! `claudehud install` — write statusLine config into ~/.claude/settings.json.

use std::fs;
use std::io::{self, IsTerminal};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use pico_args::Arguments;
use serde_json::Value;

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

#[allow(dead_code)]
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

#[allow(dead_code)]
pub struct Config {
    pub settings_path: PathBuf,
    pub command: String,
    pub force: bool,
    pub dry_run: bool,
}

#[allow(dead_code)]
#[derive(Debug)]
pub enum Outcome {
    SkippedMissingParent,
    Created,
    Added,
    Overwrote,
    SkippedCollision,
    DryRan,
}

#[allow(dead_code)]
pub enum PromptFn {
    NonInteractive,
    Canned(String),
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

#[allow(dead_code)]
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

#[allow(dead_code)]
fn apply(cfg: &Config) -> io::Result<Outcome> {
    let mut prompt = if io::stdin().is_terminal() {
        PromptFn::Stdin
    } else {
        PromptFn::NonInteractive
    };
    apply_with_prompt(cfg, &mut prompt)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use tempfile::tempdir;

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
        let keys: Vec<&str> = got.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys, vec!["statusLine", "theme"]);
    }

    #[test]
    fn set_on_non_object_replaces_with_object() {
        let v = serde_json::json!([1, 2, 3]);
        let got = set_statusline_command(v, "/bin/claudehud");
        assert_eq!(got["statusLine"]["command"], "/bin/claudehud");
        assert_eq!(got.as_object().unwrap().len(), 1);
    }

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
        assert!(matches!(
            err.kind(),
            io::ErrorKind::NotFound | io::ErrorKind::Other
        ));
    }

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
}
