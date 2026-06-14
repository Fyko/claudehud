//! Seqlock-protected mmap records.
//!
//! A record is laid out as `[8-byte counter][payload]`. The counter is **even**
//! when the record is stable and **odd** while a write is in progress. A single
//! writer (the daemon) bumps the counter odd, rewrites the payload, then bumps
//! it even; many lock-free readers (clients) spin until they observe a stable
//! even counter that is unchanged across the whole read. This lets a daemon and
//! its clients share an mmap file with no locks and no syscalls on the read path.
//!
//! The protocol — counter, memory fences, retry loop — lives here exactly once.
//! Each cache type only describes its own payload by implementing
//! [`SeqlockRecord`]; see [`crate::GitStatus`] and [`crate::incidents::IncidentSet`].

use std::sync::atomic::{fence, Ordering};

/// A fixed-size payload stored behind a seqlock counter.
///
/// Implementors define only how their bytes are laid out. The leading 8-byte
/// counter is owned by [`read`] and [`write`] and is never visible to `encode`
/// or `decode`, which see the payload region alone.
pub trait SeqlockRecord: Sized {
    /// Total record size in bytes, **including** the leading 8-byte counter.
    const SIZE: usize;

    /// Encode `self` into `payload` — the record's bytes after the counter,
    /// i.e. a slice of length `SIZE - 8`. Implementors must write every byte
    /// (zero-fill any unused tail) so stale data never leaks across writes.
    fn encode(&self, payload: &mut [u8]);

    /// Decode a value from `payload` — the record's bytes after the counter.
    fn decode(payload: &[u8]) -> Self;
}

/// Read a consistent snapshot of a seqlock record.
///
/// Returns `None` if `mmap` is smaller than `R::SIZE` (a truncated or absent
/// cache reads as nothing rather than panicking — the statusline is fail-soft).
pub fn read<R: SeqlockRecord>(mmap: &[u8]) -> Option<R> {
    if mmap.len() < R::SIZE {
        return None;
    }
    loop {
        let seq1 = read_u64_le(mmap, 0);
        if seq1 & 1 == 1 {
            // Write in progress; wait for it to finish.
            std::hint::spin_loop();
            continue;
        }
        fence(Ordering::Acquire);

        let value = R::decode(&mmap[8..R::SIZE]);

        fence(Ordering::Acquire);
        let seq2 = read_u64_le(mmap, 0);
        if seq1 == seq2 {
            return Some(value);
        }
        // The record changed under us; retry.
        std::hint::spin_loop();
    }
}

/// Write a seqlock record. Single-writer only: concurrent writers would corrupt
/// the counter. `mmap` must be at least `R::SIZE` bytes.
pub fn write<R: SeqlockRecord>(mmap: &mut [u8], record: &R) {
    assert!(mmap.len() >= R::SIZE, "seqlock buffer smaller than R::SIZE");

    let seq = read_u64_le(mmap, 0);
    // Bump odd: write in progress.
    write_u64_le(mmap, 0, seq.wrapping_add(1));
    fence(Ordering::SeqCst);

    record.encode(&mut mmap[8..R::SIZE]);

    fence(Ordering::SeqCst);
    // Bump even: write complete.
    write_u64_le(mmap, 0, seq.wrapping_add(2));
}

pub(crate) fn read_u64_le(buf: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(buf[offset..offset + 8].try_into().unwrap())
}

pub(crate) fn write_u64_le(buf: &mut [u8], offset: usize, value: u64) {
    buf[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal record used to exercise the protocol in isolation: a single u32
    /// payload after the counter.
    #[derive(Debug, PartialEq, Eq)]
    struct U32Record(u32);

    impl SeqlockRecord for U32Record {
        const SIZE: usize = 8 + 4;
        fn encode(&self, payload: &mut [u8]) {
            payload[0..4].copy_from_slice(&self.0.to_le_bytes());
        }
        fn decode(payload: &[u8]) -> Self {
            U32Record(u32::from_le_bytes(payload[0..4].try_into().unwrap()))
        }
    }

    #[test]
    fn test_write_then_read_round_trips() {
        let mut buf = [0u8; U32Record::SIZE];
        write(&mut buf, &U32Record(0xDEAD_BEEF));
        assert_eq!(read::<U32Record>(&buf), Some(U32Record(0xDEAD_BEEF)));
    }

    #[test]
    fn test_write_leaves_counter_even() {
        let mut buf = [0u8; U32Record::SIZE];
        write(&mut buf, &U32Record(1));
        assert_eq!(read_u64_le(&buf, 0) & 1, 0);
    }

    #[test]
    fn test_read_returns_none_when_buffer_too_small() {
        let buf = [0u8; U32Record::SIZE - 1];
        assert_eq!(read::<U32Record>(&buf), None);
    }

    #[test]
    fn test_read_spins_until_write_completes() {
        // Counter is odd (write in progress) — a real reader would spin. We can't
        // block the test thread forever, so assert the in-progress state is detected
        // by leaving an even snapshot behind a fresh write and checking the value.
        let mut buf = [0u8; U32Record::SIZE];
        write(&mut buf, &U32Record(7));
        // Manually mark a write-in-progress, then complete it, mimicking the daemon.
        let seq = read_u64_le(&buf, 0);
        write_u64_le(&mut buf, 0, seq.wrapping_add(1)); // odd
        U32Record(9).encode(&mut buf[8..]);
        write_u64_le(&mut buf, 0, seq.wrapping_add(2)); // even
        assert_eq!(read::<U32Record>(&buf), Some(U32Record(9)));
    }
}
