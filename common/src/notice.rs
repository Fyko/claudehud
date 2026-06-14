//! The one-shot "updated to vX" notice shared between daemon (writer) and
//! client (reader). Plain file: line 1 = version, line 2 = show-until epoch.

use std::path::PathBuf;

use crate::cache_dir;

#[derive(Debug, PartialEq, Eq)]
pub struct Notice {
    pub version: String,
    pub show_until: u64,
}

/// Path to the notice file in the cache dir.
pub fn update_notice_path() -> PathBuf {
    cache_dir().join("clhud-update-notice")
}

/// Serialize to file contents (`version\nshow_until\n`).
pub fn format_notice(n: &Notice) -> String {
    format!("{}\n{}\n", n.version, n.show_until)
}

/// Parse file contents. `None` on malformed input.
pub fn parse_notice(text: &str) -> Option<Notice> {
    let mut lines = text.lines();
    let version = lines.next()?.trim().to_string();
    if version.is_empty() {
        return None;
    }
    let show_until = lines.next()?.trim().parse::<u64>().ok()?;
    Some(Notice {
        version,
        show_until,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let n = Notice {
            version: "0.2.0".into(),
            show_until: 1_700_000_300,
        };
        assert_eq!(parse_notice(&format_notice(&n)), Some(n));
    }

    #[test]
    fn garbled_is_none() {
        assert_eq!(parse_notice(""), None);
        assert_eq!(parse_notice("0.2.0"), None); // missing epoch line
        assert_eq!(parse_notice("0.2.0\nnotnum"), None);
        assert_eq!(parse_notice("\n123"), None); // empty version
    }
}
