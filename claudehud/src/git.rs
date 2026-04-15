use std::fs;
use std::io::Write;
use std::path::Path;

use common::{hash_path, mmap_path, read_git_status, seqlock_read, watch_path, MMAP_SIZE, WATCH_DIR};
use memmap2::Mmap;

/// Returns (branch, is_dirty) for the git repo containing `cwd`.
/// Fast path: reads from daemon mmap file (~10µs).
/// Slow path (first render or daemon not running): registers path + runs git.
pub fn branch_and_dirty(cwd: &Path) -> Option<(String, bool)> {
    let hash = hash_path(cwd);

    // ── Fast path: mmap ──────────────────────────────────
    if let Some(result) = try_mmap_read(hash) {
        return Some(result);
    }

    // ── Slow path: register + direct git ─────────────────
    register(cwd, hash);
    read_git_status(cwd)
}

fn try_mmap_read(hash: u32) -> Option<(String, bool)> {
    let file = fs::File::open(mmap_path(hash)).ok()?;
    if file.metadata().ok()?.len() != MMAP_SIZE as u64 {
        return None;
    }
    // Safety: `file` holds the fd open; even if the daemon unlinks and
    // recreates the path on disk, we map the original inode, so the
    // MMAP_SIZE check above is sufficient to validate the buffer layout.
    let mmap = unsafe { Mmap::map(&file) }.ok()?;
    let (branch, dirty) = seqlock_read(&mmap);
    if branch.is_empty() {
        None
    } else {
        Some((branch, dirty))
    }
}

fn register(cwd: &Path, hash: u32) {
    let _ = fs::create_dir_all(WATCH_DIR);
    if let Ok(mut f) = fs::File::create(watch_path(hash)) {
        let _ = f.write_all(cwd.as_os_str().as_encoded_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_branch_and_dirty_in_git_repo() {
        // This test runs inside the claudehud repo
        let cwd = std::env::current_dir().unwrap();
        let result = branch_and_dirty(&cwd);
        assert!(result.is_some(), "expected git info for current dir");
        let (branch, _dirty) = result.unwrap();
        assert!(!branch.is_empty(), "branch should not be empty");
    }

    #[test]
    fn test_branch_and_dirty_not_git() {
        let result = branch_and_dirty(Path::new("/tmp"));
        assert!(result.is_none());
    }
}
