use std::io::{self, Read};
use std::path::Path;
use std::process::ExitCode;

use claudehud::render::RoundingMode;
use claudehud::{git, incidents, input, render};

const HELP: &str = "\
claudehud

USAGE:
  claudehud [OPTIONS]

OPTIONS:
  --usage-rounding-mode <MODE>   How to round usage percentages.
                                 Values: floor (default), ceiling, nearest
  -h, --help                     Print this help
";

fn main() -> ExitCode {
    let mut args = pico_args::Arguments::from_env();

    if args.contains(["-h", "--help"]) {
        print!("{HELP}");
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
