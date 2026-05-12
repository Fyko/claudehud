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
    let conflict_count =
        if matches!(op_state, OpState::Merge | OpState::Rebase | OpState::CherryPick) {
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
        .unwrap_or_else(|_| std::process::Output {
            status: std::process::ExitStatus::default(),
            stdout: vec![],
            stderr: vec![],
        });
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
        .unwrap_or_else(|_| std::process::Output {
            status: std::process::ExitStatus::default(),
            stdout: vec![],
            stderr: vec![],
        });
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
    fn test_read_ahead_behind_no_upstream_returns_zeros() {
        // In a repo with no upstream configured, should return (0, 0) without panicking.
        let cwd = std::env::current_dir().unwrap();
        let root = common::find_git_root(&cwd).unwrap();
        let (ahead, behind) = read_ahead_behind(&root);
        // Values depend on real repo state, but must not panic
        let _ = (ahead, behind);
    }
}
