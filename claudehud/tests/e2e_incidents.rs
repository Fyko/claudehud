//! End-to-end integration tests for the incident pipeline.
//!
//! Covers mmap-write-then-read-then-render for two scenarios:
//! (1) a single ongoing incident, (2) two concurrent ongoing incidents.
//! Atom-parse coverage lives in `claudehud-daemon::status::tests`.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use common::incidents::{seqlock_write_incidents, Incident, Severity, INCIDENTS_MMAP_SIZE};

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

fn write_incidents_to_tmp(path: &std::path::Path, incidents: &[Incident], total: u8) {
    let mut buf = vec![0u8; INCIDENTS_MMAP_SIZE];
    seqlock_write_incidents(&mut buf, incidents, total);
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
    };
    write_incidents_to_tmp(&path, &[incident.clone()], 1);

    let (read_back, total) = claudehud::incidents::read_incidents_from(&path);
    assert_eq!(total, 1);
    assert_eq!(read_back.len(), 1);
    assert_eq!(read_back[0], incident);

    let input = claudehud::input::Input::default();
    let out = claudehud::render::render(&input, None, &read_back, total, claudehud::render::RoundingMode::Floor);
    let plain = strip_ansi(&out);

    assert!(out.contains(claudehud::fmt::ORANGE), "expected orange color for major severity, got: {plain}");
    assert!(plain.contains("Elevated API errors"));
    assert!(plain.contains("started 7m ago"), "expected '7m ago', got: {plain}");
    assert!(out.contains("\x1b]8;;https://status.claude.com/incidents/single"));
    assert!(!plain.contains("more"), "unexpected +N more suffix: {plain}");

    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_e2e_two_ongoing_incidents() {
    let path = tmp_mmap_path("double");
    let incidents = vec![
        Incident {
            severity: Severity::Critical,
            started_at: now_epoch().saturating_sub(30 * 60),
            title: "API fully down".to_string(),
            url: "https://status.claude.com/incidents/newer".to_string(),
        },
        Incident {
            severity: Severity::Minor,
            started_at: now_epoch().saturating_sub(60 * 60),
            title: "Elevated latency".to_string(),
            url: "https://status.claude.com/incidents/older".to_string(),
        },
    ];
    write_incidents_to_tmp(&path, &incidents, 2);

    let (read_back, total) = claudehud::incidents::read_incidents_from(&path);
    assert_eq!(total, 2);
    assert_eq!(read_back.len(), 2);

    let input = claudehud::input::Input::default();
    let out = claudehud::render::render(&input, None, &read_back, total, claudehud::render::RoundingMode::Floor);
    let plain = strip_ansi(&out);

    assert!(out.contains(claudehud::fmt::RED), "expected red color for critical severity, got: {plain}");
    assert!(plain.contains("API fully down"));
    assert!(plain.contains("Elevated latency"), "second incident should be visible");
    assert!(!plain.contains("more"), "no overflow with 2 stored of 2 total");

    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_e2e_overflow_shows_plus_n_more() {
    let path = tmp_mmap_path("overflow");
    let incident = Incident {
        severity: Severity::Minor,
        started_at: now_epoch(),
        title: "Thing A".to_string(),
        url: "https://status.claude.com/incidents/a".to_string(),
    };
    // 1 stored, total=3 → "+2 more"
    write_incidents_to_tmp(&path, &[incident], 3);

    let (read_back, total) = claudehud::incidents::read_incidents_from(&path);
    let input = claudehud::input::Input::default();
    let out = claudehud::render::render(&input, None, &read_back, total, claudehud::render::RoundingMode::Floor);
    let plain = strip_ansi(&out);
    assert!(plain.contains("+2 more"), "got: {plain}");
    assert!(out.contains("\x1b]8;;https://status.claude.com/\x1b\\"));

    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_e2e_no_incident_mmap_absent() {
    let path = tmp_mmap_path("absent");
    let _ = std::fs::remove_file(&path);
    let (incidents, total) = claudehud::incidents::read_incidents_from(&path);
    assert!(incidents.is_empty());
    assert_eq!(total, 0);

    let input = claudehud::input::Input::default();
    let out = claudehud::render::render(&input, None, &incidents, total, claudehud::render::RoundingMode::Floor);
    let plain = strip_ansi(&out);
    assert!(!plain.contains("·"), "incident separator should not appear without incident");
}
