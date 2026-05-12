use std::fs::OpenOptions;
use std::path::Path;
use std::sync::atomic::{fence, Ordering};

use common::{
    find_git_root, hash_path, mmap_path, read_git_status, GitExtra, BRANCH_MAX, MMAP_SIZE,
};
use memmap2::MmapMut;

use crate::git_extra::read_git_extra;

/// Re-run git status for `cwd` and write result to the mmap cache file.
pub fn update(cwd: &Path) {
    let Some((branch, dirty)) = read_git_status(cwd) else {
        return;
    };
    let git_root = match find_git_root(cwd) {
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
    // Safety: the file is freshly opened with read+write, sized to MMAP_SIZE,
    // and no other thread holds a reference to this path's mmap simultaneously
    // (the seqlock protocol handles concurrent reads from the client).
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
    // Increment to odd → write in progress
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
    // Increment to even → write complete
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
