use std::fs;
use std::path::PathBuf;

use common::WATCH_DIR;
use crossbeam_channel::Sender;
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

/// Watch /tmp/clhud-watch/ for new marker files. Each file contains an absolute
/// path as UTF-8 bytes. Sends each path to `tx` for the watcher to pick up.
/// Also drains any existing marker files on startup (handles daemon restarts).
pub fn start(tx: Sender<PathBuf>) {
    let watch_dir = std::path::Path::new(WATCH_DIR);
    fs::create_dir_all(watch_dir).expect("failed to create /tmp/clhud-watch");

    let tx2 = tx.clone();
    let mut watcher = RecommendedWatcher::new(
        move |res: notify::Result<Event>| {
            if let Ok(event) = res {
                if matches!(event.kind, EventKind::Create(_)) {
                    for path in &event.paths {
                        send_path_from_marker(path, &tx2);
                    }
                }
            }
        },
        Config::default(),
    )
    .expect("failed to create notify watcher");

    watcher
        .watch(watch_dir, RecursiveMode::NonRecursive)
        .expect("failed to watch /tmp/clhud-watch");

    // Drain existing markers AFTER starting the watcher so no new
    // markers are missed in the window between drain and watch start.
    // Duplicates are safe — the consumer deduplicates by git root.
    if let Ok(entries) = fs::read_dir(watch_dir) {
        for entry in entries.flatten() {
            send_path_from_marker(&entry.path(), &tx);
        }
    }

    // Park this thread — `_watcher` must stay alive to keep watching.
    let _watcher = watcher;
    std::thread::park();
}

fn send_path_from_marker(marker: &std::path::Path, tx: &Sender<PathBuf>) {
    if let Ok(bytes) = fs::read(marker) {
        if let Ok(s) = std::str::from_utf8(&bytes) {
            let path = PathBuf::from(s.trim());
            if path.is_absolute() {
                let _ = tx.send(path);
            }
        }
    }
}
