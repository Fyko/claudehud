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
}
