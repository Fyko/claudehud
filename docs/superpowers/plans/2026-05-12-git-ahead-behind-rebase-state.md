# Git Ahead/Behind and Operation State Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extend the daemon cache and statusline client to surface ahead/behind upstream commit counts, in-progress git operation state (merge/rebase/cherry-pick/revert/bisect), and conflict count.

**Architecture:** The cache file grows from 138 to 151 bytes with a layout version byte and 12 bytes of new fields (ahead u32, behind u32, op_state u8, op_step u8, op_total u8, conflict_count u8). The daemon gains a new `git_extra.rs` module that shells to git for ahead/behind and reads `.git/` sentinel files for operation state. The watcher adds extra watch targets so fetches and in-progress operations trigger cache updates. The client render reads `GitExtra` from the extended mmap and inserts ahead/behind + op-state badges into both comfortable and condensed layouts.

**Tech Stack:** Rust 2021, `std::process::Command`, `memmap2`, `notify`, `crossbeam-channel` (all already present).

**Reference spec:** `docs/superpowers/specs/2026-05-12-git-ahead-behind-rebase-state.md`

---

## File Structure

**New files:**
- `claudehud-daemon/src/git_extra.rs` — ahead/behind shell invocation + op-state detection from `.git/` sentinels

**Modified files:**
- `common/src/lib.rs` — bump `MMAP_SIZE`, add `MMAP_SIZE_V0`, `OpState` enum, `GitExtra` struct, `seqlock_read_full`
- `claudehud-daemon/src/cache.rs` — extend `seqlock_write` to accept `GitExtra`, update file sizing
- `claudehud-daemon/src/watcher.rs` — register additional watch targets per git root
- `claudehud-daemon/src/main.rs` — `mod git_extra;`
- `claudehud/src/git.rs` — switch fast path to `seqlock_read_full`, expose `GitExtra` to caller
- `claudehud/src/render.rs` — render ahead/behind + op-state badge in both layouts; add fixture tests
- `README.md` — update cache layout table to 151-byte layout

**Test surfaces:** inline `#[cfg(test)] mod tests` in each modified/created file, matching existing project pattern.

---

## Task 1: `OpState` + `GitExtra` types and extended seqlock in `common`

**Files:**
- Modify: `common/src/lib.rs`

- [ ] **Step 1: Write the failing tests**

Append inside the `#[cfg(test)] mod tests` block at the bottom of `common/src/lib.rs`:

```rust
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
        use super::{GitExtra, OpState};
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
        use super::{GitExtra, OpState};
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
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p common test_op_state_roundtrip test_seqlock_read_full 2>&1 | head -30
```
Expected: compile error — `MMAP_SIZE_V0`, `OpState`, `GitExtra`, `seqlock_read_full` not defined.

- [ ] **Step 3: Implement the types and constants**

In `common/src/lib.rs`, replace the existing constant block and add new types. The existing `MMAP_SIZE` constant moves to `MMAP_SIZE_V0`. Add after the existing `use` statements:

```rust
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
```

- [ ] **Step 4: Add `seqlock_read_full`**

Add this function after the existing `seqlock_read` function in `common/src/lib.rs`:

```rust
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
```

- [ ] **Step 5: Run tests to verify they pass**

```bash
cargo test -p common 2>&1 | tail -20
```
Expected: all tests pass including new ones.

- [ ] **Step 6: Commit**

```bash
git add common/src/lib.rs
git commit -m "feat(common): OpState, GitExtra types and seqlock_read_full"
```

---

## Task 2: Extend daemon cache writer

**Files:**
- Modify: `claudehud-daemon/src/cache.rs`

- [ ] **Step 1: Write the failing tests**

Append inside the `#[cfg(test)] mod tests` block in `claudehud-daemon/src/cache.rs`:

```rust
    #[test]
    fn test_seqlock_write_with_extra_roundtrip() {
        use common::{seqlock_read_full, GitExtra, OpState, MMAP_SIZE};
        let mut buf = vec![0u8; MMAP_SIZE];
        let extra = GitExtra {
            ahead: 3,
            behind: 1,
            op_state: OpState::Rebase,
            op_step: 2,
            op_total: 5,
            conflict_count: 0,
        };
        seqlock_write(&mut buf, "main", true, &extra);
        let (branch, dirty, got_extra) = seqlock_read_full(&buf);
        assert_eq!(branch, "main");
        assert!(dirty);
        let ex = got_extra.unwrap();
        assert_eq!(ex.ahead, 3);
        assert_eq!(ex.behind, 1);
        assert_eq!(ex.op_state, OpState::Rebase);
        assert_eq!(ex.op_step, 2);
        assert_eq!(ex.op_total, 5);
        assert_eq!(ex.conflict_count, 0);
    }

    #[test]
    fn test_seqlock_write_merge_state() {
        use common::{seqlock_read_full, GitExtra, OpState, MMAP_SIZE};
        let mut buf = vec![0u8; MMAP_SIZE];
        let extra = GitExtra {
            op_state: OpState::Merge,
            conflict_count: 3,
            ..Default::default()
        };
        seqlock_write(&mut buf, "feat", false, &extra);
        let (_, _, got_extra) = seqlock_read_full(&buf);
        let ex = got_extra.unwrap();
        assert_eq!(ex.op_state, OpState::Merge);
        assert_eq!(ex.conflict_count, 3);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p claudehud-daemon test_seqlock_write_with_extra_roundtrip test_seqlock_write_merge_state 2>&1 | head -30
```
Expected: compile error — `seqlock_write` has wrong arity.

