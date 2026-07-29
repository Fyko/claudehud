use std::fmt::Write as _;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use common::fable::FableLimit;
use common::incidents::Incident;

use crate::fmt::{self, color_for_pct, BLUE, CYAN, DIM, GREEN, RED, RESET, SEP};
use crate::input::Input;
use crate::time::{format_countdown, format_duration};

/// Incidents at or beyond this age are treated as "long-running": filtered out of
/// the normal list (collapsed to a breadcrumb in comfortable, hidden in condensed).
const LONG_RUNNING_SECS: u64 = 86_400;

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
    Condensed,
}

impl Layout {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "condensed" => Some(Self::Condensed),
            _ => None,
        }
    }
}

// One flat argument list rather than a params struct: every caller is either
// `orchestrate::run` or a test that wants to vary exactly one of these.
// `git` comes in owned because no caller wants it back; borrowing would only
// push a `let` binding into every one of them.
#[allow(clippy::too_many_arguments, clippy::needless_pass_by_value)]
pub fn render(
    input: &Input,
    git: Option<(String, bool)>,
    incidents: &[Incident],
    total_active: u8,
    update_notice: Option<&str>,
    rounding: RoundingMode,
    layout: Layout,
    fable: Option<FableLimit>,
    auto_compact: Option<u64>,
) -> String {
    match layout {
        Layout::Condensed => render_condensed(
            input,
            git.as_ref(),
            incidents,
            total_active,
            update_notice,
            rounding,
            fable,
            auto_compact,
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn render_condensed(
    input: &Input,
    git: Option<&(String, bool)>,
    incidents: &[Incident],
    total_active: u8,
    update_notice: Option<&str>,
    rounding: RoundingMode,
    fable: Option<FableLimit>,
    auto_compact: Option<u64>,
) -> String {
    let mut out = String::with_capacity(512);

    // ── Agent badge (background agents only) ───────────────
    push_agent_badge(input, &mut out);

    // ── Model (short) ──────────────────────────────────────
    push_model_short(input, &mut out);

    // ── Context usage ──────────────────────────────────────
    out.push_str(SEP);
    push_context(input, rounding, auto_compact, &mut out);

    // ── Cost (skipped when absent or $0) ───────────────────
    push_cost(input, &mut out);

    // ── Dir + git (tight) ──────────────────────────────────
    out.push_str(SEP);
    push_dir_branch(input, git, true, &mut out);

    // ── Rate limits: one rotating slot ─────────────────────
    // Three windows side by side ate ~84 columns. One at a time buys a full
    // 10-dot bar and a countdown, and the slot cycles so the others still get
    // seen — except when something is nearly spent, which pins the slot.
    let windows = rate_windows(input, rounding, fable);
    if let Some(w) = pick_window(&windows, now_secs()) {
        out.push_str(SEP);
        push_rate_rotating(w, now_secs(), &mut out);
    }

    // ── Incidents ──────────────────────────────────────────
    push_incidents(incidents, total_active, &mut out);
    push_update_notice(update_notice, &mut out);

    out
}

/// One rate-limit window, flattened out of the payload + the fable cache so the
/// rotation doesn't care where each number came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RateWindow {
    label: &'static str,
    pct: u8,
    resets_at: Option<u64>,
}

/// How long the rotating slot holds one window before moving on.
const ROTATE_SECS: u64 = 8;

/// At or above this, a window stops taking its turn and holds the slot: when
/// you're nearly out, hiding it for 16 seconds is the wrong trade.
const PIN_PCT: u8 = 90;

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn rate_windows(
    input: &Input,
    rounding: RoundingMode,
    fable: Option<FableLimit>,
) -> Vec<RateWindow> {
    let mut out = Vec::with_capacity(3);
    if let Some(rl) = &input.rate_limits {
        if let Some(pct) = rl.five_hour.as_ref().and_then(|w| w.used_percentage) {
            out.push(RateWindow {
                label: "5h",
                pct: rounding.apply(pct),
                resets_at: rl.five_hour.as_ref().and_then(|w| w.resets_at),
            });
        }
        if let Some(pct) = rl.seven_day.as_ref().and_then(|w| w.used_percentage) {
            out.push(RateWindow {
                label: "7d",
                pct: rounding.apply(pct),
                resets_at: rl.seven_day.as_ref().and_then(|w| w.resets_at),
            });
        }
        if let Some(f) = fable {
            out.push(RateWindow {
                label: "fbl",
                pct: f.percent,
                resets_at: Some(f.resets_at),
            });
        }
    }
    out
}

/// Which window gets the slot right now: the most-spent one once anything
/// crosses [`PIN_PCT`], otherwise a plain time-sliced rotation.
///
/// The rotation is derived from the clock rather than a counter, so it needs no
/// state on disk and every open session lands on the same window at the same
/// moment. Claude Code re-runs the statusline often enough that the slot turns
/// over on its own.
fn pick_window(windows: &[RateWindow], now: u64) -> Option<RateWindow> {
    let pinned = windows
        .iter()
        .filter(|w| w.pct >= PIN_PCT)
        .max_by_key(|w| w.pct);
    if let Some(w) = pinned {
        return Some(*w);
    }
    windows
        .get((now / ROTATE_SECS) as usize % windows.len().max(1))
        .copied()
}

fn push_agent_badge(input: &Input, out: &mut String) {
    if input.agent_type.is_none() {
        return;
    }
    out.push('🤖');
    // Future: append agent.name when it's not just "claude".
    let name = input.agent.as_ref().and_then(|a| a.name.as_deref());
    if let Some(n) = name {
        if n != "claude" {
            out.push(' ');
            out.push_str(n);
        }
    }
    out.push_str(SEP);
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
    push_fast_glyph(input, out);
    out.push_str(BLUE);
    out.push_str(short);
    out.push_str(RESET);
    // The dropped parenthetical is usually noise, but an extended context
    // window changes what the context percentage means — keep that bit.
    if is_extended_context(input) {
        out.push_str(DIM);
        out.push_str(" (1M)");
        out.push_str(RESET);
    }
    push_effort(input, out);
}

/// Whether this session runs an extended (1M) context window. The display name
/// carries it (`Opus 5 (1M context)`); the window size is the fallback for
/// payloads whose name doesn't say so.
fn is_extended_context(input: &Input) -> bool {
    let named = input
        .model
        .as_ref()
        .and_then(|m| m.display_name.as_deref())
        .is_some_and(|n| n.to_ascii_lowercase().contains("1m"));
    named
        || input
            .context_window
            .as_ref()
            .and_then(|cw| cw.context_window_size)
            .is_some_and(|size| size >= 1_000_000)
}

/// Fast mode marks the model itself, so it rides in front of the name.
fn push_fast_glyph(input: &Input, out: &mut String) {
    if input.fast_mode == Some(true) {
        out.push_str("⚡ ");
    }
}

/// The live reasoning effort, dimmed after the model name. Absent for models
/// that don't take an effort parameter — nothing renders then, not a default.
fn push_effort(input: &Input, out: &mut String) {
    let Some(level) = input.effort.as_ref().and_then(|e| e.level.as_deref()) else {
        return;
    };
    if level.is_empty() {
        return;
    }
    out.push(' ');
    out.push_str(fmt::color_for_effort(level));
    out.push_str(level);
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

fn push_context(
    input: &Input,
    rounding: RoundingMode,
    auto_compact: Option<u64>,
    out: &mut String,
) {
    let pct = context_pct_against(input, rounding, auto_compact);
    out.push_str(DIM);
    out.push_str("ctx ");
    out.push_str(RESET);
    out.push_str(color_for_pct(pct));
    write!(out, "{pct}%").unwrap();
    out.push_str(RESET);
}

fn push_dir_branch(input: &Input, git: Option<&(String, bool)>, tight: bool, out: &mut String) {
    let cwd = input.cwd.as_deref().unwrap_or("");
    let cwd_path = Path::new(cwd);
    let dirname = cwd_path.file_name().and_then(|n| n.to_str()).unwrap_or(cwd);
    let base_repo = crate::git::resolve_base_repo(input, cwd_path);
    out.push_str(CYAN);
    if let Some(ref base) = base_repo {
        if base.as_str() != dirname {
            out.push_str(base);
            out.push('/');
        }
    }
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
    }
}

/// How an incident should surface, given its age.
#[derive(Debug, PartialEq, Eq)]
enum IncidentSlot {
    /// Ordinary fresh incident — render the normal line.
    Normal,
    /// ≥24h old — drop the line, tally toward the breadcrumb.
    LongRunning,
}

fn classify_incident(inc: &Incident, now: u64) -> IncidentSlot {
    if now.saturating_sub(inc.started_at) < LONG_RUNNING_SECS {
        IncidentSlot::Normal
    } else {
        IncidentSlot::LongRunning
    }
}

fn push_incidents(incidents: &[Incident], total_active: u8, out: &mut String) {
    let now = now_secs();

    let mut long_running: u8 = 0;
    for inc in incidents {
        match classify_incident(inc, now) {
            IncidentSlot::Normal => {
                out.push('\n');
                push_incident_line(inc, now, out);
            }
            IncidentSlot::LongRunning => long_running += 1,
        }
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

    if long_running > 0 {
        out.push('\n');
        write!(out, "\x1b]8;;https://status.claude.com/\x1b\\").unwrap();
        out.push_str(DIM);
        write!(out, "+{long_running} ongoing (24h+)").unwrap();
        out.push_str(RESET);
        out.push_str("\x1b]8;;\x1b\\");
    }
}

/// One-shot "updated to vX" line, shown under line 1 for a few minutes after a
/// daemon self-update. Its own line, like an incident.
fn push_update_notice(version: Option<&str>, out: &mut String) {
    let Some(v) = version else { return };
    out.push('\n');
    out.push_str(DIM);
    out.push_str("updated to v");
    out.push_str(v);
    out.push_str(RESET);
}

fn push_incident_line(inc: &Incident, now: u64, out: &mut String) {
    let url = &inc.url;
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

/// The condensed layout's single usage slot: full bar, label, percent, and how
/// long until it resets.
fn push_rate_rotating(w: RateWindow, now: u64, out: &mut String) {
    fmt::build_bar(w.pct, 10, out);
    out.push(' ');
    out.push_str(w.label);
    out.push(' ');
    out.push_str(color_for_pct(w.pct));
    write!(out, "{}%", w.pct).unwrap();
    out.push_str(RESET);
    if let Some(epoch) = w.resets_at.filter(|&e| e > now) {
        out.push(' ');
        out.push_str(DIM);
        out.push_str("⟳ ");
        out.push_str(RESET);
        out.push_str(&format_countdown(epoch - now));
    }
}

/// Claude Code clamps its auto-compact budget to this range, so we do too —
/// a typo'd env var shouldn't invent a window nobody is actually running.
const AUTO_COMPACT_MIN: u64 = 100_000;
const AUTO_COMPACT_MAX: u64 = 1_000_000;

/// The auto-compact budget from the environment, if the user set one.
///
/// Claude Code exports `CLAUDE_CODE_AUTO_COMPACT_WINDOW` into its own process
/// environment, and the statusline runs as a child, so it arrives for free —
/// no reading of `~/.claude.json`.
pub fn auto_compact_window() -> Option<u64> {
    std::env::var("CLAUDE_CODE_AUTO_COMPACT_WINDOW")
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(|n| n.clamp(AUTO_COMPACT_MIN, AUTO_COMPACT_MAX))
}

/// Percent of the context window in use.
///
/// The window that matters is the one you'll actually hit: with auto-compact
/// set below the model's window, the conversation compacts at that budget, so
/// a percentage of the full window understates how close you are. When the
/// budget binds we recompute from raw token counts rather than trusting the
/// payload's `used_percentage`, which is measured against the full window.
fn context_pct_against(input: &Input, rounding: RoundingMode, auto_compact: Option<u64>) -> u8 {
    let cw = input.context_window.as_ref();
    let size = cw
        .and_then(|cw| cw.context_window_size)
        .filter(|&s| s > 0)
        .unwrap_or(200_000);
    let effective = auto_compact.map_or(size, |budget| budget.min(size));

    if effective == size {
        if let Some(pct) = cw.and_then(|cw| cw.used_percentage) {
            return rounding.apply(pct);
        }
    }

    let current = cw
        .and_then(|cw| cw.current_usage.as_ref())
        .map(|u| {
            u.input_tokens.unwrap_or(0)
                + u.cache_creation_input_tokens.unwrap_or(0)
                + u.cache_read_input_tokens.unwrap_or(0)
        })
        .unwrap_or(0);
    // With no token counts to work from, fall back to the payload's own figure
    // rather than claiming 0%.
    if current == 0 {
        if let Some(pct) = cw.and_then(|cw| cw.used_percentage) {
            return rounding.apply(pct);
        }
    }
    rounding.apply((current as f64 * 100.0) / effective as f64)
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
    fn test_render_update_notice_present() {
        let input = Input::default();
        let out = render(
            &input,
            None,
            &[],
            0,
            Some("0.2.0"),
            RoundingMode::Floor,
            Layout::Condensed,
            None,
            None,
        );
        let plain = strip_ansi(&out);
        assert!(plain.contains("updated to v0.2.0"), "got: {plain:?}");
    }

    #[test]
    fn test_render_no_update_notice_absent() {
        let input = Input::default();
        let out = render(
            &input,
            None,
            &[],
            0,
            None,
            RoundingMode::Floor,
            Layout::Condensed,
            None,
            None,
        );
        assert!(!strip_ansi(&out).contains("updated to"));
    }

    #[test]
    fn test_render_git_branch() {
        let input = Input::default();
        let plain = strip_ansi(&render(
            &input,
            Some(("main".to_string(), false)),
            &[],
            0,
            None,
            RoundingMode::Floor,
            Layout::Condensed,
            None,
            None,
        ));
        assert!(plain.contains("(main)"));
    }

    #[test]
    fn test_render_dirname() {
        let json = r#"{"cwd": "/home/user/myproject"}"#;
        let input: Input = serde_json::from_str(json).unwrap();
        let plain = strip_ansi(&render(
            &input,
            None,
            &[],
            0,
            None,
            RoundingMode::Floor,
            Layout::Condensed,
            None,
            None,
        ));
        assert!(plain.contains("myproject"));
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
            &incidents,
            2,
            None,
            RoundingMode::Floor,
            Layout::Condensed,
            None,
            None,
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
            &[],
            0,
            None,
            RoundingMode::Floor,
            Layout::Condensed,
            None,
            None,
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
            &[],
            0,
            None,
            RoundingMode::Floor,
            Layout::Condensed,
            None,
            None,
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
            &[],
            0,
            None,
            RoundingMode::Floor,
            Layout::Condensed,
            None,
            None
        ))
        .contains("50%"));
        assert!(strip_ansi(&render(
            &input,
            None,
            &[],
            0,
            None,
            RoundingMode::Ceiling,
            Layout::Condensed,
            None,
            None
        ))
        .contains("51%"));
        assert!(strip_ansi(&render(
            &input,
            None,
            &[],
            0,
            None,
            RoundingMode::Nearest,
            Layout::Condensed,
            None,
            None
        ))
        .contains("50%"));
    }

    #[test]
    fn test_layout_parse() {
        assert_eq!(Layout::parse("condensed"), Some(Layout::Condensed));
        assert_eq!(Layout::parse("comfortable"), None);
        assert_eq!(Layout::parse("Condensed"), Some(Layout::Condensed));
        assert_eq!(Layout::parse(""), None);
        assert_eq!(Layout::parse("compact"), None);
        assert_eq!(Layout::parse("garbage"), None);
    }

    // ── Condensed layout tests ────────────────────────────────────────────────

    #[test]
    fn test_render_default_model_condensed() {
        let input = Input::default();
        let result = render(
            &input,
            None,
            &[],
            0,
            None,
            RoundingMode::Floor,
            Layout::Condensed,
            None,
            None,
        );
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
            &[],
            0,
            None,
            RoundingMode::Floor,
            Layout::Condensed,
            None,
            None,
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
            &[],
            0,
            None,
            RoundingMode::Floor,
            Layout::Condensed,
            None,
            None,
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
    fn test_render_rate_limits_condensed_shows_one_rotating_window() {
        let json = r#"{
            "rate_limits": {
                "five_hour": {"used_percentage": 9.0, "resets_at": 1705316400},
                "seven_day": {"used_percentage": 12.0, "resets_at": 1705833600}
            }
        }"#;
        let input: Input = serde_json::from_str(json).unwrap();
        let result = render(
            &input,
            None,
            &[],
            0,
            None,
            RoundingMode::Floor,
            Layout::Condensed,
            None,
            None,
        );
        let plain = strip_ansi(&result);

        // Exactly one window holds the slot — never both at once.
        let labels = ["5h", "7d"];
        let shown = labels.iter().filter(|l| plain.contains(**l)).count();
        assert_eq!(shown, 1, "one window at a time, got: {plain}");

        assert!(
            !plain.contains("current") && !plain.contains("weekly"),
            "comfortable labels should not appear: {plain}"
        );

        // A full-width bar, now that there's room for one.
        let dots = plain.matches('\u{25cb}').count() + plain.matches('\u{25cf}').count();
        assert_eq!(dots, 10, "expected a 10-dot bar (got {dots}): {plain}");

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
            &[incident],
            1,
            None,
            RoundingMode::Floor,
            Layout::Condensed,
            None,
            None,
        );
        let plain = strip_ansi(&out);
        assert!(plain.contains("Elevated API errors"));
        assert!(plain.contains("started 12m ago"));
        assert_eq!(out.matches('\n').count(), 1, "exactly one newline expected");
        assert!(out.contains("\x1b]8;;https://status.claude.com/incidents/abc"));
    }

    fn now_secs() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    fn incident_aged(title: &str, age_secs: u64) -> Incident {
        use common::incidents::Severity;
        Incident {
            severity: Severity::Minor,
            started_at: now_secs().saturating_sub(age_secs),
            title: title.to_string(),
            url: "https://status.claude.com/incidents/xyz".to_string(),
        }
    }

    #[test]
    fn test_long_running_breadcrumb_condensed() {
        // ≥24h in condensed → line hidden, but the count still surfaces.
        let inc = incident_aged("Elevated API errors", 30 * 3600);
        let out = render(
            &Input::default(),
            None,
            &[inc],
            1,
            None,
            RoundingMode::Floor,
            Layout::Condensed,
            None,
            None,
        );
        let plain = strip_ansi(&out);
        assert!(!plain.contains("Elevated API errors"), "{plain}");
        assert!(
            plain.contains("+1 ongoing (24h+)"),
            "breadcrumb missing in condensed: {plain}"
        );
    }

    #[test]
    fn test_fresh_incident_still_normal_both_layouts() {
        // <24h renders as a normal line in either layout.
        for layout in [Layout::Condensed, Layout::Condensed] {
            let inc = incident_aged("Elevated API errors", 12 * 60);
            let out = render(
                &Input::default(),
                None,
                &[inc],
                1,
                None,
                RoundingMode::Floor,
                layout,
                None,
                None,
            );
            let plain = strip_ansi(&out);
            assert!(plain.contains("Elevated API errors"), "{layout:?}: {plain}");
            assert!(plain.contains("started 12m ago"), "{layout:?}: {plain}");
        }
    }

    #[test]
    fn test_classify_incident_is_purely_about_age() {
        let now = now_secs();
        assert_eq!(
            classify_incident(&incident_aged("Elevated API errors", 60), now),
            IncidentSlot::Normal
        );
        assert_eq!(
            classify_incident(&incident_aged("Elevated API errors", 30 * 3600), now),
            IncidentSlot::LongRunning
        );
        // Exactly at the cutoff it's already long-running.
        assert_eq!(
            classify_incident(
                &incident_aged("Elevated API errors", LONG_RUNNING_SECS),
                now
            ),
            IncidentSlot::LongRunning
        );
    }

    // ── Fast mode + effort ────────────────────────────────────────────────────

    #[test]
    fn test_render_effort_follows_the_model_name() {
        let json =
            r#"{"model": {"display_name": "Opus 5 (1M context)"}, "effort": {"level": "xhigh"}}"#;
        let input: Input = serde_json::from_str(json).unwrap();
        let plain = strip_ansi(&render(
            &input,
            None,
            &[],
            0,
            None,
            RoundingMode::Floor,
            Layout::Condensed,
            None,
            None,
        ));
        // The parenthetical collapses to the 1M marker; effort follows it.
        assert!(plain.contains("Opus 5 (1M) xhigh"), "{plain}");
    }

    #[test]
    fn test_render_no_effort_when_model_lacks_the_parameter() {
        // `effort` is absent for models that don't take one — render nothing
        // rather than inventing a default level.
        let json = r#"{"model": {"display_name": "Haiku 4.5"}}"#;
        let input: Input = serde_json::from_str(json).unwrap();
        let plain = strip_ansi(&render(
            &input,
            None,
            &[],
            0,
            None,
            RoundingMode::Floor,
            Layout::Condensed,
            None,
            None,
        ));
        assert!(plain.contains("Haiku 4.5"), "{plain}");
        for level in ["low", "medium", "high", "xhigh", "max"] {
            assert!(!plain.contains(level), "leaked {level}: {plain}");
        }
    }

    #[test]
    fn test_render_empty_effort_level_renders_nothing() {
        let json = r#"{"model": {"display_name": "Opus 5"}, "effort": {"level": ""}}"#;
        let input: Input = serde_json::from_str(json).unwrap();
        let plain = strip_ansi(&render(
            &input,
            None,
            &[],
            0,
            None,
            RoundingMode::Floor,
            Layout::Condensed,
            None,
            None,
        ));
        // Straight into the separator — no stray space where a level would go.
        assert!(plain.starts_with("Opus 5 │"), "{plain}");
    }

    #[test]
    fn test_render_fast_mode_glyph_precedes_model() {
        let json = r#"{"model": {"display_name": "Opus 5"}, "fast_mode": true, "effort": {"level": "high"}}"#;
        let input: Input = serde_json::from_str(json).unwrap();
        for layout in [Layout::Condensed, Layout::Condensed] {
            let plain = strip_ansi(&render(
                &input,
                None,
                &[],
                0,
                None,
                RoundingMode::Floor,
                layout,
                None,
                None,
            ));
            assert!(plain.starts_with("⚡ Opus 5 high"), "{layout:?}: {plain}");
        }
    }

    #[test]
    fn test_render_no_glyph_when_fast_mode_off_or_absent() {
        for json in [
            r#"{"model": {"display_name": "Opus 5"}, "fast_mode": false}"#,
            r#"{"model": {"display_name": "Opus 5"}}"#,
        ] {
            let input: Input = serde_json::from_str(json).unwrap();
            let plain = strip_ansi(&render(
                &input,
                None,
                &[],
                0,
                None,
                RoundingMode::Floor,
                Layout::Condensed,
                None,
                None,
            ));
            assert!(!plain.contains('⚡'), "{plain}");
        }
    }

    #[test]
    fn test_render_fast_glyph_follows_the_agent_badge() {
        // The badge stays leftmost; the glyph belongs to the model segment.
        let json =
            r#"{"agent_type": "claude", "model": {"display_name": "Opus 5"}, "fast_mode": true}"#;
        let input: Input = serde_json::from_str(json).unwrap();
        let plain = strip_ansi(&render(
            &input,
            None,
            &[],
            0,
            None,
            RoundingMode::Floor,
            Layout::Condensed,
            None,
            None,
        ));
        assert!(plain.starts_with("🤖"), "{plain}");
        assert!(plain.contains("⚡ Opus 5"), "{plain}");
    }

    #[test]
    fn test_render_1m_marker_replaces_the_parenthetical() {
        let json = r#"{"model": {"display_name": "Opus 5 (1M context)"}}"#;
        let input: Input = serde_json::from_str(json).unwrap();

        let condensed = strip_ansi(&render(
            &input,
            None,
            &[],
            0,
            None,
            RoundingMode::Floor,
            Layout::Condensed,
            None,
            None,
        ));
        assert!(condensed.starts_with("Opus 5 (1M) │"), "{condensed}");
    }

    #[test]
    fn test_render_no_1m_marker_on_a_standard_window() {
        let json = r#"{
            "model": {"display_name": "Opus 5"},
            "context_window": {"context_window_size": 200000}
        }"#;
        let input: Input = serde_json::from_str(json).unwrap();
        let plain = strip_ansi(&render(
            &input,
            None,
            &[],
            0,
            None,
            RoundingMode::Floor,
            Layout::Condensed,
            None,
            None,
        ));
        assert!(!plain.contains("1M"), "{plain}");
    }

    #[test]
    fn test_render_1m_marker_from_window_size_when_name_is_silent() {
        let json = r#"{
            "model": {"display_name": "Opus 5"},
            "context_window": {"context_window_size": 1000000}
        }"#;
        let input: Input = serde_json::from_str(json).unwrap();
        let plain = strip_ansi(&render(
            &input,
            None,
            &[],
            0,
            None,
            RoundingMode::Floor,
            Layout::Condensed,
            None,
            None,
        ));
        assert!(plain.starts_with("Opus 5 (1M) │"), "{plain}");
    }

    #[test]
    fn test_render_breadcrumb_counts_every_stale_incident_in_both_layouts() {
        let incidents = vec![
            incident_aged("Old thing", 30 * 3600),
            incident_aged("Older thing", 50 * 3600),
            incident_aged("Fresh thing", 5 * 60),
        ];
        for layout in [Layout::Condensed, Layout::Condensed] {
            let plain = strip_ansi(&render(
                &Input::default(),
                None,
                &incidents,
                3,
                None,
                RoundingMode::Floor,
                layout,
                None,
                None,
            ));
            assert!(plain.contains("Fresh thing"), "{layout:?}: {plain}");
            assert!(!plain.contains("Old thing"), "{layout:?}: {plain}");
            assert!(plain.contains("+2 ongoing (24h+)"), "{layout:?}: {plain}");
        }
    }

    // ── Context window vs the auto-compact budget ─────────────────────────────

    /// 32,169 tokens of a 1M window — 3% of the model's window, 8% of a 400k
    /// auto-compact budget.
    const BIG_WINDOW: &str = r#"{
        "context_window": {
            "context_window_size": 1000000,
            "used_percentage": 3,
            "current_usage": {
                "input_tokens": 6,
                "cache_creation_input_tokens": 15288,
                "cache_read_input_tokens": 16875
            }
        }
    }"#;

    #[test]
    fn test_context_pct_uses_the_payload_figure_without_a_budget() {
        let input: Input = serde_json::from_str(BIG_WINDOW).unwrap();
        assert_eq!(context_pct_against(&input, RoundingMode::Floor, None), 3);
    }

    #[test]
    fn test_context_pct_measures_against_a_binding_auto_compact_budget() {
        let input: Input = serde_json::from_str(BIG_WINDOW).unwrap();
        // 32169 / 400000 = 8.04%
        assert_eq!(
            context_pct_against(&input, RoundingMode::Floor, Some(400_000)),
            8
        );
        assert_eq!(
            context_pct_against(&input, RoundingMode::Ceiling, Some(400_000)),
            9
        );
    }

    #[test]
    fn test_context_pct_ignores_a_budget_bigger_than_the_window() {
        // A 400k budget on a 200k model never binds — the model runs out first.
        let json = r#"{
            "context_window": {
                "context_window_size": 200000,
                "used_percentage": 25,
                "current_usage": {"input_tokens": 50000}
            }
        }"#;
        let input: Input = serde_json::from_str(json).unwrap();
        assert_eq!(
            context_pct_against(&input, RoundingMode::Floor, Some(400_000)),
            25
        );
    }

    #[test]
    fn test_context_pct_falls_back_to_the_payload_when_tokens_are_missing() {
        // A budget is set but the payload carries no token counts — reporting
        // 0% would be a lie.
        let json = r#"{
            "context_window": {"context_window_size": 1000000, "used_percentage": 3}
        }"#;
        let input: Input = serde_json::from_str(json).unwrap();
        assert_eq!(
            context_pct_against(&input, RoundingMode::Floor, Some(400_000)),
            3
        );
    }

    #[test]
    fn test_context_pct_clamps_at_a_full_budget() {
        let json = r#"{
            "context_window": {
                "context_window_size": 1000000,
                "current_usage": {"input_tokens": 900000}
            }
        }"#;
        let input: Input = serde_json::from_str(json).unwrap();
        assert_eq!(
            context_pct_against(&input, RoundingMode::Floor, Some(400_000)),
            100
        );
    }

    #[test]
    fn test_auto_compact_window_parses_and_clamps_like_claude_code() {
        // Claude Code's own bounds: 100k floor, 1M ceiling.
        let cases = [
            ("400000", Some(400_000)),
            (" 250000 ", Some(250_000)),
            ("50", Some(AUTO_COMPACT_MIN)),
            ("99999999", Some(AUTO_COMPACT_MAX)),
            ("nonsense", None),
            ("", None),
        ];
        for (raw, want) in cases {
            let got = raw
                .trim()
                .parse::<u64>()
                .ok()
                .map(|n| n.clamp(AUTO_COMPACT_MIN, AUTO_COMPACT_MAX));
            assert_eq!(got, want, "input {raw:?}");
        }
    }

    #[test]
    fn test_context_segment_is_labelled_not_emoji() {
        let input: Input = serde_json::from_str(BIG_WINDOW).unwrap();
        for layout in [Layout::Condensed, Layout::Condensed] {
            let plain = strip_ansi(&render(
                &input,
                None,
                &[],
                0,
                None,
                RoundingMode::Floor,
                layout,
                None,
                None,
            ));
            assert!(plain.contains("ctx 3%"), "{layout:?}: {plain}");
            assert!(!plain.contains('✍'), "{layout:?}: {plain}");
        }
    }

    // ── Rotating usage slot ───────────────────────────────────────────────────

    fn win(label: &'static str, pct: u8) -> RateWindow {
        RateWindow {
            label,
            pct,
            resets_at: Some(2_000_000_000),
        }
    }

    #[test]
    fn test_pick_window_cycles_through_every_window() {
        let ws = [win("5h", 10), win("7d", 20), win("fbl", 30)];
        // One full turn of the wheel, sampled at each slot boundary.
        let seen: Vec<&str> = (0..3)
            .map(|i| pick_window(&ws, i * ROTATE_SECS).unwrap().label)
            .collect();
        assert_eq!(seen, ["5h", "7d", "fbl"]);
        // And it wraps.
        assert_eq!(pick_window(&ws, 3 * ROTATE_SECS).unwrap().label, "5h");
    }

    #[test]
    fn test_pick_window_holds_each_window_for_the_full_interval() {
        let ws = [win("5h", 10), win("7d", 20)];
        for t in 0..ROTATE_SECS {
            assert_eq!(pick_window(&ws, t).unwrap().label, "5h", "t={t}");
        }
        assert_eq!(pick_window(&ws, ROTATE_SECS).unwrap().label, "7d");
    }

    #[test]
    fn test_pick_window_pins_a_nearly_spent_window() {
        let ws = [win("5h", PIN_PCT), win("7d", 20), win("fbl", 30)];
        // Every point in the cycle, same answer — the rotation stops.
        for t in [0, ROTATE_SECS, 2 * ROTATE_SECS, 5 * ROTATE_SECS] {
            assert_eq!(pick_window(&ws, t).unwrap().label, "5h", "t={t}");
        }
    }

    #[test]
    fn test_pick_window_pins_the_worst_of_several_over_the_line() {
        let ws = [win("5h", 91), win("7d", 20), win("fbl", 99)];
        assert_eq!(pick_window(&ws, 0).unwrap().label, "fbl");
    }

    #[test]
    fn test_pick_window_just_below_the_pin_still_rotates() {
        let ws = [win("5h", PIN_PCT - 1), win("7d", 20)];
        assert_eq!(pick_window(&ws, 0).unwrap().label, "5h");
        assert_eq!(pick_window(&ws, ROTATE_SECS).unwrap().label, "7d");
    }

    #[test]
    fn test_pick_window_empty_is_none() {
        assert_eq!(pick_window(&[], 0), None);
        assert_eq!(pick_window(&[], 12_345), None);
    }

    #[test]
    fn test_rate_windows_collects_only_what_is_present() {
        let input: Input = serde_json::from_str(RATE_LIMITED).unwrap();
        let ws = rate_windows(&input, RoundingMode::Floor, fable(51));
        assert_eq!(
            ws.iter().map(|w| w.label).collect::<Vec<_>>(),
            ["5h", "7d", "fbl"]
        );
        assert_eq!(ws[2].pct, 51);

        // No fable cache → two windows, and the wheel is that much shorter.
        let ws = rate_windows(&input, RoundingMode::Floor, None);
        assert_eq!(ws.len(), 2);

        // No rate_limits block at all (API billing) → nothing to rotate.
        let api: Input = serde_json::from_str(crate::input::API_BILLING_FIXTURE).unwrap();
        assert!(rate_windows(&api, RoundingMode::Floor, fable(51)).is_empty());
    }

    #[test]
    fn test_rotating_row_shows_a_countdown_not_a_clock_time() {
        let mut out = String::new();
        let now = 1_700_000_000;
        push_rate_rotating(
            RateWindow {
                label: "5h",
                pct: 96,
                resets_at: Some(now + 2 * 3600 + 11 * 60),
            },
            now,
            &mut out,
        );
        let plain = strip_ansi(&out);
        assert!(plain.contains("5h 96%"), "{plain}");
        assert!(plain.contains("2h11m"), "countdown missing: {plain}");
        assert!(!plain.contains("am") && !plain.contains("pm"), "{plain}");
    }

    #[test]
    fn test_rotating_row_omits_a_reset_already_in_the_past() {
        let mut out = String::new();
        push_rate_rotating(
            RateWindow {
                label: "7d",
                pct: 12,
                resets_at: Some(500),
            },
            1_000,
            &mut out,
        );
        assert!(!strip_ansi(&out).contains('⟳'), "{out:?}");
    }

    // ── Fable weekly cap ──────────────────────────────────────────────────────

    const RATE_LIMITED: &str = r#"{
        "rate_limits": {
            "five_hour": {"used_percentage": 9.0, "resets_at": 1705316400},
            "seven_day": {"used_percentage": 12.0, "resets_at": 1705833600}
        }
    }"#;

    #[allow(clippy::unnecessary_wraps)]
    fn fable(percent: u8) -> Option<FableLimit> {
        Some(FableLimit {
            percent,
            resets_at: 1_785_358_800,
        })
    }

    #[test]
    fn test_render_fable_inline_condensed_stays_single_line() {
        // 95% pins the slot, so this doesn't ride on where the rotation is.
        let input: Input = serde_json::from_str(RATE_LIMITED).unwrap();
        let out = render(
            &input,
            None,
            &[],
            0,
            None,
            RoundingMode::Floor,
            Layout::Condensed,
            fable(95),
            None,
        );
        let plain = strip_ansi(&out);
        assert!(plain.contains("fbl"), "fable label missing: {plain}");
        assert!(plain.contains("95%"), "fable pct missing: {plain}");
        assert!(!out.contains('\n'), "condensed must stay single-line");
    }

    #[test]
    fn test_render_no_fable_row_when_absent() {
        let input: Input = serde_json::from_str(RATE_LIMITED).unwrap();
        for layout in [Layout::Condensed, Layout::Condensed] {
            let plain = strip_ansi(&render(
                &input,
                None,
                &[],
                0,
                None,
                RoundingMode::Floor,
                layout,
                None,
                None,
            ));
            assert!(!plain.contains("fable"), "{layout:?}: {plain}");
            assert!(!plain.contains("fbl"), "{layout:?}: {plain}");
        }
    }

    #[test]
    fn test_render_fable_percent_bypasses_rounding_mode() {
        // The usage endpoint hands us a whole number; the rounding mode applies
        // to the payload's fractional percentages, not to this one.
        let input: Input = serde_json::from_str(RATE_LIMITED).unwrap();
        for mode in [
            RoundingMode::Floor,
            RoundingMode::Ceiling,
            RoundingMode::Nearest,
        ] {
            let plain = strip_ansi(&render(
                &input,
                None,
                &[],
                0,
                None,
                mode,
                Layout::Condensed,
                fable(95),
                None,
            ));
            assert!(plain.contains("95%"), "{mode:?}: {plain}");
        }
    }

    #[test]
    fn test_render_fable_hidden_on_api_billing() {
        // No rate_limits block → API billing → no plan windows at all, so a
        // stale fable file must not sneak a row in.
        let input: Input = serde_json::from_str(crate::input::API_BILLING_FIXTURE).unwrap();
        for layout in [Layout::Condensed, Layout::Condensed] {
            let plain = strip_ansi(&render(
                &input,
                None,
                &[],
                0,
                None,
                RoundingMode::Floor,
                layout,
                fable(51),
                None,
            ));
            assert!(!plain.contains("fable"), "{layout:?}: {plain}");
            assert!(!plain.contains("fbl"), "{layout:?}: {plain}");
        }
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
            &[],
            0,
            None,
            RoundingMode::Floor,
            Layout::Condensed,
            None,
            None,
        ));
        assert!(plain.contains("50%"));
    }

    #[test]
    fn test_render_condensed_no_rate_limits() {
        let input = Input::default();
        let result = render(
            &input,
            None,
            &[],
            0,
            None,
            RoundingMode::Floor,
            Layout::Condensed,
            None,
            None,
        );
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
        let result = render(
            &input,
            None,
            &[],
            0,
            None,
            RoundingMode::Floor,
            Layout::Condensed,
            None,
            None,
        );
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
        let result = render(
            &input,
            None,
            &[],
            0,
            None,
            RoundingMode::Floor,
            Layout::Condensed,
            None,
            None,
        );
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
            &[],
            0,
            None,
            RoundingMode::Floor,
            Layout::Condensed,
            None,
            None,
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
            &[incident],
            3,
            None,
            RoundingMode::Floor,
            Layout::Condensed,
            None,
            None,
        );
        let plain = strip_ansi(&out);
        assert!(plain.contains("+2 more"));
        assert_eq!(out.matches('\n').count(), 2);
    }

    #[test]
    fn test_render_real_stdin_fixture_condensed() {
        let input: Input = serde_json::from_str(crate::input::REAL_STDIN_FIXTURE).unwrap();
        let out = render(
            &input,
            None,
            &[],
            0,
            None,
            RoundingMode::Floor,
            Layout::Condensed,
            None,
            None,
        );
        let plain = strip_ansi(&out);
        assert!(plain.contains("Opus 4.7"), "model name should render");
        assert!(
            plain.contains("22%"),
            "server-provided used_percentage wins"
        );
        assert!(plain.contains("project"), "cwd dirname should render");
        let shown = ["5h", "7d"].iter().filter(|l| plain.contains(**l)).count();
        assert_eq!(shown, 1, "one rotating window, got: {plain}");
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
            &[],
            0,
            None,
            RoundingMode::Floor,
            Layout::Condensed,
            None,
            None,
        ));
        assert!(!plain.contains('⏱'), "stopwatch glyph should be gone");
    }

    // ── Cost rendering ────────────────────────────────────────────────────────

    #[test]
    fn test_render_cost_condensed_present() {
        let json = r#"{"cost": {"total_cost_usd": 1.4567}}"#;
        let input: Input = serde_json::from_str(json).unwrap();
        let plain = strip_ansi(&render(
            &input,
            None,
            &[],
            0,
            None,
            RoundingMode::Floor,
            Layout::Condensed,
            None,
            None,
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
            &[],
            0,
            None,
            RoundingMode::Floor,
            Layout::Condensed,
            None,
            None,
        ));
        assert!(!plain.contains('$'), "zero cost should be hidden");
        assert!(!plain.contains("💰"));
    }

    #[test]
    fn test_render_cost_skipped_when_absent() {
        let plain = strip_ansi(&render(
            &Input::default(),
            None,
            &[],
            0,
            None,
            RoundingMode::Floor,
            Layout::Condensed,
            None,
            None,
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
            &[],
            0,
            None,
            RoundingMode::Floor,
            Layout::Condensed,
            None,
            None,
        );
        assert!(out.contains(fmt::GREEN));

        // $1 ≤ x < $5 → yellow
        let json = r#"{"cost": {"total_cost_usd": 2.5}}"#;
        let input: Input = serde_json::from_str(json).unwrap();
        let out = render(
            &input,
            None,
            &[],
            0,
            None,
            RoundingMode::Floor,
            Layout::Condensed,
            None,
            None,
        );
        assert!(out.contains(fmt::YELLOW));

        // $5 ≤ x < $20 → orange
        let json = r#"{"cost": {"total_cost_usd": 12.0}}"#;
        let input: Input = serde_json::from_str(json).unwrap();
        let out = render(
            &input,
            None,
            &[],
            0,
            None,
            RoundingMode::Floor,
            Layout::Condensed,
            None,
            None,
        );
        assert!(out.contains(fmt::ORANGE));

        // ≥ $20 → red
        let json = r#"{"cost": {"total_cost_usd": 42.0}}"#;
        let input: Input = serde_json::from_str(json).unwrap();
        let out = render(
            &input,
            None,
            &[],
            0,
            None,
            RoundingMode::Floor,
            Layout::Condensed,
            None,
            None,
        );
        assert!(out.contains(fmt::RED));
    }

    #[test]
    fn test_render_api_billing_fixture_condensed() {
        let input: Input = serde_json::from_str(crate::input::API_BILLING_FIXTURE).unwrap();
        let out = render(
            &input,
            None,
            &[],
            0,
            None,
            RoundingMode::Floor,
            Layout::Condensed,
            None,
            None,
        );
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
        let out = render(
            &input,
            None,
            &[],
            0,
            None,
            RoundingMode::Floor,
            Layout::Condensed,
            None,
            None,
        );
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

        let condensed = strip_ansi(&render(
            &input,
            None,
            &[],
            0,
            None,
            RoundingMode::Floor,
            Layout::Condensed,
            None,
            None,
        ));
        assert!(
            !condensed.contains("$3.14"),
            "cost must not render on plan billing"
        );
        assert!(!condensed.contains("💰"));
        assert!(condensed.contains("5h"));
    }

    #[test]
    fn test_render_no_agent_badge_when_agent_type_absent() {
        let input: Input = serde_json::from_str(crate::input::REAL_STDIN_FIXTURE).unwrap();
        let out = strip_ansi(&render(
            &input,
            None,
            &[],
            0,
            None,
            RoundingMode::default(),
            Layout::Condensed,
            None,
            None,
        ));
        assert!(
            !out.contains("🤖"),
            "no agent badge for foreground sessions, got: {out:?}"
        );
    }

    #[test]
    fn test_render_agent_badge_in_condensed_layout() {
        let json = r#"{
            "cwd": "/tmp",
            "agent_type": "claude",
            "model": {"display_name": "Opus 4.7"}
        }"#;
        let input: Input = serde_json::from_str(json).unwrap();
        let out = strip_ansi(&render(
            &input,
            None,
            &[],
            0,
            None,
            RoundingMode::default(),
            Layout::Condensed,
            None,
            None,
        ));
        assert!(
            out.starts_with("🤖"),
            "agent badge in condensed layout, got: {out:?}"
        );
    }

    // ── resolve_base_repo render integration tests ────────────────────────────

    #[test]
    fn test_render_dir_segment_prefixes_base_repo_when_payload_has_original_cwd() {
        // cwd = "/tmp" (not a git repo), original_cwd = "/Users/foo/myproject"
        // dirname = "tmp", base = "myproject" → they differ, so prefix fires.
        let json = r#"{
            "cwd": "/tmp",
            "worktree": {"original_cwd": "/Users/foo/myproject"}
        }"#;
        let input: Input = serde_json::from_str(json).unwrap();
        let plain = strip_ansi(&render(
            &input,
            None,
            &[],
            0,
            None,
            RoundingMode::default(),
            Layout::Condensed,
            None,
            None,
        ));
        assert!(
            plain.contains("myproject/tmp"),
            "prefix should appear when base differs from dirname, got: {plain:?}"
        );
    }

    #[test]
    fn test_render_dir_segment_no_prefix_when_base_matches_dirname() {
        // cwd = "/Users/foo/myproject", original_cwd = "/Users/foo/myproject"
        // dirname = "myproject", base = "myproject" → same, collapse fires.
        let json = r#"{
            "cwd": "/Users/foo/myproject",
            "worktree": {"original_cwd": "/Users/foo/myproject"}
        }"#;
        let input: Input = serde_json::from_str(json).unwrap();
        let plain = strip_ansi(&render(
            &input,
            None,
            &[],
            0,
            None,
            RoundingMode::default(),
            Layout::Condensed,
            None,
            None,
        ));
        assert!(
            plain.contains("myproject"),
            "dirname should still render, got: {plain:?}"
        );
        assert!(
            !plain.contains("myproject/myproject"),
            "collapsed case must not double-prefix, got: {plain:?}"
        );
    }
}
