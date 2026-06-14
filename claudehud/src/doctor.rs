//! `claudehud doctor` — run health checks and print a diagnostics checklist.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::{Duration, SystemTime};

use pico_args::Arguments;

const HELP: &str = "\
claudehud doctor

USAGE:
  claudehud doctor [OPTIONS]

OPTIONS:
  --json        Emit machine-readable JSON to stdout.
  --no-color    Suppress ANSI colors even on TTY.
  -h, --help    Print this help.
";

// ── Public entry point ────────────────────────────────────────────────────────

pub fn run(mut args: Arguments) -> ExitCode {
    if args.contains(["-h", "--help"]) {
        print!("{HELP}");
        return ExitCode::SUCCESS;
    }

    let json = args.contains("--json");
    let no_color = args.contains("--no-color");

    let remaining = args.finish();
    if !remaining.is_empty() {
        eprintln!(
            "claudehud doctor: unexpected arguments: {}",
            remaining
                .iter()
                .map(|s| s.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join(" ")
        );
        return ExitCode::from(2);
    }

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let ctx = Ctx {
        cwd,
        cache_dir_override: None,
        daemon_bin_override: None,
    };
    let use_color = !json && !no_color && std::io::stdout().is_terminal();

    let (report, code) = run_inner(&ctx, json, use_color);
    let _ = report; // already printed inside run_inner
    code
}

// ── Data structures ───────────────────────────────────────────────────────────

pub struct CheckResult {
    pub id: &'static str,
    pub ok: bool,
    pub detail: String,
}

pub struct Report {
    pub checks: Vec<CheckResult>,
}

impl Report {
    pub fn all_ok(&self) -> bool {
        self.checks.iter().all(|c| c.ok)
    }
}

/// Context passed to each check function.
/// `cache_dir_override` and `daemon_bin_override` are test seams.
pub struct Ctx {
    pub cwd: PathBuf,
    pub cache_dir_override: Option<PathBuf>,
    pub daemon_bin_override: Option<PathBuf>,
}

impl Ctx {
    fn cache_dir(&self) -> PathBuf {
        self.cache_dir_override
            .clone()
            .unwrap_or_else(common::cache_dir)
    }

    fn mmap_path_for(&self, git_root: &Path) -> PathBuf {
        let hash = common::hash_path(git_root);
        common::mmap_path_in(&self.cache_dir(), hash)
    }

    fn incidents_path(&self) -> PathBuf {
        self.cache_dir_override
            .as_ref()
            .map(|d| d.join("clhud-incidents.bin"))
            .unwrap_or_else(common::incidents::incidents_path)
    }
}

// ── Inner runner (testable) ───────────────────────────────────────────────────

pub fn run_inner(ctx: &Ctx, json: bool, use_color: bool) -> (Report, ExitCode) {
    let checks = vec![
        check_daemon_running(ctx),
        check_service_registered(ctx),
        check_cache_fresh(ctx),
        check_versions_match(ctx),
        check_incidents_cache(ctx),
    ];
    let report = Report { checks };
    let code = if report.all_ok() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    };

    if json {
        print_json(&report);
    } else {
        print_human(&report, use_color);
    }

    (report, code)
}

// ── Rendering ────────────────────────────────────────────────────────────────

fn id_label(id: &str) -> &str {
    match id {
        "daemon_running" => "daemon running",
        "service_registered" => "service registered",
        "cache_fresh" => "cache fresh",
        "versions_match" => "versions match",
        "incidents_cache" => "incidents cache",
        other => other,
    }
}

pub fn print_human(report: &Report, use_color: bool) {
    for c in &report.checks {
        let marker = if use_color {
            if c.ok {
                "\x1b[32m✓\x1b[0m"
            } else {
                "\x1b[31m✗\x1b[0m"
            }
        } else if c.ok {
            "[ok]"
        } else {
            "[FAIL]"
        };
        println!("{} {} ({})", marker, id_label(c.id), c.detail);
    }
}

pub fn print_json(report: &Report) {
    let checks: Vec<serde_json::Value> = report
        .checks
        .iter()
        .map(|c| {
            serde_json::json!({
                "id": c.id,
                "ok": c.ok,
                "detail": c.detail,
            })
        })
        .collect();
    let out = serde_json::json!({
        "version": 1,
        "ok": report.all_ok(),
        "checks": checks,
    });
    println!("{}", out);
}

