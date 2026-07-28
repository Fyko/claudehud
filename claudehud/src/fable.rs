//! Client-side reader for the Fable weekly cap the daemon polls. Degrades
//! silently like the update notice: any error → no fable row.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use common::fable::{fable_limit_path, parse_fable_limit, FableLimit};

/// The live Fable limit, or `None` when the daemon never wrote one (feature
/// off) or the window it describes has already rolled over.
pub fn fable_limit() -> Option<FableLimit> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    fable_limit_at(&fable_limit_path(), now)
}

/// Test seam: evaluate against an explicit path + clock.
///
/// Past `resets_at` the percentage describes a window that no longer exists, so
/// the row disappears rather than showing a stale number until the daemon's
/// next poll. If the daemon is dead it stays gone, which is the honest answer.
pub fn fable_limit_at(path: &Path, now: u64) -> Option<FableLimit> {
    let limit = parse_fable_limit(&std::fs::read_to_string(path).ok()?)?;
    (now < limit.resets_at).then_some(limit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::fable::format_fable_limit;

    fn tmp(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("clhud-fable-{name}-{}", std::process::id()))
    }

    fn write(path: &Path, percent: u8, resets_at: u64) {
        std::fs::write(path, format_fable_limit(&FableLimit { percent, resets_at })).unwrap();
    }

    #[test]
    fn live_window_is_returned() {
        let p = tmp("live");
        write(&p, 51, 1000);
        assert_eq!(
            fable_limit_at(&p, 500),
            Some(FableLimit {
                percent: 51,
                resets_at: 1000
            })
        );
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn rolled_over_window_is_dropped() {
        let p = tmp("stale");
        write(&p, 51, 1000);
        assert_eq!(fable_limit_at(&p, 1000), None);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn missing_file_is_none() {
        assert_eq!(fable_limit_at(&tmp("missing"), 0), None);
    }

    #[test]
    fn garbled_file_is_none() {
        let p = tmp("garbled");
        std::fs::write(&p, "wat").unwrap();
        assert_eq!(fable_limit_at(&p, 0), None);
        let _ = std::fs::remove_file(&p);
    }
}