- [ ] **Step 3: Update `seqlock_write` and `update` in `cache.rs`**

Replace the full contents of `claudehud-daemon/src/cache.rs` with:

```rust
use std::fs::OpenOptions;
use std::path::Path;
use std::sync::atomic::{fence, Ordering};

use common::{hash_path, mmap_path, read_git_status, GitExtra, BRANCH_MAX, MMAP_SIZE};
use memmap2::MmapMut;

use crate::git_extra::read_git_extra;

/// Re-run git status for `cwd` and write result to the mmap cache file.
pub fn update(cwd: &Path) {
    let Some((branch, dirty)) = read_git_status(cwd) else {
        return;
    };
    let git_root = match common::find_git_root(cwd) {
        Some(r) => r,
        None => return,
    };
    let extra = read_git_extra(&git_root);
    let hash = hash_path(cwd);
    let path = mmap_path(hash);

    let file = match OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
    {
        Ok(f) => f,
        Err(_) => return,
    };
    if file.set_len(MMAP_SIZE as u64).is_err() {
        return;
    }
    let mut mmap = match unsafe { MmapMut::map_mut(&file) } {
        Ok(m) if m.len() >= MMAP_SIZE => m,
        _ => return,
    };
    seqlock_write(&mut mmap[..], &branch, dirty, &extra);
}

/// Write branch + dirty + extra to a raw byte slice using a seqlock protocol.
/// Exported (pub) for testing with plain Vec<u8>.
pub fn seqlock_write(buf: &mut [u8], branch: &str, dirty: bool, extra: &GitExtra) {
    let seq = u64::from_le_bytes(buf[0..8].try_into().unwrap());
    buf[0..8].copy_from_slice(&seq.wrapping_add(1).to_le_bytes());
    fence(Ordering::SeqCst);

    buf[8] = dirty as u8;
    let bytes = branch.as_bytes();
    let len = bytes.len().min(BRANCH_MAX);
    buf[9] = len as u8;
    buf[10..10 + len].copy_from_slice(&bytes[..len]);
    buf[10 + len..10 + BRANCH_MAX].fill(0);

    // v1 extension
    buf[138] = 1;
    buf[139..143].copy_from_slice(&extra.ahead.to_le_bytes());
    buf[143..147].copy_from_slice(&extra.behind.to_le_bytes());
    buf[147] = extra.op_state as u8;
    buf[148] = extra.op_step;
    buf[149] = extra.op_total;
    buf[150] = extra.conflict_count;

    fence(Ordering::SeqCst);
    let seq2 = u64::from_le_bytes(buf[0..8].try_into().unwrap());
    buf[0..8].copy_from_slice(&seq2.wrapping_add(1).to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_seqlock_write_readable() {
        use common::{seqlock_read, GitExtra, MMAP_SIZE};
        let mut buf = vec![0u8; MMAP_SIZE];
        seqlock_write(&mut buf, "feature-branch", true, &GitExtra::default());
        let (branch, dirty) = seqlock_read(&buf);
        assert_eq!(branch, "feature-branch");
        assert!(dirty);
    }

    #[test]
    fn test_seqlock_write_clean() {
        use common::{seqlock_read, GitExtra, MMAP_SIZE};
        let mut buf = vec![0u8; MMAP_SIZE];
        seqlock_write(&mut buf, "main", false, &GitExtra::default());
        let (branch, dirty) = seqlock_read(&buf);
        assert_eq!(branch, "main");
        assert!(!dirty);
    }

    #[test]
    fn test_seqlock_write_with_extra_roundtrip() {
        use common::{seqlock_read_full, GitExtra, OpState, MMAP_SIZE};
        let mut buf = vec![0u8; MMAP_SIZE];
        let extra = GitExtra {
            ahead: 3,
            behind: 1,
            op_state: OpState::Rebase,
            op_step: 2,
            op_total: 5,
            conflict_count: 0,
        };
        seqlock_write(&mut buf, "main", true, &extra);
        let (branch, dirty, got_extra) = seqlock_read_full(&buf);
        assert_eq!(branch, "main");
        assert!(dirty);
        let ex = got_extra.unwrap();
        assert_eq!(ex.ahead, 3);
        assert_eq!(ex.behind, 1);
        assert_eq!(ex.op_state, OpState::Rebase);
        assert_eq!(ex.op_step, 2);
        assert_eq!(ex.op_total, 5);
        assert_eq!(ex.conflict_count, 0);
    }

    #[test]
    fn test_seqlock_write_merge_state() {
        use common::{seqlock_read_full, GitExtra, OpState, MMAP_SIZE};
        let mut buf = vec![0u8; MMAP_SIZE];
        let extra = GitExtra {
            op_state: OpState::Merge,
            conflict_count: 3,
            ..Default::default()
        };
        seqlock_write(&mut buf, "feat", false, &extra);
        let (_, _, got_extra) = seqlock_read_full(&buf);
        let ex = got_extra.unwrap();
        assert_eq!(ex.op_state, OpState::Merge);
        assert_eq!(ex.conflict_count, 3);
    }
}
```

- [ ] **Step 4: Run tests**

