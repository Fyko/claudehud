use std::fmt::Write as _;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use common::incidents::Incident;

use crate::fmt::{self, *};
use crate::input::Input;
use crate::time::{format_duration, format_reset_time, parse_iso8601, ResetStyle};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RoundingMode {
    #[default]
    Floor,
    Ceiling,
    Nearest,
}

impl RoundingMode {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "floor" => Some(Self::Floor),
            "ceil" | "ceiling" => Some(Self::Ceiling),
            "nearest" | "round" | "regular" => Some(Self::Nearest),
            _ => None,
        }
    }

    fn apply(self, pct: f64) -> u8 {
        let rounded = match self {
            Self::Floor => pct.floor(),
            Self::Ceiling => pct.ceil(),
            Self::Nearest => pct.round(),
        };
        rounded.clamp(0.0, 100.0) as u8
    }
}

pub fn render(
    input: &Input,
    git: Option<(String, bool)>,
    incidents: &[Incident],
    total_active: u8,
    rounding: RoundingMode,
) -> String {
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
    let pct = context_pct(input, rounding);
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

    // ── Incident lines (between line 1 and rate limits) ────
    for inc in incidents {
        out.push('\n');
        push_incident_line(inc, &mut out);
    }
    let overflow = total_active.saturating_sub(incidents.len() as u8);
    if overflow > 0 {
        out.push('\n');
        write!(out, "\x1b]8;;https://status.claude.com/\x1b\\").unwrap();
        out.push_str(DIM);
        write!(out, "+{overflow} more").unwrap();
        out.push_str(RESET);
        out.push_str("\x1b]8;;\x1b\\");
    }

    // ── Rate limits ────────────────────────────────────────
    if let Some(rl) = &input.rate_limits {
        if let Some(fh) = &rl.five_hour {
            if let Some(pct_f) = fh.used_percentage {
                let pct = rounding.apply(pct_f);
                out.push_str("\n\n");
                push_rate_row("current", pct, fh.resets_at, ResetStyle::Time, &mut out);

                if let Some(sd) = &rl.seven_day {
                    if let Some(pct_f) = sd.used_percentage {
                        let pct = rounding.apply(pct_f);
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
    let url = &inc.url;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let elapsed = now.saturating_sub(inc.started_at);
    let since = format_duration(elapsed);

    write!(out, "\x1b]8;;{url}\x1b\\").unwrap();
    out.push_str(fmt::color_for_severity(inc.severity));
    out.push_str(&inc.title);
    out.push(' ');
    out.push_str(DIM);
    write!(out, "· started {since} ago").unwrap();
    out.push_str(RESET);
    out.push_str("\x1b]8;;\x1b\\");
}

fn push_rate_row(
    label: &str,
    pct: u8,
    resets_at: Option<u64>,
    style: ResetStyle,
    out: &mut String,
) {
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

fn context_pct(input: &Input, rounding: RoundingMode) -> u8 {
    let cw = input.context_window.as_ref();
    if let Some(pct) = cw.and_then(|cw| cw.used_percentage) {
        return rounding.apply(pct);
    }
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
    rounding.apply((current as f64 * 100.0) / size as f64)
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
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            if c != '\x1b' {
                out.push(c);
                continue;
            }
            // Next char decides the sequence type.
            match chars.next() {
                Some('[') => {
                    // CSI — consume until final byte in 0x40..=0x7E.
                    for c2 in chars.by_ref() {
                        if ('\x40'..='\x7e').contains(&c2) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    // OSC — consume until BEL (0x07) or ST (ESC \).
                    while let Some(c2) = chars.next() {
                        if c2 == '\x07' {
                            break;
                        }
                        if c2 == '\x1b' {
                            // Peek for trailing '\\'
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

    #[test]
    fn test_render_default_model() {
        let input = Input::default();
        let result = render(&input, None, &[], 0, RoundingMode::Floor);
        let plain = strip_ansi(&result);
        assert!(
            plain.contains("Claude"),
            "should contain default model name"
        );
    }

    #[test]
    fn test_render_model_name() {
        let json = r#"{"model": {"display_name": "claude-sonnet-4-5"}}"#;
        let input: Input = serde_json::from_str(json).unwrap();
        let plain = strip_ansi(&render(&input, None, &[], 0, RoundingMode::Floor));
        assert!(plain.contains("claude-sonnet-4-5"));
    }

    #[test]
    fn test_render_context_pct() {
        let json = r#"{
            "context_window": {
                "context_window_size": 200000,
                "current_usage": {"input_tokens": 100000, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0}
            }
        }"#;
        let input: Input = serde_json::from_str(json).unwrap();
        let plain = strip_ansi(&render(&input, None, &[], 0, RoundingMode::Floor));
        assert!(plain.contains("50%"));
    }

    #[test]
    fn test_render_git_branch() {
        let input = Input::default();
        let plain = strip_ansi(&render(
            &input,
            Some(("main".to_string(), false)),
            &[],
            0,
            RoundingMode::Floor,
        ));
        assert!(plain.contains("(main)"));
    }

    #[test]
    fn test_render_git_dirty() {
        let input = Input::default();
        let plain = strip_ansi(&render(
            &input,
            Some(("main".to_string(), true)),
            &[],
            0,
            RoundingMode::Floor,
        ));
        assert!(plain.contains("(main*") || plain.contains("main") && plain.contains('*'));
    }

    #[test]
    fn test_render_dirname() {
        let json = r#"{"cwd": "/home/user/myproject"}"#;
        let input: Input = serde_json::from_str(json).unwrap();
        let plain = strip_ansi(&render(&input, None, &[], 0, RoundingMode::Floor));
        assert!(plain.contains("myproject"));
    }

    #[test]
    fn test_render_rate_limits_present() {
        let json = r#"{
            "rate_limits": {
                "five_hour": {"used_percentage": 45.0, "resets_at": 1705316400},
                "seven_day": {"used_percentage": 12.0, "resets_at": 1705833600}
            }
        }"#;
        let input: Input = serde_json::from_str(json).unwrap();
        let result = render(&input, None, &[], 0, RoundingMode::Floor);
        assert!(
            result.contains('\n'),
            "should have newlines for rate limits"
        );
        let plain = strip_ansi(&result);
        assert!(plain.contains("current"));
        assert!(plain.contains("weekly"));
    }

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
        };
        let input = Input::default();
        let out = render(&input, None, &[incident], 1, RoundingMode::Floor);
        let plain = strip_ansi(&out);
        assert!(
            out.contains(fmt::ORANGE),
            "title should be orange for major severity"
        );
        assert!(plain.contains("Elevated API errors"));
        assert!(plain.contains("started 12m ago"));
        assert!(out.contains("\x1b]8;;https://status.claude.com/incidents/abc"));
        assert!(!plain.contains("more"));
    }

    #[test]
    fn test_render_incident_with_plus_n_more() {
        use common::incidents::{Incident, Severity};
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let incident = Incident {
            severity: Severity::Minor,
            started_at: now,
            title: "Thing A".to_string(),
            url: "https://status.claude.com/incidents/a".to_string(),
        };
        // 1 stored, total=3 → "+2 more"
        let out = render(&Input::default(), None, &[incident], 3, RoundingMode::Floor);
        let plain = strip_ansi(&out);
        assert!(plain.contains("+2 more"));
    }

    #[test]
    fn test_render_multiple_incidents() {
        use common::incidents::{Incident, Severity};
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let incidents = vec![
            Incident {
                severity: Severity::Critical,
                started_at: now.saturating_sub(5 * 60),
                title: "API down".to_string(),
                url: "https://status.claude.com/incidents/x".to_string(),
            },
            Incident {
                severity: Severity::Minor,
                started_at: now.saturating_sub(20 * 60),
                title: "Elevated latency".to_string(),
                url: "https://status.claude.com/incidents/y".to_string(),
            },
        ];
        let out = render(&Input::default(), None, &incidents, 2, RoundingMode::Floor);
        let plain = strip_ansi(&out);
        assert!(plain.contains("API down"));
        assert!(plain.contains("Elevated latency"));
        assert!(!plain.contains("more"));
        assert!(out.contains(fmt::RED));
        assert!(out.contains(fmt::YELLOW));
    }

    #[test]
    fn test_render_no_incident_unchanged_shape() {
        let out = render(&Input::default(), None, &[], 0, RoundingMode::Floor);
        let plain = strip_ansi(&out);
        assert!(
            !plain.contains("·"),
            "incident separator should not appear without incident"
        );
    }

    #[test]
    fn test_rounding_mode_parse() {
        assert_eq!(RoundingMode::parse("floor"), Some(RoundingMode::Floor));
        assert_eq!(RoundingMode::parse("FLOOR"), Some(RoundingMode::Floor));
        assert_eq!(RoundingMode::parse("ceil"), Some(RoundingMode::Ceiling));
        assert_eq!(RoundingMode::parse("ceiling"), Some(RoundingMode::Ceiling));
        assert_eq!(RoundingMode::parse("nearest"), Some(RoundingMode::Nearest));
        assert_eq!(RoundingMode::parse("round"), Some(RoundingMode::Nearest));
        assert_eq!(RoundingMode::parse("regular"), Some(RoundingMode::Nearest));
        assert_eq!(RoundingMode::parse("huh"), None);
    }

    #[test]
    fn test_rounding_mode_apply() {
        assert_eq!(RoundingMode::Floor.apply(49.9), 49);
        assert_eq!(RoundingMode::Ceiling.apply(49.1), 50);
        assert_eq!(RoundingMode::Nearest.apply(49.5), 50);
        assert_eq!(RoundingMode::Nearest.apply(49.4), 49);
        // clamping
        assert_eq!(RoundingMode::Ceiling.apply(120.0), 100);
        assert_eq!(RoundingMode::Floor.apply(-5.0), 0);
    }

    #[test]
    fn test_render_real_stdin_fixture() {
        let input: Input = serde_json::from_str(crate::input::REAL_STDIN_FIXTURE).unwrap();
        let out = render(&input, None, &[], 0, RoundingMode::Floor);
        let plain = strip_ansi(&out);
        assert!(plain.contains("Opus 4.7"), "model name should render");
        assert!(
            plain.contains("22%"),
            "server-provided used_percentage wins"
        );
        assert!(plain.contains("project"), "cwd dirname should render");
        assert!(
            plain.contains("current"),
            "five-hour rate row should render"
        );
        assert!(plain.contains("weekly"), "seven-day rate row should render");
    }

    #[test]
    fn test_render_prefers_server_used_percentage() {
        // current_usage would sum to well over 100%, but used_percentage says 10.
        let json = r#"{
            "context_window": {
                "context_window_size": 200000,
                "used_percentage": 10,
                "current_usage": {"input_tokens": 999999, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0}
            }
        }"#;
        let input: Input = serde_json::from_str(json).unwrap();
        let plain = strip_ansi(&render(&input, None, &[], 0, RoundingMode::Floor));
        assert!(plain.contains("10%"));
        assert!(!plain.contains("100%"));
    }

    #[test]
    fn test_render_context_pct_rounding_modes() {
        // 100_001 / 200_000 = 50.0005%
        let json = r#"{
            "context_window": {
                "context_window_size": 200000,
                "current_usage": {"input_tokens": 100001, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0}
            }
        }"#;
        let input: Input = serde_json::from_str(json).unwrap();
        assert!(strip_ansi(&render(&input, None, &[], 0, RoundingMode::Floor)).contains("50%"));
        assert!(strip_ansi(&render(&input, None, &[], 0, RoundingMode::Ceiling)).contains("51%"));
        assert!(strip_ansi(&render(&input, None, &[], 0, RoundingMode::Nearest)).contains("50%"));
    }
}
