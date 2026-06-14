use std::fs::OpenOptions;
use std::path::Path;

use common::{hash_path, mmap_path, read_git_status, seqlock_write, MMAP_SIZE};
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