```bash
cargo test -p claudehud-daemon 2>&1 | tail -20
```
Expected: compile error — `crate::git_extra` not found. That's fine; we'll add it next.

- [ ] **Step 5: Add `mod git_extra;` stub**

In `claudehud-daemon/src/main.rs`, add `mod git_extra;` after `mod cache;`. Then create `claudehud-daemon/src/git_extra.rs` with a temporary stub:

```rust
use std::path::Path;
use common::GitExtra;

pub fn read_git_extra(_git_root: &Path) -> GitExtra {
    GitExtra::default()
}
```

- [ ] **Step 6: Run tests again**

```bash
cargo test -p claudehud-daemon 2>&1 | tail -20
```
Expected: all tests pass.

- [ ] **Step 7: Commit**

```bash
git add claudehud-daemon/src/cache.rs claudehud-daemon/src/main.rs claudehud-daemon/src/git_extra.rs
git commit -m "feat(daemon/cache): extend seqlock_write to write GitExtra v1 fields"
```

---

## Task 3: `git_extra.rs` — operation state + ahead/behind detection

**Files:**
- Modify: `claudehud-daemon/src/git_extra.rs`

- [ ] **Step 1: Write the failing tests**

Replace the stub `claudehud-daemon/src/git_extra.rs` with the full implementation and tests below. The tests use a tempdir fake `.git/` directory — no actual git repo needed.

```rust
use std::path::Path;
use std::process::Command;

use common::{GitExtra, OpState};

pub fn read_git_extra(git_root: &Path) -> GitExtra {
    let dot_git = git_root.join(".git");
    let op_state = detect_op_state(&dot_git);
    let (op_step, op_total) = if op_state == OpState::Rebase {
        read_rebase_progress(&dot_git)
    } else {
        (0, 0)
    };
    let conflict_count = if matches!(op_state, OpState::Merge | OpState::Rebase | OpState::CherryPick) {
        count_conflicts(git_root)
    } else {
        0
    };
    let (ahead, behind) = read_ahead_behind(git_root);
    GitExtra { ahead, behind, op_state, op_step, op_total, conflict_count }
}

fn detect_op_state(dot_git: &Path) -> OpState {
    if dot_git.join("MERGE_HEAD").exists() {
        return OpState::Merge;
    }
    if dot_git.join("rebase-merge").is_dir() || dot_git.join("rebase-apply").is_dir() {
        return OpState::Rebase;
    }
    if dot_git.join("CHERRY_PICK_HEAD").exists() {
        return OpState::CherryPick;
    }
    if dot_git.join("REVERT_HEAD").exists() {
        return OpState::Revert;
    }
    if dot_git.join("BISECT_LOG").exists() {
        return OpState::Bisect;
    }
    OpState::None
}

fn read_rebase_progress(dot_git: &Path) -> (u8, u8) {
    // rebase-merge uses "msgnum" / "end"; rebase-apply uses "next" / "last"
    let (step_file, total_file) = if dot_git.join("rebase-merge").is_dir() {
        (dot_git.join("rebase-merge/msgnum"), dot_git.join("rebase-merge/end"))
    } else {
        (dot_git.join("rebase-apply/next"), dot_git.join("rebase-apply/last"))
    };
    let step = read_u8_file(&step_file);
    let total = read_u8_file(&total_file);
    (step, total)
}

fn read_u8_file(path: &Path) -> u8 {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| s.trim().parse::<u8>().ok())
        .unwrap_or(0)
}

fn count_conflicts(git_root: &Path) -> u8 {
    let out = Command::new("git")
        .args(["--no-optional-locks", "-C"])
        .arg(git_root)
        .args(["ls-files", "--unmerged", "-z"])
        .output()
        .unwrap_or_default();
    if out.stdout.is_empty() {
        return 0;
    }
    // Each unmerged path appears 2-3 times (one per stage). Count unique paths
    // by collecting NUL-separated entries and deduplicating the filename portion.
    let mut seen = std::collections::HashSet::new();
    for entry in out.stdout.split(|&b| b == 0) {
        // Format: "mode SP hash SP stage TAB path"
        if let Some(tab) = entry.iter().position(|&b| b == b'\t') {
            seen.insert(entry[tab + 1..].to_vec());
        }
    }
    seen.len().min(u8::MAX as usize) as u8
}

fn read_ahead_behind(git_root: &Path) -> (u32, u32) {
    let out = Command::new("git")
        .args(["--no-optional-locks", "-C"])
        .arg(git_root)
        .args(["rev-list", "--count", "--left-right", "@{upstream}...HEAD"])
        .output()
        .unwrap_or_default();
    if !out.status.success() {
        return (0, 0);
    }
    let s = String::from_utf8_lossy(&out.stdout);
    let mut parts = s.trim().split_whitespace();
    let behind: u32 = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    let ahead: u32 = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    (ahead, behind)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn make_fake_git(tmp: &TempDir) -> std::path::PathBuf {
        let dot_git = tmp.path().join(".git");
        fs::create_dir_all(&dot_git).unwrap();
        dot_git
    }

    #[test]
    fn test_detect_no_op() {
        let tmp = TempDir::new().unwrap();
        let dot_git = make_fake_git(&tmp);
        assert_eq!(detect_op_state(&dot_git), OpState::None);
    }

    #[test]
    fn test_detect_merge() {
        let tmp = TempDir::new().unwrap();
        let dot_git = make_fake_git(&tmp);
        fs::write(dot_git.join("MERGE_HEAD"), "abc123\n").unwrap();
        assert_eq!(detect_op_state(&dot_git), OpState::Merge);
    }

    #[test]
    fn test_detect_rebase_merge_dir() {
        let tmp = TempDir::new().unwrap();
        let dot_git = make_fake_git(&tmp);
        fs::create_dir_all(dot_git.join("rebase-merge")).unwrap();
        assert_eq!(detect_op_state(&dot_git), OpState::Rebase);
    }

    #[test]
    fn test_detect_rebase_apply_dir() {
        let tmp = TempDir::new().unwrap();
        let dot_git = make_fake_git(&tmp);
        fs::create_dir_all(dot_git.join("rebase-apply")).unwrap();
        assert_eq!(detect_op_state(&dot_git), OpState::Rebase);
    }

    #[test]
    fn test_detect_cherry_pick() {
        let tmp = TempDir::new().unwrap();
        let dot_git = make_fake_git(&tmp);
        fs::write(dot_git.join("CHERRY_PICK_HEAD"), "abc\n").unwrap();
        assert_eq!(detect_op_state(&dot_git), OpState::CherryPick);
    }

    #[test]
    fn test_detect_revert() {
        let tmp = TempDir::new().unwrap();
        let dot_git = make_fake_git(&tmp);
        fs::write(dot_git.join("REVERT_HEAD"), "abc\n").unwrap();
        assert_eq!(detect_op_state(&dot_git), OpState::Revert);
    }

    #[test]
    fn test_detect_bisect() {
        let tmp = TempDir::new().unwrap();
        let dot_git = make_fake_git(&tmp);
        fs::write(dot_git.join("BISECT_LOG"), "git bisect start\n").unwrap();
        assert_eq!(detect_op_state(&dot_git), OpState::Bisect);
    }

    #[test]
    fn test_rebase_progress_merge_dir() {
        let tmp = TempDir::new().unwrap();
        let dot_git = make_fake_git(&tmp);
        let rb = dot_git.join("rebase-merge");
        fs::create_dir_all(&rb).unwrap();
        fs::write(rb.join("msgnum"), "2\n").unwrap();
        fs::write(rb.join("end"), "5\n").unwrap();
        let (step, total) = read_rebase_progress(&dot_git);
        assert_eq!(step, 2);
        assert_eq!(total, 5);
    }

    #[test]
    fn test_rebase_progress_apply_dir() {
        let tmp = TempDir::new().unwrap();
        let dot_git = make_fake_git(&tmp);
        let ra = dot_git.join("rebase-apply");
        fs::create_dir_all(&ra).unwrap();
        fs::write(ra.join("next"), "3\n").unwrap();
        fs::write(ra.join("last"), "7\n").unwrap();
        let (step, total) = read_rebase_progress(&dot_git);
        assert_eq!(step, 3);
        assert_eq!(total, 7);
    }

    #[test]
    fn test_rebase_progress_missing_files() {
        let tmp = TempDir::new().unwrap();
        let dot_git = make_fake_git(&tmp);
        fs::create_dir_all(dot_git.join("rebase-merge")).unwrap();
        // no msgnum / end files
        let (step, total) = read_rebase_progress(&dot_git);
        assert_eq!(step, 0);
        assert_eq!(total, 0);
    }

    #[test]
    fn test_read_ahead_behind_parse() {
        // Unit-test the parser directly by calling the fn with a repo that
        // has no upstream — it should return (0, 0) without panicking.
        let cwd = std::env::current_dir().unwrap();
        let root = common::find_git_root(&cwd).unwrap();
        // Just verify it returns something without panicking.
        let (ahead, behind) = read_ahead_behind(&root);
        let _ = (ahead, behind); // values depend on real repo state
    }
}
```

