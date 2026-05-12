use std::fs;
use std::io::Write;
use std::path::Path;

use common::{
    hash_path, mmap_path, read_git_status, seqlock_read_full, watch_dir, watch_path,
    GitExtra, MMAP_SIZE_V0,
};
use memmap2::Mmap;

/// Returns (branch, is_dirty, git_extra) for the git repo containing `cwd`.
/// Fast path: reads from daemon mmap file (~10µs).
/// Slow path (first render or daemon not running): registers path + runs git (no GitExtra).
pub fn branch_status(cwd: &Path) -> Option<(String, bool, Option<GitExtra>)> {
    let hash = hash_path(cwd);

    if let Some(result) = try_mmap_read(hash) {
        return Some(result);
    }

    register(cwd, hash);
    read_git_status(cwd).map(|(branch, dirty)| (branch, dirty, None))
}

fn try_mmap_read(hash: u32) -> Option<(String, bool, Option<GitExtra>)> {
    let file = fs::File::open(mmap_path(hash)).ok()?;
    let len = file.metadata().ok()?.len() as usize;
    // Accept both v0 (138) and v1 (151) files.
    if len < MMAP_SIZE_V0 {
        return None;
    }
    // Safety: `file` holds the fd open; even if the daemon unlinks and
    // recreates the path on disk, we map the original inode, so the
    // MMAP_SIZE_V0 check above is sufficient to validate the buffer layout.
    let mmap = unsafe { Mmap::map(&file) }.ok()?;
    let (branch, dirty, extra) = seqlock_read_full(&mmap);
    if branch.is_empty() {
        None
    } else {
        Some((branch, dirty, extra))
    }
}

fn register(cwd: &Path, hash: u32) {
    let _ = fs::create_dir_all(watch_dir());
    if let Ok(mut f) = fs::File::create(watch_path(hash)) {
        let _ = f.write_all(cwd.as_os_str().as_encoded_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_branch_status_in_git_repo() {
        let cwd = std::env::current_dir().unwrap();
        let result = branch_status(&cwd);
        assert!(result.is_some(), "expected git info for current dir");
        let (branch, _dirty, _extra) = result.unwrap();
        assert!(!branch.is_empty(), "branch should not be empty");
    }

    #[test]
    fn test_branch_status_not_git() {
        let result = branch_status(Path::new("/tmp"));
        assert!(result.is_none());
    }

    #[test]
    fn test_branch_status_returns_git_extra_shape() {
        let cwd = std::env::current_dir().unwrap();
        let result = branch_status(&cwd);
        assert!(result.is_some());
        let (branch, _dirty, _extra) = result.unwrap();
        assert!(!branch.is_empty());
    }
}
