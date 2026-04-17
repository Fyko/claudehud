use std::fs;
use std::path::Path;

use common::incidents::{seqlock_read_incident, Incident, INCIDENTS_MMAP_PATH, INCIDENTS_MMAP_SIZE};
use memmap2::Mmap;

pub fn read_incident() -> Option<Incident> {
    read_incident_from(Path::new(INCIDENTS_MMAP_PATH))
}

pub fn read_incident_from(path: &Path) -> Option<Incident> {
    let file = fs::File::open(path).ok()?;
    if file.metadata().ok()?.len() != INCIDENTS_MMAP_SIZE as u64 {
        return None;
    }
    // Safety: `file` holds the fd open; the daemon uses a seqlock protocol
    // so concurrent writes are safe to observe.
    let mmap = unsafe { Mmap::map(&file) }.ok()?;
    seqlock_read_incident(&mmap)
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::incidents::{seqlock_write_incident, Severity};

    #[test]
    fn test_read_missing_file_returns_none() {
        let path = std::env::temp_dir().join(format!("clhud-test-missing-{}.bin", std::process::id()));
        let _ = fs::remove_file(&path);
        assert_eq!(read_incident_from(&path), None);
    }

    #[test]
    fn test_read_populated_file_returns_incident() {
        let path = std::env::temp_dir().join(format!("clhud-test-read-{}.bin", std::process::id()));
        let incident = Incident {
            severity: Severity::Major,
            started_at: 1_700_000_000,
            title: "Read test".to_string(),
            url: "https://example.com/x".to_string(),
            active_count: 1,
        };

        let mut buf = vec![0u8; INCIDENTS_MMAP_SIZE];
        seqlock_write_incident(&mut buf, Some(&incident));
        fs::write(&path, &buf).unwrap();

        let got = read_incident_from(&path).expect("populated");
        assert_eq!(got, incident);

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_read_wrong_size_file_returns_none() {
        let path = std::env::temp_dir().join(format!("clhud-test-trunc-{}.bin", std::process::id()));
        fs::write(&path, b"short").unwrap();
        assert_eq!(read_incident_from(&path), None);
        let _ = fs::remove_file(&path);
    }
}
