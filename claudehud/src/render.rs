use std::fmt::Write as _;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use common::incidents::Incident;
use common::{GitExtra, OpState};

use crate::fmt::{self, *};
use crate::input::Input;
use crate::time::{format_duration, format_reset_time, ResetStyle};

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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Layout {
    #[default]
    Comfortable,
    Condensed,
}

impl Layout {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "comfortable" => Some(Self::Comfortable),
            "condensed" => Some(Self::Condensed),
            _ => None,
        }
    }
}

pub fn render(
    input: &Input,
    git: Option<(String, bool)>,
    git_extra: Option<&GitExtra>,
    incidents: &[Incident],
    total_active: u8,
    rounding: RoundingMode,
    layout: Layout,
) -> String {
    match layout {
        Layout::Comfortable => {
            render_comfortable(input, git, git_extra, incidents, total_active, rounding)
        }
        Layout::Condensed => {
            render_condensed(input, git, git_extra, incidents, total_active, rounding)
        }
    }
}

fn render_comfortable(
    input: &Input,
    git: Option<(String, bool)>,
    git_extra: Option<&GitExtra>,
    incidents: &[Incident],
    total_active: u8,
    rounding: RoundingMode,
) -> String {
    let mut out = String::with_capacity(512);

    // ── Model ──────────────────────────────────────────────
    push_model_full(input, &mut out);

    // ── Context usage ──────────────────────────────────────
    out.push_str(SEP);
    push_context(input, rounding, &mut out);

    // ── Cost (skipped when absent or $0) ───────────────────
    push_cost(input, &mut out);

    // ── Dir + git ──────────────────────────────────────────
    out.push_str(SEP);
    push_dir_branch(input, git.as_ref(), git_extra, false, &mut out);

    // ── Incident lines (between line 1 and rate limits) ────
    push_incidents(incidents, total_active, &mut out);

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

fn render_condensed(
    input: &Input,
    git: Option<(String, bool)>,
    git_extra: Option<&GitExtra>,
    incidents: &[Incident],
    total_active: u8,
    rounding: RoundingMode,
) -> String {
    let mut out = String::with_capacity(512);

    // ── Model (short) ──────────────────────────────────────
    push_model_short(input, &mut out);

    // ── Context usage ──────────────────────────────────────
    out.push_str(SEP);
    push_context(input, rounding, &mut out);

    // ── Cost (skipped when absent or $0) ───────────────────
    push_cost(input, &mut out);

    // ── Dir + git (tight) ──────────────────────────────────
    out.push_str(SEP);
    push_dir_branch(input, git.as_ref(), git_extra, true, &mut out);

    // ── Rate limits inline ─────────────────────────────────
    if let Some(rl) = &input.rate_limits {
        if let Some(fh) = &rl.five_hour {
            if let Some(pct_f) = fh.used_percentage {
                let pct = rounding.apply(pct_f);
                out.push_str(SEP);
                push_rate_inline("5h", pct, fh.resets_at, ResetStyle::Time, &mut out);
            }
        }
        if let Some(sd) = &rl.seven_day {
            if let Some(pct_f) = sd.used_percentage {
                let pct = rounding.apply(pct_f);
                out.push_str(SEP);
                push_rate_inline("7d", pct, sd.resets_at, ResetStyle::DateTime, &mut out);
            }
        }
    }

    // ── Incidents ──────────────────────────────────────────
    push_incidents(incidents, total_active, &mut out);

    out
}

fn push_model_short(input: &Input, out: &mut String) {
    let raw = input
        .model
        .as_ref()
        .and_then(|m| m.display_name.as_deref())
        .unwrap_or("Claude");
    let short = raw
        .split_once(" (")
        .map(|(prefix, _)| prefix)
        .unwrap_or(raw);
    out.push_str(BLUE);
    out.push_str(short);
    out.push_str(RESET);
}

fn push_model_full(input: &Input, out: &mut String) {
    let model = input
        .model
        .as_ref()
        .and_then(|m| m.display_name.as_deref())
        .unwrap_or("Claude");
    out.push_str(BLUE);
    out.push_str(model);
    out.push_str(RESET);
}

fn push_cost(input: &Input, out: &mut String) {
    // The harness reports total_cost_usd on plan billing too, but it's an
    // estimate against pay-per-token rates — not what the user actually owes.
    // Presence of rate_limits is our cleanest plan-vs-API signal: API users
    // never get a rate_limits block.
    if input.rate_limits.is_some() {
        return;
    }
    let Some(usd) = input.cost.as_ref().and_then(|c| c.total_cost_usd) else {
        return;
    };
    if !usd.is_finite() || usd <= 0.0 {
        return;
    }
    out.push_str(SEP);
    out.push_str("💰 ");
    out.push_str(fmt::color_for_cost(usd));
    write!(out, "${usd:.2}").unwrap();
    out.push_str(RESET);
}

fn push_context(input: &Input, rounding: RoundingMode, out: &mut String) {
    let pct = context_pct(input, rounding);
    out.push_str("✍️ ");
    out.push_str(color_for_pct(pct));
    write!(out, "{pct}%").unwrap();
    out.push_str(RESET);
}

fn push_dir_branch(
    input: &Input,
    git: Option<&(String, bool)>,
    extra: Option<&GitExtra>,
    tight: bool,
    out: &mut String,
) {
    let cwd = input.cwd.as_deref().unwrap_or("");
    let dirname = Path::new(cwd)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(cwd);

    // Comfortable: op-state badge prepended before dir+branch
    if !tight {
        if let Some(ex) = extra {
            push_op_badge_comfortable(ex, out);
        }
    }

    out.push_str(CYAN);
    out.push_str(dirname);
    out.push_str(RESET);

    if let Some((branch, dirty)) = git {
        if !tight {
            out.push(' ');
        }
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

        // Ahead/behind
        if let Some(ex) = extra {
            if ex.ahead > 0 {
                out.push(' ');
                out.push_str(GREEN);
                write!(out, "↑{}", ex.ahead).unwrap();
                out.push_str(RESET);
            }
            if ex.behind > 0 {
                out.push(' ');
                out.push_str(RED);
                write!(out, "↓{}", ex.behind).unwrap();
                out.push_str(RESET);
            }
        }

        // Condensed: op-state + conflicts inline after branch
        if tight {
            if let Some(ex) = extra {
                push_op_badge_condensed(ex, out);
            }
        }
    }
}

fn push_op_badge_comfortable(ex: &GitExtra, out: &mut String) {
    let label = match ex.op_state {
        OpState::None => return,
        OpState::Merge => "MERGING".to_string(),
        OpState::Rebase => {
            if ex.op_step > 0 || ex.op_total > 0 {
                format!("REBASE {}/{}", ex.op_step, ex.op_total)
            } else {
                "REBASE".to_string()
            }
        }
        OpState::CherryPick => "CHERRY-PICK".to_string(),
        OpState::Revert => "REVERTING".to_string(),
        OpState::Bisect => "BISECTING".to_string(),
    };
    out.push_str(YELLOW);
    out.push_str(&label);
    out.push_str(RESET);
    if ex.conflict_count > 0 {
        out.push_str(DIM);
        write!(out, " · {} conflicts", ex.conflict_count).unwrap();
        out.push_str(RESET);
    }
    out.push_str(SEP);
}

fn push_op_badge_condensed(ex: &GitExtra, out: &mut String) {
    let badge = match ex.op_state {
        OpState::None => return,
        OpState::Merge => "M".to_string(),
        OpState::Rebase => {
            if ex.op_step > 0 || ex.op_total > 0 {
                format!("R {}/{}", ex.op_step, ex.op_total)
            } else {
                "R".to_string()
            }
        }
        OpState::CherryPick => "CP".to_string(),
        OpState::Revert => "REV".to_string(),
        OpState::Bisect => "BIS".to_string(),
    };
    out.push(' ');
    out.push_str(YELLOW);
    out.push_str(&badge);
    out.push_str(RESET);
    if ex.conflict_count > 0 {
        out.push_str(RED);
        write!(out, "!{}", ex.conflict_count).unwrap();
        out.push_str(RESET);
    }
}

fn push_incidents(incidents: &[Incident], total_active: u8, out: &mut String) {
    for inc in incidents {
        out.push('\n');
        push_incident_line(inc, out);
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

fn push_rate_inline(
    label: &str,
    pct: u8,
    resets_at: Option<u64>,
    style: ResetStyle,
    out: &mut String,
) {
    fmt::build_bar(pct, 4, out);
    out.push(' ');
    out.push_str(WHITE);
    out.push_str(label);
    out.push_str(RESET);
    out.push(' ');
    out.push_str(color_for_pct(pct));
    write!(out, "{pct}%").unwrap();
    out.push_str(RESET);
    if let Some(epoch) = resets_at.filter(|&e| e > 0) {
        out.push(' ');
        out.push_str(DIM);
        out.push_str("⟳ ");
        out.push_str(RESET);
        out.push_str(WHITE);
        out.push_str(&format_reset_time(epoch, style));
        out.push_str(RESET);
    }
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
        let result = render(
            &input,
            None,
            None,
            &[],
            0,
            RoundingMode::Floor,
            Layout::Comfortable,
        );
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
        let plain = strip_ansi(&render(
            &input,
            None,
            None,
            &[],
            0,
            RoundingMode::Floor,
            Layout::Comfortable,
        ));
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
        let plain = strip_ansi(&render(
            &input,
            None,
            None,
            &[],
            0,
            RoundingMode::Floor,
            Layout::Comfortable,
        ));
        assert!(plain.contains("50%"));
    }

    #[test]
    fn test_render_git_branch() {
        let input = Input::default();
        let plain = strip_ansi(&render(
            &input,
            Some(("main".to_string(), false)),
            None,
            &[],
            0,
            RoundingMode::Floor,
            Layout::Comfortable,
        ));
        assert!(plain.contains("(main)"));
    }

    #[test]
    fn test_render_git_dirty() {
        let input = Input::default();
        let plain = strip_ansi(&render(
            &input,
            Some(("main".to_string(), true)),
            None,
            &[],
            0,
            RoundingMode::Floor,
            Layout::Comfortable,
        ));
        assert!(plain.contains("(main*") || plain.contains("main") && plain.contains('*'));
    }

    #[test]
    fn test_render_dirname() {
        let json = r#"{"cwd": "/home/user/myproject"}"#;
        let input: Input = serde_json::from_str(json).unwrap();
        let plain = strip_ansi(&render(
            &input,
            None,
            None,
            &[],
            0,
            RoundingMode::Floor,
            Layout::Comfortable,
        ));
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
        let result = render(
            &input,
            None,
            None,
            &[],
            0,
            RoundingMode::Floor,
            Layout::Comfortable,
        );
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
        let out = render(
            &input,
            None,
            None,
            &[incident],
            1,
            RoundingMode::Floor,
            Layout::Comfortable,
        );
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
        let out = render(
            &Input::default(),
            None,
            None,
            &[incident],
            3,
            RoundingMode::Floor,
            Layout::Comfortable,
        );
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
        let out = render(
            &Input::default(),
            None,
            None,
            &incidents,
            2,
            RoundingMode::Floor,
            Layout::Comfortable,
        );
        let plain = strip_ansi(&out);
        assert!(plain.contains("API down"));
        assert!(plain.contains("Elevated latency"));
        assert!(!plain.contains("more"));
        assert!(out.contains(fmt::RED));
        assert!(out.contains(fmt::YELLOW));
    }

    #[test]
    fn test_render_no_incident_unchanged_shape() {
        let out = render(
            &Input::default(),
            None,
            None,
            &[],
            0,
            RoundingMode::Floor,
            Layout::Comfortable,
        );
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
        let out = render(
            &input,
            None,
            None,
            &[],
            0,
            RoundingMode::Floor,
            Layout::Comfortable,
        );
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
        let plain = strip_ansi(&render(
            &input,
            None,
            None,
            &[],
            0,
            RoundingMode::Floor,
            Layout::Comfortable,
        ));
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
        assert!(strip_ansi(&render(
            &input,
            None,
            None,
            &[],
            0,
            RoundingMode::Floor,
            Layout::Comfortable
        ))
        .contains("50%"));
        assert!(strip_ansi(&render(
            &input,
            None,
            None,
            &[],
            0,
            RoundingMode::Ceiling,
            Layout::Comfortable
        ))
        .contains("51%"));
        assert!(strip_ansi(&render(
            &input,
            None,
            None,
            &[],
            0,
            RoundingMode::Nearest,
            Layout::Comfortable
        ))
        .contains("50%"));
    }

    #[test]
    fn test_layout_parse() {
        assert_eq!(Layout::parse("comfortable"), Some(Layout::Comfortable));
        assert_eq!(Layout::parse("COMFORTABLE"), Some(Layout::Comfortable));
        assert_eq!(Layout::parse("condensed"), Some(Layout::Condensed));
        assert_eq!(Layout::parse("Condensed"), Some(Layout::Condensed));
        assert_eq!(Layout::parse(""), None);
        assert_eq!(Layout::parse("compact"), None);
        assert_eq!(Layout::parse("garbage"), None);
    }

    #[test]
    fn test_layout_default_is_comfortable() {
        assert_eq!(Layout::default(), Layout::Comfortable);
    }

    // ── Condensed layout tests ────────────────────────────────────────────────

    #[test]
    fn test_render_default_model_condensed() {
        let input = Input::default();
        let result = render(&input, None, None, &[], 0, RoundingMode::Floor, Layout::Condensed);
        let plain = strip_ansi(&result);
        assert!(plain.contains("Claude"), "default model name should render");
    }

    #[test]
    fn test_render_model_name_condensed_strips_paren() {
        let json = r#"{"model": {"display_name": "Opus 4.7 (1M context)"}}"#;
        let input: Input = serde_json::from_str(json).unwrap();
        let plain = strip_ansi(&render(
            &input,
            None,
            None,
            &[],
            0,
            RoundingMode::Floor,
            Layout::Condensed,
        ));
        assert!(plain.contains("Opus 4.7"), "short model name should render");
        assert!(
            !plain.contains("(1M context)"),
            "parenthetical suffix should be stripped"
        );
    }

    #[test]
    fn test_render_dir_branch_condensed_tight() {
        let json = r#"{"cwd": "/home/user/myproject"}"#;
        let input: Input = serde_json::from_str(json).unwrap();
        let plain = strip_ansi(&render(
            &input,
            Some(("main".to_string(), false)),
            None,
            &[],
            0,
            RoundingMode::Floor,
            Layout::Condensed,
        ));
        assert!(
            plain.contains("myproject(main)"),
            "dir and branch should be tight (no space)"
        );
        assert!(
            !plain.contains("myproject (main)"),
            "comfortable spacing should not appear"
        );
    }

    #[test]
    fn test_render_rate_limits_condensed_inline() {
        let json = r#"{
            "rate_limits": {
                "five_hour": {"used_percentage": 9.0, "resets_at": 1705316400},
                "seven_day": {"used_percentage": 12.0, "resets_at": 1705833600}
            }
        }"#;
        let input: Input = serde_json::from_str(json).unwrap();
        let result = render(&input, None, None, &[], 0, RoundingMode::Floor, Layout::Condensed);
        let plain = strip_ansi(&result);

        assert!(plain.contains("5h"), "5h label should render");
        assert!(plain.contains("7d"), "7d label should render");
        assert!(
            !plain.contains("current"),
            "comfortable label should not appear"
        );
        assert!(
            !plain.contains("weekly"),
            "comfortable label should not appear"
        );

        assert!(plain.contains("9%"), "5h pct should render");
        assert!(plain.contains("12%"), "7d pct should render");

        let dots = plain.matches('○').count() + plain.matches('●').count();
        assert!(dots >= 8, "expected ≥8 bar dots inline (got {dots})");

        assert!(
            !result.contains('\n'),
            "condensed idle output should be single-line"
        );
    }

    #[test]
    fn test_render_incident_condensed_keeps_own_line() {
        use common::incidents::{Incident, Severity};
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let incident = Incident {
            severity: Severity::Major,
            started_at: now.saturating_sub(12 * 60),
            title: "Elevated API errors".to_string(),
            url: "https://status.claude.com/incidents/abc".to_string(),
        };
        let out = render(
            &Input::default(),
            None,
            None,
            &[incident],
            1,
            RoundingMode::Floor,
            Layout::Condensed,
        );
        let plain = strip_ansi(&out);
        assert!(plain.contains("Elevated API errors"));
        assert!(plain.contains("started 12m ago"));
        assert_eq!(out.matches('\n').count(), 1, "exactly one newline expected");
        assert!(out.contains("\x1b]8;;https://status.claude.com/incidents/abc"));
    }

    #[test]
    fn test_render_context_pct_condensed() {
        let json = r#"{
            "context_window": {
                "context_window_size": 200000,
                "current_usage": {"input_tokens": 100000, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0}
            }
        }"#;
        let input: Input = serde_json::from_str(json).unwrap();
        let plain = strip_ansi(&render(
            &input,
            None,
            None,
            &[],
            0,
            RoundingMode::Floor,
            Layout::Condensed,
        ));
        assert!(plain.contains("50%"));
    }

    #[test]
    fn test_render_condensed_no_rate_limits() {
        let input = Input::default();
        let result = render(&input, None, None, &[], 0, RoundingMode::Floor, Layout::Condensed);
        let plain = strip_ansi(&result);
        assert!(plain.contains("Claude"));
        assert!(!plain.contains("5h"));
        assert!(!plain.contains("7d"));
        assert!(!result.contains('\n'));
    }

    #[test]
    fn test_render_condensed_only_5h() {
        let json = r#"{
            "rate_limits": {
                "five_hour": {"used_percentage": 9.0, "resets_at": 1705316400}
            }
        }"#;
        let input: Input = serde_json::from_str(json).unwrap();
        let result = render(&input, None, None, &[], 0, RoundingMode::Floor, Layout::Condensed);
        let plain = strip_ansi(&result);
        assert!(plain.contains("5h"));
        assert!(plain.contains("9%"));
        assert!(!plain.contains("7d"));
    }

    #[test]
    fn test_render_condensed_only_7d() {
        let json = r#"{
            "rate_limits": {
                "seven_day": {"used_percentage": 12.0, "resets_at": 1705833600}
            }
        }"#;
        let input: Input = serde_json::from_str(json).unwrap();
        let result = render(&input, None, None, &[], 0, RoundingMode::Floor, Layout::Condensed);
        let plain = strip_ansi(&result);
        assert!(plain.contains("7d"));
        assert!(plain.contains("12%"));
        assert!(!plain.contains("5h"));
    }

    #[test]
    fn test_render_git_dirty_condensed() {
        let input = Input::default();
        let out = render(
            &input,
            Some(("main".to_string(), true)),
            None,
            &[],
            0,
            RoundingMode::Floor,
            Layout::Condensed,
        );
        let plain = strip_ansi(&out);
        assert!(
            plain.contains("(main*)"),
            "dirty marker should appear inside paren"
        );
    }

    #[test]
    fn test_render_incident_plus_n_more_condensed() {
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
        let out = render(
            &Input::default(),
            None,
            None,
            &[incident],
            3,
            RoundingMode::Floor,
            Layout::Condensed,
        );
        let plain = strip_ansi(&out);
        assert!(plain.contains("+2 more"));
        assert_eq!(out.matches('\n').count(), 2);
    }

    #[test]
    fn test_render_real_stdin_fixture_condensed() {
        let input: Input = serde_json::from_str(crate::input::REAL_STDIN_FIXTURE).unwrap();
        let out = render(&input, None, None, &[], 0, RoundingMode::Floor, Layout::Condensed);
        let plain = strip_ansi(&out);
        assert!(plain.contains("Opus 4.7"), "model name should render");
        assert!(
            plain.contains("22%"),
            "server-provided used_percentage wins"
        );
        assert!(plain.contains("project"), "cwd dirname should render");
        assert!(plain.contains("5h"), "5h rate label should render");
        assert!(plain.contains("7d"), "7d rate label should render");
        assert!(
            !plain.contains("current"),
            "comfortable label should not appear"
        );
        assert!(
            !plain.contains("weekly"),
            "comfortable label should not appear"
        );
        assert!(
            !out.contains('\n'),
            "fixture has no incidents → single-line output"
        );
    }

    #[test]
    fn test_render_no_session_duration() {
        // Build an Input with a session.start_time that would have produced "Xh Ym".
        let json = r#"{
            "session": {"start_time": "2024-01-15T10:30:00Z"}
        }"#;
        let input: Input = serde_json::from_str(json).unwrap();
        let plain = strip_ansi(&render(
            &input,
            None,
            None,
            &[],
            0,
            RoundingMode::Floor,
            Layout::Comfortable,
        ));
        assert!(!plain.contains('⏱'), "stopwatch glyph should be gone");
    }

    // ── Cost rendering ────────────────────────────────────────────────────────

    #[test]
    fn test_render_cost_comfortable_present() {
        let json = r#"{"cost": {"total_cost_usd": 0.1298}}"#;
        let input: Input = serde_json::from_str(json).unwrap();
        let plain = strip_ansi(&render(
            &input,
            None,
            None,
            &[],
            0,
            RoundingMode::Floor,
            Layout::Comfortable,
        ));
        assert!(
            plain.contains("$0.13"),
            "cost should render with 2 decimals"
        );
        assert!(plain.contains("💰"), "cost glyph should render");
    }

    #[test]
    fn test_render_cost_condensed_present() {
        let json = r#"{"cost": {"total_cost_usd": 1.4567}}"#;
        let input: Input = serde_json::from_str(json).unwrap();
        let plain = strip_ansi(&render(
            &input,
            None,
            None,
            &[],
            0,
            RoundingMode::Floor,
            Layout::Condensed,
        ));
        assert!(plain.contains("$1.46"));
        assert!(plain.contains("💰"));
    }

    #[test]
    fn test_render_cost_skipped_when_zero() {
        let json = r#"{"cost": {"total_cost_usd": 0}}"#;
        let input: Input = serde_json::from_str(json).unwrap();
        let plain = strip_ansi(&render(
            &input,
            None,
            None,
            &[],
            0,
            RoundingMode::Floor,
            Layout::Comfortable,
        ));
        assert!(!plain.contains('$'), "zero cost should be hidden");
        assert!(!plain.contains("💰"));
    }

    #[test]
    fn test_render_cost_skipped_when_absent() {
        let plain = strip_ansi(&render(
            &Input::default(),
            None,
            None,
            &[],
            0,
            RoundingMode::Floor,
            Layout::Comfortable,
        ));
        assert!(!plain.contains('$'));
        assert!(!plain.contains("💰"));
    }

    #[test]
    fn test_render_cost_color_tiers() {
        // < $1 → green
        let json = r#"{"cost": {"total_cost_usd": 0.5}}"#;
        let input: Input = serde_json::from_str(json).unwrap();
        let out = render(
            &input,
            None,
            None,
            &[],
            0,
            RoundingMode::Floor,
            Layout::Comfortable,
        );
        assert!(out.contains(fmt::GREEN));

        // $1 ≤ x < $5 → yellow
        let json = r#"{"cost": {"total_cost_usd": 2.5}}"#;
        let input: Input = serde_json::from_str(json).unwrap();
        let out = render(
            &input,
            None,
            None,
            &[],
            0,
            RoundingMode::Floor,
            Layout::Comfortable,
        );
        assert!(out.contains(fmt::YELLOW));

        // $5 ≤ x < $20 → orange
        let json = r#"{"cost": {"total_cost_usd": 12.0}}"#;
        let input: Input = serde_json::from_str(json).unwrap();
        let out = render(
            &input,
            None,
            None,
            &[],
            0,
            RoundingMode::Floor,
            Layout::Comfortable,
        );
        assert!(out.contains(fmt::ORANGE));

        // ≥ $20 → red
        let json = r#"{"cost": {"total_cost_usd": 42.0}}"#;
        let input: Input = serde_json::from_str(json).unwrap();
        let out = render(
            &input,
            None,
            None,
            &[],
            0,
            RoundingMode::Floor,
            Layout::Comfortable,
        );
        assert!(out.contains(fmt::RED));
    }

    #[test]
    fn test_render_api_billing_fixture_comfortable() {
        let input: Input = serde_json::from_str(crate::input::API_BILLING_FIXTURE).unwrap();
        let out = render(
            &input,
            None,
            None,
            &[],
            0,
            RoundingMode::Floor,
            Layout::Comfortable,
        );
        let plain = strip_ansi(&out);
        assert!(plain.contains("Opus 4.7"), "model should render");
        assert!(plain.contains("3%"), "context % should render");
        assert!(plain.contains("$0.10"), "cost should render");
        assert!(
            !plain.contains("current"),
            "no rate-limit row on API billing"
        );
        assert!(
            !plain.contains("weekly"),
            "no weekly rate-limit row on API billing"
        );
    }

    #[test]
    fn test_render_api_billing_fixture_condensed() {
        let input: Input = serde_json::from_str(crate::input::API_BILLING_FIXTURE).unwrap();
        let out = render(&input, None, None, &[], 0, RoundingMode::Floor, Layout::Condensed);
        let plain = strip_ansi(&out);
        assert!(plain.contains("Opus 4.7"));
        assert!(plain.contains("$0.10"));
        assert!(!plain.contains("5h"), "no 5h inline on API billing");
        assert!(!plain.contains("7d"), "no 7d inline on API billing");
        assert!(!out.contains('\n'), "single-line output");
    }

    #[test]
    fn test_render_cost_condensed_single_line() {
        let json = r#"{"cost": {"total_cost_usd": 0.42}}"#;
        let input: Input = serde_json::from_str(json).unwrap();
        let out = render(&input, None, None, &[], 0, RoundingMode::Floor, Layout::Condensed);
        assert!(
            !out.contains('\n'),
            "condensed layout must stay single-line"
        );
        let plain = strip_ansi(&out);
        assert!(plain.contains("$0.42"));
    }

    #[test]
    fn test_render_cost_hidden_when_rate_limits_present() {
        // Presence of rate_limits → plan billing → cost is an estimate, not
        // actual spend, so we hide it. Cost is otherwise non-zero here.
        let json = r#"{
            "cost": {"total_cost_usd": 3.14},
            "rate_limits": {
                "five_hour": {"used_percentage": 9.0, "resets_at": 1705316400}
            }
        }"#;
        let input: Input = serde_json::from_str(json).unwrap();

        let comfortable = strip_ansi(&render(
            &input,
            None,
            None,
            &[],
            0,
            RoundingMode::Floor,
            Layout::Comfortable,
        ));
        assert!(
            !comfortable.contains("$3.14"),
            "cost must not render on plan billing (comfortable)"
        );
        assert!(!comfortable.contains("💰"));
        // Plan-side fields still render.
        assert!(comfortable.contains("current"));

        let condensed = strip_ansi(&render(
            &input,
            None,
            None,
            &[],
            0,
            RoundingMode::Floor,
            Layout::Condensed,
        ));
        assert!(
            !condensed.contains("$3.14"),
            "cost must not render on plan billing (condensed)"
        );
        assert!(!condensed.contains("💰"));
        assert!(condensed.contains("5h"));
    }

    // ── Ahead/behind and op-state tests ──────────────────────────────────────

    fn make_extra(
        ahead: u32,
        behind: u32,
        op_state: OpState,
        op_step: u8,
        op_total: u8,
        conflict_count: u8,
    ) -> GitExtra {
        GitExtra { ahead, behind, op_state, op_step, op_total, conflict_count }
    }

    #[test]
    fn test_render_ahead_only() {
        let input = Input::default();
        let extra = make_extra(3, 0, OpState::None, 0, 0, 0);
        let plain = strip_ansi(&render(
            &input,
            Some(("main".to_string(), false)),
            Some(&extra),
            &[],
            0,
            RoundingMode::Floor,
            Layout::Comfortable,
        ));
        assert!(plain.contains("↑3"), "ahead arrow should appear");
        assert!(!plain.contains("↓"), "no behind arrow when behind=0");
    }

    #[test]
    fn test_render_behind_only() {
        let input = Input::default();
        let extra = make_extra(0, 2, OpState::None, 0, 0, 0);
        let plain = strip_ansi(&render(
            &input,
            Some(("main".to_string(), false)),
            Some(&extra),
            &[],
            0,
            RoundingMode::Floor,
            Layout::Comfortable,
        ));
        assert!(plain.contains("↓2"), "behind arrow should appear");
        assert!(!plain.contains("↑"), "no ahead arrow when ahead=0");
    }

    #[test]
    fn test_render_ahead_and_behind() {
        let input = Input::default();
        let extra = make_extra(3, 1, OpState::None, 0, 0, 0);
        let plain = strip_ansi(&render(
            &input,
            Some(("main".to_string(), false)),
            Some(&extra),
            &[],
            0,
            RoundingMode::Floor,
            Layout::Comfortable,
        ));
        assert!(plain.contains("↑3"), "ahead");
        assert!(plain.contains("↓1"), "behind");
    }

    #[test]
    fn test_render_zero_ahead_behind_hidden() {
        let input = Input::default();
        let extra = make_extra(0, 0, OpState::None, 0, 0, 0);
        let plain = strip_ansi(&render(
            &input,
            Some(("main".to_string(), false)),
            Some(&extra),
            &[],
            0,
            RoundingMode::Floor,
            Layout::Comfortable,
        ));
        assert!(!plain.contains('↑'), "no ahead when zero");
        assert!(!plain.contains('↓'), "no behind when zero");
    }

    #[test]
    fn test_render_merge_state() {
        let input = Input::default();
        let extra = make_extra(0, 0, OpState::Merge, 0, 0, 0);
        let plain = strip_ansi(&render(
            &input,
            Some(("main".to_string(), false)),
            Some(&extra),
            &[],
            0,
            RoundingMode::Floor,
            Layout::Comfortable,
        ));
        assert!(plain.contains("MERGING"), "MERGING badge");
    }

    #[test]
    fn test_render_rebase_state_with_progress() {
        let input = Input::default();
        let extra = make_extra(0, 0, OpState::Rebase, 2, 5, 0);
        let plain = strip_ansi(&render(
            &input,
            Some(("main".to_string(), false)),
            Some(&extra),
            &[],
            0,
            RoundingMode::Floor,
            Layout::Comfortable,
        ));
        assert!(plain.contains("REBASE 2/5"), "rebase badge with progress");
    }

    #[test]
    fn test_render_cherry_pick_no_conflicts() {
        let input = Input::default();
        let extra = make_extra(0, 0, OpState::CherryPick, 0, 0, 0);
        let plain = strip_ansi(&render(
            &input,
            Some(("main".to_string(), false)),
            Some(&extra),
            &[],
            0,
            RoundingMode::Floor,
            Layout::Comfortable,
        ));
        assert!(plain.contains("CHERRY-PICK"), "cherry-pick badge");
        assert!(!plain.contains("conflict"), "no conflict text when count=0");
    }

    #[test]
    fn test_render_merge_with_conflicts() {
        let input = Input::default();
        let extra = make_extra(0, 0, OpState::Merge, 0, 0, 3);
        let plain = strip_ansi(&render(
            &input,
            Some(("main".to_string(), false)),
            Some(&extra),
            &[],
            0,
            RoundingMode::Floor,
            Layout::Comfortable,
        ));
        assert!(plain.contains("MERGING"));
        assert!(plain.contains("3 conflicts"), "conflict count shown");
    }

    #[test]
    fn test_render_revert_state() {
        let input = Input::default();
        let extra = make_extra(0, 0, OpState::Revert, 0, 0, 0);
        let plain = strip_ansi(&render(
            &input,
            Some(("main".to_string(), false)),
            Some(&extra),
            &[],
            0,
            RoundingMode::Floor,
            Layout::Comfortable,
        ));
        assert!(plain.contains("REVERTING"), "revert badge");
    }

    #[test]
    fn test_render_bisect_state() {
        let input = Input::default();
        let extra = make_extra(0, 0, OpState::Bisect, 0, 0, 0);
        let plain = strip_ansi(&render(
            &input,
            Some(("main".to_string(), false)),
            Some(&extra),
            &[],
            0,
            RoundingMode::Floor,
            Layout::Comfortable,
        ));
        assert!(plain.contains("BISECTING"), "bisect badge");
    }

    #[test]
    fn test_render_ahead_behind_condensed() {
        let input = Input::default();
        let extra = make_extra(3, 1, OpState::None, 0, 0, 0);
        let plain = strip_ansi(&render(
            &input,
            Some(("main".to_string(), false)),
            Some(&extra),
            &[],
            0,
            RoundingMode::Floor,
            Layout::Condensed,
        ));
        assert!(plain.contains("↑3"), "ahead condensed");
        assert!(plain.contains("↓1"), "behind condensed");
    }

    #[test]
    fn test_render_rebase_condensed() {
        let input = Input::default();
        let extra = make_extra(0, 0, OpState::Rebase, 2, 5, 0);
        let plain = strip_ansi(&render(
            &input,
            Some(("main".to_string(), false)),
            Some(&extra),
            &[],
            0,
            RoundingMode::Floor,
            Layout::Condensed,
        ));
        assert!(plain.contains("R 2/5"), "condensed rebase badge");
    }

    #[test]
    fn test_render_merge_condensed_with_conflicts() {
        let input = Input::default();
        let extra = make_extra(0, 0, OpState::Merge, 0, 0, 3);
        let plain = strip_ansi(&render(
            &input,
            Some(("main".to_string(), false)),
            Some(&extra),
            &[],
            0,
            RoundingMode::Floor,
            Layout::Condensed,
        ));
        assert!(plain.contains('M'), "condensed merge badge");
        assert!(plain.contains("!3"), "condensed conflict count");
    }

    #[test]
    fn test_render_cherry_pick_condensed() {
        let input = Input::default();
        let extra = make_extra(0, 0, OpState::CherryPick, 0, 0, 0);
        let plain = strip_ansi(&render(
            &input,
            Some(("main".to_string(), false)),
            Some(&extra),
            &[],
            0,
            RoundingMode::Floor,
            Layout::Condensed,
        ));
        assert!(plain.contains("CP"), "condensed cherry-pick badge");
    }

    #[test]
    fn test_render_none_extra_no_arrows() {
        let input = Input::default();
        let plain = strip_ansi(&render(
            &input,
            Some(("main".to_string(), false)),
            None,
            &[],
            0,
            RoundingMode::Floor,
            Layout::Comfortable,
        ));
        assert!(!plain.contains('↑'));
        assert!(!plain.contains('↓'));
        assert!(!plain.contains("MERGING"));
        assert!(!plain.contains("REBASE"));
    }
}
