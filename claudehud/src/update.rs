//! `claudehud update` — thin wrapper that re-runs the official install script.
//!
//! install.sh is the source of truth for upgrades: it diffs the installed
//! `--version` against the latest GitHub release, no-ops when equal, otherwise
//! swaps the binary in place and restarts the daemon. This subcommand just
//! shells out to it so users don't need to remember the curl URL.

use std::io;
use std::process::{Command, ExitCode, Stdio};

use common::version::{compare, VersionState};
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
    common::version::parse_tag(&body)
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
