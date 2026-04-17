use std::sync::atomic::fence;
use std::sync::atomic::Ordering;

pub const INCIDENTS_MMAP_PATH: &str = "/tmp/clhud-incidents.bin";
pub const INCIDENTS_MMAP_SIZE: usize = 408;
pub const TITLE_MAX: usize = 128;
pub const URL_MAX: usize = 255;

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    None = 0,
    Minor = 1,
    Major = 2,
    Critical = 3,
    Maintenance = 4,
}

impl Severity {
    pub fn from_u8(b: u8) -> Self {
        match b {
            0 => Severity::None,
            1 => Severity::Minor,
            2 => Severity::Major,
            3 => Severity::Critical,
            4 => Severity::Maintenance,
            _ => Severity::None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Incident {
    pub severity: Severity,
    pub started_at: u64,
    pub title: String,
    pub url: String,
    pub active_count: u8,
}

/// Seqlock read for incident data.
/// Layout:
/// [0..8]     u64 seqlock counter (even=stable, odd=write in progress)
/// [8]        u8 active_count
/// [9]        u8 severity
/// [10..18]   u64 LE started_at (unix epoch)
/// [18]       u8 title length
/// [19..147]  title bytes (UTF-8, zero-padded) — 128 bytes of storage
/// [147]      u8 url length
/// [148..403] url bytes (ASCII, zero-padded) — 255 bytes
/// [403..408] 5 padding bytes
pub fn seqlock_read_incident(mmap: &[u8]) -> Option<Incident> {
    if mmap.len() < INCIDENTS_MMAP_SIZE {
        return None;
    }

    loop {
        let seq1 = read_u64_le(mmap, 0);
        if seq1 & 1 == 1 {
            std::hint::spin_loop();
            continue;
        }
        fence(Ordering::Acquire);

        let active_count = mmap[8];
        let severity = Severity::from_u8(mmap[9]);
        let started_at = read_u64_le(mmap, 10);

        let title_len = (mmap[18] as usize).min(TITLE_MAX);
        let title = String::from_utf8_lossy(&mmap[19..19 + title_len]).into_owned();

        let url_len = (mmap[147] as usize).min(URL_MAX);
        let url = String::from_utf8_lossy(&mmap[148..148 + url_len]).into_owned();

        fence(Ordering::Acquire);
        let seq2 = read_u64_le(mmap, 0);
        if seq1 == seq2 {
            if active_count == 0 {
                return None;
            }
            return Some(Incident {
                severity,
                started_at,
                title,
                url,
                active_count,
            });
        }
        std::hint::spin_loop();
    }
}

fn read_u64_le(buf: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(buf[offset..offset + 8].try_into().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_read_returns_none_when_active_count_zero() {
        let buf = [0u8; INCIDENTS_MMAP_SIZE];
        assert_eq!(seqlock_read_incident(&buf), None);
    }

    #[test]
    fn test_read_parses_populated_buffer() {
        let mut buf = [0u8; INCIDENTS_MMAP_SIZE];
        buf[0..8].copy_from_slice(&2u64.to_le_bytes());
        buf[8] = 3;
        buf[9] = 2;
        buf[10..18].copy_from_slice(&1_700_000_000u64.to_le_bytes());
        let title = b"Elevated errors";
        buf[18] = title.len() as u8;
        buf[19..19 + title.len()].copy_from_slice(title);
        let url = b"https://status.claude.com/incidents/abc";
        buf[147] = url.len() as u8;
        buf[148..148 + url.len()].copy_from_slice(url);

        let got = seqlock_read_incident(&buf).expect("should parse");
        assert_eq!(got.active_count, 3);
        assert_eq!(got.severity, Severity::Major);
        assert_eq!(got.started_at, 1_700_000_000);
        assert_eq!(got.title, "Elevated errors");
        assert_eq!(got.url, "https://status.claude.com/incidents/abc");
    }
}
