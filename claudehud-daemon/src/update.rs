//! Daemon self-update: poll GitHub releases, then download/verify/swap binaries.
//! Pure helpers (target triple, update decision) are unit-tested; the IO-heavy
//! `perform_update` is integration-tested for its swap step.

use std::path::Path;
use std::time::Duration;

use common::version::{compare, VersionState};
use sha2::{Digest, Sha256};

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
///
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
    let Some(target) = target_triple() else {
        return;
    };
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
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

const BASE_URL: &str = "https://github.com/fyko/claudehud/releases/download";
const BINARIES: [&str; 2] = ["claudehud", "claudehud-daemon"];

/// Download both binaries + sidecars, verify, swap in place, write the notice,
/// then exit so the service manager relaunches the new daemon. On any failure,
/// returns `Err` having mutated nothing on disk (verify-before-swap).
fn perform_update(
    agent: &ureq::Agent,
    install_dir: &Path,
    target: &str,
    tag: &str,
) -> Result<(), String> {
    // 1. download + verify BOTH before swapping EITHER.
    let mut payloads: Vec<(&str, Vec<u8>)> = Vec::with_capacity(2);
    for bin in BINARIES {
        let bin_url = format!("{BASE_URL}/{tag}/{bin}-{target}");
        let bytes = download(agent, &bin_url)?;
        let sidecar = download_text(agent, &format!("{bin_url}.sha256"))?;
        let expected = sidecar.split_whitespace().next().unwrap_or("");
        if !verify_sha256(&bytes, expected) {
            return Err(format!("checksum mismatch for {bin}-{target}"));
        }
        payloads.push((bin, bytes));
    }

    // 2. swap both into place.
    for (bin, bytes) in &payloads {
        swap_binary(install_dir, bin, bytes).map_err(|e| format!("swap {bin}: {e}"))?;
    }

    // 3. write the one-shot notice (now + 5 min).
    write_notice(tag);

    // 4. restart: exit so launchd/systemd relaunch the new binary.
    eprintln!("==> autoupdate: installed {tag}, restarting");
    std::process::exit(0);
}

fn download(agent: &ureq::Agent, url: &str) -> Result<Vec<u8>, String> {
    let resp = agent.get(url).call().map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    std::io::copy(&mut resp.into_reader(), &mut buf).map_err(|e| e.to_string())?;
    Ok(buf)
}

fn download_text(agent: &ureq::Agent, url: &str) -> Result<String, String> {
    agent
        .get(url)
        .call()
        .map_err(|e| e.to_string())?
        .into_string()
        .map_err(|e| e.to_string())
}

fn verify_sha256(bytes: &[u8], expected_hex: &str) -> bool {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let actual: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    !expected_hex.is_empty() && actual.eq_ignore_ascii_case(expected_hex)
}

/// Atomically replace `install_dir/name` with `bytes`. Writes a sibling temp in
/// the SAME directory (guarantees a same-filesystem, atomic `rename`), sets the
/// exec bit on unix, then renames over the target. Replacing a running binary's
/// path is safe on unix — the live process keeps its open inode.
fn swap_binary(install_dir: &Path, name: &str, bytes: &[u8]) -> std::io::Result<()> {
    let dest = install_dir.join(name);
    let tmp = install_dir.join(format!("{name}.new"));
    std::fs::write(&tmp, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
    }
    std::fs::rename(&tmp, &dest)
}

fn write_notice(tag: &str) {
    use std::time::{SystemTime, UNIX_EPOCH};
    let Ok(now) = SystemTime::now().duration_since(UNIX_EPOCH) else {
        return;
    };
    let notice = common::notice::Notice {
        version: tag.trim_start_matches('v').to_string(),
        show_until: now.as_secs() + 300,
    };
    let _ = std::fs::write(
        common::notice::update_notice_path(),
        common::notice::format_notice(&notice),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decide_unpinned_newer() {
        assert_eq!(
            decide_target("0.1.0", "v0.2.0", None),
            Some("v0.2.0".into())
        );
    }

    #[test]
    fn decide_unpinned_uptodate() {
        assert_eq!(decide_target("0.2.0", "v0.2.0", None), None);
    }

    #[test]
    fn decide_pinned_targets_pin_not_latest() {
        assert_eq!(
            decide_target("0.1.0", "v0.3.0", Some("v0.2.0")),
            Some("v0.2.0".into())
        );
    }

    #[test]
    fn decide_pinned_already_at_pin_is_noop() {
        assert_eq!(decide_target("0.2.0", "v0.3.0", Some("v0.2.0")), None);
    }

    #[test]
    fn installed_path_rejects_target_dir() {
        assert!(!is_installed_path(Path::new(
            "/home/u/claudehud/target/release/claudehud-daemon"
        )));
        assert!(is_installed_path(Path::new(
            "/home/u/.local/bin/claudehud-daemon"
        )));
    }

    #[test]
    fn swap_binary_replaces_in_place() {
        let dir = std::env::temp_dir().join(format!("clhud-swap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("claudehud-daemon");
        std::fs::write(&dest, b"OLD").unwrap();

        swap_binary(&dir, "claudehud-daemon", b"NEW").unwrap();

        assert_eq!(std::fs::read(&dest).unwrap(), b"NEW");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn verify_sha256_matches_known_vector() {
        // sha256("abc")
        let expected = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert!(verify_sha256(b"abc", expected));
        assert!(!verify_sha256(b"abc", "deadbeef"));
        assert!(
            !verify_sha256(b"abc", ""),
            "empty expected hex must fail closed"
        );
    }
}