- [ ] **Step 2: Add `tempfile` dev dependency**

In `claudehud-daemon/Cargo.toml`, add to `[dev-dependencies]`:

```toml
tempfile = "3"
```

Also add it to `[workspace.dependencies]` in the root `Cargo.toml` so it's version-pinned:

```toml
tempfile = "3"
```

- [ ] **Step 3: Run tests**

```bash
cargo test -p claudehud-daemon git_extra 2>&1 | tail -30
```
Expected: all `git_extra` tests pass.

- [ ] **Step 4: Commit**

```bash
git add claudehud-daemon/src/git_extra.rs claudehud-daemon/Cargo.toml Cargo.toml Cargo.lock
git commit -m "feat(daemon): git_extra module — op-state detection and ahead/behind"
```

---

## Task 4: Extend watcher watch targets

**Files:**
- Modify: `claudehud-daemon/src/watcher.rs`

- [ ] **Step 1: Update `watcher.rs` to watch additional `.git/` paths**

The existing `or_insert_with` closure registers `index` + `HEAD`. Extend it to also watch the sentinel files and ref dirs. Replace the `or_insert_with` closure body:

```rust
let cwds = repo_cwds.entry(git_root.clone()).or_insert_with(|| {
    let dot_git = git_root.join(".git");
    // Existing watches
    let _ = watcher.watch(&dot_git.join("index"), RecursiveMode::NonRecursive);
    let _ = watcher.watch(&dot_git.join("HEAD"), RecursiveMode::NonRecursive);
    // Op-state sentinels
    let _ = watcher.watch(&dot_git.join("MERGE_HEAD"), RecursiveMode::NonRecursive);
    let _ = watcher.watch(&dot_git.join("CHERRY_PICK_HEAD"), RecursiveMode::NonRecursive);
    let _ = watcher.watch(&dot_git.join("REVERT_HEAD"), RecursiveMode::NonRecursive);
    let _ = watcher.watch(&dot_git.join("BISECT_LOG"), RecursiveMode::NonRecursive);
    let _ = watcher.watch(&dot_git.join("rebase-merge"), RecursiveMode::NonRecursive);
    let _ = watcher.watch(&dot_git.join("rebase-apply"), RecursiveMode::NonRecursive);
    // Ref updates (git fetch, local commits)
    let _ = watcher.watch(&dot_git.join("packed-refs"), RecursiveMode::NonRecursive);
    let _ = watcher.watch(&dot_git.join("refs").join("heads"), RecursiveMode::NonRecursive);
    let _ = watcher.watch(&dot_git.join("refs").join("remotes"), RecursiveMode::NonRecursive);
    Vec::new()
});
```

