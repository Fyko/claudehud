use serde::Deserialize;

#[derive(Deserialize, Default)]
pub struct Input {
    pub model: Option<Model>,
    pub context_window: Option<ContextWindow>,
    pub cwd: Option<String>,
    pub session: Option<Session>,
    pub session_id: Option<String>,
    pub session_name: Option<String>,
    pub transcript_path: Option<String>,
    pub version: Option<String>,
    pub workspace: Option<Workspace>,
    pub output_style: Option<OutputStyle>,
    pub cost: Option<Cost>,
    pub effort: Option<Effort>,
    pub thinking: Option<Thinking>,
    pub fast_mode: Option<bool>,
    pub exceeds_200k_tokens: Option<bool>,
    pub rate_limits: Option<RateLimits>,
    pub agent: Option<Agent>,
    pub agent_type: Option<String>,
    pub worktree: Option<Worktree>,
}

#[derive(Deserialize)]
pub struct Model {
    pub id: Option<String>,
    pub display_name: Option<String>,
}

#[derive(Deserialize)]
pub struct ContextWindow {
    pub context_window_size: Option<u64>,
    pub current_usage: Option<TokenUsage>,
    pub total_input_tokens: Option<u64>,
    pub total_output_tokens: Option<u64>,
    pub used_percentage: Option<f64>,
    pub remaining_percentage: Option<f64>,
}

#[derive(Deserialize)]
pub struct TokenUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cache_creation_input_tokens: Option<u64>,
    pub cache_read_input_tokens: Option<u64>,
}

#[derive(Deserialize)]
pub struct Session {
    pub start_time: Option<String>,
}

#[derive(Deserialize)]
pub struct Workspace {
    pub current_dir: Option<String>,
    pub project_dir: Option<String>,
    pub added_dirs: Option<Vec<String>>,
}

#[derive(Deserialize)]
pub struct OutputStyle {
    pub name: Option<String>,
}

#[derive(Deserialize)]
pub struct Cost {
    pub total_cost_usd: Option<f64>,
    pub total_duration_ms: Option<u64>,
    pub total_api_duration_ms: Option<u64>,
    pub total_lines_added: Option<u64>,
    pub total_lines_removed: Option<u64>,
}

#[derive(Deserialize)]
pub struct Effort {
    pub level: Option<String>,
}

#[derive(Deserialize)]
pub struct Thinking {
    pub enabled: Option<bool>,
}

#[derive(Deserialize)]
pub struct RateLimits {
    pub five_hour: Option<RateWindow>,
    pub seven_day: Option<RateWindow>,
}

#[derive(Deserialize)]
pub struct RateWindow {
    pub used_percentage: Option<f64>,
    pub resets_at: Option<u64>,
}

#[derive(Deserialize)]
pub struct Agent {
    pub name: Option<String>,
}

#[derive(Deserialize)]
pub struct Worktree {
    pub name: Option<String>,
    pub branch: Option<String>,
    pub original_cwd: Option<String>,
    pub original_branch: Option<String>,
}

/// Anonymized capture of a real Claude Code statusline stdin payload on a
/// Claude plan (rate-limited tier) — harness v2.1.139. Keep in sync with
/// upstream shape changes.
#[cfg(test)]
pub(crate) const REAL_STDIN_FIXTURE: &str = r#"{
        "session_id": "00000000-0000-0000-0000-000000000000",
        "transcript_path": "/tmp/transcripts/session.jsonl",
        "cwd": "/home/user/project",
        "effort": {"level": "xhigh"},
        "session_name": "Testing session setup",
        "model": {"id": "claude-opus-4-7", "display_name": "Opus 4.7"},
        "workspace": {
            "current_dir": "/home/user/project",
            "project_dir": "/home/user/project",
            "added_dirs": []
        },
        "version": "2.1.139",
        "output_style": {"name": "Gen-Z"},
        "cost": {
            "total_cost_usd": 0.75355175,
            "total_duration_ms": 156069,
            "total_api_duration_ms": 83074,
            "total_lines_added": 18,
            "total_lines_removed": 2
        },
        "context_window": {
            "total_input_tokens": 399,
            "total_output_tokens": 5208,
            "context_window_size": 200000,
            "current_usage": {
                "input_tokens": 1,
                "output_tokens": 83,
                "cache_creation_input_tokens": 330,
                "cache_read_input_tokens": 43617
            },
            "used_percentage": 22,
            "remaining_percentage": 78
        },
        "exceeds_200k_tokens": false,
        "fast_mode": false,
        "thinking": {"enabled": true},
        "rate_limits": {
            "five_hour": {"used_percentage": 10, "resets_at": 1776567600},
            "seven_day": {"used_percentage": 22, "resets_at": 1776974400}
        }
    }"#;

