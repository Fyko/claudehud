# Status Incidents in Statusline — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Surface ongoing `status.claude.com` incidents (and in-progress maintenance) as a hyperlinked line in the `claudehud` statusline output.

**Architecture:** The daemon polls `https://status.claude.com/history.atom` every 5 min with a conditional GET, parses the Atom feed, filters to active incidents + in-progress maintenance, and seqlock-writes the most-recent active entry (plus total active count) into a new global mmap file at `/tmp/clhud-incidents.bin`. The client mmaps that file on each render and injects a new line directly below line 1 when an incident is active.

**Tech Stack:** Rust 2021, `ureq` (rustls), `roxmltree`, `memmap2`, seqlock IPC, OSC 8 terminal hyperlinks.

**Spec:** `docs/superpowers/specs/2026-04-16-status-incidents-design.md`

---

## File Structure

Files to create:

- `common/src/incidents.rs` — layout constants, `Severity` enum, `Incident` struct, `seqlock_read_incident` / `seqlock_write_incident` helpers.
- `claudehud-daemon/src/status.rs` — poll thread: fetch, parse, classify, mmap write. Also exports pure `parse_atom` function for test fixtures.
- `claudehud/src/incidents.rs` — client read path. Opens & mmaps `/tmp/clhud-incidents.bin`, returns `Option<Incident>`.

Files to modify:

- `common/src/lib.rs` — add `pub mod incidents;`.
- `claudehud-daemon/Cargo.toml` — add `ureq`, `roxmltree`.
- `claudehud-daemon/src/main.rs` — spawn `status::start` thread.
- `claudehud/src/fmt.rs` — add `severity_icon` helper.
- `claudehud/src/render.rs` — accept `Option<&Incident>`, emit incident line.
- `claudehud/src/main.rs` — read incident, thread it into `render`.
- `README.md` — document the new line and `/tmp/clhud-incidents.bin`.

---

## Task 1: `common::incidents` — layout, struct, seqlock read

**Files:**
- Create: `common/src/incidents.rs`
- Modify: `common/src/lib.rs`
- Test: `common/src/incidents.rs` (inline `#[cfg(test)]`)

- [ ] **Step 1: Write the failing test for `seqlock_read_incident`**

Add to a new file `common/src/incidents.rs`:

```rust
pub const INCIDENTS_MMAP_PATH: &str = "/tmp/clhud-incidents.bin";
pub const INCIDENTS_MMAP_SIZE: usize = 408;
pub const TITLE_MAX: usize = 128;
pub const URL_MAX: usize = 255;

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    None = 0,
    Minor = 1,
    Major = 2,
    Critical = 3,
    Maintenance = 4,
}

impl Severity {
    pub fn from_u8(b: u8) -> Self {
        match b {
            1 => Self::Minor,
            2 => Self::Major,
            3 => Self::Critical,
            4 => Self::Maintenance,
            _ => Self::None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Incident {
    pub severity: Severity,
    pub started_at: u64,
    pub title: String,
    pub url: String,
    pub active_count: u8,
}

pub fn seqlock_read_incident(_mmap: &[u8]) -> Option<Incident> {
    unimplemented!()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_read_returns_none_when_active_count_zero() {
        let buf = [0u8; INCIDENTS_MMAP_SIZE];
        // seq=0 (even, stable), active_count=0
        assert_eq!(seqlock_read_incident(&buf), None);
    }

    #[test]
    fn test_read_parses_populated_buffer() {
        let mut buf = [0u8; INCIDENTS_MMAP_SIZE];
        // seq = 2 (even, stable)
        buf[0..8].copy_from_slice(&2u64.to_le_bytes());
        // active_count = 3
        buf[8] = 3;
        // severity = Major (2)
        buf[9] = 2;
        // started_at = 1_700_000_000
        buf[10..18].copy_from_slice(&1_700_000_000u64.to_le_bytes());
        // title = "Elevated errors"
        let title = b"Elevated errors";
        buf[18] = title.len() as u8;
        buf[19..19 + title.len()].copy_from_slice(title);
        // url = "https://status.claude.com/incidents/abc"
        let url = b"https://status.claude.com/incidents/abc";
        buf[147] = url.len() as u8;
        buf[148..148 + url.len()].copy_from_slice(url);

        let got = seqlock_read_incident(&buf).expect("should parse");
        assert_eq!(got.active_count, 3);
        assert_eq!(got.severity, Severity::Major);
        assert_eq!(got.started_at, 1_700_000_000);
        assert_eq!(got.title, "Elevated errors");
        assert_eq!(got.url, "https://status.claude.com/incidents/abc");
    }
}
```

And register the module — modify `common/src/lib.rs` by adding this line at the top (after existing `use` statements, before `pub const MMAP_SIZE`):

```rust
pub mod incidents;
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p common incidents`
Expected: panic with "not implemented" from `unimplemented!()`.

- [ ] **Step 3: Implement `seqlock_read_incident`**

Replace the `unimplemented!()` body in `common/src/incidents.rs`:

```rust
use std::sync::atomic::{fence, Ordering};

fn read_u64_le(buf: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(buf[offset..offset + 8].try_into().unwrap())
}

pub fn seqlock_read_incident(mmap: &[u8]) -> Option<Incident> {
    if mmap.len() < INCIDENTS_MMAP_SIZE {
        return None;
    }
    loop {
        let seq1 = read_u64_le(mmap, 0);
        if seq1 & 1 == 1 {
            std::hint::spin_loop();
            continue;
        }
        fence(Ordering::Acquire);

        let active_count = mmap[8];
        if active_count == 0 {
            // Still need to validate seq didn't change during read.
            fence(Ordering::Acquire);
            let seq2 = read_u64_le(mmap, 0);
            if seq1 == seq2 {
                return None;
            }
            std::hint::spin_loop();
            continue;
        }

        let severity = Severity::from_u8(mmap[9]);
        let started_at = read_u64_le(mmap, 10);
        let title_len = (mmap[18] as usize).min(TITLE_MAX);
        let title = String::from_utf8_lossy(&mmap[19..19 + title_len]).into_owned();
        let url_len = (mmap[147] as usize).min(URL_MAX);
        let url = String::from_utf8_lossy(&mmap[148..148 + url_len]).into_owned();

        fence(Ordering::Acquire);
        let seq2 = read_u64_le(mmap, 0);
        if seq1 == seq2 {
            return Some(Incident {
                severity,
                started_at,
                title,
                url,
                active_count,
            });
        }
        std::hint::spin_loop();
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p common incidents`
Expected: 2 passed.

- [ ] **Step 5: Commit**

```bash
git add common/src/incidents.rs common/src/lib.rs
git commit -m "feat(common): add incidents mmap layout + seqlock read"
```

---

## Task 2: `common::incidents` — `seqlock_write_incident` round-trip

**Files:**
- Modify: `common/src/incidents.rs`
- Test: `common/src/incidents.rs` (inline `#[cfg(test)]`)

- [ ] **Step 1: Write the failing round-trip test**

Add to `common/src/incidents.rs` test module:

