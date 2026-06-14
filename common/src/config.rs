//! Daemon-readable config: `${XDG_CONFIG_HOME:-$HOME/.config}/claudehud/config`.
//! Dead-simple `key=value`, one per line. `#` comments + blank lines ignored.
//! Absent file → defaults (autoupdate on, no pin).

use std::path::PathBuf;

#[derive(Debug, PartialEq, Eq)]
pub struct Config {
    pub autoupdate: bool,
    pub pin: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            autoupdate: true,
            pin: None,
        }
    }
}

/// Resolve the config file path. Honors `XDG_CONFIG_HOME`, else `$HOME/.config`.
/// Windows path is a seam (unimplemented in v1) — returns an empty path there so
/// callers treat it as "absent → defaults".
pub fn config_path() -> PathBuf {
    #[cfg(unix)]
    {
        if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
            return PathBuf::from(xdg).join("claudehud").join("config");
        }
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home)
                .join(".config")
                .join("claudehud")
                .join("config");
        }
        PathBuf::new()
    }
    #[cfg(not(unix))]
    {
        PathBuf::new()
    }
}

/// Read + parse the config file, falling back to defaults on any error.
pub fn load() -> Config {
    let path = config_path();
    match std::fs::read_to_string(&path) {
        Ok(s) => parse(&s),
        Err(_) => Config::default(),
    }
}

/// Parse config text. Unknown keys ignored. Missing keys keep their defaults.
pub fn parse(text: &str) -> Config {
    let mut cfg = Config::default();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let (k, v) = (k.trim(), v.trim());
        match k {
            "autoupdate" => cfg.autoupdate = !matches!(v, "false" | "0" | "no" | "off"),
            "pin" if !v.is_empty() => cfg.pin = Some(v.to_string()),
            _ => {}
        }
    }
    cfg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_text_is_default() {
        assert_eq!(parse(""), Config::default());
    }

    #[test]
    fn autoupdate_false_disables() {
        assert!(!parse("autoupdate=false").autoupdate);
        assert!(!parse("autoupdate = off").autoupdate);
        assert!(parse("autoupdate=true").autoupdate);
    }

    #[test]
    fn pin_is_captured() {
        assert_eq!(parse("pin=v0.2.0").pin, Some("v0.2.0".to_string()));
        assert_eq!(parse("pin=").pin, None);
    }

    #[test]
    fn comments_and_blanks_ignored() {
        let text = "# a comment\n\n  autoupdate=false  \npin=v1.0.0\n";
        let c = parse(text);
        assert!(!c.autoupdate);
        assert_eq!(c.pin, Some("v1.0.0".to_string()));
    }

    #[test]
    fn unknown_keys_ignored() {
        let c = parse("wat=1\nautoupdate=false");
        assert!(!c.autoupdate);
    }
}