/// Same shape as `REAL_STDIN_FIXTURE`, but on API billing — no `rate_limits`
/// block, since API users pay per-token instead of against quota windows.
#[cfg(test)]
pub(crate) const API_BILLING_FIXTURE: &str = r#"{
        "session_id": "00000000-0000-0000-0000-000000000000",
        "transcript_path": "/tmp/transcripts/session.jsonl",
        "cwd": "/home/user/project",
        "effort": {"level": "xhigh"},
        "model": {"id": "claude-opus-4-7[1m]", "display_name": "Opus 4.7 (1M context)"},
        "workspace": {
            "current_dir": "/home/user/project",
            "project_dir": "/home/user/project",
            "added_dirs": []
        },
        "version": "2.1.139",
        "output_style": {"name": "Gen-Z"},
        "cost": {
            "total_cost_usd": 0.1048615,
            "total_duration_ms": 6044,
            "total_api_duration_ms": 3372,
            "total_lines_added": 0,
            "total_lines_removed": 0
        },
        "context_window": {
            "total_input_tokens": 32169,
            "total_output_tokens": 18,
            "context_window_size": 1000000,
            "current_usage": {
                "input_tokens": 6,
                "output_tokens": 18,
                "cache_creation_input_tokens": 15288,
                "cache_read_input_tokens": 16875
            },
            "used_percentage": 3,
            "remaining_percentage": 97
        },
        "exceeds_200k_tokens": false,
        "fast_mode": false,
        "thinking": {"enabled": true}
    }"#;

/// Anonymized capture of a real background-agent statusline payload.
/// Distinguishing keys vs `REAL_STDIN_FIXTURE`: `agent`, `agent_type`.
#[cfg(test)]
pub(crate) const BG_AGENT_FIXTURE: &str = r#"{
    "session_id": "00000000-0000-0000-0000-000000000000",
    "transcript_path": "/tmp/transcripts/bg.jsonl",
    "cwd": "/home/user/project",
    "agent": {"name": "claude"},
    "agent_type": "claude",
    "model": {"id": "claude-opus-4-7", "display_name": "Opus 4.7"},
    "workspace": {
        "current_dir": "/home/user/project",
        "project_dir": "/home/user/project",
        "added_dirs": []
    },
    "version": "2.1.139",
    "output_style": {"name": "Gen-Z"},
    "exceeds_200k_tokens": false,
    "fast_mode": false,
    "thinking": {"enabled": true}
}"#;

