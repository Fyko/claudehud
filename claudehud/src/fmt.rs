//! Colors come from the terminal's own 16-color palette rather than fixed RGB,
//! so the HUD inherits whatever theme the user runs — light or dark. Hardcoded
//! truecolor looked sharp on one background and vanished on the other (a near
//! white label on a near-white terminal).
//!
//! The one exception is [`ORANGE`]: ANSI has no orange slot, and the severity
//! ladder needs a step between yellow and red. 256-color 208 is legible on both
//! light and dark backgrounds.

pub const BLUE: &str = "\x1b[34m";
pub const ORANGE: &str = "\x1b[38;5;208m";
pub const GREEN: &str = "\x1b[32m";
pub const CYAN: &str = "\x1b[36m";
pub const RED: &str = "\x1b[31m";
pub const YELLOW: &str = "\x1b[33m";
pub const DIM: &str = "\x1b[2m";
pub const RESET: &str = "\x1b[0m";
pub const SEP: &str = " \x1b[2m│\x1b[0m ";

pub fn color_for_pct(pct: u8) -> &'static str {
    if pct >= 90 {
        RED
    } else if pct >= 70 {
        ORANGE
    } else if pct >= 50 {
        YELLOW
    } else {
        GREEN
    }
}

pub fn color_for_cost(usd: f64) -> &'static str {
    if usd >= 20.0 {
        RED
    } else if usd >= 5.0 {
        ORANGE
    } else if usd >= 1.0 {
        YELLOW
    } else {
        GREEN
    }
}

use common::incidents::Severity;

pub fn color_for_severity(sev: Severity) -> &'static str {
    match sev {
        Severity::Minor => YELLOW,
        Severity::Major => ORANGE,
        Severity::Critical => RED,
        Severity::Maintenance => CYAN,
        Severity::None => RESET,
    }
}

/// Write a color-coded progress bar into `out`. width=10 is standard.
pub fn build_bar(pct: u8, width: usize, out: &mut String) {
    let pct = pct.min(100);
    let filled = pct as usize * width / 100;
    let empty = width - filled;
    out.push_str(color_for_pct(pct));
    for _ in 0..filled {
        out.push('●');
    }
    out.push_str(DIM);
    for _ in 0..empty {
        out.push('○');
    }
    out.push_str(RESET);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_color_for_pct() {
        assert_eq!(color_for_pct(0), GREEN);
        assert_eq!(color_for_pct(49), GREEN);
        assert_eq!(color_for_pct(50), YELLOW);
        assert_eq!(color_for_pct(69), YELLOW);
        assert_eq!(color_for_pct(70), ORANGE);
        assert_eq!(color_for_pct(89), ORANGE);
        assert_eq!(color_for_pct(90), RED);
        assert_eq!(color_for_pct(100), RED);
    }

    #[test]
    fn test_build_bar_half() {
        let mut s = String::new();
        build_bar(50, 10, &mut s);
        let plain: String = s.chars().filter(|&c| c == '●' || c == '○').collect();
        assert_eq!(plain, "●●●●●○○○○○");
    }

    #[test]
    fn test_build_bar_full() {
        let mut s = String::new();
        build_bar(100, 10, &mut s);
        let plain: String = s.chars().filter(|&c| c == '●' || c == '○').collect();
        assert_eq!(plain, "●●●●●●●●●●");
    }

    #[test]
    fn test_build_bar_empty() {
        let mut s = String::new();
        build_bar(0, 10, &mut s);
        let plain: String = s.chars().filter(|&c| c == '●' || c == '○').collect();
        assert_eq!(plain, "○○○○○○○○○○");
    }

    #[test]
    fn test_color_for_cost() {
        assert_eq!(color_for_cost(0.0), GREEN);
        assert_eq!(color_for_cost(0.99), GREEN);
        assert_eq!(color_for_cost(1.0), YELLOW);
        assert_eq!(color_for_cost(4.99), YELLOW);
        assert_eq!(color_for_cost(5.0), ORANGE);
        assert_eq!(color_for_cost(19.99), ORANGE);
        assert_eq!(color_for_cost(20.0), RED);
        assert_eq!(color_for_cost(1000.0), RED);
    }

    #[test]
    fn test_color_for_severity() {
        use common::incidents::Severity;
        assert_eq!(color_for_severity(Severity::Minor), YELLOW);
        assert_eq!(color_for_severity(Severity::Major), ORANGE);
        assert_eq!(color_for_severity(Severity::Critical), RED);
        assert_eq!(color_for_severity(Severity::Maintenance), CYAN);
    }
}