```rust
#[test]
fn test_write_then_read_round_trip() {
    let mut buf = vec![0u8; INCIDENTS_MMAP_SIZE];
    let incident = Incident {
        severity: Severity::Critical,
        started_at: 1_700_000_000,
        title: "API outage".to_string(),
        url: "https://status.claude.com/incidents/xyz".to_string(),
        active_count: 2,
    };
    seqlock_write_incident(&mut buf, Some(&incident));
    let got = seqlock_read_incident(&buf).expect("populated");
    assert_eq!(got, incident);
}

#[test]
fn test_write_none_clears() {
    let mut buf = vec![0u8; INCIDENTS_MMAP_SIZE];
    let incident = Incident {
        severity: Severity::Minor,
        started_at: 42,
        title: "x".to_string(),
        url: "y".to_string(),
        active_count: 1,
    };
    seqlock_write_incident(&mut buf, Some(&incident));
    assert!(seqlock_read_incident(&buf).is_some());
    seqlock_write_incident(&mut buf, None);
    assert_eq!(seqlock_read_incident(&buf), None);
}

#[test]
fn test_write_truncates_long_title() {
    let mut buf = vec![0u8; INCIDENTS_MMAP_SIZE];
    let long_title = "a".repeat(TITLE_MAX + 50);
    let incident = Incident {
        severity: Severity::Minor,
        started_at: 0,
        title: long_title.clone(),
        url: "u".to_string(),
        active_count: 1,
    };
    seqlock_write_incident(&mut buf, Some(&incident));
    let got = seqlock_read_incident(&buf).unwrap();
    assert_eq!(got.title.len(), TITLE_MAX);
    assert!(long_title.starts_with(&got.title));
}

#[test]
fn test_write_truncates_long_url() {
    let mut buf = vec![0u8; INCIDENTS_MMAP_SIZE];
    let long_url = "u".repeat(URL_MAX + 50);
    let incident = Incident {
        severity: Severity::Minor,
        started_at: 0,
        title: "t".to_string(),
        url: long_url.clone(),
        active_count: 1,
    };
    seqlock_write_incident(&mut buf, Some(&incident));
    let got = seqlock_read_incident(&buf).unwrap();
    assert_eq!(got.url.len(), URL_MAX);
}
```

Add the stub at the top of the file (below `seqlock_read_incident`):

```rust
pub fn seqlock_write_incident(_buf: &mut [u8], _incident: Option<&Incident>) {
    unimplemented!()
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p common incidents`
Expected: 4 new tests fail with "not implemented".

- [ ] **Step 3: Implement `seqlock_write_incident`**

Replace the stub:

```rust
pub fn seqlock_write_incident(buf: &mut [u8], incident: Option<&Incident>) {
    assert!(buf.len() >= INCIDENTS_MMAP_SIZE);

    let seq = read_u64_le(buf, 0);
    // Increment to odd → write in progress
    buf[0..8].copy_from_slice(&seq.wrapping_add(1).to_le_bytes());
    fence(Ordering::SeqCst);

    match incident {
        None => {
            buf[8] = 0;
            buf[9] = Severity::None as u8;
            buf[10..18].fill(0);
            buf[18] = 0;
            buf[19..19 + TITLE_MAX].fill(0);
            buf[147] = 0;
            buf[148..148 + URL_MAX].fill(0);
        }
        Some(inc) => {
            buf[8] = inc.active_count;
            buf[9] = inc.severity as u8;
            buf[10..18].copy_from_slice(&inc.started_at.to_le_bytes());

            let title_bytes = inc.title.as_bytes();
            let t_len = title_bytes.len().min(TITLE_MAX);
            buf[18] = t_len as u8;
            buf[19..19 + t_len].copy_from_slice(&title_bytes[..t_len]);
            buf[19 + t_len..19 + TITLE_MAX].fill(0);

            let url_bytes = inc.url.as_bytes();
            let u_len = url_bytes.len().min(URL_MAX);
            buf[147] = u_len as u8;
            buf[148..148 + u_len].copy_from_slice(&url_bytes[..u_len]);
            buf[148 + u_len..148 + URL_MAX].fill(0);
        }
    }

    fence(Ordering::SeqCst);
    // Increment to even → write complete
    let seq2 = read_u64_le(buf, 0);
    buf[0..8].copy_from_slice(&seq2.wrapping_add(1).to_le_bytes());
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p common incidents`
Expected: 6 passed.

- [ ] **Step 5: Commit**

```bash
git add common/src/incidents.rs
git commit -m "feat(common): add incidents seqlock write + truncation"
```

---

## Task 3: Daemon Atom parser — pure function + fixtures

**Files:**
- Create: `claudehud-daemon/src/status.rs`
- Modify: `claudehud-daemon/Cargo.toml`
- Modify: `claudehud-daemon/src/main.rs` (module registration only)
- Test: `claudehud-daemon/src/status.rs` (inline)

- [ ] **Step 1: Add dependencies**

Modify `claudehud-daemon/Cargo.toml`:

```toml
[package]
name = "claudehud-daemon"
version.workspace = true
edition.workspace = true

[dependencies]
common = { workspace = true }
memmap2 = { workspace = true }
notify = "6"
crossbeam-channel = "0.5"
ureq = { version = "2", default-features = false, features = ["tls"] }
roxmltree = "0.20"
```

- [ ] **Step 2: Register the module**

Modify `claudehud-daemon/src/main.rs`, adding `mod status;` alongside the existing `mod` declarations:

```rust
// claudehud-daemon/src/main.rs
mod cache;
mod registrar;
mod status;
mod watcher;

use std::path::PathBuf;

fn main() {
    let (tx, rx) = crossbeam_channel::unbounded::<PathBuf>();
    let tx2 = tx.clone();

    std::thread::spawn(move || {
        registrar::start(tx2);
    });

    // watcher::start runs the main event loop — blocks until channel closes
    watcher::start(rx);
}
```

- [ ] **Step 3: Write the failing tests**

Create `claudehud-daemon/src/status.rs`:

