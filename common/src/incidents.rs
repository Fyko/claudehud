use std::sync::atomic::fence;
use std::sync::atomic::Ordering;

pub const INCIDENTS_MMAP_PATH: &str = "/tmp/clhud-incidents.bin";
pub const MAX_STORED_INCIDENTS: usize = 5;
pub const TITLE_MAX: usize = 128;
pub const URL_MAX: usize = 255;

// Per-slot: severity(1) + started_at(8) + title_len(1) + title(128) + url_len(1) + url(255) = 394
const SLOT_SIZE: usize = 1 + 8 + 1 + TITLE_MAX + 1 + URL_MAX;

// Layout: seqlock(8) + total_count(1) + stored_count(1) + 5 slots(1970) + padding(4) = 1984
pub const INCIDENTS_MMAP_SIZE: usize = 8 + 1 + 1 + MAX_STORED_INCIDENTS * SLOT_SIZE + 4;

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
}

/// Returns (stored_incidents, total_active_count).
/// stored_incidents.len() <= min(total_active_count, MAX_STORED_INCIDENTS).
/// Returns (vec![], 0) when no active incidents or mmap too small.
pub fn seqlock_read_incidents(mmap: &[u8]) -> (Vec<Incident>, u8) {
    if mmap.len() < INCIDENTS_MMAP_SIZE {
        return (vec![], 0);
    }
    loop {
        let seq1 = read_u64_le(mmap, 0);
        if seq1 & 1 == 1 {
            std::hint::spin_loop();
            continue;
        }
        fence(Ordering::Acquire);

        let total = mmap[8];
        let stored = mmap[9] as usize;
        let mut incidents = Vec::with_capacity(stored);
        for i in 0..stored {
            let b = 10 + i * SLOT_SIZE;
            let severity = Severity::from_u8(mmap[b]);
            let started_at = read_u64_le(mmap, b + 1);
            let title_len = (mmap[b + 9] as usize).min(TITLE_MAX);
            let title = String::from_utf8_lossy(&mmap[b + 10..b + 10 + title_len]).into_owned();
            let url_len = (mmap[b + 10 + TITLE_MAX] as usize).min(URL_MAX);
            let url = String::from_utf8_lossy(&mmap[b + 11 + TITLE_MAX..b + 11 + TITLE_MAX + url_len]).into_owned();
            incidents.push(Incident { severity, started_at, title, url });
        }

        fence(Ordering::Acquire);
        let seq2 = read_u64_le(mmap, 0);
        if seq1 == seq2 {
            return (incidents, total);
        }
        std::hint::spin_loop();
    }
}

/// Writes up to MAX_STORED_INCIDENTS from `incidents` into the seqlock mmap.
/// `total` is the full count of active incidents (may exceed MAX_STORED_INCIDENTS).
pub fn seqlock_write_incidents(buf: &mut [u8], incidents: &[Incident], total: u8) {
    assert!(buf.len() >= INCIDENTS_MMAP_SIZE);
    let stored = incidents.len().min(MAX_STORED_INCIDENTS);

    let seq = read_u64_le(buf, 0);
    buf[0..8].copy_from_slice(&seq.wrapping_add(1).to_le_bytes());
    fence(Ordering::SeqCst);

    buf[8] = total;
    buf[9] = stored as u8;

    for i in 0..MAX_STORED_INCIDENTS {
        let b = 10 + i * SLOT_SIZE;
        if i < stored {
            let inc = &incidents[i];
            buf[b] = inc.severity as u8;
            buf[b + 1..b + 9].copy_from_slice(&inc.started_at.to_le_bytes());
            let tb = inc.title.as_bytes();
            let tl = tb.len().min(TITLE_MAX);
            buf[b + 9] = tl as u8;
            buf[b + 10..b + 10 + tl].copy_from_slice(&tb[..tl]);
            buf[b + 10 + tl..b + 10 + TITLE_MAX].fill(0);
            let ub = inc.url.as_bytes();
            let ul = ub.len().min(URL_MAX);
            buf[b + 10 + TITLE_MAX] = ul as u8;
            buf[b + 11 + TITLE_MAX..b + 11 + TITLE_MAX + ul].copy_from_slice(&ub[..ul]);
            buf[b + 11 + TITLE_MAX + ul..b + SLOT_SIZE].fill(0);
        } else {
            buf[b..b + SLOT_SIZE].fill(0);
        }
    }

    fence(Ordering::SeqCst);
    let seq2 = read_u64_le(buf, 0);
    buf[0..8].copy_from_slice(&seq2.wrapping_add(1).to_le_bytes());
}