Note: `watcher.watch` on non-existent paths returns an error which we discard with `let _ =`. This is intentional and matches the existing pattern — if `MERGE_HEAD` doesn't exist, the watch silently no-ops and we'll catch the sentinel file creation via the parent `.git/` directory's existing index watch anyway (most git ops also touch the index).

- [ ] **Step 2: Verify the event path resolution still works**

The event callback derives `git_root` via `path.parent().and_then(|p| p.parent())`. For top-level files (`MERGE_HEAD`, `packed-refs`) the chain is:
```
{git_root}/.git/MERGE_HEAD → parent = .git/ → parent = git_root ✓
```
For subdirectory files (`rebase-merge/msgnum`):
```
{git_root}/.git/rebase-merge/msgnum → parent = rebase-merge/ → parent = .git/ ≠ git_root ✗
```

This means rebase step updates (msgnum changes) won't trigger an update via the subdirectory path. Fix this by also adjusting the event path extraction to handle one extra level. Replace the event callback's path handling in `watcher.rs`:

```rust
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
}
```

- [ ] **Step 3: Build check**

```bash
cargo build -p claudehud-daemon 2>&1 | tail -20
```
Expected: builds cleanly.

- [ ] **Step 4: Commit**

```bash
git add claudehud-daemon/src/watcher.rs
git commit -m "feat(daemon/watcher): watch op-state sentinels and ref dirs"
```

---

## Task 5: Client `git.rs` — expose `GitExtra` on fast path

**Files:**
- Modify: `claudehud/src/git.rs`

- [ ] **Step 1: Write the failing tests**

Append inside the `#[cfg(test)] mod tests` block in `claudehud/src/git.rs`:

```rust
    #[test]
    fn test_branch_status_returns_git_extra_shape() {
        let cwd = std::env::current_dir().unwrap();
        // This may hit the slow path (no mmap in test env), but should not panic.
        let result = branch_status(&cwd);
        assert!(result.is_some());
        let (branch, _dirty, _extra) = result.unwrap();
        assert!(!branch.is_empty());
    }

    #[test]
    fn test_branch_status_not_git() {
        let result = branch_status(std::path::Path::new("/tmp"));
        assert!(result.is_none());
    }
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p claudehud test_branch_status 2>&1 | head -20
```
Expected: compile error — `branch_status` not defined.

- [ ] **Step 3: Update `git.rs`**

Replace the full contents of `claudehud/src/git.rs`:

```rust
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
```

- [ ] **Step 4: Update call site in `claudehud/src/main.rs`**

