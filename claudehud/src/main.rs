use std::io::{self, IsTerminal, Read, Write};
use std::process::ExitCode;

use claudehud::orchestrate::{self, Options, SystemEnv};
use claudehud::render::RoundingMode;
use claudehud::{install, render, update};

const HELP: &str = "\
claudehud — statusline renderer for Claude Code

USAGE:
  claudehud render [OPTIONS]     Read JSON from stdin, write the statusline.
                                 Invoked by Claude Code; bare `claudehud` with
                                 piped stdin still routes here for back-compat.
  claudehud install [OPTIONS]    Configure Claude Code to use this binary
                                 as its statusLine. See `claudehud install -h`.
  claudehud update [OPTIONS]     Upgrade to the latest release (or check with
                                 --check). See `claudehud update -h`.

OPTIONS (for `render`):
  --usage-rounding-mode <MODE>   How to round usage percentages.
                                 Values: floor (default), ceiling, nearest

GLOBAL OPTIONS:
  -V, --version                  Print version and exit
  -h, --help                     Print this help

ENVIRONMENT:
  CLAUDEHUD_LAYOUT               Render layout: comfortable (default) or condensed.
  CLAUDEHUD_LOG                  Path; appends each stdin JSON payload here for
                                 debugging the render path.
  CLAUDEHUD_CACHE_DIR            Override the cache directory holding mmap files
                                 + watch markers. Default: /tmp on Unix,
                                 %LOCALAPPDATA%\\claudehud\\cache on Windows.
  CLAUDE_CONFIG_DIR              Alternate Claude config directory used by
                                 `claudehud install` when resolving settings.json.

  `claudehud update` forwards additional env vars to the install script —
  see `claudehud update -h`.
";

fn main() -> ExitCode {
    let mut args = pico_args::Arguments::from_env();

    match args.subcommand().ok().flatten().as_deref() {
        Some("install") => return install::run(args),
        Some("update") => return update::run(args),
        Some("render") => return render(args),
        Some(other) => {
            eprintln!("claudehud: unknown subcommand '{other}'");
            eprintln!("run `claudehud --help` for available subcommands");
            return ExitCode::from(2);
        }
        None => {}
    }

    if args.contains(["-h", "--help"]) {
        print!("{HELP}");
        return ExitCode::SUCCESS;
    }

    if args.contains(["-V", "--version"]) {
        println!("claudehud {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }

    // Bare `claudehud` with no subcommand. If stdin is a TTY the user ran us
    // from a shell — probably looking for help, not piping a JSON payload
    // (which would deadlock on read_to_string). Print help and exit.
    // If stdin is piped (Claude Code's statusLine), fall through to render
    // so legacy settings.json wired to bare `claudehud` keeps working.
    if io::stdin().is_terminal() {
        print!("{HELP}");
        return ExitCode::SUCCESS;
    }

    render(args)
}

fn render(mut args: pico_args::Arguments) -> ExitCode {
    if args.contains(["-h", "--help"]) {
        print!("{HELP}");
        return ExitCode::SUCCESS;
    }

    let rounding = match args.opt_value_from_str::<_, String>("--usage-rounding-mode") {
        Ok(Some(s)) => {
            match RoundingMode::parse(&s) {
                Some(m) => m,
                None => {
                    eprintln!("claudehud: unknown --usage-rounding-mode '{s}' (want: floor|ceiling|nearest)");
                    return ExitCode::from(2);
                }
            }
        }
        Ok(None) => RoundingMode::default(),
        Err(e) => {
            eprintln!("claudehud: {e}");
            return ExitCode::from(2);
        }
    };

    let layout = match std::env::var("CLAUDEHUD_LAYOUT") {
        Ok(s) if !s.is_empty() => match render::Layout::parse(&s) {
            Some(l) => l,
            None => {
                eprintln!(
                    "claudehud: unknown CLAUDEHUD_LAYOUT '{s}' (want: comfortable|condensed)"
                );
                render::Layout::default()
            }
        },
        _ => render::Layout::default(),
    };

    // Thin adapter: read real stdin, then hand the raw payload + the live
    // environment (real git cache/registration, incident mmap, on-disk notice
    // read against the wall clock) to the orchestration. All decisions live in
    // `orchestrate::run`; `main` only wires real I/O to it.
    let mut raw = String::new();
    io::stdin().read_to_string(&mut raw).unwrap_or(0);
    log_stdin(&raw);

    let hud = orchestrate::run(&raw, &SystemEnv, Options { rounding, layout });
    print!("{hud}");
    ExitCode::SUCCESS
}

fn log_stdin(raw: &str) {
    let Ok(path) = std::env::var("CLAUDEHUD_LOG") else {
        return;
    };
    let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    else {
        return;
    };
    let ts = time::OffsetDateTime::now_utc().unix_timestamp();
    let _ = writeln!(f, "--- {ts} ---\n{raw}");
}
