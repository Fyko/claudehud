//! The Fable weekly-scoped usage limit, shared between daemon (writer) and
//! client (reader). Plain file, same shape as the update notice:
//! line 1 = percent used, line 2 = window-reset epoch.
//!
//! Claude Code's statusline payload only carries the five-hour and seven-day
//! windows, so the per-model weekly cap has to come from `/api/oauth/usage`.
//! The daemon does that fetch; this file is the handoff.

use std::path::PathBuf;

use crate::cache_dir;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FableLimit {
    pub percent: u8,
    pub resets_at: u64,
}

/// Path to the fable-limit file in the cache dir.
pub fn fable_limit_path() -> PathBuf {
    cache_dir().join("clhud-fable")
}

/// Serialize to file contents (`percent\nresets_at\n`).
pub fn format_fable_limit(f: &FableLimit) -> String {
    format!("{}\n{}\n", f.percent, f.resets_at)
}

/// Parse file contents. `None` on malformed input.
pub fn parse_fable_limit(text: &str) -> Option<FableLimit> {
    let mut lines = text.lines();
    let percent: u8 = lines.next()?.trim().parse().ok()?;
    let resets_at: u64 = lines.next()?.trim().parse().ok()?;
    Some(FableLimit {
        percent: percent.min(100),
        resets_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let f = FableLimit {
            percent: 51,
            resets_at: 1_785_099_600,
        };
        assert_eq!(parse_fable_limit(&format_fable_limit(&f)), Some(f));
    }

    #[test]
    fn garbled_is_none() {
        assert_eq!(parse_fable_limit(""), None);
        assert_eq!(parse_fable_limit("51"), None); // missing epoch line
        assert_eq!(parse_fable_limit("51\nnotnum"), None);
        assert_eq!(parse_fable_limit("notnum\n123"), None);
        assert_eq!(parse_fable_limit("999\n123"), None); // overflows u8
    }

    #[test]
    fn percent_clamped_to_100() {
        assert_eq!(parse_fable_limit("120\n5").unwrap().percent, 100);
    }
}