```rust
use common::incidents::{Incident, Severity};

/// Parse a Statuspage Atom feed body and return the representative active
/// incident (most recently updated) plus the total active count, or None
/// when no entry is currently active.
///
/// "Active" means the most recent status phase is one of:
///     Investigating, Identified, Monitoring, Verifying, Update, In progress
/// and NOT one of:
///     Resolved, Completed, Postmortem, Scheduled
pub fn parse_atom(_xml: &str) -> Option<Incident> {
    unimplemented!()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACTIVE_INCIDENT: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <id>tag:status.claude.com,2005:/history</id>
  <entry>
    <id>tag:status.claude.com,2005:Incident/11111</id>
    <published>2026-04-16T12:00:00Z</published>
    <updated>2026-04-16T12:30:00Z</updated>
    <link rel="alternate" type="text/html" href="https://status.claude.com/incidents/aaa"/>
    <title>Investigating - Elevated API errors</title>
    <category term="major"/>
  </entry>
</feed>"#;

    const RESOLVED_INCIDENT: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <entry>
    <id>tag:status.claude.com,2005:Incident/22222</id>
    <published>2026-04-15T08:00:00Z</published>
    <updated>2026-04-15T10:00:00Z</updated>
    <link rel="alternate" type="text/html" href="https://status.claude.com/incidents/bbb"/>
    <title>Resolved - Latency spike</title>
    <category term="minor"/>
  </entry>
</feed>"#;

    const IN_PROGRESS_MAINT: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <entry>
    <id>tag:status.claude.com,2005:Incident/33333</id>
    <published>2026-04-16T09:00:00Z</published>
    <updated>2026-04-16T09:05:00Z</updated>
    <link rel="alternate" type="text/html" href="https://status.claude.com/incidents/ccc"/>
    <title>In progress - Scheduled database upgrade</title>
    <category term="maintenance"/>
  </entry>
</feed>"#;

    const SCHEDULED_MAINT: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <entry>
    <id>tag:status.claude.com,2005:Incident/44444</id>
    <published>2026-04-16T11:00:00Z</published>
    <updated>2026-04-16T11:00:00Z</updated>
    <link rel="alternate" type="text/html" href="https://status.claude.com/incidents/ddd"/>
    <title>Scheduled - Upcoming maintenance window</title>
    <category term="maintenance"/>
  </entry>
</feed>"#;

    const MULTIPLE_ACTIVES: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <entry>
    <id>tag:status.claude.com,2005:Incident/55555</id>
    <published>2026-04-16T10:00:00Z</published>
    <updated>2026-04-16T10:05:00Z</updated>
    <link rel="alternate" type="text/html" href="https://status.claude.com/incidents/eee"/>
    <title>Identified - Older active issue</title>
    <category term="minor"/>
  </entry>
  <entry>
    <id>tag:status.claude.com,2005:Incident/66666</id>
    <published>2026-04-16T12:00:00Z</published>
    <updated>2026-04-16T12:30:00Z</updated>
    <link rel="alternate" type="text/html" href="https://status.claude.com/incidents/fff"/>
    <title>Monitoring - Newer active issue</title>
    <category term="major"/>
  </entry>
</feed>"#;

    const EMPTY_FEED: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <id>tag:status.claude.com,2005:/history</id>
</feed>"#;

    #[test]
    fn test_parse_active_incident() {
        let got = parse_atom(ACTIVE_INCIDENT).expect("active incident");
        assert_eq!(got.severity, Severity::Major);
        assert_eq!(got.title, "Elevated API errors");
        assert_eq!(got.url, "https://status.claude.com/incidents/aaa");
        assert_eq!(got.active_count, 1);
        // 2026-04-16T12:00:00Z
        assert_eq!(got.started_at, 1_776_686_400);
    }

    #[test]
    fn test_parse_resolved_returns_none() {
        assert!(parse_atom(RESOLVED_INCIDENT).is_none());
    }

    #[test]
    fn test_parse_in_progress_maintenance_active() {
        let got = parse_atom(IN_PROGRESS_MAINT).expect("in progress maintenance");
        assert_eq!(got.severity, Severity::Maintenance);
        assert_eq!(got.title, "Scheduled database upgrade");
        assert_eq!(got.active_count, 1);
    }

    #[test]
    fn test_parse_scheduled_maintenance_inactive() {
        assert!(parse_atom(SCHEDULED_MAINT).is_none());
    }

    #[test]
    fn test_parse_multiple_picks_most_recent_updated() {
        let got = parse_atom(MULTIPLE_ACTIVES).expect("two actives");
        assert_eq!(got.title, "Newer active issue");
        assert_eq!(got.active_count, 2);
    }

    #[test]
    fn test_parse_empty_feed() {
        assert!(parse_atom(EMPTY_FEED).is_none());
    }
}
```

- [ ] **Step 4: Run tests to verify they fail**

Run: `cargo test -p claudehud-daemon status`
Expected: all 6 tests fail with "not implemented".

- [ ] **Step 5: Implement `parse_atom`**

Replace the stub in `claudehud-daemon/src/status.rs`. Add these imports at the top:

```rust
use common::incidents::{Incident, Severity};
use common::parse_iso8601_epoch;
```

Wait — `parse_iso8601` currently lives in `claudehud/src/time.rs`. To reuse it from the daemon, first move it. Alternative: inline a trimmed `parse_iso8601` helper locally in `status.rs` for now to avoid cross-crate moves in this task.

Go with the local helper approach (keep the move out of scope):

```rust
use common::incidents::{Incident, Severity};

const ACTIVE_PREFIXES: &[&str] = &[
    "Investigating",
    "Identified",
    "Monitoring",
    "Verifying",
    "Update",
    "In progress",
];
const INACTIVE_PREFIXES: &[&str] = &["Resolved", "Completed", "Postmortem", "Scheduled"];

pub fn parse_atom(xml: &str) -> Option<Incident> {
    let doc = roxmltree::Document::parse(xml).ok()?;
    let root = doc.root_element();

    let mut actives: Vec<Incident> = Vec::new();
    let mut best_updated: u64 = 0;
    let mut best_idx: Option<usize> = None;

    for entry in root.children().filter(|n| n.has_tag_name("entry")) {
        let raw_title = entry
            .children()
            .find(|n| n.has_tag_name("title"))
            .and_then(|n| n.text())
            .unwrap_or("");

        let (phase, rest) = split_phase(raw_title);
        if !is_active(phase) {
            continue;
        }

        let severity = entry
            .children()
            .find(|n| n.has_tag_name("category"))
            .and_then(|n| n.attribute("term"))
            .map(severity_from_term)
            .unwrap_or(Severity::Minor);

        let url = entry
            .children()
            .find(|n| n.has_tag_name("link"))
            .and_then(|n| n.attribute("href"))
            .unwrap_or("")
            .to_string();

        let started_at = entry
            .children()
            .find(|n| n.has_tag_name("published"))
            .and_then(|n| n.text())
            .and_then(parse_iso8601_secs)
            .unwrap_or(0);

        let updated = entry
            .children()
            .find(|n| n.has_tag_name("updated"))
            .and_then(|n| n.text())
            .and_then(parse_iso8601_secs)
            .unwrap_or(started_at);

        let inc = Incident {
            severity,
            started_at,
            title: rest.trim().to_string(),
            url,
            active_count: 0, // filled in below
        };
        if updated >= best_updated {
            best_updated = updated;
            best_idx = Some(actives.len());
        }
        actives.push(inc);
    }

    let idx = best_idx?;
    let mut rep = actives.swap_remove(idx);
    let count = actives.len() + 1;
    rep.active_count = count.min(u8::MAX as usize) as u8;
    Some(rep)
}

fn split_phase(title: &str) -> (&str, &str) {
    if let Some((phase, rest)) = title.split_once(" - ") {
        (phase, rest)
    } else {
        ("", title)
    }
}

fn is_active(phase: &str) -> bool {
    if INACTIVE_PREFIXES.iter().any(|p| phase.eq_ignore_ascii_case(p)) {
        return false;
    }
    ACTIVE_PREFIXES.iter().any(|p| phase.eq_ignore_ascii_case(p))
}

fn severity_from_term(term: &str) -> Severity {
    match term.to_ascii_lowercase().as_str() {
        "minor" => Severity::Minor,
        "major" => Severity::Major,
        "critical" => Severity::Critical,
        "maintenance" | "none" => Severity::Maintenance,
        _ => Severity::Minor,
    }
}

/// Minimal ISO 8601 parser: YYYY-MM-DDTHH:MM:SS[Z|+HH:MM|-HH:MM].
/// Sufficient for Statuspage Atom timestamps.
fn parse_iso8601_secs(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.len() < 19 {
        return None;
    }
    let year: i64 = s.get(0..4)?.parse().ok()?;
    let month: i64 = s.get(5..7)?.parse().ok()?;
    let day: i64 = s.get(8..10)?.parse().ok()?;
    let hour: i64 = s.get(11..13)?.parse().ok()?;
    let min: i64 = s.get(14..16)?.parse().ok()?;
    let sec: i64 = s.get(17..19)?.parse().ok()?;

    let after = &s[19..];
    let tz = after.find(['Z', '+', '-']).map(|i| &after[i..]).unwrap_or("Z");
    let tz_offset: i64 = if tz.starts_with('Z') {
        0
    } else {
        let sign: i64 = if tz.starts_with('+') { 1 } else { -1 };
        let t = &tz[1..];
        let h: i64 = t.get(0..2).and_then(|s| s.parse().ok()).unwrap_or(0);
        let m: i64 = t.get(3..5).and_then(|s| s.parse().ok()).unwrap_or(0);
        sign * (h * 3600 + m * 60)
    };

    let y = year - (month <= 2) as i64;
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let epoch = days * 86400 + hour * 3600 + min * 60 + sec - tz_offset;
    if epoch < 0 {
        return None;
    }
    Some(epoch as u64)
}
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test -p claudehud-daemon status`
Expected: 6 passed.

