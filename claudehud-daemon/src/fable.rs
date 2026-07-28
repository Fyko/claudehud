//! Poller for the Fable weekly-scoped usage cap.
//!
//! The statusline payload Claude Code hands the client only carries the
//! five-hour and seven-day windows. The per-model weekly caps (`Current week
//! (Fable)` in `/usage`) live behind `GET /api/oauth/usage`, which wants the
//! same OAuth token Claude Code itself uses. So: read that token, poll the
//! endpoint on the status-poll cadence, and drop the result in a small file the
//! client reads (see [`common::fable`]).
//!
//! Opt-in only — `fable=true` in the daemon config. Nothing here runs otherwise,
//! because this is the one code path that touches the user's credentials.
//!
//! Per ADR-0001 every failure is silent: no token, an expired token, a 401, a
//! body without a Fable entry — all just leave the prior file in place.

use std::time::Duration;

use common::fable::{fable_limit_path, format_fable_limit, FableLimit};

use crate::poll::{ConditionalGet, FetchOutcome};

const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const POLL_INTERVAL: Duration = Duration::from_mins(5);
const USER_AGENT: &str = concat!("claudehud-daemon/", env!("CARGO_PKG_VERSION"));

/// Entry point for the fable-polling thread. Returns immediately (thread ends)
/// when the feature is off, so `main` can spawn it unconditionally.
pub(crate) fn start() {
    if !common::config::load().fable {
        return;
    }
    let agent = ureq::AgentBuilder::new()
        .user_agent(USER_AGENT)
        .timeout_connect(Duration::from_secs(5))
        .timeout_read(Duration::from_secs(10))
        .build();

    run_fable_poll(
        &OauthSource { agent },
        &crate::poll::RealClock,
        POLL_INTERVAL,
        &fable_limit_path(),
    );
}

/// The fable-poll adapter over the shared poll loop: on each fresh body, pull
/// the Fable weekly entry out and write it for the client. A body we can't read
/// is logged and the prior file retained.
fn run_fable_poll<S, C>(source: &S, clock: &C, interval: Duration, path: &std::path::Path)
where
    S: ConditionalGet,
    C: crate::poll::Clock,
{
    crate::poll::run_poll_loop(source, clock, "fable", None, interval, |body| {
        match parse_fable_usage(body) {
            Some(limit) => {
                if let Err(e) = std::fs::write(path, format_fable_limit(&limit)) {
                    eprintln!("WARN fable write: {e}");
                }
            }
            None => eprintln!("WARN fable parse: no weekly Fable entry in usage response"),
        }
    });
}

/// A [`ConditionalGet`] that re-reads the OAuth token on every request.
///
/// Re-reading is the whole refresh story: Claude Code rotates the token in the
/// keychain / credentials file, and we just pick up whatever is current. No
/// refresh-token dance here.
struct OauthSource {
    agent: ureq::Agent,
}

impl ConditionalGet for OauthSource {
    fn fetch(&self, _etag: Option<&str>) -> Result<FetchOutcome, String> {
        // ponytail: no etag — the usage endpoint doesn't send one, and a 5-min
        // poll of a ~1KB JSON body isn't worth a conditional-GET dance.
        let token = oauth_token().ok_or("no Claude Code OAuth token available")?;
        let resp = self
            .agent
            .get(USAGE_URL)
            .set("Authorization", &format!("Bearer {token}"))
            .set("anthropic-beta", "oauth-2025-04-20")
            .call()
            .map_err(|e| e.to_string())?;
        let body = resp.into_string().map_err(|e| e.to_string())?;
        Ok(FetchOutcome::Body { body, etag: None })
    }
}

/// The Fable entry in a `/api/oauth/usage` body, as a [`FableLimit`].
///
/// The response is a `limits` array of windows; the one we want is the
/// weekly-scoped entry whose model display name mentions Fable (today it's a
/// bare `"Fable"`, but `"Fable 5"` should keep working).
fn parse_fable_usage(body: &str) -> Option<FableLimit> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let entry = v.get("limits")?.as_array()?.iter().find(|e| {
        e.get("kind").and_then(|k| k.as_str()) == Some("weekly_scoped")
            && e.pointer("/scope/model/display_name")
                .and_then(|n| n.as_str())
                .is_some_and(|n| n.to_ascii_lowercase().contains("fable"))
    })?;
    let percent = entry.get("percent")?.as_f64()?;
    let resets_at = entry
        .get("resets_at")?
        .as_str()
        .and_then(crate::status::parse_iso8601_secs)?;
    Some(FableLimit {
        percent: percent.round().clamp(0.0, 100.0) as u8,
        resets_at,
    })
}