Find the call to `branch_and_dirty` in `main.rs` and replace it with `branch_status`. The return type changes from `Option<(String, bool)>` to `Option<(String, bool, Option<GitExtra>)>`. Pass only `(branch, dirty)` to `render` for now (we'll wire up `extra` in Task 6):

```rust
// Before:
let git = crate::git::branch_and_dirty(&cwd);

// After:
let git_full = crate::git::branch_status(&cwd);
let git = git_full.as_ref().map(|(b, d, _)| (b.clone(), *d));
let git_extra = git_full.as_ref().and_then(|(_, _, e)| e.as_ref());
```

Also add `use common::GitExtra;` if needed.

- [ ] **Step 5: Run tests**

```bash
cargo test -p claudehud 2>&1 | tail -20
```
Expected: all tests pass. (`render` tests still pass because `git` tuple shape is unchanged for now.)

- [ ] **Step 6: Commit**

```bash
git add claudehud/src/git.rs claudehud/src/main.rs
git commit -m "feat(client/git): branch_status exposes GitExtra from extended mmap"
```

---

## Task 6: Update `render.rs` signatures and wiring

**Files:**
- Modify: `claudehud/src/render.rs`

- [ ] **Step 1: Write the failing render tests**

Append inside the `#[cfg(test)] mod tests` block in `claudehud/src/render.rs`:

```rust
    // ── Ahead/behind tests ────────────────────────────────────────────────────

    fn make_extra(ahead: u32, behind: u32, op_state: common::OpState, op_step: u8, op_total: u8, conflict_count: u8) -> common::GitExtra {
        common::GitExtra { ahead, behind, op_state, op_step, op_total, conflict_count }
    }

    #[test]
    fn test_render_ahead_only() {
        let input = Input::default();
        let extra = make_extra(3, 0, common::OpState::None, 0, 0, 0);
        let plain = strip_ansi(&render(
            &input,
            Some(("main".to_string(), false)),
            Some(&extra),
            &[],
            0,
            RoundingMode::Floor,
            Layout::Comfortable,
        ));
        assert!(plain.contains("↑3"), "ahead arrow should appear");
        assert!(!plain.contains("↓"), "no behind arrow when behind=0");
    }

    #[test]
    fn test_render_behind_only() {
        let input = Input::default();
        let extra = make_extra(0, 2, common::OpState::None, 0, 0, 0);
        let plain = strip_ansi(&render(
            &input,
            Some(("main".to_string(), false)),
            Some(&extra),
            &[],
            0,
            RoundingMode::Floor,
            Layout::Comfortable,
        ));
        assert!(plain.contains("↓2"), "behind arrow should appear");
        assert!(!plain.contains("↑"), "no ahead arrow when ahead=0");
    }

    #[test]
    fn test_render_ahead_and_behind() {
        let input = Input::default();
        let extra = make_extra(3, 1, common::OpState::None, 0, 0, 0);
        let plain = strip_ansi(&render(
            &input,
            Some(("main".to_string(), false)),
            Some(&extra),
            &[],
            0,
            RoundingMode::Floor,
            Layout::Comfortable,
        ));
        assert!(plain.contains("↑3"), "ahead");
        assert!(plain.contains("↓1"), "behind");
    }

    #[test]
    fn test_render_zero_ahead_behind_hidden() {
        let input = Input::default();
        let extra = make_extra(0, 0, common::OpState::None, 0, 0, 0);
        let plain = strip_ansi(&render(
            &input,
            Some(("main".to_string(), false)),
            Some(&extra),
            &[],
            0,
            RoundingMode::Floor,
            Layout::Comfortable,
        ));
        assert!(!plain.contains('↑'), "no ahead when zero");
        assert!(!plain.contains('↓'), "no behind when zero");
    }

    #[test]
    fn test_render_merge_state() {
        let input = Input::default();
        let extra = make_extra(0, 0, common::OpState::Merge, 0, 0, 0);
        let plain = strip_ansi(&render(
            &input,
            Some(("main".to_string(), false)),
            Some(&extra),
            &[],
            0,
            RoundingMode::Floor,
            Layout::Comfortable,
        ));
        assert!(plain.contains("MERGING"), "MERGING badge");
    }

    #[test]
    fn test_render_rebase_state_with_progress() {
        let input = Input::default();
        let extra = make_extra(0, 0, common::OpState::Rebase, 2, 5, 0);
        let plain = strip_ansi(&render(
            &input,
            Some(("main".to_string(), false)),
            Some(&extra),
            &[],
            0,
            RoundingMode::Floor,
            Layout::Comfortable,
        ));
        assert!(plain.contains("REBASE 2/5"), "rebase badge with progress");
    }

    #[test]
    fn test_render_cherry_pick_no_conflicts() {
        let input = Input::default();
        let extra = make_extra(0, 0, common::OpState::CherryPick, 0, 0, 0);
        let plain = strip_ansi(&render(
            &input,
            Some(("main".to_string(), false)),
            Some(&extra),
            &[],
            0,
            RoundingMode::Floor,
            Layout::Comfortable,
        ));
        assert!(plain.contains("CHERRY-PICK"), "cherry-pick badge");
        assert!(!plain.contains("conflict"), "no conflict text when count=0");
    }

    #[test]
    fn test_render_merge_with_conflicts() {
        let input = Input::default();
        let extra = make_extra(0, 0, common::OpState::Merge, 0, 0, 3);
        let plain = strip_ansi(&render(
            &input,
            Some(("main".to_string(), false)),
            Some(&extra),
            &[],
            0,
            RoundingMode::Floor,
            Layout::Comfortable,
        ));
        assert!(plain.contains("MERGING"));
        assert!(plain.contains("3 conflicts"), "conflict count shown");
    }

    #[test]
    fn test_render_revert_state() {
        let input = Input::default();
        let extra = make_extra(0, 0, common::OpState::Revert, 0, 0, 0);
        let plain = strip_ansi(&render(
            &input,
            Some(("main".to_string(), false)),
            Some(&extra),
            &[],
            0,
            RoundingMode::Floor,
            Layout::Comfortable,
        ));
        assert!(plain.contains("REVERTING"), "revert badge");
    }

    #[test]
    fn test_render_bisect_state() {
        let input = Input::default();
        let extra = make_extra(0, 0, common::OpState::Bisect, 0, 0, 0);
        let plain = strip_ansi(&render(
            &input,
            Some(("main".to_string(), false)),
            Some(&extra),
            &[],
            0,
            RoundingMode::Floor,
            Layout::Comfortable,
        ));
        assert!(plain.contains("BISECTING"), "bisect badge");
    }

    // ── Condensed variants ────────────────────────────────────────────────────

    #[test]
    fn test_render_ahead_behind_condensed() {
        let input = Input::default();
        let extra = make_extra(3, 1, common::OpState::None, 0, 0, 0);
        let plain = strip_ansi(&render(
            &input,
            Some(("main".to_string(), false)),
            Some(&extra),
            &[],
            0,
            RoundingMode::Floor,
            Layout::Condensed,
        ));
        assert!(plain.contains("↑3"), "ahead condensed");
        assert!(plain.contains("↓1"), "behind condensed");
    }

    #[test]
    fn test_render_rebase_condensed() {
        let input = Input::default();
        let extra = make_extra(0, 0, common::OpState::Rebase, 2, 5, 0);
        let plain = strip_ansi(&render(
            &input,
            Some(("main".to_string(), false)),
            Some(&extra),
            &[],
            0,
            RoundingMode::Floor,
            Layout::Condensed,
        ));
        assert!(plain.contains("R 2/5"), "condensed rebase badge");
    }

    #[test]
    fn test_render_merge_condensed_with_conflicts() {
        let input = Input::default();
        let extra = make_extra(0, 0, common::OpState::Merge, 0, 0, 3);
        let plain = strip_ansi(&render(
            &input,
            Some(("main".to_string(), false)),
            Some(&extra),
            &[],
            0,
            RoundingMode::Floor,
            Layout::Condensed,
        ));
        assert!(plain.contains('M'), "condensed merge badge");
        assert!(plain.contains("!3"), "condensed conflict count");
    }

    #[test]
    fn test_render_cherry_pick_condensed() {
        let input = Input::default();
        let extra = make_extra(0, 0, common::OpState::CherryPick, 0, 0, 0);
        let plain = strip_ansi(&render(
            &input,
            Some(("main".to_string(), false)),
            Some(&extra),
            &[],
            0,
            RoundingMode::Floor,
            Layout::Condensed,
        ));
        assert!(plain.contains("CP"), "condensed cherry-pick badge");
    }

    #[test]
    fn test_render_none_extra_no_arrows() {
        // When git_extra is None (daemon not running / slow path), no new fields appear.
        let input = Input::default();
        let plain = strip_ansi(&render(
            &input,
            Some(("main".to_string(), false)),
            None,
            &[],
            0,
            RoundingMode::Floor,
            Layout::Comfortable,
        ));
        assert!(!plain.contains('↑'));
        assert!(!plain.contains('↓'));
        assert!(!plain.contains("MERGING"));
        assert!(!plain.contains("REBASE"));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p claudehud test_render_ahead 2>&1 | head -20
```
Expected: compile error — `render` has wrong arity (missing `git_extra` parameter).

- [ ] **Step 3: Update `render` public API**

In `render.rs`, update the `render` function signature to accept `git_extra`:

```rust
pub fn render(
    input: &Input,
    git: Option<(String, bool)>,
    git_extra: Option<&common::GitExtra>,
    incidents: &[Incident],
    total_active: u8,
    rounding: RoundingMode,
    layout: Layout,
) -> String {
    match layout {
        Layout::Comfortable => render_comfortable(input, git, git_extra, incidents, total_active, rounding),
        Layout::Condensed => render_condensed(input, git, git_extra, incidents, total_active, rounding),
    }
}
```

Update `render_comfortable` and `render_condensed` signatures similarly, adding `git_extra: Option<&common::GitExtra>` and passing it through to `push_dir_branch`.

- [ ] **Step 4: Update `push_dir_branch` to render ahead/behind and op-state**

Replace the existing `push_dir_branch` function with:

```rust
fn push_dir_branch(
    input: &Input,
    git: Option<&(String, bool)>,
    extra: Option<&common::GitExtra>,
    tight: bool,
    out: &mut String,
) {
    use common::OpState;

    let cwd = input.cwd.as_deref().unwrap_or("");
    let dirname = Path::new(cwd)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(cwd);

    // Op-state badge (comfortable: before dir; condensed: inline after branch)
    if !tight {
        if let Some(ex) = extra {
            push_op_badge_comfortable(ex, out);
        }
    }

    out.push_str(CYAN);
    out.push_str(dirname);
    out.push_str(RESET);

    if let Some((branch, dirty)) = git {
        if !tight {
            out.push(' ');
        }
        out.push_str(GREEN);
        out.push('(');
        out.push_str(branch);
        if *dirty {
            out.push_str(RED);
            out.push('*');
        }
        out.push_str(GREEN);
        out.push(')');
        out.push_str(RESET);

        // Ahead/behind
        if let Some(ex) = extra {
            if ex.ahead > 0 {
                out.push(' ');
                out.push_str(GREEN);
                write!(out, "↑{}", ex.ahead).unwrap();
                out.push_str(RESET);
            }
            if ex.behind > 0 {
                out.push(' ');
                out.push_str(RED);
                write!(out, "↓{}", ex.behind).unwrap();
                out.push_str(RESET);
            }
        }

        // Condensed op-state + conflicts inline after branch
        if tight {
            if let Some(ex) = extra {
                push_op_badge_condensed(ex, out);
            }
        }
    }
}

fn push_op_badge_comfortable(ex: &common::GitExtra, out: &mut String) {
    use common::OpState;
    let label = match ex.op_state {
        OpState::None => return,
        OpState::Merge => "MERGING".to_string(),
        OpState::Rebase => {
            if ex.op_step > 0 || ex.op_total > 0 {
                format!("REBASE {}/{}", ex.op_step, ex.op_total)
            } else {
                "REBASE".to_string()
            }
        }
        OpState::CherryPick => "CHERRY-PICK".to_string(),
        OpState::Revert => "REVERTING".to_string(),
        OpState::Bisect => "BISECTING".to_string(),
    };
    out.push_str(YELLOW);
    out.push_str(&label);
    out.push_str(RESET);
    if ex.conflict_count > 0 {
        out.push_str(DIM);
        write!(out, " · {} conflicts", ex.conflict_count).unwrap();
        out.push_str(RESET);
    }
    out.push_str(SEP);
}

fn push_op_badge_condensed(ex: &common::GitExtra, out: &mut String) {
    use common::OpState;
    let badge = match ex.op_state {
        OpState::None => return,
        OpState::Merge => "M".to_string(),
        OpState::Rebase => {
            if ex.op_step > 0 || ex.op_total > 0 {
                format!("R {}/{}", ex.op_step, ex.op_total)
            } else {
                "R".to_string()
            }
        }
        OpState::CherryPick => "CP".to_string(),
        OpState::Revert => "REV".to_string(),
        OpState::Bisect => "BIS".to_string(),
    };
    out.push(' ');
    out.push_str(YELLOW);
    out.push_str(&badge);
    out.push_str(RESET);
    if ex.conflict_count > 0 {
        out.push_str(RED);
        write!(out, "!{}", ex.conflict_count).unwrap();
        out.push_str(RESET);
    }
}
```

Update all `push_dir_branch` call sites in `render_comfortable` and `render_condensed` to pass `git_extra`:

```rust
// In render_comfortable:
push_dir_branch(input, git.as_ref(), git_extra, false, &mut out);

// In render_condensed:
push_dir_branch(input, git.as_ref(), git_extra, true, &mut out);
```

Also add `use common::GitExtra;` at the top of `render.rs` and add `use std::fmt::Write as _;` if not already present (it already is on line 1).

- [ ] **Step 5: Fix all existing test call sites**

Every existing `render(...)` call in `#[cfg(test)]` currently has 6 arguments. They all need `None` inserted as the third argument. Find-and-replace the render call pattern. Each call like:

```rust
render(&input, Some(...), &[], 0, RoundingMode::Floor, Layout::Comfortable)
```

becomes:

```rust
render(&input, Some(...), None, &[], 0, RoundingMode::Floor, Layout::Comfortable)
```

And calls like:

```rust
render(&input, None, &[], 0, RoundingMode::Floor, Layout::Comfortable)
```

become:

```rust
render(&input, None, None, &[], 0, RoundingMode::Floor, Layout::Comfortable)
```

There are approximately 25 such calls in the existing test block. Update all of them.

- [ ] **Step 6: Update the call site in `claudehud/src/main.rs`**

Find the `render(...)` call in `main.rs` and add `git_extra` as the third argument:

```rust
let output = render::render(&input, git, git_extra, &incidents, total_active, rounding, layout);
```

- [ ] **Step 7: Run tests**

```bash
cargo test -p claudehud 2>&1 | tail -30
```
Expected: all tests pass including the new fixture tests.

- [ ] **Step 8: Commit**

```bash
git add claudehud/src/render.rs claudehud/src/main.rs
git commit -m "feat(client/render): ahead/behind arrows and op-state badges"
```

---

## Task 7: Update README cache layout table

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Replace the cache layout table**

Find the section in `README.md` under `### IPC: mmap + seqlock` containing:

```
**Cache file layout (138 bytes):**
```

Replace the entire table block with:

```markdown
**Cache file layout (151 bytes, v1):**

| Offset | Size | Field |
|--------|------|-------|
| 0 | 8 | `u64` seqlock counter (even = stable, odd = write in progress) |
| 8 | 1 | `u8` dirty flag |
| 9 | 1 | `u8` branch name length |
| 10 | 128 | `[u8; 128]` branch name (UTF-8) |
| 138 | 1 | `u8` layout version (`0` = legacy 138-byte file, `1` = v1 extension present) |
| 139 | 4 | `u32` ahead count (LE), commits on local HEAD not on upstream |
| 143 | 4 | `u32` behind count (LE), commits on upstream not on local HEAD |
| 147 | 1 | `u8` op_state: 0=None 1=Merge 2=Rebase 3=CherryPick 4=Revert 5=Bisect |
| 148 | 1 | `u8` op_step (rebase current step, 0 when not rebasing) |
| 149 | 1 | `u8` op_total (rebase total steps, 0 when not rebasing) |
| 150 | 1 | `u8` conflict_count (unmerged paths, saturates at 255) |

Old clients reading a new (151-byte) file safely ignore bytes 138–150 — their
`MMAP_SIZE` guard accepts files ≥ 138 bytes and they only read through offset 137.
Old daemon files (138 bytes) are accepted by new clients; `GitExtra` fields default
to zero/None.
```

- [ ] **Step 2: Commit**

```bash
git add README.md
git commit -m "docs: update cache layout table to 151-byte v1 layout"
```

---

## Task 8: Final verification

- [ ] **Step 1: `cargo fmt --check`**

```bash
cargo fmt --check 2>&1
```
Expected: no output (all files formatted).

If there are issues: `cargo fmt` then re-check.

- [ ] **Step 2: `cargo clippy`**

```bash
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -40
```
Expected: no warnings.

Fix any clippy warnings before proceeding.

- [ ] **Step 3: Full test suite**

```bash
cargo test --workspace 2>&1 | tail -40
```
Expected: all tests pass.

- [ ] **Step 4: Final commit if any fmt/clippy fixes needed**

```bash
git add -p  # stage only what you changed
git commit -m "chore: fmt + clippy fixes for ahead-behind feature"
```