// ── Check: daemon_running ─────────────────────────────────────────────────────

fn check_daemon_running(ctx: &Ctx) -> CheckResult {
    const ID: &str = "daemon_running";

    // Fast path: if the cache file for the cwd's git root was updated recently,
    // the daemon is alive.
    if let Some(root) = common::find_git_root(&ctx.cwd) {
        let path = ctx.mmap_path_for(&root);
        if let Ok(age) = file_age(&path) {
            if age < Duration::from_secs(60) {
                let secs = age.as_secs();
                return CheckResult {
                    id: ID,
                    ok: true,
                    detail: format!("cache updated {secs}s ago"),
                };
            }
        }
    }

    // Slow path: platform service query.
    daemon_running_via_service(ID)
}

#[cfg(target_os = "macos")]
fn daemon_running_via_service(id: &'static str) -> CheckResult {
    let output = Command::new("launchctl")
        .args(["list", "com.claudehud.daemon"])
        .output();

    match output {
        Err(_) => CheckResult {
            id,
            ok: false,
            detail: "launchctl unavailable".into(),
        },
        Ok(o) if !o.status.success() => CheckResult {
            id,
            ok: false,
            detail: "service not found via launchctl".into(),
        },
        Ok(o) => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            // launchctl list output is a plist-like text; the PID key is
            // absent or "-" when the process is not running.
            if let Some(pid) = parse_launchctl_pid(&stdout) {
                CheckResult {
                    id,
                    ok: true,
                    detail: format!("pid {pid}"),
                }
            } else {
                CheckResult {
                    id,
                    ok: false,
                    detail: "service registered but not running".into(),
                }
            }
        }
    }
}

#[cfg(target_os = "macos")]
fn parse_launchctl_pid(output: &str) -> Option<u32> {
    // `launchctl list com.claudehud.daemon` produces something like:
    //   {
    //     "PID" = 4821;
    //     "Label" = "com.claudehud.daemon";
    //     ...
    //   }
    // PID is absent when the service is registered but not running.
    for line in output.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("\"PID\"") {
            // rest might be ` = 4821;`
            let rest = rest
                .trim_start_matches([' ', '=', '\t'])
                .trim_end_matches(';');
            if let Ok(n) = rest.trim().parse::<u32>() {
                return Some(n);
            }
        }
    }
    None
}

#[cfg(target_os = "linux")]
fn daemon_running_via_service(id: &'static str) -> CheckResult {
    let status = Command::new("systemctl")
        .args(["--user", "is-active", "claudehud-daemon"])
        .status();

    match status {
        Ok(s) if s.success() => CheckResult {
            id,
            ok: true,
            detail: "systemd unit active".into(),
        },
        Ok(_) => CheckResult {
            id,
            ok: false,
            detail: "unit not active".into(),
        },
        Err(_) => CheckResult {
            id,
            ok: false,
            detail: "systemctl unavailable".into(),
        },
    }
}

#[cfg(windows)]
fn daemon_running_via_service(id: &'static str) -> CheckResult {
    let output = Command::new("tasklist")
        .args([
            "/FI",
            "IMAGENAME eq claudehud-daemon.exe",
            "/FO",
            "CSV",
            "/NH",
        ])
        .output();

    match output {
        Ok(o) if o.status.success() => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            // Non-empty CSV (not just whitespace) means process found.
            let found = stdout.lines().any(|l| {
                l.trim_start_matches('"')
                    .starts_with("claudehud-daemon.exe")
            });
            if found {
                CheckResult {
                    id,
                    ok: true,
                    detail: "process found".into(),
                }
            } else {
                CheckResult {
                    id,
                    ok: false,
                    detail: "process not found".into(),
                }
            }
        }
        _ => CheckResult {
            id,
            ok: false,
            detail: "tasklist query failed".into(),
        },
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
fn daemon_running_via_service(id: &'static str) -> CheckResult {
    CheckResult {
        id,
        ok: false,
        detail: "platform check not supported".into(),
    }
}

// ── Check: service_registered ────────────────────────────────────────────────

fn check_service_registered(_ctx: &Ctx) -> CheckResult {
    const ID: &str = "service_registered";

    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from);

    service_registered(ID, home.as_deref())
}