/// Background-agent payload running inside a CC-native worktree. Adds the
/// top-level `worktree` block.
#[cfg(test)]
pub(crate) const BG_AGENT_WORKTREE_FIXTURE: &str = r#"{
    "session_id": "00000000-0000-0000-0000-000000000000",
    "transcript_path": "/tmp/transcripts/bg.jsonl",
    "cwd": "/home/user/.claude/worktrees/example",
    "agent": {"name": "claude"},
    "agent_type": "claude",
    "worktree": {
        "name": "example/branch-name",
        "path": "/home/user/.claude/worktrees/example",
        "branch": "worktree-example+branch-name",
        "original_cwd": "/home/user/project",
        "original_branch": "feature/example"
    },
    "model": {"id": "claude-opus-4-7", "display_name": "Opus 4.7"},
    "version": "2.1.139",
    "exceeds_200k_tokens": false,
    "fast_mode": false,
    "thinking": {"enabled": true}
}"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deserialize_minimal() {
        let json = r#"{}"#;
        let input: Input = serde_json::from_str(json).unwrap();
        assert!(input.model.is_none());
        assert!(input.cwd.is_none());
    }

    #[test]
    fn test_deserialize_real_stdin_fixture() {
        let input: Input = serde_json::from_str(REAL_STDIN_FIXTURE).unwrap();

        assert_eq!(
            input.session_id.as_deref(),
            Some("00000000-0000-0000-0000-000000000000")
        );
        assert_eq!(input.version.as_deref(), Some("2.1.139"));
        assert_eq!(input.cwd.as_deref(), Some("/home/user/project"));
        assert_eq!(input.exceeds_200k_tokens, Some(false));

        let model = input.model.as_ref().unwrap();
        assert_eq!(model.id.as_deref(), Some("claude-opus-4-7"));
        assert_eq!(model.display_name.as_deref(), Some("Opus 4.7"));

        let ws = input.workspace.as_ref().unwrap();
        assert_eq!(ws.current_dir.as_deref(), Some("/home/user/project"));
        assert_eq!(ws.added_dirs.as_ref().unwrap().len(), 0);

        let style = input.output_style.as_ref().unwrap();
        assert_eq!(style.name.as_deref(), Some("Gen-Z"));

        let cost = input.cost.as_ref().unwrap();
        assert!((cost.total_cost_usd.unwrap() - 0.753_551_75).abs() < 1e-9);
        assert_eq!(cost.total_lines_added, Some(18));
        assert_eq!(cost.total_lines_removed, Some(2));

        let cw = input.context_window.as_ref().unwrap();
        assert_eq!(cw.context_window_size, Some(200_000));
        assert_eq!(cw.total_input_tokens, Some(399));
        assert_eq!(cw.total_output_tokens, Some(5208));
        assert!((cw.used_percentage.unwrap() - 22.0).abs() < 1e-9);
        assert!((cw.remaining_percentage.unwrap() - 78.0).abs() < 1e-9);

        let usage = cw.current_usage.as_ref().unwrap();
        assert_eq!(usage.input_tokens, Some(1));
        assert_eq!(usage.output_tokens, Some(83));
        assert_eq!(usage.cache_creation_input_tokens, Some(330));
        assert_eq!(usage.cache_read_input_tokens, Some(43_617));

        let rl = input.rate_limits.as_ref().unwrap();
        assert!((rl.five_hour.as_ref().unwrap().used_percentage.unwrap() - 10.0).abs() < 1e-9);
        assert_eq!(
            rl.seven_day.as_ref().unwrap().resets_at,
            Some(1_776_974_400)
        );

        assert_eq!(input.session_name.as_deref(), Some("Testing session setup"));
        assert_eq!(input.fast_mode, Some(false));
        assert_eq!(
            input.effort.as_ref().and_then(|e| e.level.as_deref()),
            Some("xhigh")
        );
        assert_eq!(input.thinking.as_ref().and_then(|t| t.enabled), Some(true));
    }

    #[test]
    fn test_deserialize_api_billing_fixture() {
        let input: Input = serde_json::from_str(API_BILLING_FIXTURE).unwrap();

        // Shared fields still parse correctly.
        assert_eq!(input.version.as_deref(), Some("2.1.139"));
        assert_eq!(
            input.model.as_ref().and_then(|m| m.display_name.as_deref()),
            Some("Opus 4.7 (1M context)")
        );
        assert!((input.cost.as_ref().unwrap().total_cost_usd.unwrap() - 0.104_861_5).abs() < 1e-9);

        // Key marker of API billing: no rate_limits.
        assert!(
            input.rate_limits.is_none(),
            "API billing payload should have no rate_limits"
        );

        // Newer fields still surface.
        assert_eq!(
            input.effort.as_ref().and_then(|e| e.level.as_deref()),
            Some("xhigh")
        );
        assert_eq!(input.fast_mode, Some(false));
        assert_eq!(input.thinking.as_ref().and_then(|t| t.enabled), Some(true));

        // 1M context window from the [1m] model variant.
        assert_eq!(
            input
                .context_window
                .as_ref()
                .and_then(|cw| cw.context_window_size),
            Some(1_000_000)
        );
    }

    #[test]
    fn test_deserialize_bg_agent_fixture() {
        let input: Input = serde_json::from_str(BG_AGENT_FIXTURE).unwrap();
        assert_eq!(input.agent_type.as_deref(), Some("claude"));
        assert_eq!(
            input.agent.as_ref().and_then(|a| a.name.as_deref()),
            Some("claude")
        );
        assert!(
            input.worktree.is_none(),
            "non-native-worktree bg payload has no worktree block"
        );
    }

    #[test]
    fn test_deserialize_bg_agent_worktree_fixture() {
        let input: Input = serde_json::from_str(BG_AGENT_WORKTREE_FIXTURE).unwrap();
        let wt = input.worktree.as_ref().expect("worktree block must parse");
        assert_eq!(wt.original_branch.as_deref(), Some("feature/example"));
        assert_eq!(wt.name.as_deref(), Some("example/branch-name"));
        assert_eq!(wt.original_cwd.as_deref(), Some("/home/user/project"));
        // Auto-generated branch present but ignored downstream.
        assert_eq!(wt.branch.as_deref(), Some("worktree-example+branch-name"));
    }

    #[test]
    fn test_fg_fixture_has_no_agent_fields() {
        let input: Input = serde_json::from_str(REAL_STDIN_FIXTURE).unwrap();
        assert!(input.agent.is_none());
        assert!(input.agent_type.is_none());
        assert!(input.worktree.is_none());
    }
}