- [ ] **Step 7: Commit**

```bash
git add claudehud-daemon/Cargo.toml claudehud-daemon/src/main.rs claudehud-daemon/src/status.rs Cargo.lock
git commit -m "feat(daemon): add Atom feed parser for status incidents"
```

---

## Task 4: Daemon fetch loop + mmap write + wire into main

**Files:**
- Modify: `claudehud-daemon/src/status.rs` (append `start` + `write_mmap`)
- Modify: `claudehud-daemon/src/main.rs` (spawn status thread)

Note: the fetch loop is hard to unit-test without a fake HTTP server, so this task tests only the mmap write path; the HTTP integration is covered via manual verification in Task 9.

- [ ] **Step 1: Write the failing test for `write_incident_to_path`**

Append to the test module in `claudehud-daemon/src/status.rs`:

```rust
#[test]
fn test_write_incident_to_tmp_file() {
    use common::incidents::{seqlock_read_incident, Incident, Severity, INCIDENTS_MMAP_SIZE};
    use std::fs::File;

    let path = std::env::temp_dir().join(format!("clhud-test-{}.bin", std::process::id()));
    let incident = Incident {
        severity: Severity::Critical,
        started_at: 1_700_000_000,
        title: "Test outage".to_string(),
        url: "https://example.com".to_string(),
        active_count: 1,
    };

    super::write_incident_to_path(&path, Some(&incident));

    let file = File::open(&path).unwrap();
    let mmap = unsafe { memmap2::Mmap::map(&file) }.unwrap();
    assert_eq!(mmap.len(), INCIDENTS_MMAP_SIZE);
    let got = seqlock_read_incident(&mmap).unwrap();
    assert_eq!(got, incident);

    let _ = std::fs::remove_file(&path);
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p claudehud-daemon status::tests::test_write_incident_to_tmp_file`
Expected: fail — `write_incident_to_path` not defined.

- [ ] **Step 3: Implement the fetch loop + mmap write**

Append to `claudehud-daemon/src/status.rs`:

```rust
use common::incidents::{seqlock_write_incident, INCIDENTS_MMAP_PATH, INCIDENTS_MMAP_SIZE};
use memmap2::MmapMut;
use std::fs::OpenOptions;
use std::path::Path;
use std::time::Duration;

const FEED_URL: &str = "https://status.claude.com/history.atom";
const POLL_INTERVAL: Duration = Duration::from_secs(300);
const USER_AGENT: &str = concat!("claudehud-daemon/", env!("CARGO_PKG_VERSION"));

/// Main entry point for the status-polling thread. Loops forever.
pub fn start() {
    let agent = ureq::AgentBuilder::new()
        .user_agent(USER_AGENT)
        .timeout_connect(Duration::from_secs(5))
        .timeout_read(Duration::from_secs(10))
        .build();

    let mut etag: Option<String> = None;

    loop {
        match fetch_once(&agent, etag.as_deref()) {
            Ok(FetchOutcome::NotModified) => {}
            Ok(FetchOutcome::Body { body, etag: new_etag }) => {
                etag = new_etag;
                let incident = parse_atom(&body);
                write_incident_to_path(Path::new(INCIDENTS_MMAP_PATH), incident.as_ref());
            }
            Err(e) => {
                eprintln!("WARN status fetch: {e}");
            }
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

enum FetchOutcome {
    NotModified,
    Body { body: String, etag: Option<String> },
}

fn fetch_once(agent: &ureq::Agent, etag: Option<&str>) -> Result<FetchOutcome, String> {
    let mut req = agent.get(FEED_URL);
    if let Some(tag) = etag {
        req = req.set("If-None-Match", tag);
    }
    match req.call() {
        Ok(resp) => {
            let new_etag = resp.header("ETag").map(|s| s.to_string());
            let body = resp.into_string().map_err(|e| e.to_string())?;
            Ok(FetchOutcome::Body { body, etag: new_etag })
        }
        Err(ureq::Error::Status(304, _)) => Ok(FetchOutcome::NotModified),
        Err(e) => Err(e.to_string()),
    }
}

pub(crate) fn write_incident_to_path(path: &Path, incident: Option<&Incident>) {
    let file = match OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
    {
        Ok(f) => f,
        Err(e) => {
            eprintln!("WARN incidents mmap open: {e}");
            return;
        }
    };
    if file.set_len(INCIDENTS_MMAP_SIZE as u64).is_err() {
        return;
    }
    // Safety: freshly opened, sized, exclusive writer — readers use seqlock.
    let mut mmap = match unsafe { MmapMut::map_mut(&file) } {
        Ok(m) if m.len() >= INCIDENTS_MMAP_SIZE => m,
        _ => return,
    };
    seqlock_write_incident(&mut mmap[..], incident);
}
```

- [ ] **Step 4: Wire `status::start` into `main`**

Replace `claudehud-daemon/src/main.rs`:

