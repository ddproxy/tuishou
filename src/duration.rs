//! Small duration/time-of-day helpers used by auto-update configuration.
//!
//! Configuration uses human strings such as `5m`, `10m`, `1h`, `300s` or a
//! bare number of seconds. Nothing here executes code from a release.

use crate::error::{Error, Result};
use std::time::Duration;
use time::OffsetDateTime;

/// Parse a duration string. Suffixes: `s`, `m`, `h`, `d`; compound values such
/// as `1h30m` are accepted. A bare number is interpreted as seconds.
pub fn parse_duration(value: &str) -> Result<Duration> {
    let input = value.trim();
    if input.is_empty() {
        return Err(Error::msg("empty duration"));
    }
    if let Ok(secs) = input.parse::<u64>() {
        return Ok(Duration::from_secs(secs));
    }
    let mut rest = input;
    let mut total = 0u64;
    let mut seen_digit = false;
    while !rest.is_empty() {
        let digits = rest.bytes().take_while(|b| b.is_ascii_digit()).count();
        if digits == 0 {
            return Err(Error::msg(format!("invalid duration '{value}'")));
        }
        seen_digit = true;
        let amount: u64 = rest[..digits]
            .parse()
            .map_err(|_| Error::msg(format!("invalid duration '{value}'")))?;
        rest = &rest[digits..];
        let (mult, consumed) = match rest.as_bytes().first() {
            Some(b's') => (1u64, 1usize),
            Some(b'm') => (60, 1),
            Some(b'h') => (3_600, 1),
            Some(b'd') => (86_400, 1),
            _ => return Err(Error::msg(format!("invalid duration '{value}'"))),
        };
        total = total
            .checked_add(
                amount
                    .checked_mul(mult)
                    .ok_or_else(|| Error::msg(format!("duration '{value}' is too large")))?,
            )
            .ok_or_else(|| Error::msg(format!("duration '{value}' is too large")))?;
        rest = &rest[consumed..];
    }
    if !seen_digit {
        return Err(Error::msg(format!("invalid duration '{value}'")));
    }
    Ok(Duration::from_secs(total))
}

/// Render a duration in the compact form used in generated systemd units.
pub fn format_duration(d: Duration) -> String {
    let secs = d.as_secs();
    if secs.is_multiple_of(86_400) {
        format!("{}d", secs / 86_400)
    } else if secs.is_multiple_of(3_600) {
        format!("{}h", secs / 3_600)
    } else if secs.is_multiple_of(60) {
        format!("{}min", secs / 60)
    } else {
        format!("{secs}s")
    }
}

/// Parse `HH:MM` (24-hour clock) for controller update windows.
pub fn parse_hhmm(value: &str) -> Result<(u8, u8)> {
    let parts: Vec<&str> = value.split(':').collect();
    if parts.len() != 2 {
        return Err(Error::msg(format!(
            "invalid time '{value}', expected HH:MM"
        )));
    }
    let hour: u8 = parts[0]
        .parse()
        .map_err(|_| Error::msg(format!("invalid time '{value}', expected HH:MM")))?;
    let minute: u8 = parts[1]
        .parse()
        .map_err(|_| Error::msg(format!("invalid time '{value}', expected HH:MM")))?;
    if hour > 23 || minute > 59 {
        return Err(Error::msg(format!(
            "invalid time '{value}', expected HH:MM"
        )));
    }
    Ok((hour, minute))
}

/// Whether `now` falls in the `start..end` window (UTC, half-open, may wrap
/// midnight). An empty window (`start == end`) means "no update".
pub fn in_window(now: OffsetDateTime, start: (u8, u8), end: (u8, u8)) -> bool {
    let minutes = u16::from(now.time().hour()) * 60 + u16::from(now.time().minute());
    let from = u16::from(start.0) * 60 + u16::from(start.1);
    let to = u16::from(end.0) * 60 + u16::from(end.1);
    if from == to {
        return false;
    }
    if from < to {
        minutes >= from && minutes < to
    } else {
        minutes >= from || minutes < to
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_units_and_compound() {
        assert_eq!(parse_duration("5m").unwrap(), Duration::from_secs(300));
        assert_eq!(parse_duration("1h30m").unwrap(), Duration::from_secs(5400));
        assert_eq!(parse_duration("42").unwrap(), Duration::from_secs(42));
        assert!(parse_duration("").is_err());
        assert!(parse_duration("10x").is_err());
        assert!(parse_duration("m10").is_err());
    }

    #[test]
    fn window_handles_wrap_around() {
        let start = parse_hhmm("22:00").unwrap();
        let end = parse_hhmm("02:00").unwrap();
        let night =
            OffsetDateTime::from_unix_timestamp(0).unwrap() + Duration::from_secs(23 * 3600);
        let noon = OffsetDateTime::from_unix_timestamp(0).unwrap() + Duration::from_secs(12 * 3600);
        assert!(in_window(night, start, end));
        assert!(!in_window(noon, start, end));
    }
}