/// The access token Claude Code is currently using, or `None` if we can't get
/// at it. macOS keeps it in the login keychain; everywhere else it's a file.
fn oauth_token() -> Option<String> {
    let raw = read_credentials()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    v.pointer("/claudeAiOauth/accessToken")?
        .as_str()
        .map(str::to_string)
}

#[cfg(target_os = "macos")]
fn read_credentials() -> Option<String> {
    let out = std::process::Command::new("/usr/bin/security")
        .args([
            "find-generic-password",
            "-s",
            "Claude Code-credentials",
            "-w",
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout).ok()
}

#[cfg(not(target_os = "macos"))]
fn read_credentials() -> Option<String> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    std::fs::read_to_string(
        std::path::PathBuf::from(home)
            .join(".claude")
            .join(".credentials.json"),
    )
    .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trimmed from a real `/api/oauth/usage` response.
    const REAL_USAGE: &str = r#"{
      "limits": [
        {"kind": "session", "group": "session", "percent": 96, "severity": "critical",
         "resets_at": "2026-07-27T17:19:59.334342+00:00", "scope": null, "is_active": true},
        {"kind": "weekly_all", "group": "weekly", "percent": 52, "severity": "normal",
         "resets_at": "2026-07-29T20:59:59.334363+00:00", "scope": null, "is_active": false},
        {"kind": "weekly_scoped", "group": "weekly", "percent": 51, "severity": "normal",
         "resets_at": "2026-07-29T21:00:00.334586+00:00",
         "scope": {"model": {"id": null, "display_name": "Fable"}, "surface": null},
         "is_active": false}
      ]
    }"#;

    #[test]
    fn parses_the_fable_window_from_a_real_body() {
        let got = parse_fable_usage(REAL_USAGE).expect("fable entry");
        assert_eq!(got.percent, 51);
        // 2026-07-29T21:00:00Z
        assert_eq!(got.resets_at, 1_785_358_800);
    }

    #[test]
    fn ignores_session_and_all_model_windows() {
        // Both non-fable windows sit above the fable one in the array; picking
        // the first `weekly_*` entry would grab the wrong percent (52, not 51).
        let got = parse_fable_usage(REAL_USAGE).unwrap();
        assert_ne!(got.percent, 52, "must not pick up weekly_all");
        assert_ne!(got.percent, 96, "must not pick up the session window");
    }

    #[test]
    fn matches_a_versioned_display_name() {
        let body = REAL_USAGE.replace("\"Fable\"", "\"Fable 5\"");
        assert_eq!(parse_fable_usage(&body).unwrap().percent, 51);
    }

    #[test]
    fn no_fable_entry_is_none() {
        let body = r#"{"limits": [{"kind": "weekly_all", "percent": 52,
                       "resets_at": "2026-07-29T20:59:59+00:00", "scope": null}]}"#;
        assert!(parse_fable_usage(body).is_none());
    }

    #[test]
    fn garbage_body_is_none() {
        assert!(parse_fable_usage("not json").is_none());
        assert!(parse_fable_usage("{}").is_none());
        assert!(parse_fable_usage(r#"{"limits": []}"#).is_none());
    }

    #[test]
    fn malformed_reset_timestamp_is_none() {
        let body = REAL_USAGE.replace("2026-07-29T21:00:00.334586+00:00", "soon");
        assert!(parse_fable_usage(&body).is_none());
    }

    #[test]
    fn percent_is_rounded_not_scaled() {
        // The usage endpoint reports 0-100 already, unlike the statusline
        // payload's 0-1 utilization. Fractions round to the nearest whole.
        let body = REAL_USAGE.replace("\"percent\": 51", "\"percent\": 50.6");
        assert_eq!(parse_fable_usage(&body).unwrap().percent, 51);
    }

    #[test]
    fn token_is_read_from_the_credentials_json_shape() {
        let creds = r#"{"claudeAiOauth": {"accessToken": "sk-ant-oat01-xyz",
                        "refreshToken": "sk-ant-ort01-abc", "expiresAt": 1785099600}}"#;
        let v: serde_json::Value = serde_json::from_str(creds).unwrap();
        assert_eq!(
            v.pointer("/claudeAiOauth/accessToken").unwrap().as_str(),
            Some("sk-ant-oat01-xyz")
        );
    }

    #[test]
    fn poll_cycle_writes_the_limit_file() {
        use crate::poll::test_support::{FakeClock, FakeSource};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clhud-fable");

        let source = FakeSource::new(vec![Ok(FetchOutcome::Body {
            body: REAL_USAGE.to_string(),
            etag: None,
        })]);
        run_fable_poll(
            &source,
            &FakeClock::keep_for(0),
            Duration::from_mins(5),
            &path,
        );

        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            common::fable::parse_fable_limit(&text),
            Some(FableLimit {
                percent: 51,
                resets_at: 1_785_358_800
            })
        );
    }
}