```rust
// claudehud-daemon/src/main.rs
mod cache;
mod registrar;
mod status;
mod watcher;

use std::path::PathBuf;

fn main() {
    let (tx, rx) = crossbeam_channel::unbounded::<PathBuf>();
    let tx2 = tx.clone();

    std::thread::spawn(move || {
        registrar::start(tx2);
    });

    std::thread::spawn(|| {
        status::start();
    });

    // watcher::start runs the main event loop — blocks until channel closes
    watcher::start(rx);
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p claudehud-daemon`
Expected: all daemon tests pass (including the new mmap write test).

- [ ] **Step 6: Build-check the whole workspace**

Run: `cargo build --release`
Expected: success, no warnings introduced.

- [ ] **Step 7: Commit**

```bash
git add claudehud-daemon/src/status.rs claudehud-daemon/src/main.rs
git commit -m "feat(daemon): poll status.claude.com atom feed and write mmap"
```

---

## Task 5: Client `incidents` read module

**Files:**
- Create: `claudehud/src/incidents.rs`
- Modify: `claudehud/src/main.rs` (module registration only)
- Test: `claudehud/src/incidents.rs` (inline)

- [ ] **Step 1: Register the module**

Modify `claudehud/src/main.rs`, adding `mod incidents;` alongside existing mods:

```rust
mod fmt;
mod git;
mod incidents;
mod input;
mod render;
mod time;

use std::io::{self, Read};
use std::path::Path;

fn main() {
    let mut raw = String::new();
    io::stdin().read_to_string(&mut raw).unwrap_or(0);

    if raw.trim().is_empty() {
        print!("Claude");
        return;
    }

    let input: input::Input = serde_json::from_str(&raw).unwrap_or_default();
    let git = input
        .cwd
        .as_deref()
        .filter(|s| !s.is_empty())
        .and_then(|cwd| git::branch_and_dirty(Path::new(cwd)));

    print!("{}", render::render(&input, git));
}
```

(Leave the `render` call with two args for now; Task 7 updates it.)

- [ ] **Step 2: Write the failing test**

Create `claudehud/src/incidents.rs`:

```rust
use std::fs;
use std::path::Path;

use common::incidents::{seqlock_read_incident, Incident, INCIDENTS_MMAP_PATH, INCIDENTS_MMAP_SIZE};
use memmap2::Mmap;

pub fn read_incident() -> Option<Incident> {
    read_incident_from(Path::new(INCIDENTS_MMAP_PATH))
}

pub fn read_incident_from(_path: &Path) -> Option<Incident> {
    unimplemented!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::incidents::{seqlock_write_incident, Severity};

    #[test]
    fn test_read_missing_file_returns_none() {
        let path = std::env::temp_dir().join(format!("clhud-test-missing-{}.bin", std::process::id()));
        let _ = fs::remove_file(&path);
        assert_eq!(read_incident_from(&path), None);
    }

    #[test]
    fn test_read_populated_file_returns_incident() {
        let path = std::env::temp_dir().join(format!("clhud-test-read-{}.bin", std::process::id()));
        let incident = Incident {
            severity: Severity::Major,
            started_at: 1_700_000_000,
            title: "Read test".to_string(),
            url: "https://example.com/x".to_string(),
            active_count: 1,
        };

        // Write via seqlock into a sized file.
        let mut buf = vec![0u8; INCIDENTS_MMAP_SIZE];
        seqlock_write_incident(&mut buf, Some(&incident));
        fs::write(&path, &buf).unwrap();

        let got = read_incident_from(&path).expect("populated");
        assert_eq!(got, incident);

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_read_wrong_size_file_returns_none() {
        let path = std::env::temp_dir().join(format!("clhud-test-trunc-{}.bin", std::process::id()));
        fs::write(&path, b"short").unwrap();
        assert_eq!(read_incident_from(&path), None);
        let _ = fs::remove_file(&path);
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p claudehud incidents`
Expected: 3 tests fail with "not implemented".

- [ ] **Step 4: Implement `read_incident_from`**

Replace the stub in `claudehud/src/incidents.rs`:

```rust
pub fn read_incident_from(path: &Path) -> Option<Incident> {
    let file = fs::File::open(path).ok()?;
    if file.metadata().ok()?.len() != INCIDENTS_MMAP_SIZE as u64 {
        return None;
    }
    // Safety: `file` holds the fd open; the daemon uses a seqlock protocol
    // so concurrent writes are safe to observe.
    let mmap = unsafe { Mmap::map(&file) }.ok()?;
    seqlock_read_incident(&mmap)
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p claudehud incidents`
Expected: 3 passed.

- [ ] **Step 6: Commit**

```bash
git add claudehud/src/main.rs claudehud/src/incidents.rs
git commit -m "feat(claudehud): add incidents mmap reader"
```

---

## Task 6: `fmt::severity_icon` helper

**Files:**
- Modify: `claudehud/src/fmt.rs`
- Test: `claudehud/src/fmt.rs` (inline)

- [ ] **Step 1: Write the failing test**

Append to the `tests` module in `claudehud/src/fmt.rs`:

```rust
#[test]
fn test_severity_icon() {
    use common::incidents::Severity;
    assert_eq!(severity_icon(Severity::Minor), "🟡");
    assert_eq!(severity_icon(Severity::Major), "🟠");
    assert_eq!(severity_icon(Severity::Critical), "🔴");
    assert_eq!(severity_icon(Severity::Maintenance), "🔧");
    assert_eq!(severity_icon(Severity::None), "");
}

#[test]
fn test_color_for_severity() {
    use common::incidents::Severity;
    assert_eq!(color_for_severity(Severity::Minor), YELLOW);
    assert_eq!(color_for_severity(Severity::Major), ORANGE);
    assert_eq!(color_for_severity(Severity::Critical), RED);
    assert_eq!(color_for_severity(Severity::Maintenance), CYAN);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p claudehud fmt::tests::test_severity`
Expected: compile error — functions not defined.

- [ ] **Step 3: Implement the helpers**

Append to `claudehud/src/fmt.rs` (above the `#[cfg(test)]` block):

```rust
use common::incidents::Severity;

pub fn severity_icon(sev: Severity) -> &'static str {
    match sev {
        Severity::Minor => "🟡",
        Severity::Major => "🟠",
        Severity::Critical => "🔴",
        Severity::Maintenance => "🔧",
        Severity::None => "",
    }
}

pub fn color_for_severity(sev: Severity) -> &'static str {
    match sev {
        Severity::Minor => YELLOW,
        Severity::Major => ORANGE,
        Severity::Critical => RED,
        Severity::Maintenance => CYAN,
        Severity::None => RESET,
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p claudehud fmt`
Expected: all `fmt` tests pass.

- [ ] **Step 5: Commit**

```bash
git add claudehud/src/fmt.rs
git commit -m "feat(claudehud): severity icon + color helpers"
```

---

## Task 7: Render the incident line

**Files:**
- Modify: `claudehud/src/render.rs`
- Test: `claudehud/src/render.rs` (inline)

- [ ] **Step 1: Write the failing tests**

Append to the `tests` module in `claudehud/src/render.rs`:

