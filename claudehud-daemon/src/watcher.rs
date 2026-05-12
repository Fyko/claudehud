use std::collections::HashMap;
use std::path::PathBuf;

use crossbeam_channel::Receiver;
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::cache;
use common::find_git_root;

/// Receive new cwd paths from the registrar, find their git roots, watch
/// .git/index + .git/HEAD and op-state sentinels, and call cache::update on every FS change.
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
                        // path may be:
                        //   {root}/.git/index            → parent×2 = root
                        //   {root}/.git/rebase-merge/foo → parent×3 = root
                        // Try both depths.
                        let git_root = path
                            .parent()
                            .and_then(|p| p.parent())
                            .filter(|p| p.join(".git").is_dir())
                            .map(|p| p.to_path_buf())
                            .or_else(|| {
                                path.parent()
                                    .and_then(|p| p.parent())
                                    .and_then(|p| p.parent())
                                    .filter(|p| p.join(".git").is_dir())
                                    .map(|p| p.to_path_buf())
                            });
                        if let Some(root) = git_root {
                            let _ = event_tx.send(root);
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
                        let dot_git = git_root.join(".git");
                        // Core status watches
                        let _ = watcher.watch(&dot_git.join("index"), RecursiveMode::NonRecursive);
                        let _ = watcher.watch(&dot_git.join("HEAD"), RecursiveMode::NonRecursive);
                        // Op-state sentinels (may not exist yet; silently ignored if absent)
                        let _ = watcher.watch(&dot_git.join("MERGE_HEAD"), RecursiveMode::NonRecursive);
                        let _ = watcher.watch(&dot_git.join("CHERRY_PICK_HEAD"), RecursiveMode::NonRecursive);
                        let _ = watcher.watch(&dot_git.join("REVERT_HEAD"), RecursiveMode::NonRecursive);
                        let _ = watcher.watch(&dot_git.join("BISECT_LOG"), RecursiveMode::NonRecursive);
                        let _ = watcher.watch(&dot_git.join("rebase-merge"), RecursiveMode::NonRecursive);
                        let _ = watcher.watch(&dot_git.join("rebase-apply"), RecursiveMode::NonRecursive);
                        // Ref updates (git fetch, local commits change these)
                        let _ = watcher.watch(&dot_git.join("packed-refs"), RecursiveMode::NonRecursive);
                        let _ = watcher.watch(&dot_git.join("refs").join("heads"), RecursiveMode::NonRecursive);
                        let _ = watcher.watch(&dot_git.join("refs").join("remotes"), RecursiveMode::NonRecursive);
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
