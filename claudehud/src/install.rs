//! `claudehud install` — write statusLine config into ~/.claude/settings.json.

use std::path::PathBuf;
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
