// claudehud-daemon/src/main.rs

// Release builds on Windows use the windows subsystem so no console window
// flashes at logon when Task Scheduler launches the daemon. Debug builds keep
// the console subsystem so developers still see stdout/stderr.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod cache;
mod registrar;
mod status;
mod update;
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

    // Ensure cache + watch dirs exist before any thread tries to write into them.
    // On Unix these resolve to /tmp/ (already present); on Windows they live
    // under %LOCALAPPDATA%\claudehud\cache and may not exist yet.
    let _ = std::fs::create_dir_all(common::cache_dir());
    let _ = std::fs::create_dir_all(common::watch_dir());

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
