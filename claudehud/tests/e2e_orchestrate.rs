//! End-to-end integration test for the client's stdin→HUD orchestration.
//!
//! Exercises the full path — raw JSON in, rendered HUD string out — through the
//! public `orchestrate::run` seam with a fully-injected environment: no real
//! `/tmp` cache file, no network, no `git` subprocess, and no wall clock. The
//! git segment, incident line, and update notice are all supplied by a fake.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use claudehud::input::Input;
use claudehud::orchestrate::{run, Env, Options};
use claudehud::render::{Layout, RoundingMode};
use common::incidents::{Incident, Severity};

fn strip_ansi(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('[') => {
                for c2 in chars.by_ref() {
                    if ('\x40'..='\x7e').contains(&c2) {
                        break;
                    }
                }
            }
            Some(']') => {
                while let Some(c2) = chars.next() {
                    if c2 == '\x07' {
                        break;
                    }
                    if c2 == '\x1b' {
                        if let Some('\\') = chars.peek() {
                            chars.next();
                        }
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Fully-injected environment. No real I/O of any kind.
struct FakeEnv {
    branch: Option<(String, bool)>,
    incidents: (Vec<Incident>, u8),
    notice: Option<String>,
}

impl Env for FakeEnv {
    fn resolve_branch(&self, _input: &Input, _cwd: &Path) -> Option<(String, bool)> {
        self.branch.clone()
    }
    fn read_incidents(&self) -> (Vec<Incident>, u8) {
        self.incidents.clone()
    }
    fn active_notice(&self) -> Option<String> {
        self.notice.clone()
    }
}

const FIXTURE: &str = r#"{
    "cwd": "/home/user/project",
    "model": {"id": "claude-opus-4-7", "display_name": "Opus 4.7"},
    "version": "2.1.139",
    "context_window": {
        "context_window_size": 200000,
        "used_percentage": 22,
        "remaining_percentage": 78
    }
}"#;

#[test]
fn test_e2e_stdin_to_hud_all_segments_injected() {
    let env = FakeEnv {
        branch: Some(("feature/seam".to_string(), true)),
        incidents: (
            vec![Incident {
                severity: Severity::Major,
                // Recent so render shows the title rather than collapsing it to
                // the long-running (24h+) breadcrumb. render's age math is the
                // pre-existing wall-clock read inside `render::render`, not the
                // orchestration seam under test.
                started_at: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs()
                    .saturating_sub(5 * 60),
                title: "Elevated API errors".to_string(),
                url: "https://status.claude.com/incidents/x".to_string(),
            }],
            1,
        ),
        notice: Some("9.9.9".to_string()),
    };

    let out = run(
        FIXTURE,
        &env,
        Options {
            rounding: RoundingMode::Floor,
            layout: Layout::Comfortable,
        },
    );
    let plain = strip_ansi(&out);

    // Model segment from the JSON.
    assert!(plain.contains("Opus 4.7"), "model segment, got: {plain}");
    // Git segment from the injected branch.
    assert!(
        plain.contains("feature/seam"),
        "injected branch, got: {plain}"
    );
    // Incident line from the injected incident.
    assert!(
        plain.contains("Elevated API errors"),
        "incident line, got: {plain}"
    );
    // Update notice from the injected notice.
    assert!(plain.contains("9.9.9"), "update notice, got: {plain}");
}

#[test]
fn test_e2e_empty_stdin_yields_bare_claude() {
    let env = FakeEnv {
        branch: None,
        incidents: (vec![], 0),
        notice: None,
    };
    let out = run(
        "",
        &env,
        Options {
            rounding: RoundingMode::Floor,
            layout: Layout::Comfortable,
        },
    );
    assert_eq!(out, "Claude");
}

#[test]
fn test_e2e_condensed_layout_is_single_line() {
    let env = FakeEnv {
        branch: Some(("main".to_string(), false)),
        incidents: (vec![], 0),
        notice: None,
    };
    let out = run(
        FIXTURE,
        &env,
        Options {
            rounding: RoundingMode::Floor,
            layout: Layout::Condensed,
        },
    );
    assert!(
        !strip_ansi(&out).contains('\n'),
        "condensed layout should be a single line, got: {out:?}"
    );
}
