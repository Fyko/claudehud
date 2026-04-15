pub const BLUE: &str = "\x1b[38;2;0;153;255m";
pub const ORANGE: &str = "\x1b[38;2;255;176;85m";
pub const GREEN: &str = "\x1b[38;2;0;175;80m";
pub const CYAN: &str = "\x1b[38;2;86;182;194m";
pub const RED: &str = "\x1b[38;2;255;85;85m";
pub const YELLOW: &str = "\x1b[38;2;230;200;0m";
pub const WHITE: &str = "\x1b[38;2;220;220;220m";
pub const DIM: &str = "\x1b[2m";
pub const RESET: &str = "\x1b[0m";
pub const SEP: &str = " \x1b[2m│\x1b[0m ";

pub fn color_for_pct(pct: u8) -> &'static str {
    if pct >= 90 {
        RED
    } else if pct >= 70 {
        YELLOW
    } else if pct >= 50 {
        ORANGE
    } else {
        GREEN
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
        assert_eq!(color_for_pct(50), ORANGE);
        assert_eq!(color_for_pct(69), ORANGE);
        assert_eq!(color_for_pct(70), YELLOW);
        assert_eq!(color_for_pct(89), YELLOW);
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
}