fn read_u64_le(buf: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(buf[offset..offset + 8].try_into().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_read_returns_empty_when_total_zero() {
        let buf = [0u8; INCIDENTS_MMAP_SIZE];
        let (incidents, total) = seqlock_read_incidents(&buf);
        assert!(incidents.is_empty());
        assert_eq!(total, 0);
    }

    #[test]
    fn test_write_then_read_single() {
        let mut buf = vec![0u8; INCIDENTS_MMAP_SIZE];
        let inc = Incident {
            severity: Severity::Major,
            started_at: 1_700_000_000,
            title: "Elevated errors".to_string(),
            url: "https://status.claude.com/incidents/abc".to_string(),
        };
        seqlock_write_incidents(&mut buf, &[inc.clone()], 1);
        let (got, total) = seqlock_read_incidents(&buf);
        assert_eq!(total, 1);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0], inc);
    }

    #[test]
    fn test_write_then_read_multiple() {
        let mut buf = vec![0u8; INCIDENTS_MMAP_SIZE];
        let a = Incident {
            severity: Severity::Critical,
            started_at: 1_000,
            title: "Alpha".to_string(),
            url: "https://example.com/a".to_string(),
        };
        let b = Incident {
            severity: Severity::Minor,
            started_at: 2_000,
            title: "Beta".to_string(),
            url: "https://example.com/b".to_string(),
        };
        seqlock_write_incidents(&mut buf, &[a.clone(), b.clone()], 2);
        let (got, total) = seqlock_read_incidents(&buf);
        assert_eq!(total, 2);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0], a);
        assert_eq!(got[1], b);
    }

    #[test]
    fn test_total_exceeds_stored() {
        let mut buf = vec![0u8; INCIDENTS_MMAP_SIZE];
        let inc = Incident {
            severity: Severity::Minor,
            started_at: 0,
            title: "t".to_string(),
            url: "u".to_string(),
        };
        seqlock_write_incidents(&mut buf, &[inc], 10);
        let (got, total) = seqlock_read_incidents(&buf);
        assert_eq!(total, 10);
        assert_eq!(got.len(), 1);
    }

    #[test]
    fn test_write_clears_on_empty() {
        let mut buf = vec![0u8; INCIDENTS_MMAP_SIZE];
        let inc = Incident {
            severity: Severity::Minor,
            started_at: 42,
            title: "x".to_string(),
            url: "y".to_string(),
        };
        seqlock_write_incidents(&mut buf, &[inc], 1);
        seqlock_write_incidents(&mut buf, &[], 0);
        let (got, total) = seqlock_read_incidents(&buf);
        assert!(got.is_empty());
        assert_eq!(total, 0);
    }

    #[test]
    fn test_write_caps_at_max_stored() {
        let mut buf = vec![0u8; INCIDENTS_MMAP_SIZE];
        let incidents: Vec<Incident> = (0..MAX_STORED_INCIDENTS + 3)
            .map(|i| Incident {
                severity: Severity::Minor,
                started_at: i as u64,
                title: format!("inc{i}"),
                url: format!("https://example.com/{i}"),
            })
            .collect();
        seqlock_write_incidents(&mut buf, &incidents, incidents.len() as u8);
        let (got, total) = seqlock_read_incidents(&buf);
        assert_eq!(total, incidents.len() as u8);
        assert_eq!(got.len(), MAX_STORED_INCIDENTS);
        assert_eq!(got[0].title, "inc0");
    }

    #[test]
    fn test_write_truncates_long_title() {
        let mut buf = vec![0u8; INCIDENTS_MMAP_SIZE];
        let long_title: String = "a".repeat(TITLE_MAX + 50);
        let inc = Incident {
            severity: Severity::Minor,
            started_at: 0,
            title: long_title.clone(),
            url: "u".to_string(),
        };
        seqlock_write_incidents(&mut buf, &[inc], 1);
        let (got, _) = seqlock_read_incidents(&buf);
        assert_eq!(got[0].title.len(), TITLE_MAX);
        assert_eq!(got[0].title, &long_title[..TITLE_MAX]);
    }

    #[test]
    fn test_write_truncates_long_url() {
        let mut buf = vec![0u8; INCIDENTS_MMAP_SIZE];
        let long_url: String = "u".repeat(URL_MAX + 50);
        let inc = Incident {
            severity: Severity::Minor,
            started_at: 0,
            title: "t".to_string(),
            url: long_url.clone(),
        };
        seqlock_write_incidents(&mut buf, &[inc], 1);
        let (got, _) = seqlock_read_incidents(&buf);
        assert_eq!(got[0].url.len(), URL_MAX);
        assert_eq!(got[0].url, &long_url[..URL_MAX]);
    }

    #[test]
    fn test_write_short_after_long_clears_tail() {
        let mut buf = vec![0u8; INCIDENTS_MMAP_SIZE];
        let long = Incident {
            severity: Severity::Minor,
            started_at: 0,
            title: "x".repeat(TITLE_MAX),
            url: "y".repeat(URL_MAX),
        };
        seqlock_write_incidents(&mut buf, &[long], 1);
        let short = Incident {
            severity: Severity::Minor,
            started_at: 0,
            title: "short".to_string(),
            url: "https://ex.com".to_string(),
        };
        seqlock_write_incidents(&mut buf, &[short], 1);
        let (got, _) = seqlock_read_incidents(&buf);
        assert_eq!(got[0].title, "short");
        assert_eq!(got[0].url, "https://ex.com");
    }
}
