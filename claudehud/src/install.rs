//! `claudehud install` — write statusLine config into ~/.claude/settings.json.

use std::fs;
use std::io;
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

#[allow(dead_code)]
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

#[allow(dead_code)]
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

#[allow(dead_code)]
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

#[allow(dead_code)]
fn tempfile_path(target: &Path) -> PathBuf {
    let mut name = target
        .file_name()
        .map(|s| s.to_os_string())
        .unwrap_or_else(|| "settings.json".into());
    name.push(".claudehud-tmp");
    target.with_file_name(name)
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
}
