use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{fence, Ordering};

pub mod incidents;

pub const MMAP_SIZE_V0: usize = 138;
pub const MMAP_SIZE: usize = 151;
pub const BRANCH_MAX: usize = 128;

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OpState {
    #[default]
    None = 0,
    Merge = 1,
    Rebase = 2,
    CherryPick = 3,
    Revert = 4,
    Bisect = 5,
}

impl OpState {
    pub fn from_u8(b: u8) -> Self {
        match b {
            1 => Self::Merge,
            2 => Self::Rebase,
            3 => Self::CherryPick,
            4 => Self::Revert,
            5 => Self::Bisect,
            _ => Self::None,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GitExtra {
    pub ahead: u32,
    pub behind: u32,
    pub op_state: OpState,
    pub op_step: u8,
    pub op_total: u8,
    pub conflict_count: u8,
}

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

/// Extended seqlock read. Returns branch, dirty, and `GitExtra` when the buffer
/// is v1 (151 bytes with version byte = 1). Returns `None` for `GitExtra` on
/// v0 (138-byte) files so old clients degrade gracefully.
pub fn seqlock_read_full(mmap: &[u8]) -> (String, bool, Option<GitExtra>) {
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

        let extra = if mmap.len() >= MMAP_SIZE && mmap[138] == 1 {
            let ahead = u32::from_le_bytes(mmap[139..143].try_into().unwrap());
            let behind = u32::from_le_bytes(mmap[143..147].try_into().unwrap());
            let op_state = OpState::from_u8(mmap[147]);
            let op_step = mmap[148];
            let op_total = mmap[149];
            let conflict_count = mmap[150];
            Some(GitExtra { ahead, behind, op_state, op_step, op_total, conflict_count })
        } else {
            None
        };

        fence(Ordering::Acquire);
        let seq2 = read_u64_le(mmap, 0);
        if seq1 == seq2 {
            return (branch, dirty, extra);
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
    fn test_op_state_roundtrip() {
        use super::OpState;
        for v in [0u8, 1, 2, 3, 4, 5] {
            assert_eq!(OpState::from_u8(OpState::from_u8(v) as u8), OpState::from_u8(v));
        }
        // unknown value maps to None
        assert_eq!(OpState::from_u8(99) as u8, 0);
    }

    #[test]
    fn test_seqlock_read_full_v0_file() {
        // 138-byte file: should return GitExtra = default (zeros)
        let mut buf = vec![0u8; MMAP_SIZE_V0];
        buf[0..8].copy_from_slice(&2u64.to_le_bytes());
        buf[8] = 1; // dirty
        buf[9] = 4;
        buf[10..14].copy_from_slice(b"main");
        let (branch, dirty, extra) = seqlock_read_full(&buf);
        assert_eq!(branch, "main");
        assert!(dirty);
        assert!(extra.is_none());
    }

    #[test]
    fn test_seqlock_read_full_v1_file() {
        use super::OpState;
        let mut buf = vec![0u8; MMAP_SIZE];
        buf[0..8].copy_from_slice(&2u64.to_le_bytes());
        buf[8] = 0; // clean
        buf[9] = 4;
        buf[10..14].copy_from_slice(b"main");
        buf[138] = 1; // version
        buf[139..143].copy_from_slice(&3u32.to_le_bytes()); // ahead=3
        buf[143..147].copy_from_slice(&1u32.to_le_bytes()); // behind=1
        buf[147] = OpState::Rebase as u8;
        buf[148] = 2; // step
        buf[149] = 5; // total
        buf[150] = 0; // no conflicts
        let (branch, dirty, extra) = seqlock_read_full(&buf);
        assert_eq!(branch, "main");
        assert!(!dirty);
        let ex = extra.unwrap();
        assert_eq!(ex.ahead, 3);
        assert_eq!(ex.behind, 1);
        assert_eq!(ex.op_state, OpState::Rebase);
        assert_eq!(ex.op_step, 2);
        assert_eq!(ex.op_total, 5);
        assert_eq!(ex.conflict_count, 0);
    }

    #[test]
    fn test_seqlock_read_full_v1_no_upstream() {
        use super::OpState;
        let mut buf = vec![0u8; MMAP_SIZE];
        buf[0..8].copy_from_slice(&2u64.to_le_bytes());
        buf[9] = 4;
        buf[10..14].copy_from_slice(b"feat");
        buf[138] = 1;
        // ahead=0, behind=0, op_state=None
        let (branch, _dirty, extra) = seqlock_read_full(&buf);
        assert_eq!(branch, "feat");
        let ex = extra.unwrap();
        assert_eq!(ex.ahead, 0);
        assert_eq!(ex.behind, 0);
        assert_eq!(ex.op_state, OpState::None);
    }
}
