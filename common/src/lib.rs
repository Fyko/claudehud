use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{fence, Ordering};

pub mod incidents;

pub const MMAP_SIZE: usize = 138;
pub const BRANCH_MAX: usize = 128;
pub const WATCH_DIR: &str = "/tmp/clhud-watch";

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

pub fn mmap_path(hash: u32) -> PathBuf {
    PathBuf::from(format!("/tmp/clhud-{hash}.bin"))
}

pub fn watch_path(hash: u32) -> PathBuf {
    PathBuf::from(format!("/tmp/clhud-watch/{hash}"))
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
    let head = std::fs::read_to_string(git_root.join(".git/HEAD")).ok()?;
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
    fn test_mmap_path_format() {
        let p = mmap_path(12345);
        assert_eq!(p.to_str().unwrap(), "/tmp/clhud-12345.bin");
    }

    #[test]
    fn test_watch_path_format() {
        let p = watch_path(12345);
        assert_eq!(p.to_str().unwrap(), "/tmp/clhud-watch/12345");
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
}
