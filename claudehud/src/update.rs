//! `claudehud update` — thin wrapper that re-runs the official install script.
//!
//! install.sh is the source of truth for upgrades: it diffs the installed
//! `--version` against the latest GitHub release, no-ops when equal, otherwise
//! swaps the binary in place and restarts the daemon. This subcommand just
//! shells out to it so users don't need to remember the curl URL.

use std::io;
use std::process::{Command, ExitCode, Stdio};

use pico_args::Arguments;

const HELP: &str = "\
claudehud update

USAGE:
  claudehud update [OPTIONS]

OPTIONS:
  --check        Compare the installed version against the latest release and
                 exit without downloading. Exit 0 if up to date, 1 if a newer
                 release exists, 2 on error.
  -h, --help     Print this help.

ENVIRONMENT:
  CLAUDEHUD_VERSION         Pin a specific release tag (e.g. v0.1.0).
  CLAUDEHUD_FORCE_INSTALL   Reinstall even if already at target version.
  CLAUDEHUD_SKIP_CONFIG     Don't reconfigure Claude Code statusLine.
  CLAUDEHUD_FORCE_CONFIG    Overwrite existing statusLine without prompting.
  CLAUDEHUD_INSTALL_DIR     Override install dir (default ~/.local/bin).

  These are forwarded to the install script unchanged.
";

const INSTALL_URL: &str = "https://raw.githubusercontent.com/fyko/claudehud/main/install.sh";
const RELEASES_API: &str = "https://api.github.com/repos/fyko/claudehud/releases/latest";

pub fn run(mut args: Arguments) -> ExitCode {
    if args.contains(["-h", "--help"]) {
        print!("{HELP}");
        return ExitCode::SUCCESS;
    }

    let check = args.contains("--check");

    let remaining = args.finish();
    if !remaining.is_empty() {
        eprintln!(
            "claudehud update: unexpected arguments: {}",
            remaining
                .iter()
                .map(|s| s.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join(" ")
        );
        return ExitCode::from(2);
    }

    if check {
        return run_check();
    }

    run_install_sh()
}

fn run_check() -> ExitCode {
    let installed = env!("CARGO_PKG_VERSION");
    match latest_tag() {
        Ok(tag) => match compare(installed, &tag) {
            VersionState::UpToDate => {
                println!("claudehud {installed} is up to date");
                ExitCode::SUCCESS
            }
            VersionState::Newer(latest) => {
                println!("claudehud {installed} → {latest} available");
                println!("hint: run `claudehud update` to upgrade");
                ExitCode::from(1)
            }
            VersionState::Ahead(latest) => {
                println!("claudehud {installed} is ahead of latest release ({latest})");
                ExitCode::SUCCESS
            }
        },
        Err(e) => {
            eprintln!("claudehud update: cannot check latest version: {e}");
            ExitCode::from(2)
        }
    }
}

fn run_install_sh() -> ExitCode {
    eprintln!("==> fetching install script from {INSTALL_URL}");

    // download to a tempfile before piping to sh so a transport failure
    // surfaces a non-zero exit instead of silently feeding empty stdin.
    let cmd = format!(
        "set -e\n\
         tmp=$(mktemp) || exit 1\n\
         trap 'rm -f \"$tmp\"' EXIT INT TERM\n\
         if command -v curl >/dev/null 2>&1; then\n\
         \tcurl -fsSL '{INSTALL_URL}' -o \"$tmp\"\n\
         elif command -v wget >/dev/null 2>&1; then\n\
         \twget -qO \"$tmp\" '{INSTALL_URL}'\n\
         else\n\
         \techo 'claudehud update: neither curl nor wget found' >&2; exit 1\n\
         fi\n\
         sh \"$tmp\"\n"
    );

    match Command::new("sh")
        .arg("-c")
        .arg(&cmd)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
    {
        Ok(s) if s.success() => ExitCode::SUCCESS,
        Ok(s) => {
            let code = u8::try_from(s.code().unwrap_or(1)).unwrap_or(1);
            ExitCode::from(code)
        }
        Err(e) => {
            eprintln!("claudehud update: failed to invoke sh: {e}");
            ExitCode::from(1)
        }
    }
}

fn latest_tag() -> io::Result<String> {
    let body = fetch(RELEASES_API)?;
    parse_tag(&body)
}

fn fetch(url: &str) -> io::Result<Vec<u8>> {
    // try curl first, fall back to wget — same order as install.sh
    if let Ok(out) = Command::new("curl").args(["-fsSL", url]).output() {
        if out.status.success() {
            return Ok(out.stdout);
        }
    }
    let wget = Command::new("wget")
        .args(["-qO-", url])
        .output()
        .map_err(|e| io::Error::other(format!("neither curl nor wget worked: {e}")))?;
    if !wget.status.success() {
        return Err(io::Error::other(format!(
            "wget exited with status {}",
            wget.status
        )));
    }
    Ok(wget.stdout)
}

fn parse_tag(body: &[u8]) -> io::Result<String> {
    let v: serde_json::Value = serde_json::from_slice(body).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("bad JSON from GitHub: {e}"),
        )
    })?;
    v.get("tag_name")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "GitHub response had no tag_name",
            )
        })
}

