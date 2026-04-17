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

/// Seqlock write for incident data.
/// Panics if buf.len() < INCIDENTS_MMAP_SIZE.
pub fn seqlock_write_incident(buf: &mut [u8], incident: Option<&Incident>) {
    assert!(buf.len() >= INCIDENTS_MMAP_SIZE);

    // Read current seq and increment to odd (write in progress)
    let seq = read_u64_le(buf, 0);
    buf[0..8].copy_from_slice(&seq.wrapping_add(1).to_le_bytes());
    fence(Ordering::SeqCst);

    if let Some(inc) = incident {
        // Write active_count, severity, started_at
        buf[8] = inc.active_count;
        buf[9] = inc.severity as u8;
        buf[10..18].copy_from_slice(&inc.started_at.to_le_bytes());

        // Write title: truncate to TITLE_MAX, write len byte + data, zero-pad remainder
        let title_bytes = inc.title.as_bytes();
        let title_len = title_bytes.len().min(TITLE_MAX);
        buf[18] = title_len as u8;
        buf[19..19 + title_len].copy_from_slice(&title_bytes[..title_len]);
        buf[19 + title_len..19 + TITLE_MAX].fill(0);

        // Write url: truncate to URL_MAX, write len byte + data, zero-pad remainder
        let url_bytes = inc.url.as_bytes();
        let url_len = url_bytes.len().min(URL_MAX);
        buf[147] = url_len as u8;
        buf[148..148 + url_len].copy_from_slice(&url_bytes[..url_len]);
        buf[148 + url_len..148 + URL_MAX].fill(0);
    } else {
        // Clear incident: zero out active_count, severity, started_at, title region, url region
        buf[8] = 0;
        buf[9] = 0;
        buf[10..18].fill(0);
        buf[18] = 0;
        buf[19..19 + TITLE_MAX].fill(0);
        buf[147] = 0;
        buf[148..148 + URL_MAX].fill(0);
    }

    fence(Ordering::SeqCst);
    // Increment seq to even (write complete)
    let seq2 = read_u64_le(buf, 0);
    buf[0..8].copy_from_slice(&seq2.wrapping_add(1).to_le_bytes());
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

    #[test]
    fn test_write_then_read_round_trip() {
        let mut buf = vec![0u8; INCIDENTS_MMAP_SIZE];
        let incident = Incident {
            severity: Severity::Critical,
            started_at: 1_700_000_000,
            title: "API outage".to_string(),
            url: "https://status.claude.com/incidents/xyz".to_string(),
            active_count: 2,
        };
        seqlock_write_incident(&mut buf, Some(&incident));
        let got = seqlock_read_incident(&buf).expect("populated");
        assert_eq!(got, incident);
    }

    #[test]
    fn test_write_none_clears() {
        let mut buf = vec![0u8; INCIDENTS_MMAP_SIZE];
        let incident = Incident {
            severity: Severity::Minor,
            started_at: 42,
            title: "x".to_string(),
            url: "y".to_string(),
            active_count: 1,
        };
        seqlock_write_incident(&mut buf, Some(&incident));
        assert!(seqlock_read_incident(&buf).is_some());
        seqlock_write_incident(&mut buf, None);
        assert_eq!(seqlock_read_incident(&buf), None);
    }

    #[test]
    fn test_write_truncates_long_title() {
        let mut buf = vec![0u8; INCIDENTS_MMAP_SIZE];
        let long_title: String = (0..TITLE_MAX + 50)
            .map(|i| ((i % 26) as u8 + b'a') as char)
            .collect();
        let incident = Incident {
            severity: Severity::Minor,
            started_at: 0,
            title: long_title.clone(),
            url: "u".to_string(),
            active_count: 1,
        };
        seqlock_write_incident(&mut buf, Some(&incident));
        let got = seqlock_read_incident(&buf).unwrap();
        assert_eq!(got.title.len(), TITLE_MAX);
        assert_eq!(got.title, long_title[..TITLE_MAX]);
    }

    #[test]
    fn test_write_truncates_long_url() {
        let mut buf = vec![0u8; INCIDENTS_MMAP_SIZE];
        let long_url: String = (0..URL_MAX + 50)
            .map(|i| ((i % 26) as u8 + b'a') as char)
            .collect();
        let incident = Incident {
            severity: Severity::Minor,
            started_at: 0,
            title: "t".to_string(),
            url: long_url.clone(),
            active_count: 1,
        };
        seqlock_write_incident(&mut buf, Some(&incident));
        let got = seqlock_read_incident(&buf).unwrap();
        assert_eq!(got.url.len(), URL_MAX);
        assert_eq!(got.url, long_url[..URL_MAX]);
    }

    #[test]
    fn test_write_short_after_long_clears_tail() {
        let mut buf = vec![0u8; INCIDENTS_MMAP_SIZE];
        let long = Incident {
            severity: Severity::Minor,
            started_at: 0,
            title: "x".repeat(TITLE_MAX).to_string(),
            url: "y".repeat(URL_MAX).to_string(),
            active_count: 1,
        };
        seqlock_write_incident(&mut buf, Some(&long));
        let short = Incident {
            severity: Severity::Minor,
            started_at: 0,
            title: "short".to_string(),
            url: "https://ex.com".to_string(),
            active_count: 1,
        };
        seqlock_write_incident(&mut buf, Some(&short));
        let got = seqlock_read_incident(&buf).unwrap();
        assert_eq!(got.title, "short");
        assert_eq!(got.url, "https://ex.com");
    }
}
