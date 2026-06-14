//! Client-side reader for the one-shot update notice. Degrades silently like
//! the git cache: any error → no notice.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use common::notice::{parse_notice, update_notice_path};

/// Returns the version string to advertise, or `None` if there's no active
/// notice. Best-effort removes the file once it has expired.
pub fn active_notice() -> Option<String> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    active_notice_at(&update_notice_path(), now)
}

/// Test seam: evaluate against an explicit path + clock.
pub fn active_notice_at(path: &Path, now: u64) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let notice = parse_notice(&text)?;
    if now < notice.show_until {
        Some(notice.version)
    } else {
        let _ = std::fs::remove_file(path);
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::notice::{format_notice, Notice};

    fn tmp(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("clhud-notice-{name}-{}", std::process::id()))
    }

    #[test]
    fn active_when_before_deadline() {
        let p = tmp("active");
        std::fs::write(
            &p,
            format_notice(&Notice {
                version: "0.2.0".into(),
                show_until: 1000,
            }),
        )
        .unwrap();
        assert_eq!(active_notice_at(&p, 500), Some("0.2.0".to_string()));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn expired_returns_none_and_removes() {
        let p = tmp("expired");
        std::fs::write(
            &p,
            format_notice(&Notice {
                version: "0.2.0".into(),
                show_until: 1000,
            }),
        )
        .unwrap();
        assert_eq!(active_notice_at(&p, 1000), None);
        assert!(!p.exists(), "expired notice file should be removed");
    }

    #[test]
    fn missing_file_is_none() {
        assert_eq!(active_notice_at(&tmp("missing"), 0), None);
    }
}
