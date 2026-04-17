//! End-to-end integration tests for the incident pipeline.
//!
//! Covers mmap-write-then-read-then-render for two scenarios:
//! (1) a single ongoing incident, (2) two concurrent ongoing incidents.
//! Atom-parse coverage lives in `claudehud-daemon::status::tests`.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use common::incidents::{seqlock_write_incident, Incident, Severity, INCIDENTS_MMAP_SIZE};

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

fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn tmp_mmap_path(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "clhud-e2e-{}-{}-{}.bin",
        tag,
        std::process::id(),
        now_epoch()
    ))
}

fn write_incident_to_tmp(path: &std::path::Path, incident: Option<&Incident>) {
    let mut buf = vec![0u8; INCIDENTS_MMAP_SIZE];
    seqlock_write_incident(&mut buf, incident);
    std::fs::write(path, &buf).unwrap();
}

#[test]
fn test_e2e_single_ongoing_incident() {
    let path = tmp_mmap_path("single");
    let incident = Incident {
        severity: Severity::Major,
        started_at: now_epoch().saturating_sub(7 * 60),
        title: "Elevated API errors".to_string(),
        url: "https://status.claude.com/incidents/single".to_string(),
        active_count: 1,
    };
    write_incident_to_tmp(&path, Some(&incident));

    let read_back = claudehud::incidents::read_incident_from(&path).expect("incident present");
    assert_eq!(read_back, incident);

    let input = claudehud::input::Input::default();
    let out = claudehud::render::render(&input, None, Some(&read_back));
    let plain = strip_ansi(&out);

    assert!(plain.contains("🟠"), "expected major severity icon, got: {plain}");
    assert!(plain.contains("Elevated API errors"));
    assert!(plain.contains("started 7m ago"), "expected '7m ago', got: {plain}");
    assert!(out.contains("\x1b]8;;https://status.claude.com/incidents/single"));
    assert!(!plain.contains("more"), "unexpected +N more suffix: {plain}");

    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_e2e_two_ongoing_incidents() {
    let path = tmp_mmap_path("double");
    let incident = Incident {
        severity: Severity::Critical,
        started_at: now_epoch().saturating_sub(30 * 60),
        title: "API fully down".to_string(),
        url: "https://status.claude.com/incidents/newer".to_string(),
        active_count: 2,
    };
    write_incident_to_tmp(&path, Some(&incident));

    let read_back = claudehud::incidents::read_incident_from(&path).expect("incident present");
    assert_eq!(read_back, incident);
    assert_eq!(read_back.active_count, 2);

    let input = claudehud::input::Input::default();
    let out = claudehud::render::render(&input, None, Some(&read_back));
    let plain = strip_ansi(&out);

    assert!(plain.contains("🔴"), "expected critical icon, got: {plain}");
    assert!(plain.contains("API fully down"));
    assert!(plain.contains("+1 more"), "expected +1 more suffix: {plain}");
    assert!(out.contains("\x1b]8;;https://status.claude.com/\x1b\\"));

    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_e2e_no_incident_mmap_absent() {
    let path = tmp_mmap_path("absent");
    let _ = std::fs::remove_file(&path);
    assert!(claudehud::incidents::read_incident_from(&path).is_none());

    let input = claudehud::input::Input::default();
    let out = claudehud::render::render(&input, None, None);
    let plain = strip_ansi(&out);
    for icon in ["🟡", "🟠", "🔴", "🔧"] {
        assert!(!plain.contains(icon));
    }
}