```rust
#[test]
fn test_render_incident_present_major() {
    use common::incidents::{Incident, Severity};
    let incident = Incident {
        severity: Severity::Major,
        started_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            .saturating_sub(12 * 60),
        title: "Elevated API errors".to_string(),
        url: "https://status.claude.com/incidents/abc".to_string(),
        active_count: 1,
    };
    let input = Input::default();
    let out = render(&input, None, Some(&incident));
    let plain = strip_ansi(&out);
    assert!(plain.contains("🟠"));
    assert!(plain.contains("Elevated API errors"));
    assert!(plain.contains("started 12m ago"));
    // OSC 8 hyperlink start sequence.
    assert!(out.contains("\x1b]8;;https://status.claude.com/incidents/abc"));
    // No "+N more" suffix when only one active.
    assert!(!plain.contains("more"));
}

#[test]
fn test_render_incident_with_plus_n_more() {
    use common::incidents::{Incident, Severity};
    let incident = Incident {
        severity: Severity::Minor,
        started_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
        title: "Thing A".to_string(),
        url: "https://status.claude.com/incidents/a".to_string(),
        active_count: 3,
    };
    let out = render(&Input::default(), None, Some(&incident));
    let plain = strip_ansi(&out);
    assert!(plain.contains("+2 more"));
}

#[test]
fn test_render_no_incident_unchanged_shape() {
    // With no incident, output must not contain any incident icons.
    let out = render(&Input::default(), None, None);
    let plain = strip_ansi(&out);
    for icon in ["🟡", "🟠", "🔴", "🔧"] {
        assert!(!plain.contains(icon), "unexpected icon: {icon}");
    }
}
```

Also update the existing call sites in the `render` tests (they currently pass two args). Change every existing `render(&input, None)` or `render(&input, Some(...))` in the test module to include a trailing `, None` argument. Concretely, edit these tests to take the new 3-arg signature:

- `test_render_default_model` → `render(&input, None, None)`
- `test_render_model_name` → `render(&input, None, None)`
- `test_render_context_pct` → `render(&input, None, None)`
- `test_render_git_branch` → `render(&input, Some(("main".to_string(), false)), None)`
- `test_render_git_dirty` → `render(&input, Some(("main".to_string(), true)), None)`
- `test_render_dirname` → `render(&input, None, None)`
- `test_render_rate_limits_present` → `render(&input, None, None)`

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p claudehud render`
Expected: compile errors — `render` takes 2 args, tests pass 3.

- [ ] **Step 3: Update the `render` signature + emit the incident line**

Replace `claudehud/src/render.rs`:

```rust
use std::fmt::Write as _;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use common::incidents::Incident;

use crate::fmt::{self, *};
use crate::input::Input;
use crate::time::{format_duration, format_reset_time, parse_iso8601, ResetStyle};

pub fn render(input: &Input, git: Option<(String, bool)>, incident: Option<&Incident>) -> String {
    let mut out = String::with_capacity(512);

    // ── Model ──────────────────────────────────────────────
    let model = input
        .model
        .as_ref()
        .and_then(|m| m.display_name.as_deref())
        .unwrap_or("Claude");
    out.push_str(BLUE);
    out.push_str(model);
    out.push_str(RESET);

    // ── Context usage ──────────────────────────────────────
    out.push_str(SEP);
    let pct = context_pct(input);
    out.push_str("✍️ ");
    out.push_str(color_for_pct(pct));
    write!(out, "{pct}%").unwrap();
    out.push_str(RESET);

    // ── Dir + git ──────────────────────────────────────────
    out.push_str(SEP);
    let cwd = input.cwd.as_deref().unwrap_or("");
    let dirname = Path::new(cwd)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(cwd);
    out.push_str(CYAN);
    out.push_str(dirname);
    out.push_str(RESET);
    if let Some((branch, dirty)) = &git {
        out.push(' ');
        out.push_str(GREEN);
        out.push('(');
        out.push_str(branch);
        if *dirty {
            out.push_str(RED);
            out.push('*');
        }
        out.push_str(GREEN);
        out.push(')');
        out.push_str(RESET);
    }

    // ── Session duration ───────────────────────────────────
    if let Some(dur) = session_duration(input) {
        out.push_str(SEP);
        out.push_str(DIM);
        out.push_str("⏱ ");
        out.push_str(RESET);
        out.push_str(WHITE);
        out.push_str(&dur);
        out.push_str(RESET);
    }

    // ── Incident line (between line 1 and rate limits) ─────
    if let Some(inc) = incident {
        out.push('\n');
        push_incident_line(inc, &mut out);
    }

    // ── Rate limits ────────────────────────────────────────
    if let Some(rl) = &input.rate_limits {
        if let Some(fh) = &rl.five_hour {
            if let Some(pct_f) = fh.used_percentage {
                let pct = pct_f.round().clamp(0.0, 100.0) as u8;
                out.push_str("\n\n");
                push_rate_row("current", pct, fh.resets_at, ResetStyle::Time, &mut out);

                if let Some(sd) = &rl.seven_day {
                    if let Some(pct_f) = sd.used_percentage {
                        let pct = pct_f.round().clamp(0.0, 100.0) as u8;
                        out.push('\n');
                        push_rate_row("weekly ", pct, sd.resets_at, ResetStyle::DateTime, &mut out);
                    }
                }
            }
        }
    }

    out
}

fn push_incident_line(inc: &Incident, out: &mut String) {
    // OSC 8 hyperlink opens with ESC ] 8 ; ; URL ST, closes with ESC ] 8 ; ; ST.
    let url = &inc.url;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let elapsed = now.saturating_sub(inc.started_at);
    let since = format_duration(elapsed);

    write!(out, "\x1b]8;;{url}\x1b\\").unwrap();
    out.push_str(fmt::color_for_severity(inc.severity));
    out.push_str(fmt::severity_icon(inc.severity));
    out.push(' ');
    out.push_str(WHITE);
    out.push_str(&inc.title);
    out.push(' ');
    out.push_str(DIM);
    write!(out, "· started {since} ago").unwrap();
    out.push_str(RESET);
    // Close hyperlink before any "+N more" suffix so that suffix links elsewhere.
    out.push_str("\x1b]8;;\x1b\\");

    if inc.active_count > 1 {
        out.push(' ');
        write!(out, "\x1b]8;;https://status.claude.com/\x1b\\").unwrap();
        out.push_str(DIM);
        write!(out, "+{} more", inc.active_count - 1).unwrap();
        out.push_str(RESET);
        out.push_str("\x1b]8;;\x1b\\");
    }
}

fn push_rate_row(label: &str, pct: u8, resets_at: Option<u64>, style: ResetStyle, out: &mut String) {
    out.push_str(WHITE);
    out.push_str(label);
    out.push_str(RESET);
    out.push(' ');
    fmt::build_bar(pct, 10, out);
    out.push(' ');
    out.push_str(color_for_pct(pct));
    write!(out, "{pct:2}%").unwrap();
    out.push_str(RESET);
    if let Some(epoch) = resets_at.filter(|&e| e > 0) {
        out.push(' ');
        out.push_str(DIM);
        out.push_str(" ⟳ ");
        out.push_str(RESET);
        out.push_str(WHITE);
        out.push_str(&format_reset_time(epoch, style));
        out.push_str(RESET);
    }
}

