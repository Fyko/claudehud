use common::incidents::{Incident, Severity};

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
        "maintenance" | "none" => Severity::Maintenance,
        _ => Severity::Minor,
    }
}

pub fn parse_atom(xml: &str) -> Option<Incident> {
    let doc = roxmltree::Document::parse(xml).ok()?;
    let root = doc.root_element();

    let mut best: Option<(u64, Incident)> = None;
    let mut active_count: u32 = 0;

    for entry in root.children().filter(|n| n.has_tag_name("entry")) {
        // Extract title text
        let title_node = entry.children().find(|n| n.has_tag_name("title"));
        let raw_title = title_node.and_then(|n| n.text()).unwrap_or("").trim();

        // Split on " - "
        let (phase, subject) = match raw_title.split_once(" - ") {
            Some((p, s)) => (p.trim(), s.trim()),
            None => continue,
        };

        let phase_lc = phase.to_ascii_lowercase();

        // Inactive-set check first (stopping rule)
        if INACTIVE_PHASES.iter().any(|p| *p == phase_lc) {
            continue;
        }
        if !ACTIVE_PHASES.iter().any(|p| *p == phase_lc) {
            continue;
        }

        active_count = active_count.saturating_add(1);

        // Severity from <category term="...">
        let severity = entry
            .children()
            .find(|n| n.has_tag_name("category"))
            .and_then(|n| n.attribute("term"))
            .map(severity_from_term)
            .unwrap_or(Severity::Minor);

        // URL from <link href="...">
        let url = entry
            .children()
            .find(|n| n.has_tag_name("link"))
            .and_then(|n| n.attribute("href"))
            .unwrap_or("")
            .to_string();

        // started_at from <published>
        let started_at = entry
            .children()
            .find(|n| n.has_tag_name("published"))
            .and_then(|n| n.text())
            .and_then(parse_iso8601_secs)
            .unwrap_or(0);

        // updated_at from <updated>, fall back to started_at
        let updated_at = entry
            .children()
            .find(|n| n.has_tag_name("updated"))
            .and_then(|n| n.text())
            .and_then(parse_iso8601_secs)
            .unwrap_or(started_at);

        let incident = Incident {
            severity,
            started_at,
            title: subject.to_string(),
            url,
            active_count: 0, // filled in below
        };

        match &best {
            Some((best_updated, _)) if *best_updated >= updated_at => {}
            _ => {
                best = Some((updated_at, incident));
            }
        }
    }

    let (_, mut incident) = best?;
    incident.active_count = active_count.min(u8::MAX as u32) as u8;
    Some(incident)
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
        let inc = parse_atom(ACTIVE_INCIDENT).expect("should parse active incident");
        assert_eq!(inc.severity, Severity::Major);
        assert_eq!(inc.title, "Elevated API errors");
        assert!(inc.url.ends_with("/aaa"));
        assert_eq!(inc.active_count, 1);
        // 2026-04-16T12:00:00Z == 1_776_340_800 unix seconds
        assert_eq!(inc.started_at, 1_776_340_800);
    }

    #[test]
    fn test_parse_resolved_returns_none() {
        assert_eq!(parse_atom(RESOLVED_INCIDENT), None);
    }

    #[test]
    fn test_parse_in_progress_maintenance_active() {
        let inc = parse_atom(IN_PROGRESS_MAINT).expect("should parse active maintenance");
        assert_eq!(inc.severity, Severity::Maintenance);
        assert_eq!(inc.title, "Scheduled database upgrade");
        assert_eq!(inc.active_count, 1);
    }

    #[test]
    fn test_parse_scheduled_maintenance_inactive() {
        assert_eq!(parse_atom(SCHEDULED_MAINT), None);
    }

    #[test]
    fn test_parse_multiple_picks_most_recent_updated() {
        let inc = parse_atom(MULTIPLE_ACTIVES).expect("should parse");
        assert_eq!(inc.title, "Newer active issue");
        assert_eq!(inc.active_count, 2);
    }

    #[test]
    fn test_parse_empty_feed() {
        assert_eq!(parse_atom(EMPTY_FEED), None);
    }
}
