// claudehud-daemon/src/main.rs
mod cache;
mod registrar;
mod watcher;

use std::path::PathBuf;

fn main() {
    let (tx, rx) = crossbeam_channel::unbounded::<PathBuf>();
    let tx2 = tx.clone();

    std::thread::spawn(move || {
        registrar::start(tx2);
    });

    // watcher::start runs the main event loop — blocks until channel closes
    watcher::start(rx);
}