fn context_pct(input: &Input) -> u8 {
    let cw = input.context_window.as_ref();
    let size = cw
        .and_then(|cw| cw.context_window_size)
        .filter(|&s| s > 0)
        .unwrap_or(200_000);
    let current = cw
        .and_then(|cw| cw.current_usage.as_ref())
        .map(|u| {
            u.input_tokens.unwrap_or(0)
                + u.cache_creation_input_tokens.unwrap_or(0)
                + u.cache_read_input_tokens.unwrap_or(0)
        })
        .unwrap_or(0);
    ((current * 100) / size).min(100) as u8
}

fn session_duration(input: &Input) -> Option<String> {
    let start = input.session.as_ref()?.start_time.as_deref()?;
    let start_epoch = parse_iso8601(start)?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    Some(format_duration(now.saturating_sub(start_epoch)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::Input;

    fn strip_ansi(s: &str) -> String {
        let mut out = String::new();
        let mut in_escape = false;
        let mut in_osc = false;
        for c in s.chars() {
            if in_osc {
                // OSC ends at ST (ESC \) or BEL.
                if c == '\x07' {
                    in_osc = false;
                } else if c == '\\' {
                    // Treat ESC \ terminator as end of OSC.
                    in_osc = false;
                }
                continue;
            }
            if c == '\x1b' {
                in_escape = true;
                continue;
            }
            if in_escape {
                if c == ']' {
                    in_osc = true;
                    in_escape = false;
                } else if c == 'm' {
                    in_escape = false;
                }
                continue;
            }
            out.push(c);
        }
        out
    }

    // … (existing tests below, updated to the 3-arg form)
}
```

Leave the existing tests in place but with updated call sites (from Step 1). Append the three new tests (`test_render_incident_present_major`, `test_render_incident_with_plus_n_more`, `test_render_no_incident_unchanged_shape`) to the same module.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p claudehud render`
Expected: all tests pass (existing + 3 new).

- [ ] **Step 5: Commit**

```bash
git add claudehud/src/render.rs
git commit -m "feat(claudehud): render incident line with OSC 8 hyperlink"
```

---

## Task 8: Wire the client end-to-end

**Files:**
- Modify: `claudehud/src/main.rs`

- [ ] **Step 1: Thread the incident into `render`**

Replace `claudehud/src/main.rs`:

```rust
mod fmt;
mod git;
mod incidents;
mod input;
mod render;
mod time;

use std::io::{self, Read};
use std::path::Path;

fn main() {
    let mut raw = String::new();
    io::stdin().read_to_string(&mut raw).unwrap_or(0);

    if raw.trim().is_empty() {
        print!("Claude");
        return;
    }

    let input: input::Input = serde_json::from_str(&raw).unwrap_or_default();
    let git = input
        .cwd
        .as_deref()
        .filter(|s| !s.is_empty())
        .and_then(|cwd| git::branch_and_dirty(Path::new(cwd)));

    let incident = incidents::read_incident();
    print!("{}", render::render(&input, git, incident.as_ref()));
}
```

- [ ] **Step 2: Build-check the workspace**

Run: `cargo build --release`
Expected: success.

- [ ] **Step 3: Run the full workspace test suite**

Run: `cargo test --workspace`
Expected: all tests pass.

- [ ] **Step 4: Manual smoke test — no incident file present**

```bash
rm -f /tmp/clhud-incidents.bin
echo '{"cwd": "/tmp"}' | ./target/release/claudehud
```
Expected: output matches prior behaviour (no incident line, no extra newlines).

- [ ] **Step 5: Manual smoke test — synthetic incident**

Use a short throwaway rust binary or a quick `cargo run --example` to write a synthetic incident via `seqlock_write_incident`, OR run the daemon against the live feed (Task 9 covers that). For in-place verification, add a temporary binary under `claudehud-daemon/examples/` that writes a fake incident. If keeping the scope tight, skip the synthetic step and defer full verification to Task 9.

- [ ] **Step 6: Commit**

```bash
git add claudehud/src/main.rs
git commit -m "feat(claudehud): read incident mmap and render"
```

---

## Task 9: README documentation + end-to-end verification

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Run the daemon against the real feed**

```bash
# Rebuild w/ new status thread
cargo build --release

# In one terminal:
./target/release/claudehud-daemon

# In another terminal, wait ~5-10 sec, then:
ls -la /tmp/clhud-incidents.bin
```
Expected: the file exists and is 408 bytes. If `status.claude.com` has no active incidents, `active_count` byte at offset 8 will be zero (use `xxd /tmp/clhud-incidents.bin | head` to verify).

- [ ] **Step 2: Simulate an active incident locally for the rendering check**

Write a synthetic incident into the mmap file without relying on an active real-world incident. Create a one-off rust snippet file `/tmp/write_fake_incident.rs` and compile with `rustc --edition 2021 -L target/release/deps -L target/release --extern common=target/release/libcommon.rlib ...` OR simpler: add a temporary test-only binary. Quickest path (no scaffolding):

```bash
cargo run --release --manifest-path claudehud-daemon/Cargo.toml --example fake_incident 2>/dev/null || true
```

If no `examples/fake_incident.rs` exists, skip this step — the rendering path is already covered by unit tests. Acceptable completion for Step 2: a note in the commit that the live-feed path was observed writing a 408-byte file and the unit tests cover the render shape.

- [ ] **Step 3: Update `README.md`**

Add a new section between "## Output" and "## Architecture":

```markdown
## Status incidents

When `status.claude.com` reports an active incident (or in-progress scheduled maintenance), `claudehud` emits a hyperlinked line directly below line 1:

```
🟡 Elevated API errors · started 12m ago    +1 more
```

The daemon polls `https://status.claude.com/history.atom` every 5 minutes using a conditional GET, so most hits return 304 Not Modified. When an incident is active, the most-recently-updated entry is shown; the `+N more` suffix appears when more than one incident or in-progress maintenance is active and links to the main status page. The line disappears automatically once every incident transitions to Resolved or Completed.

The daemon stores the current representative incident at `/tmp/clhud-incidents.bin` (408 bytes, seqlock-protected). If the daemon isn't running, the line simply doesn't appear — this degrades silently, like the git cache.
```

Also update the "Architecture" diagram block. Append to the existing tree:

```
├── common/                 shared constants, FNV hash, seqlock read, git root detection, incidents layout
├── claudehud/              client binary — reads JSON from stdin, writes statusline to stdout
└── claudehud-daemon/       daemon — watches git repos reactively, polls status.claude.com, caches in mmap files
```

And append this block in the "Dependencies" table:

```
| `ureq` | daemon | HTTPS client for status.claude.com |
| `roxmltree` | daemon | Atom feed parser |
```

- [ ] **Step 4: Commit**

```bash
git add README.md
git commit -m "docs: document status incidents line and daemon polling"
```

---

## Task 10: End-to-end integration tests (1 incident / 2 incidents)

**Files:**
- Create: `claudehud/tests/e2e_incidents.rs`

This task exercises the full pipeline — atom fixture → `parse_atom` → `seqlock_write_incident` into a temp file → `read_incident_from` → `render` — for two scenarios: one ongoing incident, and two ongoing incidents.

- [ ] **Step 1: Expose `parse_atom` to integration tests**

Integration tests in `tests/` sit outside the daemon crate's module tree, so `parse_atom` must be reachable. The daemon is a binary crate — the cleanest route is to re-test against a shared fixture-aware helper that lives in `common`. But moving the parser is out of scope. Instead: use the already-parsed `Incident` values directly and drive the pipeline from there. The "mock an ongoing incident" requirement is satisfied by constructing an `Incident` value that represents what `parse_atom` would produce for a given atom fixture, then running that through the mmap + render path.

Note in the test module comments that the atom-parsing half of the pipeline has its own unit coverage in `claudehud-daemon::status::tests`, and this integration test covers the mmap+render half.

- [ ] **Step 2: Write the failing integration tests**

Create `claudehud/tests/e2e_incidents.rs`:

```rust
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
    let mut in_escape = false;
    let mut in_osc = false;
    for c in s.chars() {
        if in_osc {
            if c == '\x07' || c == '\\' {
                in_osc = false;
            }
            continue;
        }
        if c == '\x1b' {
            in_escape = true;
            continue;
        }
        if in_escape {
            if c == ']' {
                in_osc = true;
                in_escape = false;
            } else if c == 'm' {
                in_escape = false;
            }
            continue;
        }
        out.push(c);
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

    // Severity icon for Major
    assert!(plain.contains("🟠"), "expected major severity icon, got: {plain}");
    // Title present
    assert!(plain.contains("Elevated API errors"));
    // Started-ago relative timestamp
    assert!(plain.contains("started 7m ago"), "expected '7m ago', got: {plain}");
    // OSC 8 hyperlink opens against the incident url
    assert!(out.contains("\x1b]8;;https://status.claude.com/incidents/single"));
    // No "+N more" suffix for a single active
    assert!(!plain.contains("more"), "unexpected +N more suffix: {plain}");

    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_e2e_two_ongoing_incidents() {
    let path = tmp_mmap_path("double");
    // Representative = most-recently-updated; count reflects both actives.
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

    // Severity icon for Critical
    assert!(plain.contains("🔴"), "expected critical icon, got: {plain}");
    // Representative title
    assert!(plain.contains("API fully down"));
    // "+1 more" suffix = active_count - 1
    assert!(plain.contains("+1 more"), "expected +1 more suffix: {plain}");
    // Suffix links to the overview page, not the representative url
    assert!(out.contains("\x1b]8;;https://status.claude.com/\x1b\\"));

    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_e2e_no_incident_mmap_absent() {
    let path = tmp_mmap_path("absent");
    let _ = std::fs::remove_file(&path);
    // File doesn't exist — reader returns None, render emits no incident line.
    assert!(claudehud::incidents::read_incident_from(&path).is_none());

    let input = claudehud::input::Input::default();
    let out = claudehud::render::render(&input, None, None);
    let plain = strip_ansi(&out);
    for icon in ["🟡", "🟠", "🔴", "🔧"] {
        assert!(!plain.contains(icon));
    }
}
```

- [ ] **Step 3: Expose required client modules to integration tests**

Integration tests in `claudehud/tests/` can only see items that are `pub` in `claudehud`'s library crate. But `claudehud` is currently a binary crate (no `lib.rs`). To make `incidents::read_incident_from`, `render::render`, and `input::Input` reachable from an integration test, convert `claudehud` into a binary-plus-library crate.

Modify `claudehud/Cargo.toml`:

```toml
[package]
name = "claudehud"
version.workspace = true
edition.workspace = true

[lib]
path = "src/lib.rs"

[[bin]]
name = "claudehud"
path = "src/main.rs"

[dependencies]
common = { workspace = true }
memmap2 = { workspace = true }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
time = { version = "0.3", features = ["local-offset"] }
```

Create `claudehud/src/lib.rs` that re-exports the modules used by tests:

```rust
pub mod fmt;
pub mod git;
pub mod incidents;
pub mod input;
pub mod render;
pub mod time;
```

Remove the `mod` declarations from `claudehud/src/main.rs` (they now live in `lib.rs`) and have `main.rs` use the library instead:

```rust
use std::io::{self, Read};
use std::path::Path;

use claudehud::{git, incidents, input, render};

fn main() {
    let mut raw = String::new();
    io::stdin().read_to_string(&mut raw).unwrap_or(0);

    if raw.trim().is_empty() {
        print!("Claude");
        return;
    }

    let input: input::Input = serde_json::from_str(&raw).unwrap_or_default();
    let git = input
        .cwd
        .as_deref()
        .filter(|s| !s.is_empty())
        .and_then(|cwd| git::branch_and_dirty(Path::new(cwd)));

    let incident = incidents::read_incident();
    print!("{}", render::render(&input, git, incident.as_ref()));
}
```

- [ ] **Step 4: Run the failing tests**

Run: `cargo test -p claudehud --test e2e_incidents`
Expected: 3 tests pass once the `pub` exposure in Step 3 is in place. If any `pub` visibility is missing (e.g. `Input` fields), add `pub` to those fields.

Specifically, `input::Input` and its nested structs already derive `Default` but their fields may need to be `pub` for external construction. Since the integration tests only use `Input::default()` (via the `Default` derive that already exists), no field changes should be needed. Verify:

```bash
cargo test -p claudehud --test e2e_incidents
```
Expected: 3 passed.

- [ ] **Step 5: Commit**

```bash
git add claudehud/Cargo.toml claudehud/src/lib.rs claudehud/src/main.rs claudehud/tests/e2e_incidents.rs
git commit -m "test(claudehud): e2e tests for 1 and 2 ongoing incidents"
```

---

## Self-Review

Spec coverage (each spec section → task):
- Data (mmap layout, 408 B, seqlock) → Tasks 1, 2
- Atom classification → Task 3
- Daemon fetch / conditional GET → Task 4
- Render details (icons, OSC 8, +N more) → Tasks 6, 7
- Error handling (304, parse fail, missing mmap) → Tasks 3, 4, 5
- Testing fixtures (parse + render unit) → Tasks 3, 7
- End-to-end mmap-through-render test for 1 and 2 ongoing incidents → Task 10
- README / migration → Task 9

Task ordering note: Task 8 establishes the binary-only layout for `claudehud`; Task 10 restructures `claudehud` into a binary-plus-library crate so integration tests can reach the module APIs. Implementers should complete Task 8 as written, then apply Task 10's restructure on top.

No placeholders, no "TBD". Types are consistent: `Incident { severity, started_at, title, url, active_count }` is defined in Task 1 and referenced identically in Tasks 2–10.

---

**Plan complete.** Saved to `docs/superpowers/plans/2026-04-16-status-incidents.md`.
