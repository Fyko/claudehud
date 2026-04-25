use std::fs::OpenOptions;
use std::path::Path;
use std::sync::atomic::{fence, Ordering};

use common::{hash_path, mmap_path, read_git_status, BRANCH_MAX, MMAP_SIZE};
use memmap2::MmapMut;

/// Re-run git status for `cwd` and write result to the mmap cache file.
pub fn update(cwd: &Path) {
    let Some((branch, dirty)) = read_git_status(cwd) else {
        return;
    };
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
    seqlock_write(&mut mmap[..], &branch, dirty);
}

/// Write branch + dirty to a raw byte slice using a seqlock protocol.
/// Exported (pub) for testing with plain Vec<u8>.
pub fn seqlock_write(buf: &mut [u8], branch: &str, dirty: bool) {
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
        use common::{seqlock_read, MMAP_SIZE};
        let mut buf = vec![0u8; MMAP_SIZE];
        seqlock_write(&mut buf, "feature-branch", true);
        let (branch, dirty) = seqlock_read(&buf);
        assert_eq!(branch, "feature-branch");
        assert!(dirty);
    }

    #[test]
    fn test_seqlock_write_clean() {
        use common::{seqlock_read, MMAP_SIZE};
        let mut buf = vec![0u8; MMAP_SIZE];
        seqlock_write(&mut buf, "main", false);
        let (branch, dirty) = seqlock_read(&buf);
        assert_eq!(branch, "main");
        assert!(!dirty);
    }
}
