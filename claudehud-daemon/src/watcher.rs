use std::collections::HashMap;
use std::path::PathBuf;

use crossbeam_channel::Receiver;
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::cache;
use common::find_git_root;

/// Receive new cwd paths from the registrar, find their git roots, watch
/// .git/index + .git/HEAD, and call cache::update on every FS change.
pub fn start(rx: Receiver<PathBuf>) {
    let (event_tx, event_rx) = crossbeam_channel::unbounded::<PathBuf>();

    let mut watcher = RecommendedWatcher::new(
        move |res: notify::Result<Event>| {
            if let Ok(event) = res {
                if matches!(
                    event.kind,
                    EventKind::Modify(_) | EventKind::Create(_) | EventKind::Remove(_)
                ) {
                    for path in &event.paths {
                        // path = {git_root}/.git/index  →  parent = .git/  →  parent = git_root
                        if let Some(git_root) = path.parent().and_then(|p| p.parent()) {
                            let _ = event_tx.send(git_root.to_path_buf());
                        }
                    }
                }
            }
        },
        Config::default(),
    )
    .expect("failed to create FS watcher");

    // git_root → all registered cwds within that repo
    // Using or_insert_with to register the FS watcher on first access for each git root,
    // eliminating a separate `watched` set.
    let mut repo_cwds: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();

    loop {
        crossbeam_channel::select! {
            recv(rx) -> msg => {
                let Ok(cwd) = msg else { break };
                if let Some(git_root) = find_git_root(&cwd) {
                    let cwds = repo_cwds.entry(git_root.clone()).or_insert_with(|| {
                        let _ = watcher.watch(
                            &git_root.join(".git/index"),
                            RecursiveMode::NonRecursive,
                        );
                        let _ = watcher.watch(
                            &git_root.join(".git/HEAD"),
                            RecursiveMode::NonRecursive,
                        );
                        Vec::new()
                    });
                    if !cwds.contains(&cwd) {
                        cwds.push(cwd.clone());
                        cache::update(&cwd);
                    }
                }
            }
            recv(event_rx) -> msg => {
                let Ok(git_root) = msg else { break };
                if let Some(cwds) = repo_cwds.get(&git_root) {
                    for cwd in cwds {
                        cache::update(cwd);
                    }
                }
            }
        }
    }
}
