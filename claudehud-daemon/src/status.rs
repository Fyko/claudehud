use common::incidents::{
    seqlock_write_incidents, INCIDENTS_MMAP_PATH, INCIDENTS_MMAP_SIZE, MAX_STORED_INCIDENTS,
};
use common::incidents::{Incident, Severity};
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
            Ok(FetchOutcome::Body {
                body,
                etag: new_etag,
            }) => {
                etag = new_etag;
                match parse_atom_result(&body) {
                    Ok((incidents, total)) => {
                        write_incidents_to_path(Path::new(INCIDENTS_MMAP_PATH), &incidents, total);
                    }
                    Err(e) => eprintln!("WARN status parse: {e}"),
                }
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
            Ok(FetchOutcome::Body {
                body,
                etag: new_etag,
            })
        }
        Err(ureq::Error::Status(304, _)) => Ok(FetchOutcome::NotModified),
        Err(e) => Err(e.to_string()),
    }
}

pub(crate) fn write_incidents_to_path(path: &Path, incidents: &[Incident], total: u8) {
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
    seqlock_write_incidents(&mut mmap[..], incidents, total);
}

const ACTIVE_PHASES: &[&str] = &[
    "investigating",
    "identified",
    "monitoring",
    "verifying",
    "update",
    "in progress",
];

const INACTIVE_PHASES: &[&str] = &["resolved", "completed", "postmortem", "scheduled"];

fn severity_from_term(term: &str) -> Severity {
    match term.trim().to_ascii_lowercase().as_str() {
        "minor" => Severity::Minor,
        "major" => Severity::Major,
        "critical" => Severity::Critical,
        "maintenance" => Severity::Maintenance,
        _ => Severity::Minor,
    }
}

#[cfg(test)]
pub fn parse_atom(xml: &str) -> (Vec<Incident>, u8) {
    parse_atom_result(xml).unwrap_or_default()
}

/// Returns (active_incidents sorted by updated_at desc, total_count).
/// Distinguishes XML parse failure (retain prior mmap state) from "no active
/// incidents" (Ok with empty vec), which clears the mmap.
fn extract_phase_from_html(html: &str) -> Option<&str> {
    let start = html.find("<strong>")?;
    let after = &html[start + "<strong>".len()..];
    let end = after.find("</strong>")?;
    Some(&after[..end])
}

fn parse_atom_result(xml: &str) -> Result<(Vec<Incident>, u8), roxmltree::Error> {
    let doc = roxmltree::Document::parse(xml)?;
    let root = doc.root_element();

    // Collect all active entries as (updated_at, Incident)
    let mut active: Vec<(u64, Incident)> = Vec::new();

    for entry in root.children().filter(|n| n.has_tag_name("entry")) {
        let title_node = entry.children().find(|n| n.has_tag_name("title"));
        let raw_title = title_node.and_then(|n| n.text()).unwrap_or("").trim();

        let (phase, subject) = match raw_title.split_once(" - ") {
            Some((p, s)) => (p.trim(), s.trim()),
            None => {
                let content_html = entry
                    .children()
                    .find(|n| n.has_tag_name("content"))
                    .and_then(|n| n.text())
                    .unwrap_or("");
                let phase = extract_phase_from_html(content_html).unwrap_or("");
                (phase, raw_title)
            }
        };

        let phase_lc = phase.to_ascii_lowercase();

        if INACTIVE_PHASES.iter().any(|p| *p == phase_lc) {
            continue;
        }
        if !ACTIVE_PHASES.iter().any(|p| *p == phase_lc) {
            continue;
        }

        let Some(started_at) = entry
            .children()
            .find(|n| n.has_tag_name("published"))
            .and_then(|n| n.text())
            .and_then(parse_iso8601_secs)
        else {
            continue;
        };

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

        let updated_at = entry
            .children()
            .find(|n| n.has_tag_name("updated"))
            .and_then(|n| n.text())
            .and_then(parse_iso8601_secs)
            .unwrap_or(started_at);

        active.push((
            updated_at,
            Incident {
                severity,
                started_at,
                title: subject.to_string(),
                url,
            },
        ));
    }

    let total = active.len().min(u8::MAX as usize) as u8;
    // Sort by updated_at descending so the most recently updated shows first
    active.sort_by_key(|x| std::cmp::Reverse(x.0));
    let incidents: Vec<Incident> = active
        .into_iter()
        .take(MAX_STORED_INCIDENTS)
        .map(|(_, inc)| inc)
        .collect();

    Ok((incidents, total))
}

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
    let tz = after
        .find(['Z', '+', '-'])
        .map(|i| &after[i..])
        .unwrap_or("Z");
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

    const ACTIVE_WITH_BAD_PUBLISHED: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <entry>
    <id>tag:status.claude.com,2005:Incident/99998</id>
    <published>not-a-date</published>
    <updated>2026-04-16T09:00:00Z</updated>
    <link rel="alternate" type="text/html" href="https://status.claude.com/incidents/bad"/>
    <title>Investigating - Broken timestamp</title>
    <category term="minor"/>
  </entry>
  <entry>
    <id>tag:status.claude.com,2005:Incident/99999</id>
    <published>2026-04-16T10:00:00Z</published>
    <updated>2026-04-16T10:05:00Z</updated>
    <link rel="alternate" type="text/html" href="https://status.claude.com/incidents/good"/>
    <title>Investigating - Valid entry</title>
    <category term="minor"/>
  </entry>
