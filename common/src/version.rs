//! Shared version comparison + GitHub release-tag parsing.
//! Used by the client `update` subcommand and the daemon autoupdater.

use std::io;

#[derive(Debug, PartialEq, Eq)]
pub enum VersionState {
    UpToDate,
    Newer(String),
    Ahead(String),
}

/// Parse the `tag_name` field out of a GitHub release JSON body.
pub fn parse_tag(body: &[u8]) -> io::Result<String> {
    let v: serde_json::Value = serde_json::from_slice(body).map_err(|e| {
        io::Error::new(io::ErrorKind::InvalidData, format!("bad JSON from GitHub: {e}"))
    })?;
    v.get("tag_name")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "GitHub response had no tag_name")
        })
}

/// Compare an installed version (`0.1.0`) against a release tag (`v0.2.0`).
pub fn compare(installed: &str, tag: &str) -> VersionState {
    let latest = tag.trim_start_matches('v').to_string();
    let installed_parts = parse_semver(installed);
    let latest_parts = parse_semver(&latest);
    match (installed_parts, latest_parts) {
        (Some(i), Some(l)) if i == l => VersionState::UpToDate,
        (Some(i), Some(l)) if i < l => VersionState::Newer(latest),
        (Some(_), Some(_)) => VersionState::Ahead(latest),
        _ if installed == latest => VersionState::UpToDate,
        _ => VersionState::Newer(latest),
    }
}

/// Parse `MAJOR.MINOR.PATCH[-pre]` into a comparable tuple. Pre-release suffixes
/// sort *before* the bare release.
pub fn parse_semver(s: &str) -> Option<(u64, u64, u64, Option<String>)> {
    let (core, pre) = match s.split_once('-') {
        Some((c, p)) => (c, Some(p.to_string())),
        None => (s, None),
    };
    let mut it = core.split('.');
    let major: u64 = it.next()?.parse().ok()?;
    let minor: u64 = it.next()?.parse().ok()?;
    let patch: u64 = it.next()?.parse().ok()?;
    if it.next().is_some() {
        return None;
    }
    let pre_key = pre.unwrap_or_else(|| "~".to_string());
    Some((major, minor, patch, Some(pre_key)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_tag_extracts_tag_name() {
        let body = br#"{"tag_name":"v0.1.0","name":"Release 0.1.0"}"#;
        assert_eq!(parse_tag(body).unwrap(), "v0.1.0");
    }

    #[test]
    fn parse_tag_errors_on_missing_field() {
        assert!(parse_tag(br#"{"name":"x"}"#).is_err());
    }

    #[test]
    fn parse_tag_errors_on_bad_json() {
        assert!(parse_tag(b"not json at all").is_err());
    }

    #[test]
    fn compare_equal_versions() {
        assert_eq!(compare("0.1.0", "v0.1.0"), VersionState::UpToDate);
        assert_eq!(compare("0.1.0", "0.1.0"), VersionState::UpToDate);
    }

    #[test]
    fn compare_installed_older() {
        assert_eq!(compare("0.1.0", "v0.2.0"), VersionState::Newer("0.2.0".into()));
        assert_eq!(compare("0.1.9", "v0.1.10"), VersionState::Newer("0.1.10".into()));
    }

    #[test]
    fn compare_installed_ahead() {
        assert_eq!(compare("0.2.0", "v0.1.0"), VersionState::Ahead("0.1.0".into()));
    }

    #[test]
    fn compare_prerelease_is_less_than_release() {
        assert_eq!(compare("0.1.0-alpha.4", "v0.1.0"), VersionState::Newer("0.1.0".into()));
        assert_eq!(compare("0.1.0", "v0.1.0-alpha.4"), VersionState::Ahead("0.1.0-alpha.4".into()));
    }

    #[test]
    fn compare_unparseable_falls_back_to_string_eq() {
        assert_eq!(compare("weird", "weird"), VersionState::UpToDate);
        assert_eq!(compare("weird", "other"), VersionState::Newer("other".into()));
    }
}
