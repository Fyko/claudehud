pub enum ResetStyle {
    Time,
    DateTime,
}

/// Parse ISO 8601 datetime string to Unix epoch seconds. No external deps.
/// Handles: YYYY-MM-DDTHH:MM:SS[.fff][Z|+HH:MM|-HH:MM]
pub fn parse_iso8601(s: &str) -> Option<u64> {
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

    // Find timezone marker after optional fractional seconds
    let after = &s[19..];
    let tz = after
        .find(['Z', '+', '-'])
        .map(|i| &after[i..])
        .unwrap_or("Z");

    let tz_offset: i64 = if tz.starts_with('Z') {
        0
    } else {
        let sign: i64 = if tz.starts_with('+') { 1 } else { -1 };
        let t = &tz[1..];
        let h: i64 = t.get(0..2).and_then(|s| s.parse().ok()).unwrap_or(0);
        let m: i64 = t.get(3..5).and_then(|s| s.parse().ok()).unwrap_or(0);
        sign * (h * 3600 + m * 60)
    };

    let days = days_since_epoch(year, month, day)?;
    let epoch = days * 86400 + hour * 3600 + min * 60 + sec - tz_offset;
    if epoch < 0 {
        return None;
    }
    Some(epoch as u64)
}

/// Days since 1970-01-01 using proleptic Gregorian calendar.
fn days_since_epoch(year: i64, month: i64, day: i64) -> Option<i64> {
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let y = year - (month <= 2) as i64;
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

pub fn format_duration(secs: u64) -> String {
    if secs >= 3600 {
        format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
    } else if secs >= 60 {
        format!("{}m", secs / 60)
    } else {
        format!("{}s", secs)
    }
}

/// Format epoch as local time.
pub fn format_reset_time(epoch: u64, style: ResetStyle) -> String {
    use time::{OffsetDateTime, UtcOffset};

    let offset = UtcOffset::local_offset_at(
        OffsetDateTime::from_unix_timestamp(epoch as i64).unwrap_or(OffsetDateTime::UNIX_EPOCH),
    )
    .unwrap_or(UtcOffset::UTC);

    let dt = OffsetDateTime::from_unix_timestamp(epoch as i64)
        .unwrap_or(OffsetDateTime::UNIX_EPOCH)
        .to_offset(offset);

    let hour = dt.hour();
    let min = dt.minute();
    let ampm = if hour >= 12 { "pm" } else { "am" };
    let h12 = match hour % 12 {
        0 => 12,
        h => h,
    };

    match style {
        ResetStyle::Time => format!("{h12}:{min:02}{ampm}"),
        ResetStyle::DateTime => {
            const MONTHS: [&str; 12] = [
                "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov",
                "dec",
            ];
            let month = MONTHS[dt.month() as usize - 1];
            let day = dt.day();
            format!("{month} {day}, {h12}:{min:02}{ampm}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_iso8601_utc_z() {
        // 2024-01-15T10:30:00Z = 1705314600
        assert_eq!(parse_iso8601("2024-01-15T10:30:00Z"), Some(1_705_314_600));
    }

    #[test]
    fn test_parse_iso8601_with_fractional() {
        assert_eq!(
            parse_iso8601("2024-01-15T10:30:00.000Z"),
            Some(1_705_314_600)
        );
    }

    #[test]
    fn test_parse_iso8601_offset_plus() {
        // 2024-01-15T16:00:00+05:30 = 2024-01-15T10:30:00Z = 1705314600
        assert_eq!(
            parse_iso8601("2024-01-15T16:00:00+05:30"),
            Some(1_705_314_600)
        );
    }

    #[test]
    fn test_parse_iso8601_offset_minus() {
        // 2024-01-15T05:30:00-05:00 = 2024-01-15T10:30:00Z = 1705314600
        assert_eq!(
            parse_iso8601("2024-01-15T05:30:00-05:00"),
            Some(1_705_314_600)
        );
    }

    #[test]
    fn test_parse_iso8601_invalid() {
        assert_eq!(parse_iso8601("not-a-date"), None);
        assert_eq!(parse_iso8601(""), None);
    }

    #[test]
    fn test_format_duration_seconds() {
        assert_eq!(format_duration(0), "0s");
        assert_eq!(format_duration(59), "59s");
    }

    #[test]
    fn test_format_duration_minutes() {
        assert_eq!(format_duration(60), "1m");
        assert_eq!(format_duration(90), "1m");
        assert_eq!(format_duration(3599), "59m");
    }

    #[test]
    fn test_format_duration_hours() {
        assert_eq!(format_duration(3600), "1h0m");
        assert_eq!(format_duration(3661), "1h1m");
        assert_eq!(format_duration(7384), "2h3m");
    }
}