</feed>"#;

    #[test]
    fn test_parse_active_incident() {
        let (incidents, total) = parse_atom(ACTIVE_INCIDENT);
        assert_eq!(total, 1);
        assert_eq!(incidents.len(), 1);
        let inc = &incidents[0];
        assert_eq!(inc.severity, Severity::Major);
        assert_eq!(inc.title, "Elevated API errors");
        assert!(inc.url.ends_with("/aaa"));
        assert_eq!(inc.started_at, 1_776_340_800);
    }

    #[test]
    fn test_parse_resolved_returns_empty() {
        let (incidents, total) = parse_atom(RESOLVED_INCIDENT);
        assert_eq!(total, 0);
        assert!(incidents.is_empty());
    }

    #[test]
    fn test_parse_in_progress_maintenance_active() {
        let (incidents, total) = parse_atom(IN_PROGRESS_MAINT);
        assert_eq!(total, 1);
        assert_eq!(incidents[0].severity, Severity::Maintenance);
        assert_eq!(incidents[0].title, "Scheduled database upgrade");
    }

    #[test]
    fn test_parse_scheduled_maintenance_inactive() {
        let (incidents, total) = parse_atom(SCHEDULED_MAINT);
        assert_eq!(total, 0);
        assert!(incidents.is_empty());
    }

    #[test]
    fn test_parse_multiple_sorted_by_updated_desc() {
        let (incidents, total) = parse_atom(MULTIPLE_ACTIVES);
        assert_eq!(total, 2);
        assert_eq!(incidents.len(), 2);
        assert_eq!(incidents[0].title, "Newer active issue");
        assert_eq!(incidents[1].title, "Older active issue");
    }

    #[test]
    fn test_parse_empty_feed() {
        let (incidents, total) = parse_atom(EMPTY_FEED);
        assert_eq!(total, 0);
        assert!(incidents.is_empty());
    }

    #[test]
    fn test_parse_skips_entry_with_malformed_published() {
        let (incidents, total) = parse_atom(ACTIVE_WITH_BAD_PUBLISHED);
        assert_eq!(total, 1);
        assert_eq!(incidents[0].title, "Valid entry");
    }

    #[test]
    fn test_parse_result_distinguishes_xml_error_from_empty() {
        assert!(super::parse_atom_result("not xml at all<<<").is_err());
        let (incidents, total) = super::parse_atom_result(EMPTY_FEED).unwrap();
        assert_eq!(total, 0);
        assert!(incidents.is_empty());
    }

    #[test]
    fn test_severity_term_none_maps_to_minor() {
        assert!(matches!(super::severity_from_term("none"), Severity::Minor));
    }

    // Real Statuspage format: phase is in <strong> inside <content>, not in title.
    const REAL_FEED_ACTIVE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <id>tag:status.claude.com,2005:/history</id>
  <entry>
    <id>tag:status.claude.com,2005:Incident/29804562</id>
    <published>2026-04-25T01:35:55Z</published>
    <updated>2026-04-25T01:35:55Z</updated>
    <link rel="alternate" type="text/html" href="https://status.claude.com/incidents/q93x64nrhwnn"/>
    <title>Elevated error rates on Claude Opus 4.7</title>
    <content type="html">&lt;p&gt;&lt;strong&gt;Investigating&lt;/strong&gt; - We are investigating elevated errors.&lt;/p&gt;</content>
  </entry>
  <entry>
    <id>tag:status.claude.com,2005:Incident/29799469</id>
    <published>2026-04-24T17:32:16Z</published>
    <updated>2026-04-24T17:32:16Z</updated>
    <link rel="alternate" type="text/html" href="https://status.claude.com/incidents/s0lttkq5mmt2"/>
    <title>Issues with sign-ups on platform.claude.com</title>
    <content type="html">&lt;p&gt;&lt;strong&gt;Resolved&lt;/strong&gt; - This incident has been resolved.&lt;/p&gt;</content>
  </entry>
