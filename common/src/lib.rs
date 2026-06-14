use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{fence, Ordering};

pub mod config;
pub mod incidents;
pub mod notice;
pub mod version;

pub const MMAP_SIZE: usize = 138;
pub const BRANCH_MAX: usize = 128;

/// Runtime cache directory. Honored env override: `CLAUDEHUD_CACHE_DIR`.
/// Unix default: `/tmp`. Windows default: `%LOCALAPPDATA%\claudehud\cache`.
pub fn cache_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("CLAUDEHUD_CACHE_DIR") {
        return PathBuf::from(dir);
    }
    #[cfg(windows)]
    {
        let local = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Users\Default\AppData\Local"));
        local.join("claudehud").join("cache")
    }
    #[cfg(unix)]
    {
        PathBuf::from("/tmp")
    }
}

/// Directory under `cache_dir()` where the daemon watches for client registration markers.
pub fn watch_dir() -> PathBuf {
    cache_dir().join("clhud-watch")
}

pub fn mmap_path(hash: u32) -> PathBuf {
    mmap_path_in(&cache_dir(), hash)
}

pub fn watch_path(hash: u32) -> PathBuf {
    watch_path_in(&cache_dir(), hash)
}

/// Test seam: build mmap path under an explicit root.
pub fn mmap_path_in(root: &Path, hash: u32) -> PathBuf {
    root.join(format!("clhud-{hash}.bin"))
}

/// Test seam: build watch marker path under an explicit root.
pub fn watch_path_in(root: &Path, hash: u32) -> PathBuf {
    root.join("clhud-watch").join(hash.to_string())
}

// Layout:
// [0..8]   u64 seqlock counter (even=stable, odd=write in progress)
// [8]      u8 dirty flag
// [9]      u8 branch name length
// [10..138] [u8;128] branch name bytes (zero-padded)

/// FNV-1a 32-bit hash of a path's bytes. No external deps.
pub fn hash_path(path: &Path) -> u32 {
    let bytes = path.as_os_str().as_encoded_bytes();
    let mut hash: u32 = 2_166_136_261;
    for &b in bytes {
        hash ^= b as u32;
        hash = hash.wrapping_mul(16_777_619);
    }
    hash
}

/// Seqlock read: spin until we get a consistent even-seq snapshot.
pub fn seqlock_read(mmap: &[u8]) -> (String, bool) {
    loop {
        let seq1 = read_u64_le(mmap, 0);
        if seq1 & 1 == 1 {
            std::hint::spin_loop();
            continue;
        }
        fence(Ordering::Acquire);

        let dirty = mmap[8] != 0;
        let branch_len = (mmap[9] as usize).min(BRANCH_MAX);
        let branch = String::from_utf8_lossy(&mmap[10..10 + branch_len]).into_owned();

        fence(Ordering::Acquire);
        let seq2 = read_u64_le(mmap, 0);
        if seq1 == seq2 {
            return (branch, dirty);
        }
        std::hint::spin_loop();
    }
}

fn read_u64_le(buf: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(buf[offset..offset + 8].try_into().unwrap())
}

/// Read the current branch name and dirty flag by invoking git directly.
/// Shared by the statusline (slow path) and the daemon cache updater.
pub fn read_git_status(cwd: &Path) -> Option<(String, bool)> {
    let git_root = find_git_root(cwd)?;
    let gitdir = resolve_gitdir(&git_root)?;
    let head = std::fs::read_to_string(gitdir.join("HEAD")).ok()?;
    let branch = if let Some(b) = head.trim().strip_prefix("ref: refs/heads/") {
        b.to_owned()
    } else {
        head.trim().chars().take(7).collect()
    };
    let dirty = Command::new("git")
        .args(["--no-optional-locks", "-C"])
        .arg(cwd)
        .args(["status", "--porcelain"])
        .output()
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false);
    Some((branch, dirty))
}

/// Walk up from `path` looking for a `.git` entry (directory or worktree file). Returns the repo root.
pub fn find_git_root(path: &Path) -> Option<PathBuf> {
    let mut current = path.to_path_buf();
    loop {
        if current.join(".git").exists() {
            return Some(current);
        }
        if !current.pop() {
            return None;
        }
    }
}