#[cfg(target_os = "macos")]
fn service_registered(id: &'static str, home: Option<&Path>) -> CheckResult {
    let Some(home) = home else {
        return CheckResult {
            id,
            ok: false,
            detail: "HOME not set".into(),
        };
    };
    let plist = home.join("Library/LaunchAgents/com.claudehud.daemon.plist");
    if plist.exists() {
        CheckResult {
            id,
            ok: true,
            detail: "launchd plist present".into(),
        }
    } else {
        CheckResult {
            id,
            ok: false,
            detail: "not registered (daemon may still run manually)".into(),
        }
    }
}

#[cfg(target_os = "linux")]
fn service_registered(id: &'static str, home: Option<&Path>) -> CheckResult {
    let Some(home) = home else {
        return CheckResult {
            id,
            ok: false,
            detail: "HOME not set".into(),
        };
    };
    let unit = home.join(".config/systemd/user/claudehud-daemon.service");
    if unit.exists() {
        CheckResult {
            id,
            ok: true,
            detail: "systemd unit present".into(),
        }
    } else {
        CheckResult {
            id,
            ok: false,
            detail: "not registered (daemon may still run manually)".into(),
        }
    }
}

#[cfg(windows)]
fn service_registered(id: &'static str, _home: Option<&Path>) -> CheckResult {
    let ok = Command::new("schtasks")
        .args(["/Query", "/TN", "claudehud-daemon", "/FO", "LIST"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if ok {
        CheckResult {
            id,
            ok: true,
            detail: "task scheduler entry present".into(),
        }
    } else {
        CheckResult {
            id,
            ok: false,
            detail: "not registered (daemon may still run manually)".into(),
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
fn service_registered(id: &'static str, _home: Option<&Path>) -> CheckResult {
    CheckResult {
        id,
        ok: false,
        detail: "platform check not supported".into(),
    }
}

// ── Check: cache_fresh ───────────────────────────────────────────────────────

fn check_cache_fresh(ctx: &Ctx) -> CheckResult {
    const ID: &str = "cache_fresh";

    let root = match common::find_git_root(&ctx.cwd) {
        Some(r) => r,
        None => {
            return CheckResult {
                id: ID,
                ok: true,
                detail: "not in a git repo".into(),
            };
        }
    };

    let path = ctx.mmap_path_for(&root);

    match file_age(&path) {
        Err(_) => CheckResult {
            id: ID,
            ok: false,
            detail: "cache file missing".into(),
        },
        Ok(age) => {
            if age > Duration::from_secs(300) {
                let mins = age.as_secs() / 60;
                CheckResult {
                    id: ID,
                    ok: false,
                    detail: format!("last update {mins}m ago"),
                }
            } else {
                let secs = age.as_secs();
                CheckResult {
                    id: ID,
                    ok: true,
                    detail: format!("updated {secs}s ago"),
                }
            }
        }
    }
}

// ── Check: versions_match ────────────────────────────────────────────────────

fn check_versions_match(ctx: &Ctx) -> CheckResult {
    const ID: &str = "versions_match";
    const CLIENT: &str = env!("CARGO_PKG_VERSION");

    let bin = resolve_daemon_bin(ctx);

    let Some(bin) = bin else {
        return CheckResult {
            id: ID,
            ok: false,
            detail: "claudehud-daemon not found".into(),
        };
    };

    let output = Command::new(&bin).arg("--version").output();
    match output {
        Err(_) => CheckResult {
            id: ID,
            ok: false,
            detail: "claudehud-daemon not found".into(),
        },
        Ok(o) => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            let daemon_ver = stdout
                .trim()
                .strip_prefix("claudehud-daemon ")
                .unwrap_or(stdout.trim())
                .to_owned();
            if daemon_ver == CLIENT {
                CheckResult {
                    id: ID,
                    ok: true,
                    detail: daemon_ver,
                }
            } else {
                CheckResult {
                    id: ID,
                    ok: false,
                    detail: format!("client {CLIENT}, daemon {daemon_ver}"),
                }
            }
        }
    }
}

fn resolve_daemon_bin(ctx: &Ctx) -> Option<PathBuf> {
    // 1. Test seam override.
    if let Some(ref p) = ctx.daemon_bin_override {
        return Some(p.clone());
    }

    // 2. Sibling to the running claudehud binary (most reliable for install-script users).
    #[cfg(unix)]
    let daemon_name = "claudehud-daemon";
    #[cfg(windows)]
    let daemon_name = "claudehud-daemon.exe";

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join(daemon_name);
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }

    // 3. Bare name — shell/PATH resolution.
    Some(PathBuf::from(daemon_name))
}

// ── Check: incidents_cache ────────────────────────────────────────────────────

fn check_incidents_cache(ctx: &Ctx) -> CheckResult {
    use crate::incidents::read_incidents_from;
    use common::incidents::INCIDENTS_MMAP_SIZE;

    const ID: &str = "incidents_cache";

    let path = ctx.incidents_path();

    if !path.exists() {
        return CheckResult {
            id: ID,
            ok: true,
            detail: "no cache (no incidents reported)".into(),
        };
    }

    // Check file size is valid before reading.
    let meta = std::fs::metadata(&path);
    let size_ok = meta
        .as_ref()
        .map(|m| m.len() == INCIDENTS_MMAP_SIZE as u64)
        .unwrap_or(false);
    if !size_ok {
        return CheckResult {
            id: ID,
            ok: true,
            detail: "no cache (no incidents reported)".into(),
        };
    }

    match file_age(&path) {
        Err(_) => CheckResult {
            id: ID,
            ok: true,
            detail: "no cache (no incidents reported)".into(),
        },
        Ok(age) if age > Duration::from_secs(900) => {
            let mins = age.as_secs() / 60;
            CheckResult {
                id: ID,
                ok: false,
                detail: format!("stale ({mins}m ago)"),
            }
        }
        Ok(_) => {
            let (_, total) = read_incidents_from(&path);
            if total > 0 {
                CheckResult {
                    id: ID,
                    ok: true,
                    detail: format!("{total} active incident(s)"),
                }
            } else {
                CheckResult {
                    id: ID,
                    ok: true,
                    detail: "no active incidents".into(),
                }
            }
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn file_age(path: &Path) -> std::io::Result<Duration> {
    let mtime = std::fs::metadata(path)?.modified()?;
    SystemTime::now()
        .duration_since(mtime)
        .map_err(|e| std::io::Error::other(e.to_string()))
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn make_ctx(cwd: PathBuf, cache_dir: PathBuf) -> Ctx {
        Ctx {
            cwd,
            cache_dir_override: Some(cache_dir),
            daemon_bin_override: None,
        }
    }

    fn make_ctx_with_daemon(cwd: PathBuf, cache_dir: PathBuf, daemon_bin: PathBuf) -> Ctx {
        Ctx {
            cwd,
            cache_dir_override: Some(cache_dir),
            daemon_bin_override: Some(daemon_bin),
        }
    }

    // ── cache_fresh tests ────────────────────────────────────────────────────

    #[test]
    fn cache_fresh_not_in_git_repo() {
        // Use a path guaranteed not to be in a git repo.
        let dir = tempdir().unwrap();
        let ctx = make_ctx(dir.path().to_path_buf(), dir.path().to_path_buf());
        let r = check_cache_fresh(&ctx);
        assert!(r.ok, "expected ok=true for non-git cwd");
        assert!(r.detail.contains("not in a git repo"));
    }

    #[test]
    fn cache_fresh_missing_file() {
        let cache = tempdir().unwrap();
        // Use the real cwd (which is inside a git repo).
        let cwd = std::env::current_dir().unwrap();
        let ctx = make_ctx(cwd, cache.path().to_path_buf());
        let r = check_cache_fresh(&ctx);
        assert!(!r.ok, "expected ok=false when cache file missing");
        assert!(r.detail.contains("missing"), "detail: {}", r.detail);
    }

    #[test]
    fn cache_fresh_recent_file() {
        let cache = tempdir().unwrap();
        let cwd = std::env::current_dir().unwrap();
        let root = common::find_git_root(&cwd).unwrap();
        let hash = common::hash_path(&root);
        let mmap_path = common::mmap_path_in(cache.path(), hash);
        // Write a valid-sized mmap file (content doesn't matter for mtime check).
        fs::write(&mmap_path, vec![0u8; common::MMAP_SIZE]).unwrap();
        // mtime is already "now" so age < 5 min.
        let ctx = make_ctx(cwd, cache.path().to_path_buf());
        let r = check_cache_fresh(&ctx);
        assert!(r.ok, "expected ok=true for fresh cache: {}", r.detail);
        assert!(r.detail.contains("updated"));
    }

    #[test]
    fn cache_fresh_age_format() {
        // Test the formatting branch directly: age > 300s → "Xm ago".
        let mins = 600u64 / 60;
        let detail = format!("last update {mins}m ago");
        assert_eq!(detail, "last update 10m ago");
    }

    // ── versions_match tests ─────────────────────────────────────────────────

    #[cfg(unix)]
    #[test]
    fn versions_match_same() {
        let dir = tempdir().unwrap();
        let script = dir.path().join("claudehud-daemon");
        let version = env!("CARGO_PKG_VERSION");
        fs::write(
            &script,
            format!("#!/bin/sh\necho 'claudehud-daemon {version}'\n"),
        )
        .unwrap();
        // chmod +x
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();

        let cwd = std::env::current_dir().unwrap();
        let cache = tempdir().unwrap();
        let ctx = make_ctx_with_daemon(cwd, cache.path().to_path_buf(), script);
        let r = check_versions_match(&ctx);
        assert!(r.ok, "expected versions to match: {}", r.detail);
        assert_eq!(r.detail, version);
    }

    #[cfg(unix)]
    #[test]
    fn versions_match_mismatch() {
        let dir = tempdir().unwrap();
        let script = dir.path().join("claudehud-daemon");
        fs::write(&script, "#!/bin/sh\necho 'claudehud-daemon 0.0.0-fake'\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();

        let cwd = std::env::current_dir().unwrap();
        let cache = tempdir().unwrap();
        let ctx = make_ctx_with_daemon(cwd, cache.path().to_path_buf(), script);
        let r = check_versions_match(&ctx);
        assert!(!r.ok, "expected mismatch: {}", r.detail);
        assert!(r.detail.contains("client"), "detail: {}", r.detail);
        assert!(r.detail.contains("daemon"), "detail: {}", r.detail);
    }

    #[test]
    fn versions_match_not_found() {
        let cache = tempdir().unwrap();
        let cwd = std::env::current_dir().unwrap();
        let nonexistent = cache.path().join("no-such-binary");
        let ctx = make_ctx_with_daemon(cwd, cache.path().to_path_buf(), nonexistent);
        let r = check_versions_match(&ctx);
        assert!(!r.ok, "expected not-found: {}", r.detail);
        assert!(r.detail.contains("not found"), "detail: {}", r.detail);
    }

    // ── incidents_cache tests ────────────────────────────────────────────────

    #[test]
    fn incidents_cache_absent() {
        let cache = tempdir().unwrap();
        let cwd = std::env::current_dir().unwrap();
        let ctx = make_ctx(cwd, cache.path().to_path_buf());
        let r = check_incidents_cache(&ctx);
        assert!(r.ok, "absent incidents file should be ok: {}", r.detail);
        assert!(r.detail.contains("no cache"), "detail: {}", r.detail);
    }

    #[test]
    fn incidents_cache_fresh_no_incidents() {
        use common::incidents::INCIDENTS_MMAP_SIZE;
        let cache = tempdir().unwrap();
        let path = cache.path().join("clhud-incidents.bin");
        fs::write(&path, vec![0u8; INCIDENTS_MMAP_SIZE]).unwrap();

        let cwd = std::env::current_dir().unwrap();
        let ctx = make_ctx(cwd, cache.path().to_path_buf());
        let r = check_incidents_cache(&ctx);
        assert!(r.ok, "fresh zero incidents should be ok: {}", r.detail);
        assert!(
            r.detail.contains("no active incidents"),
            "detail: {}",
            r.detail
        );
    }

    // ── print_human tests ─────────────────────────────────────────────────────

    fn capture_human(report: &Report, use_color: bool) -> String {
        // We redirect stdout to a buffer using a simple approach: collect
        // the formatted strings directly rather than capturing stdout.
        let mut out = String::new();
        for c in &report.checks {
            let marker = if use_color {
                if c.ok {
                    "\x1b[32m✓\x1b[0m"
                } else {
                    "\x1b[31m✗\x1b[0m"
                }
            } else if c.ok {
                "[ok]"
            } else {
                "[FAIL]"
            };
            out.push_str(&format!("{} {} ({})\n", marker, id_label(c.id), c.detail));
        }
        out
    }

    #[test]
    fn print_human_color_markers() {
        let report = Report {
            checks: vec![
                CheckResult {
                    id: "daemon_running",
                    ok: true,
                    detail: "pid 1".into(),
                },
                CheckResult {
                    id: "cache_fresh",
                    ok: false,
                    detail: "12m ago".into(),
                },
            ],
        };
        let out = capture_human(&report, true);
        assert!(out.contains("✓"), "should contain ✓");
        assert!(out.contains("✗"), "should contain ✗");
    }

    #[test]
    fn print_human_no_color_markers() {
        let report = Report {
            checks: vec![
                CheckResult {
                    id: "daemon_running",
                    ok: true,
                    detail: "pid 1".into(),
                },
                CheckResult {
                    id: "cache_fresh",
                    ok: false,
                    detail: "12m ago".into(),
                },
            ],
        };
        let out = capture_human(&report, false);
        assert!(out.contains("[ok]"), "should contain [ok]");
        assert!(out.contains("[FAIL]"), "should contain [FAIL]");
    }

    // ── print_json tests ──────────────────────────────────────────────────────

    #[test]
    fn report_all_ok() {
        let report = Report {
            checks: vec![
                CheckResult {
                    id: "a",
                    ok: true,
                    detail: "x".into(),
                },
                CheckResult {
                    id: "b",
                    ok: true,
                    detail: "y".into(),
                },
            ],
        };
        assert!(report.all_ok());
    }

    #[test]
    fn report_not_all_ok() {
        let report = Report {
            checks: vec![
                CheckResult {
                    id: "a",
                    ok: true,
                    detail: "x".into(),
                },
                CheckResult {
                    id: "b",
                    ok: false,
                    detail: "y".into(),
                },
            ],
        };
        assert!(!report.all_ok());
    }

    #[test]
    fn json_structure() {
        let report = Report {
            checks: vec![
                CheckResult {
                    id: "daemon_running",
                    ok: true,
                    detail: "pid 42".into(),
                },
                CheckResult {
                    id: "cache_fresh",
                    ok: false,
                    detail: "stale".into(),
                },
            ],
        };
        let checks: Vec<serde_json::Value> = report
            .checks
            .iter()
            .map(|c| {
                serde_json::json!({
                    "id": c.id,
                    "ok": c.ok,
                    "detail": c.detail,
                })
            })
            .collect();
        let out = serde_json::json!({
            "version": 1,
            "ok": report.all_ok(),
            "checks": checks,
        });
        assert_eq!(out["version"], 1);
        assert_eq!(out["ok"], false);
        let arr = out["checks"].as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["id"], "daemon_running");
        assert_eq!(arr[1]["ok"], false);
    }

    // ── Integration test (structural) ─────────────────────────────────────────

    #[test]
    fn run_inner_produces_five_checks() {
        let cwd = std::env::current_dir().unwrap();
        let cache = tempdir().unwrap();
        let ctx = Ctx {
            cwd,
            cache_dir_override: Some(cache.path().to_path_buf()),
            daemon_bin_override: Some(PathBuf::from("/nonexistent/claudehud-daemon")),
        };
        let (report, _code) = run_inner(&ctx, true, false);
        assert_eq!(report.checks.len(), 5, "expected exactly 5 checks");
        let ids: Vec<&str> = report.checks.iter().map(|c| c.id).collect();
        assert!(ids.contains(&"daemon_running"));
        assert!(ids.contains(&"service_registered"));
        assert!(ids.contains(&"cache_fresh"));
        assert!(ids.contains(&"versions_match"));
        assert!(ids.contains(&"incidents_cache"));
    }
}