</feed>"#;

    #[test]
    fn test_parse_real_feed_format_active() {
        let (incidents, total) = parse_atom(REAL_FEED_ACTIVE);
        assert_eq!(total, 1);
        assert_eq!(
            incidents[0].title,
            "Elevated error rates on Claude Opus 4.7"
        );
        assert!(incidents[0].url.contains("q93x64nrhwnn"));
    }

    #[test]
    fn test_parse_real_feed_format_resolved_skipped() {
        let resolved_only = r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <entry>
    <id>tag:status.claude.com,2005:Incident/29799469</id>
    <published>2026-04-24T17:32:16Z</published>
    <updated>2026-04-24T17:32:16Z</updated>
    <link rel="alternate" type="text/html" href="https://status.claude.com/incidents/s0lttkq5mmt2"/>
    <title>Issues with sign-ups on platform.claude.com</title>
    <content type="html">&lt;p&gt;&lt;strong&gt;Resolved&lt;/strong&gt; - This incident has been resolved.&lt;/p&gt;</content>
  </entry>
</feed>"#;
        let (incidents, total) = parse_atom(resolved_only);
        assert_eq!(total, 0);
        assert!(incidents.is_empty());
    }

    #[test]
    fn test_extract_phase_from_html() {
        assert_eq!(
            super::extract_phase_from_html("<p><strong>Investigating</strong> - details</p>"),
            Some("Investigating")
        );
        assert_eq!(
            super::extract_phase_from_html("<p>no strong tags</p>"),
            None
        );
    }

    #[test]
    fn test_write_incidents_to_tmp_file() {
        use common::incidents::{seqlock_read_incidents, Severity, INCIDENTS_MMAP_SIZE};
        use std::fs::File;

        let path = std::env::temp_dir().join(format!("clhud-test-{}.bin", std::process::id()));
        let incidents = vec![
            Incident {
                severity: Severity::Critical,
                started_at: 1_700_000_000,
                title: "Test outage".to_string(),
                url: "https://example.com".to_string(),
            },
            Incident {
                severity: Severity::Minor,
                started_at: 1_700_000_001,
                title: "Minor thing".to_string(),
                url: "https://example.com/2".to_string(),
            },
        ];

        super::write_incidents_to_path(&path, &incidents, 2);

        let file = File::open(&path).unwrap();
        let mmap = unsafe { memmap2::Mmap::map(&file) }.unwrap();
        assert_eq!(mmap.len(), INCIDENTS_MMAP_SIZE);
        let (got, total) = seqlock_read_incidents(&mmap);
        assert_eq!(total, 2);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0], incidents[0]);
        assert_eq!(got[1], incidents[1]);

        let _ = std::fs::remove_file(&path);
    }
}
