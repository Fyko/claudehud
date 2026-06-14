use std::fs;
use std::io::Write;
use std::path::Path;

use common::{
    hash_path, mmap_path, read_git_status, seqlock_read, watch_dir, watch_path, MMAP_SIZE,
};
use memmap2::Mmap;
use crate::input::Input;

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

/// Compute the branch + dirty tuple to render, honoring payload precedence.
///
/// Precedence:
///   1. `input.worktree.original_branch` — CC has already told us the branch
///      the user originated from; skip git introspection entirely.
///   2. `branch_and_dirty(cwd)` — derive from disk.
///
/// When precedence (1) fires, dirty is derived from cwd's `git status` (still
/// works in worktrees) and falls back to `false` when git is unavailable.
pub fn resolve_branch(input: &Input, cwd: &Path) -> Option<(String, bool)> {
    if let Some(branch) = input
        .worktree
        .as_ref()
        .and_then(|w| w.original_branch.as_deref())
    {
        let dirty = common::read_git_status(cwd).map(|(_, d)| d).unwrap_or(false);
        return Some((branch.to_string(), dirty));
    }
    branch_and_dirty(cwd)
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

    #[test]
    fn test_resolve_branch_prefers_worktree_original_branch() {
        let json = r#"{
            "cwd": "/nonexistent/path",
            "worktree": {"original_branch": "feature/from-payload"}
        }"#;
        let input: crate::input::Input = serde_json::from_str(json).unwrap();
        let cwd = std::path::Path::new("/nonexistent/path");
        let (branch, dirty) = resolve_branch(&input, cwd).expect("payload branch wins even without git");
        assert_eq!(branch, "feature/from-payload");
        assert!(!dirty, "no git → no dirty signal");
    }

    #[test]
    fn test_resolve_branch_falls_back_to_git_when_no_worktree_block() {
        // Run from within the claudehud repo — git introspection succeeds.
        let cwd = std::env::current_dir().unwrap();
        let input: crate::input::Input = serde_json::from_str("{}").unwrap();
        let result = resolve_branch(&input, &cwd);
        assert!(result.is_some(), "should fall through to branch_and_dirty in a real repo");
    }

    #[test]
    fn test_resolve_branch_payload_wins_over_real_git_branch() {
        // Even when cwd IS a real repo, payload's original_branch takes precedence.
        let cwd = std::env::current_dir().unwrap();
        let json = r#"{"worktree": {"original_branch": "from-payload"}}"#;
        let input: crate::input::Input = serde_json::from_str(json).unwrap();
        let (branch, _) = resolve_branch(&input, &cwd).unwrap();
        assert_eq!(branch, "from-payload");
    }
}
