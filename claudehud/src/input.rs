use serde::Deserialize;

#[derive(Deserialize, Default)]
pub struct Input {
    pub model: Option<Model>,
    pub context_window: Option<ContextWindow>,
    pub cwd: Option<String>,
    pub session: Option<Session>,
    pub rate_limits: Option<RateLimits>,
}

#[derive(Deserialize)]
pub struct Model {
    pub display_name: Option<String>,
}

#[derive(Deserialize)]
pub struct ContextWindow {
    pub context_window_size: Option<u64>,
    pub current_usage: Option<TokenUsage>,
}

#[derive(Deserialize)]
pub struct TokenUsage {
    pub input_tokens: Option<u64>,
    pub cache_creation_input_tokens: Option<u64>,
    pub cache_read_input_tokens: Option<u64>,
}

#[derive(Deserialize)]
pub struct Session {
    pub start_time: Option<String>,
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
    fn test_deserialize_full() {
        let json = r#"{
            "model": {"display_name": "Claude Sonnet 4.5"},
            "session_id": "abc123",
            "cwd": "/home/user/project",
            "context_window": {
                "context_window_size": 200000,
                "current_usage": {
                    "input_tokens": 1000,
                    "cache_creation_input_tokens": 500,
                    "cache_read_input_tokens": 200
                }
            },
            "session": {"start_time": "2024-01-15T10:00:00Z"},
            "rate_limits": {
                "five_hour": {"used_percentage": 45.5, "resets_at": 1705316400},
                "seven_day": {"used_percentage": 12.0, "resets_at": 1705833600}
            }
        }"#;
        let input: Input = serde_json::from_str(json).unwrap();
        assert_eq!(
            input.model.unwrap().display_name.unwrap(),
            "Claude Sonnet 4.5"
        );
        let cw = input.context_window.unwrap();
        assert_eq!(cw.context_window_size.unwrap(), 200_000);
        let usage = cw.current_usage.unwrap();
        assert_eq!(usage.input_tokens.unwrap(), 1000);
        let rl = input.rate_limits.unwrap();
        assert!((rl.five_hour.unwrap().used_percentage.unwrap() - 45.5).abs() < 0.01);
    }
}