#[derive(Debug, PartialEq, Eq)]
enum VersionState {
    UpToDate,
    Newer(String),
    Ahead(String),
}

fn compare(installed: &str, tag: &str) -> VersionState {
    let latest = tag.trim_start_matches('v').to_string();
    let installed_parts = parse_semver(installed);
    let latest_parts = parse_semver(&latest);
    match (installed_parts, latest_parts) {
        (Some(i), Some(l)) if i == l => VersionState::UpToDate,
        (Some(i), Some(l)) if i < l => VersionState::Newer(latest),
        (Some(_), Some(_)) => VersionState::Ahead(latest),
        // unparseable: fall back to string equality
        _ if installed == latest => VersionState::UpToDate,
        _ => VersionState::Newer(latest),
    }
}

/// Parse `MAJOR.MINOR.PATCH` into a comparable tuple. Pre-release suffixes
/// (`-alpha.4`) sort *before* the bare release, matching semver ordering.
fn parse_semver(s: &str) -> Option<(u64, u64, u64, Option<String>)> {
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
    // a pre-release version is LESS than the same core without pre-release;
    // encode that by mapping `None` → `"~"` (sorts after any alnum) so that
    // `(1,0,0,None) > (1,0,0,Some("alpha"))`.
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
        let body = br#"{"name":"Release 0.1.0"}"#;
        assert!(parse_tag(body).is_err());
    }

    #[test]
    fn parse_tag_errors_on_bad_json() {
        let body = b"not json at all";
        assert!(parse_tag(body).is_err());
    }

    #[test]
    fn compare_equal_versions() {
        assert_eq!(compare("0.1.0", "v0.1.0"), VersionState::UpToDate);
        assert_eq!(compare("0.1.0", "0.1.0"), VersionState::UpToDate);
    }

    #[test]
    fn compare_installed_older() {
        assert_eq!(
            compare("0.1.0", "v0.2.0"),
            VersionState::Newer("0.2.0".into())
        );
        assert_eq!(
            compare("0.1.9", "v0.1.10"),
            VersionState::Newer("0.1.10".into())
        );
    }

    #[test]
    fn compare_installed_ahead() {
        assert_eq!(
            compare("0.2.0", "v0.1.0"),
            VersionState::Ahead("0.1.0".into())
        );
    }

    #[test]
    fn compare_prerelease_is_less_than_release() {
        // a pre-release should be considered older than the same MAJOR.MINOR.PATCH
        assert_eq!(
            compare("0.1.0-alpha.4", "v0.1.0"),
            VersionState::Newer("0.1.0".into())
        );
        assert_eq!(
            compare("0.1.0", "v0.1.0-alpha.4"),
            VersionState::Ahead("0.1.0-alpha.4".into())
        );
    }

    #[test]
    fn compare_unparseable_falls_back_to_string_eq() {
        // weird tags shouldn't crash — they just compare as strings
        assert_eq!(compare("weird", "weird"), VersionState::UpToDate);
        assert_eq!(
            compare("weird", "other"),
            VersionState::Newer("other".into())
        );
    }
}
