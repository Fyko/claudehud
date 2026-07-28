use std::fs;
use std::io::Write;
use std::path::Path;

use crate::input::Input;
use common::{
    cache_dir, hash_path, mmap_path, read_git_status, seqlock_read, watch_path_in, MMAP_SIZE,
};
use memmap2::Mmap;

/// Returns (branch, `is_dirty`) for the git repo containing `cwd`.
/// Fast path: reads from daemon mmap file (~10µs).
/// Slow path (first render or daemon not running): registers path + runs git.
pub fn branch_and_dirty(cwd: &Path) -> Option<(String, bool)> {
    let hash = hash_path(cwd);

    // ── Fast path: mmap ──────────────────────────────────
    if let Some(result) = try_mmap_read(hash) {
        return Some(result);
    }

    // ── Slow path: register + direct git ─────────────────
    ensure_registered_for_watching(cwd, hash);
    read_git_status(cwd)
}

/// Compute the branch + dirty tuple to render, honoring payload precedence.
///
/// Precedence:
///   1. `input.worktree.original_branch` — CC has already told us the branch
///      the user originated from; we still go through `branch_and_dirty` for
///      the dirty flag so the daemon learns about this cwd (writes the watch
///      marker, hits the mmap fast path on subsequent renders) and only
///      override the branch text with the payload-supplied value.
///   2. `branch_and_dirty(cwd)` — derive everything from disk.
///
/// When `branch_and_dirty` returns `None` under precedence (1) — cwd isn't in
/// a git repo at all — we still surface the payload branch with `dirty = false`.
pub fn resolve_branch(input: &Input, cwd: &Path) -> Option<(String, bool)> {
    if let Some(branch) = input
        .worktree
        .as_ref()
        .and_then(|w| w.original_branch.as_deref())
    {
        let dirty = branch_and_dirty(cwd).map(|(_, d)| d).unwrap_or(false);
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
    #[allow(unsafe_code)]
    let mmap = unsafe { Mmap::map(&file) }.ok()?;
    let (branch, dirty) = seqlock_read(&mmap);
    if branch.is_empty() {
        None
    } else {
        Some((branch, dirty))
    }
}

/// Ensure this repo is registered for watching by the daemon.
///
/// Writes the **registration marker** under `watch_dir()` keyed by `hash`; the
/// daemon watches that directory and starts maintaining this repo's **cache
/// file** once the marker appears. This is the one side effect that wires a
/// freshly-seen `cwd` into the daemon's fast path, so it is a named step rather
/// than an invisible consequence of branch resolution: deleting it silently
/// breaks daemon registration. Degrades silently (ADR-0001): any I/O error just
/// means the repo isn't registered this render.
pub fn ensure_registered_for_watching(cwd: &Path, hash: u32) {
    ensure_registered_in(&cache_dir(), cwd, hash);
}

/// Test seam: write the registration marker under an explicit cache root,
/// avoiding the real `/tmp` (and a process-global `CLAUDEHUD_CACHE_DIR` env
/// mutation) so the marker write can be asserted in isolation.
fn ensure_registered_in(root: &Path, cwd: &Path, hash: u32) {
    let marker = watch_path_in(root, hash);
    if let Some(parent) = marker.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(mut f) = fs::File::create(marker) {
        let _ = f.write_all(cwd.as_os_str().as_encoded_bytes());
    }
}

/// Determine the base repo name for the given cwd.
///
/// Two-path precedence:
///
/// **Path A (payload):** if `input.worktree.original_cwd` is `Some`, its
/// basename is returned immediately — CC already knows the real repo root.
///
/// **Path B (payload workspace):** `workspace.git_worktree` answers "am I in a
/// linked worktree" for any git worktree, not just `--worktree` sessions, and
/// `workspace.repo.name` names the repo it belongs to. Together they replace
/// the gitdir walk below — when both are present.
///
/// **Path C (gitdir introspection):** the fallback, for worktrees of a repo
/// with no `origin` remote (where the harness omits `workspace.repo`). Walk up
/// to the git root and resolve the gitdir. Only worktrees get a prefix — if
/// `<gitdir>/commondir` exists, read that file, resolve it relative to the
/// gitdir, and return the main repo's basename. For regular repos (no
/// `commondir`) we return `None` so the dir segment renders as-was (no
/// `repo/subdir` regression on foreground renders from a repo subdirectory).
///
/// Returns `None` when cwd is not in a git repo OR when it's in a regular
/// (non-worktree) repo.
pub fn resolve_base_repo(input: &Input, cwd: &Path) -> Option<String> {
    // Path A: payload takes precedence.
    if let Some(original_cwd) = input
        .worktree
        .as_ref()
        .and_then(|w| w.original_cwd.as_deref())
    {
        return Path::new(original_cwd)
            .file_name()
            .and_then(|n| n.to_str())
            .map(std::string::ToString::to_string);
    }

    // Path B: the harness already knows both halves of the answer.
    if let Some(ws) = input.workspace.as_ref() {
        if ws.git_worktree.is_some() {
            if let Some(name) = ws.repo.as_ref().and_then(|r| r.name.as_deref()) {
                return Some(name.to_string());
            }
        }
    }

    // Path C: derive from gitdir.
    let git_root = common::find_git_root(cwd)?;
    let gitdir = common::resolve_gitdir(&git_root)?;

    let commondir_path = gitdir.join("commondir");
    if !commondir_path.exists() {
        // Regular (non-worktree) repo — no prefix. Without this guard,
        // foreground renders from a subdirectory (cwd=/repo/src) would
        // regress from `src (branch)` to `repo/src (branch)`.
        return None;
    }
    // Linked worktree. `commondir` contains a relative (or absolute) path
    // from `gitdir` to the common gitdir.
    let contents = std::fs::read_to_string(&commondir_path).ok()?;
    let pointer = contents.trim();
    let common_gitdir = {
        let p = Path::new(pointer);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            gitdir.join(p)
        }
    };
    let resolved = common_gitdir.canonicalize().ok()?;
    // resolved is the main .git dir; its parent is the repo root.
    resolved
        .parent()?
        .file_name()
        .and_then(|n| n.to_str())
        .map(std::string::ToString::to_string)
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
    fn test_ensure_registered_writes_marker_for_unwatched_repo() {
        // An un-watched repo: the cache root has no marker yet. The named
        // registration step must create it (cwd encoded as the contents) so the
        // daemon picks the repo up. No real /tmp, no env mutation.
        let root = tempfile::tempdir().unwrap();
        let cwd = Path::new("/some/unwatched/repo");
        let hash = common::hash_path(cwd);
        let marker = common::watch_path_in(root.path(), hash);
        assert!(!marker.exists(), "precondition: repo is un-watched");

        ensure_registered_in(root.path(), cwd, hash);

        assert!(marker.exists(), "registration marker must be written");
        let contents = std::fs::read(&marker).unwrap();
        assert_eq!(
            contents,
            cwd.as_os_str().as_encoded_bytes(),
            "marker holds the cwd so the daemon knows which repo to watch"
        );
    }

    #[test]
    fn test_resolve_branch_prefers_worktree_original_branch() {
        let json = r#"{
            "cwd": "/nonexistent/path",
            "worktree": {"original_branch": "feature/from-payload"}
        }"#;
        let input: crate::input::Input = serde_json::from_str(json).unwrap();
        let cwd = std::path::Path::new("/nonexistent/path");
        let (branch, dirty) =
            resolve_branch(&input, cwd).expect("payload branch wins even without git");
        assert_eq!(branch, "feature/from-payload");
        assert!(!dirty, "no git → no dirty signal");
    }

    #[test]
    fn test_resolve_branch_falls_back_to_git_when_no_worktree_block() {
        // Run from within the claudehud repo — git introspection succeeds.
        let cwd = std::env::current_dir().unwrap();
        let input: crate::input::Input = serde_json::from_str("{}").unwrap();
        let result = resolve_branch(&input, &cwd);
        assert!(
            result.is_some(),
            "should fall through to branch_and_dirty in a real repo"
        );
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

    // ── resolve_base_repo tests ───────────────────────────────────────────────

    #[test]
    fn test_resolve_base_repo_from_payload_original_cwd() {
        // Path A: payload's original_cwd wins even with a nonexistent cwd.
        let json = r#"{"worktree": {"original_cwd": "/Users/foo/hellopatient"}}"#;
        let input: crate::input::Input = serde_json::from_str(json).unwrap();
        let cwd = Path::new("/nonexistent/path/that/has/no/git");
        let result = resolve_base_repo(&input, cwd);
        assert_eq!(result, Some("hellopatient".to_string()));
    }

    #[test]
    fn test_resolve_base_repo_from_payload_workspace() {
        // Path B: git_worktree + repo.name, no filesystem access at all.
        let json = r#"{
            "workspace": {"git_worktree": "feature-xyz", "repo": {"name": "claudehud"}}
        }"#;
        let input: crate::input::Input = serde_json::from_str(json).unwrap();
        let cwd = Path::new("/nonexistent/path/that/has/no/git");
        assert_eq!(
            resolve_base_repo(&input, cwd),
            Some("claudehud".to_string())
        );
    }

    #[test]
    fn test_resolve_base_repo_ignores_repo_name_outside_a_worktree() {
        // `repo` is present in every git repo; only `git_worktree` means the
        // dir segment needs a prefix. Without this guard a plain repo would
        // start rendering `claudehud/claudehud`.
        let json = r#"{"workspace": {"repo": {"name": "claudehud"}}}"#;
        let input: crate::input::Input = serde_json::from_str(json).unwrap();
        let cwd = Path::new("/nonexistent/path/that/has/no/git");
        assert_eq!(resolve_base_repo(&input, cwd), None);
    }

    #[test]
    fn test_resolve_base_repo_original_cwd_beats_workspace() {
        let json = r#"{
            "worktree": {"original_cwd": "/Users/foo/hellopatient"},
            "workspace": {"git_worktree": "wt", "repo": {"name": "claudehud"}}
        }"#;
        let input: crate::input::Input = serde_json::from_str(json).unwrap();
        assert_eq!(
            resolve_base_repo(&input, Path::new("/nope")),
            Some("hellopatient".to_string())
        );
    }

    #[test]
    fn test_resolve_base_repo_from_worktree_gitdir() {
        use std::process::Command;

        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("myrepo");
        std::fs::create_dir(&repo).unwrap();

        let run = |cwd: &std::path::Path, args: &[&str]| {
            let out = Command::new("git")
                .args(args)
                .current_dir(cwd)
                .output()
                .expect("git failed to start");
            assert!(
                out.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };

        run(&repo, &["init", "-q", "-b", "main"]);
        run(&repo, &["config", "user.email", "t@t"]);
        run(&repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("a"), "hi").unwrap();
        run(&repo, &["add", "a"]);
        run(&repo, &["commit", "-q", "-m", "init"]);
        run(&repo, &["branch", "feature/wt"]);

        let wt = tmp.path().join("wt-one");
        run(
            &repo,
            &["worktree", "add", "-q", wt.to_str().unwrap(), "feature/wt"],
        );

        let input: crate::input::Input = serde_json::from_str("{}").unwrap();
        let result = resolve_base_repo(&input, &wt);
        assert_eq!(
            result,
            Some("myrepo".to_string()),
            "base repo name should be the main repo's basename"
        );

        // The reason Path C survives: this repo has no `origin`, so the
        // harness sends `git_worktree` with no `repo` to pair it with.
        let no_origin: crate::input::Input =
            serde_json::from_str(r#"{"workspace": {"git_worktree": "wt-one"}}"#).unwrap();
        assert_eq!(
            resolve_base_repo(&no_origin, &wt),
            Some("myrepo".to_string()),
            "should fall through to gitdir introspection"
        );
    }

    #[test]
    fn test_resolve_base_repo_returns_none_in_regular_repo() {
        // Regular (non-worktree) repos return None so the render layer
        // doesn't add a prefix that would regress foreground subdirectory
        // renders (cwd=/repo/src would otherwise become `repo/src`).
        use std::process::Command;

        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("myregularepo");
        std::fs::create_dir(&repo).unwrap();

        let run = |cwd: &std::path::Path, args: &[&str]| {
            let out = Command::new("git")
                .args(args)
                .current_dir(cwd)
                .output()
                .expect("git failed to start");
            assert!(
                out.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };

        run(&repo, &["init", "-q", "-b", "main"]);
        run(&repo, &["config", "user.email", "t@t"]);
        run(&repo, &["config", "user.name", "t"]);

        let input: crate::input::Input = serde_json::from_str("{}").unwrap();
        let result = resolve_base_repo(&input, &repo);
        assert_eq!(result, None, "regular repo should not produce a prefix");
    }

    #[test]
    fn test_resolve_base_repo_returns_none_from_subdir_of_regular_repo() {
        // Regression guard: rendering from /repo/src in a regular repo must
        // NOT produce a `repo/src` prefix.
        use std::process::Command;

        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("myrepo");
        let subdir = repo.join("src");
        std::fs::create_dir_all(&subdir).unwrap();

        let run = |cwd: &std::path::Path, args: &[&str]| {
            let out = Command::new("git")
                .args(args)
                .current_dir(cwd)
                .output()
                .expect("git failed to start");
            assert!(
                out.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };

        run(&repo, &["init", "-q", "-b", "main"]);
        run(&repo, &["config", "user.email", "t@t"]);
        run(&repo, &["config", "user.name", "t"]);

        let input: crate::input::Input = serde_json::from_str("{}").unwrap();
        let result = resolve_base_repo(&input, &subdir);
        assert_eq!(
            result, None,
            "subdir of a regular repo must not get a `repo/` prefix"
        );
    }
}
