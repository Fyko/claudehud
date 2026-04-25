// claudehud-daemon/src/main.rs
mod cache;
mod registrar;
mod status;
mod watcher;

use std::path::PathBuf;
use std::process::ExitCode;

const HELP: &str = "\
claudehud-daemon

USAGE:
  claudehud-daemon [OPTIONS]

OPTIONS:
  -V, --version   Print version and exit
  -h, --help      Print this help
";

fn main() -> ExitCode {
    if let Some(arg) = std::env::args().nth(1) {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{HELP}");
                return ExitCode::SUCCESS;
            }
            "-V" | "--version" => {
                println!("claudehud-daemon {}", env!("CARGO_PKG_VERSION"));
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!("claudehud-daemon: unknown argument '{other}'");
                return ExitCode::from(2);
            }
        }
    }

    let (tx, rx) = crossbeam_channel::unbounded::<PathBuf>();
    let tx2 = tx.clone();

    std::thread::spawn(move || {
        registrar::start(tx2);
    });

    std::thread::spawn(|| {
        status::start();
    });

    // watcher::start runs the main event loop — blocks until channel closes
    watcher::start(rx);

    ExitCode::SUCCESS
}
