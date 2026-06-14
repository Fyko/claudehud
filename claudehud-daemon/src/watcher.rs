use std::collections::HashMap;
use std::path::PathBuf;

use crossbeam_channel::Receiver;
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::cache;
use common::{find_git_root, resolve_gitdir};

/// Receive new cwd paths from the registrar, find their resolved gitdir
/// (handles worktree `.git`-as-file pointers), watch `<gitdir>/index` +
/// `<gitdir>/HEAD`, and call `cache::update` on every FS change for every
/// cwd registered against that gitdir.
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
                        // path = {gitdir}/index or {gitdir}/HEAD  →  parent = gitdir
                        if let Some(gitdir) = path.parent() {
                            let _ = event_tx.send(gitdir.to_path_buf());
                        }
                    }
                }
            }
        },
        Config::default(),
    )
    .expect("failed to create FS watcher");

    // gitdir → all registered cwds whose `.git` resolves to this gitdir.
    // For regular repos: one entry per repo, one cwd inside.
    // For worktrees: one entry per *worktree* (each worktree has its own gitdir).
    let mut cwds_by_gitdir: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();

    loop {
        crossbeam_channel::select! {
            recv(rx) -> msg => {
                let Ok(cwd) = msg else { break };
                let Some(git_root) = find_git_root(&cwd) else { continue };
                let Some(gitdir) = resolve_gitdir(&git_root) else { continue };

                let cwds = cwds_by_gitdir.entry(gitdir.clone()).or_insert_with(|| {
                    let _ = watcher.watch(&gitdir.join("index"), RecursiveMode::NonRecursive);
                    let _ = watcher.watch(&gitdir.join("HEAD"), RecursiveMode::NonRecursive);
                    Vec::new()
                });
                if !cwds.contains(&cwd) {
                    cwds.push(cwd.clone());
                    cache::update(&cwd);
                }
            }
            recv(event_rx) -> msg => {
                let Ok(gitdir) = msg else { break };
                if let Some(cwds) = cwds_by_gitdir.get(&gitdir) {
                    for cwd in cwds {
                        cache::update(cwd);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::{hash_path, mmap_path_in, seqlock_read, MMAP_SIZE};
    use crossbeam_channel::unbounded;
    use std::time::{Duration, Instant};

    fn wait_for<F: Fn() -> bool>(timeout: Duration, f: F) -> bool {
        let start = Instant::now();
        while start.elapsed() < timeout {
            if f() { return true; }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    #[test]
    fn test_watcher_updates_cache_on_worktree_head_change() {
        let cache = tempfile::tempdir().unwrap();
        // SAFETY: this test mutates process env; the test runs serially with
        // other env-mutating tests in this crate (cargo test is per-process).
        std::env::set_var("CLAUDEHUD_CACHE_DIR", cache.path());

        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        let run = |cwd: &std::path::Path, args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args).current_dir(cwd).output().unwrap();
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        };
        run(&repo, &["init", "-q", "-b", "main"]);
        run(&repo, &["config", "user.email", "t@t"]);
        run(&repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("a"), "hi").unwrap();
        run(&repo, &["add", "a"]);
        run(&repo, &["commit", "-q", "-m", "init"]);
        run(&repo, &["branch", "feature/one"]);
        run(&repo, &["branch", "feature/two"]);
        let wt = tmp.path().join("wt");
        run(&repo, &["worktree", "add", "-q", wt.to_str().unwrap(), "feature/one"]);

        let (tx, rx) = unbounded();
        let _watcher_thread = std::thread::spawn(move || start(rx));
        tx.send(wt.clone()).unwrap();

        let bin = mmap_path_in(cache.path(), hash_path(&wt));
        assert!(
            wait_for(Duration::from_secs(3), || bin.exists()),
            "watcher should write a cache bin on registration"
        );

        let initial = std::fs::read(&bin).unwrap();
        let mut buf = [0u8; MMAP_SIZE];
        buf.copy_from_slice(&initial[..MMAP_SIZE]);
        let (branch, _) = seqlock_read(&buf);
        assert_eq!(branch, "feature/one");

        run(&wt, &["switch", "-q", "feature/two"]);
        assert!(
            wait_for(Duration::from_secs(3), || {
                let bytes = std::fs::read(&bin).unwrap();
                let mut buf = [0u8; MMAP_SIZE];
                buf.copy_from_slice(&bytes[..MMAP_SIZE]);
                let (b, _) = seqlock_read(&buf);
                b == "feature/two"
            }),
            "watcher should fire cache::update when worktree HEAD changes"
        );

        drop(tx);

        std::env::remove_var("CLAUDEHUD_CACHE_DIR");
    }
}
