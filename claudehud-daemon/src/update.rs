//! Daemon self-update: poll GitHub releases, then download/verify/swap binaries.
//! Pure helpers (target triple, update decision) are unit-tested; the IO-heavy
//! `perform_update` is integration-tested for its swap step.

use std::path::Path;
use std::time::Duration;

use common::version::{compare, VersionState};

const RELEASES_API: &str = "https://api.github.com/repos/fyko/claudehud/releases/latest";
const USER_AGENT: &str = concat!("claudehud-daemon/", env!("CARGO_PKG_VERSION"));
const FIRST_DELAY: Duration = Duration::from_secs(60);
const POLL_INTERVAL: Duration = Duration::from_secs(300);

/// The release-asset target triple for the current platform. `None` on
/// unsupported / not-yet-implemented platforms (e.g. Windows in v1).
pub fn target_triple() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Some("aarch64-apple-darwin"),
        ("macos", "x86_64") => Some("x86_64-apple-darwin"),
        ("linux", "x86_64") => Some("x86_64-unknown-linux-musl"),
        ("linux", "aarch64") => Some("aarch64-unknown-linux-musl"),
        _ => None,
    }
}

/// Decide which tag (if any) to update to.
/// - `pin` present → target the pinned tag directly (cap behavior: a pinned
///   install that's already at the pin won't move; one pinned ahead moves once).
/// - `pin` absent → target `latest`.
/// Returns `Some(tag)` only when the target is strictly newer than `installed`.
pub fn decide_target(installed: &str, latest: &str, pin: Option<&str>) -> Option<String> {
    let target = pin.unwrap_or(latest);
    match compare(installed, target) {
        VersionState::Newer(_) => Some(target.to_string()),
        _ => None,
    }
}

/// True if `exe` looks like a real install (not a `cargo` build dir). Guards
/// against a dev binary self-updating.
pub fn is_installed_path(exe: &Path) -> bool {
    !exe.components().any(|c| c.as_os_str() == "target")
}

/// Entry point for the autoupdate thread. Returns (thread exits) when
/// autoupdate is disabled, on a dev build, or for an unsupported platform.
pub fn start() {
    // Never self-update a debug/dev build.
    if cfg!(debug_assertions) {
        return;
    }
    let cfg = common::config::load();
    if !cfg.autoupdate {
        return;
    }
    let Some(target) = target_triple() else { return };
    let Ok(exe) = std::env::current_exe() else { return };
    if !is_installed_path(&exe) {
        return;
    }
    let install_dir = match exe.parent() {
        Some(d) => d.to_path_buf(),
        None => return,
    };

    let agent = ureq::AgentBuilder::new()
        .user_agent(USER_AGENT)
        .timeout_connect(Duration::from_secs(5))
        .timeout_read(Duration::from_secs(15))
        .build();

    let mut etag: Option<String> = None;
    std::thread::sleep(FIRST_DELAY);
    loop {
        match fetch_latest_tag(&agent, etag.as_deref()) {
            Ok(Some((tag, new_etag))) => {
                etag = new_etag;
                let installed = env!("CARGO_PKG_VERSION");
                if let Some(target_tag) = decide_target(installed, &tag, cfg.pin.as_deref()) {
                    if let Err(e) = perform_update(&agent, &install_dir, target, &target_tag) {
                        eprintln!("WARN autoupdate: {e}");
                    }
                    // perform_update exits the process on success; if we're still
                    // here it failed — keep polling.
                }
            }
            Ok(None) => {} // 304 Not Modified
            Err(e) => eprintln!("WARN autoupdate fetch: {e}"),
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// Conditional GET of the latest release. `Ok(None)` on 304.
fn fetch_latest_tag(
    agent: &ureq::Agent,
    etag: Option<&str>,
) -> Result<Option<(String, Option<String>)>, String> {
    let mut req = agent.get(RELEASES_API);
    if let Some(tag) = etag {
        req = req.set("If-None-Match", tag);
    }
    match req.call() {
        Ok(resp) => {
            let new_etag = resp.header("ETag").map(str::to_string);
            let body = resp.into_string().map_err(|e| e.to_string())?;
            let tag = common::version::parse_tag(body.as_bytes()).map_err(|e| e.to_string())?;
            Ok(Some((tag, new_etag)))
        }
        Err(ureq::Error::Status(304, _)) => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

// TEMPORARY stub — the real download/verify/swap implementation lands in Task 6.
fn perform_update(
    _agent: &ureq::Agent,
    _install_dir: &Path,
    _target: &str,
    _tag: &str,
) -> Result<(), String> {
    Err("perform_update not yet implemented".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decide_unpinned_newer() {
        assert_eq!(decide_target("0.1.0", "v0.2.0", None), Some("v0.2.0".into()));
    }

    #[test]
    fn decide_unpinned_uptodate() {
        assert_eq!(decide_target("0.2.0", "v0.2.0", None), None);
    }

    #[test]
    fn decide_pinned_targets_pin_not_latest() {
        assert_eq!(decide_target("0.1.0", "v0.3.0", Some("v0.2.0")), Some("v0.2.0".into()));
    }

    #[test]
    fn decide_pinned_already_at_pin_is_noop() {
        assert_eq!(decide_target("0.2.0", "v0.3.0", Some("v0.2.0")), None);
    }

    #[test]
    fn installed_path_rejects_target_dir() {
        assert!(!is_installed_path(Path::new("/home/u/claudehud/target/release/claudehud-daemon")));
        assert!(is_installed_path(Path::new("/home/u/.local/bin/claudehud-daemon")));
    }
}
