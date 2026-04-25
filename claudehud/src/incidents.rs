use std::fs;
use std::path::Path;

use common::incidents::{seqlock_read_incidents, Incident, INCIDENTS_MMAP_PATH, INCIDENTS_MMAP_SIZE};
use memmap2::Mmap;

/// Returns (stored_incidents, total_active_count).
pub fn read_incidents() -> (Vec<Incident>, u8) {
    read_incidents_from(Path::new(INCIDENTS_MMAP_PATH))
}

pub fn read_incidents_from(path: &Path) -> (Vec<Incident>, u8) {
    let file = match fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return (vec![], 0),
    };
    if file.metadata().ok().map(|m| m.len()) != Some(INCIDENTS_MMAP_SIZE as u64) {
        return (vec![], 0);
    }
    // Safety: `file` holds the fd open; the daemon uses a seqlock protocol
    // so concurrent writes are safe to observe.
    let mmap = match unsafe { Mmap::map(&file) } {
        Ok(m) => m,
        Err(_) => return (vec![], 0),
    };
    seqlock_read_incidents(&mmap)
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::incidents::{seqlock_write_incidents, Severity, INCIDENTS_MMAP_SIZE};

    #[test]
    fn test_read_missing_file_returns_empty() {
        let path = std::env::temp_dir().join(format!("clhud-test-missing-{}.bin", std::process::id()));
        let _ = fs::remove_file(&path);
        let (incidents, total) = read_incidents_from(&path);
        assert!(incidents.is_empty());
        assert_eq!(total, 0);
    }

    #[test]
    fn test_read_populated_file_returns_incidents() {
        let path = std::env::temp_dir().join(format!("clhud-test-read-{}.bin", std::process::id()));
        let incident = Incident {
            severity: Severity::Major,
            started_at: 1_700_000_000,
            title: "Read test".to_string(),
            url: "https://example.com/x".to_string(),
        };

        let mut buf = vec![0u8; INCIDENTS_MMAP_SIZE];
        seqlock_write_incidents(&mut buf, &[incident.clone()], 1);
        fs::write(&path, &buf).unwrap();

        let (got, total) = read_incidents_from(&path);
        assert_eq!(total, 1);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0], incident);

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_read_wrong_size_file_returns_empty() {
        let path = std::env::temp_dir().join(format!("clhud-test-trunc-{}.bin", std::process::id()));
        fs::write(&path, b"short").unwrap();
        let (incidents, total) = read_incidents_from(&path);
        assert!(incidents.is_empty());
        assert_eq!(total, 0);
        let _ = fs::remove_file(&path);
    }
}