/// Resolve the actual gitdir for a working-tree root.
///
/// For a regular repo this returns `<repo_root>/.git`. For a worktree (where
/// `.git` is a file containing `gitdir: <abs-or-rel-path>`), follows the
/// pointer to the per-worktree control directory under
/// `<mainrepo>/.git/worktrees/<name>`.
pub fn resolve_gitdir(repo_root: &Path) -> Option<PathBuf> {
    let dotgit = repo_root.join(".git");
    if dotgit.is_dir() {
        return Some(dotgit);
    }
    if dotgit.is_file() {
        let contents = std::fs::read_to_string(&dotgit).ok()?;
        for line in contents.lines() {
            if let Some(rest) = line.strip_prefix("gitdir: ") {
                let candidate = PathBuf::from(rest.trim());
                let resolved = if candidate.is_absolute() {
                    candidate
                } else {
                    repo_root.join(candidate)
                };
                // Silently drop pointers whose target isn't a directory (corrupted/stale worktree)
                // — the statusline is fail-soft.
                if resolved.is_dir() {
                    return Some(resolved);
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_hash_path_stable() {
        let a = hash_path(Path::new("/home/user/project"));
        let b = hash_path(Path::new("/home/user/project"));
        assert_eq!(a, b);
    }

    #[test]
    fn test_hash_path_distinct() {
        let a = hash_path(Path::new("/home/user/project"));
        let b = hash_path(Path::new("/home/user/other"));
        assert_ne!(a, b);
    }

    #[test]
    fn test_mmap_path_in_format() {
        let p = mmap_path_in(Path::new("/tmp"), 12345);
        assert_eq!(p, Path::new("/tmp/clhud-12345.bin"));
    }

    #[test]
    fn test_watch_path_in_format() {
        let p = watch_path_in(Path::new("/tmp"), 12345);
        assert_eq!(p, Path::new("/tmp/clhud-watch/12345"));
    }

    #[test]
    fn test_cache_dir_respects_env_override() {
        // SAFETY: this test mutates process env; serial with other env-mutating tests.
        // We isolate via a tempdir-style path that won't collide with real cache.
        let key = "CLAUDEHUD_CACHE_DIR";
        let prev = std::env::var_os(key);
        std::env::set_var(key, "/tmp/claudehud-test-override");
        let got = cache_dir();
        if let Some(p) = prev {
            std::env::set_var(key, p);
        } else {
            std::env::remove_var(key);
        }
        assert_eq!(got, Path::new("/tmp/claudehud-test-override"));
    }

    #[test]
    fn test_seqlock_read_stable() {
        let mut buf = [0u8; MMAP_SIZE];
        // seq=2 (even, stable), dirty=1, branch="main"
        buf[0..8].copy_from_slice(&2u64.to_le_bytes());
        buf[8] = 1;
        buf[9] = 4;
        buf[10..14].copy_from_slice(b"main");
        let (branch, dirty) = seqlock_read(&buf);
        assert_eq!(branch, "main");
        assert!(dirty);
    }

    #[test]
    fn test_find_git_root_found() {
        let cwd = std::env::current_dir().unwrap();
        let root = find_git_root(&cwd);
        assert!(root.is_some());
    }

    #[test]
    fn test_resolve_gitdir_regular_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let dotgit = tmp.path().join(".git");
        std::fs::create_dir(&dotgit).unwrap();
        std::fs::write(dotgit.join("HEAD"), "ref: refs/heads/main\n").unwrap();

        let resolved = resolve_gitdir(tmp.path()).unwrap();
        assert_eq!(resolved, dotgit);
        assert!(resolved.join("HEAD").is_file());
    }

    #[test]
    fn test_resolve_gitdir_worktree_absolute_pointer() {
        let tmp = tempfile::tempdir().unwrap();
        let real_gitdir = tmp.path().join("repo/.git/worktrees/wt-one");
        std::fs::create_dir_all(&real_gitdir).unwrap();
        std::fs::write(real_gitdir.join("HEAD"), "ref: refs/heads/feature/one\n").unwrap();

        let wt_root = tmp.path().join("wt-one");
        std::fs::create_dir(&wt_root).unwrap();
        let pointer = format!("gitdir: {}\n", real_gitdir.display());
        std::fs::write(wt_root.join(".git"), pointer).unwrap();

        let resolved = resolve_gitdir(&wt_root).unwrap();
        assert_eq!(resolved, real_gitdir);
        assert!(resolved.join("HEAD").is_file());
    }

    #[test]
    fn test_resolve_gitdir_worktree_relative_pointer() {
        let tmp = tempfile::tempdir().unwrap();
        let real_gitdir = tmp.path().join("repo/.git/worktrees/wt-one");
        std::fs::create_dir_all(&real_gitdir).unwrap();
        std::fs::write(real_gitdir.join("HEAD"), "ref: refs/heads/feature/one\n").unwrap();

        let wt_root = tmp.path().join("wt-one");
        std::fs::create_dir(&wt_root).unwrap();
        // Relative path, must be resolved against wt_root.
        std::fs::write(wt_root.join(".git"), "gitdir: ../repo/.git/worktrees/wt-one\n").unwrap();

        let resolved = resolve_gitdir(&wt_root).unwrap();
        // Resolved path contains `..` segments since the pointer is relative; compare
        // canonicalized forms to confirm we landed at the intended directory rather than
        // some other location that happens to contain a HEAD file.
        assert_eq!(resolved.canonicalize().unwrap(), real_gitdir.canonicalize().unwrap());
        assert!(resolved.join("HEAD").is_file(), "resolved gitdir must contain HEAD");
    }

    #[test]
    fn test_resolve_gitdir_missing_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(resolve_gitdir(tmp.path()).is_none());
    }

    #[test]
    fn test_read_git_status_in_worktree_returns_branch() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();

        // Init repo, make an initial commit on `main`, branch `feature/one`,
        // then `git worktree add ../wt-one feature/one`.
        let run = |cwd: &std::path::Path, args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(cwd)
                .output()
                .expect("git failed to start");
            assert!(out.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
        };
        run(&repo, &["init", "-q", "-b", "main"]);
        run(&repo, &["config", "user.email", "t@t"]);
        run(&repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("a"), "hi").unwrap();
        run(&repo, &["add", "a"]);
        run(&repo, &["commit", "-q", "-m", "init"]);
        run(&repo, &["branch", "feature/one"]);

        let wt = tmp.path().join("wt-one");
        run(&repo, &["worktree", "add", "-q", wt.to_str().unwrap(), "feature/one"]);

        let (branch, _dirty) = read_git_status(&wt).expect("worktree branch must resolve");
        assert_eq!(branch, "feature/one", "branch from worktree HEAD, not cwd basename");
    }
}
