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

    // The poll source and the download step share one agent (it's an Arc
    // internally, so the clone is cheap); the source owns its clone, the loop
    // body uses the original for `perform_update`'s downloads.
    let source = crate::poll::UreqSource::new(agent.clone(), RELEASES_API);
    run_update_poll(
        &source,
        &crate::poll::RealClock,
        &agent,
        &install_dir,
        target,
        cfg.pin.as_deref(),
        FIRST_DELAY,
        POLL_INTERVAL,
    );
}

/// The autoupdate adapter over the shared poll loop: on each fresh releases
/// body, parse the latest tag, decide whether to move, and if so perform the
/// update. A parse failure (or no-update decision) no-ops per ADR-0001;
/// `perform_update` exits the process on success, so a return here means it
/// failed and the loop keeps polling.
#[allow(clippy::too_many_arguments)]
fn run_update_poll<S, C>(
    source: &S,
    clock: &C,
    download_agent: &ureq::Agent,
    install_dir: &Path,
    target: &str,
    pin: Option<&str>,
    first_delay: Duration,
    interval: Duration,
) where
    S: crate::poll::ConditionalGet,
    C: crate::poll::Clock,
{
    let installed = env!("CARGO_PKG_VERSION");
    crate::poll::run_poll_loop(
        source,
        clock,
        "autoupdate",
        Some(first_delay),
        interval,
        |body| {
            let tag = match common::version::parse_tag(body.as_bytes()) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("WARN autoupdate parse: {e}");
                    return;
                }
            };
            if let Some(target_tag) = decide_target(installed, &tag, pin) {
                if let Err(e) = perform_update(download_agent, install_dir, target, &target_tag) {
                    eprintln!("WARN autoupdate: {e}");
                }
                // perform_update exits the process on success; if we're still
                // here it failed — keep polling.
            }
        },
    );
}

const BASE_URL: &str = "https://github.com/fyko/claudehud/releases/download";
const BINARIES: [&str; 2] = ["claudehud", "claudehud-daemon"];

/// Normalize a tag to a canonical `v`-prefixed form.
/// GitHub download paths require the literal `v` prefix — a hand-edited pin
/// like `pin=0.2.0` would otherwise produce a 404 URL forever.
fn normalized_tag(tag: &str) -> String {
    format!("v{}", tag.trim_start_matches('v'))
}

/// Download both binaries + sidecars, verify, swap in place, write the notice,
/// then exit so the service manager relaunches the new daemon. On any failure,
/// returns `Err` having mutated nothing on disk (verify-before-swap).
fn perform_update(
    agent: &ureq::Agent,
    install_dir: &Path,
    target: &str,
    tag: &str,
) -> Result<(), String> {
    // GitHub download paths need the literal `v`-prefixed tag; a hand-edited
    // pin like `pin=0.2.0` would otherwise 404 forever. Normalize once.
    let tag = normalized_tag(tag);
    let tag = tag.as_str();

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
    fn normalized_tag_adds_v_once() {
        assert_eq!(normalized_tag("0.2.0"), "v0.2.0");
        assert_eq!(normalized_tag("v0.2.0"), "v0.2.0");
    }

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
    fn update_poll_cycle_uptodate_decides_no_update_no_io() {
        // Drive a full autoupdate poll cycle with a fake network + fake clock:
        // fetch a releases body whose tag equals the installed version, so
        // `decide_target` returns None and no download/swap IO is attempted.
        // A second cycle returns 304, proving the etag from cycle 1 is sent
        // back as If-None-Match. No real HTTP, no real sleep.
        use crate::poll::test_support::{FakeClock, FakeSource};
        use crate::poll::FetchOutcome;

        let installed = env!("CARGO_PKG_VERSION");
        let body = format!("{{\"tag_name\":\"v{installed}\"}}");
        let source = FakeSource::new(vec![
            Ok(FetchOutcome::Body {
                body,
                etag: Some("rel-etag".to_string()),
            }),
            Ok(FetchOutcome::NotModified),
        ]);
        let clock = FakeClock::keep_for(2);
        // The download agent is never used because no update is decided.
        let agent = ureq::AgentBuilder::new().build();

        run_update_poll(
            &source,
            &clock,
            &agent,
            Path::new("/does/not/matter"),
            "x86_64-unknown-linux-musl",
            None,
            Duration::from_secs(60),
            Duration::from_secs(300),
        );

        // First request unconditional; second carried the etag from cycle 1.
        assert_eq!(
            source.seen_etags.borrow().as_slice(),
            &[None, Some("rel-etag".to_string())]
        );
        // First-delay honored before the two interval sleeps.
        assert_eq!(
            clock.slept.borrow().as_slice(),
            &[
                Duration::from_secs(60),
                Duration::from_secs(300),
                Duration::from_secs(300)
            ]
        );
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
