//! Schedule times: parsing what the model or the user writes, and deciding
//! when a schedule fires next. No cron syntax and no time zone database:
//! times are UTC, or UTC plus the fixed `utc_offset_minutes` in
//! `agent.json`.

/// One second, minute, hour, day in milliseconds.
const SECOND: u64 = 1_000;
const MINUTE: u64 = 60 * SECOND;
const HOUR: u64 = 60 * MINUTE;
pub const DAY: u64 = 24 * HOUR;

/// The shortest interval a recurring schedule may have.
pub const MIN_EVERY_MS: u64 = 5 * MINUTE;
/// Most schedules at once.
pub const MAX_SCHEDULES: u64 = 50;
/// A one-shot schedule more overdue than this when the session finds it is
/// dropped instead of fired.
pub const MAX_LATE_MS: u64 = DAY;
/// The prefix a fired schedule's message starts with.
pub fn fired_text(name: &str, prompt: &str) -> String {
    format!("[scheduled: {name}] {prompt}")
}

/// A length of time such as `90s`, `5m`, `1h30m`, `2d` or `1w`.
pub fn parse_duration(text: &str) -> Result<u64, String> {
    let bad = || format!("not a duration: {text:?} (use forms like 30m, 2h, 1d, 1h30m)");
    let text = text.trim().to_ascii_lowercase();
    if text.is_empty() {
        return Err(bad());
    }
    let mut total: u64 = 0;
    let mut digits = String::new();
    for c in text.chars().filter(|c| !c.is_whitespace()) {
        if c.is_ascii_digit() {
            digits.push(c);
            continue;
        }
        let unit = match c {
            's' => SECOND,
            'm' => MINUTE,
            'h' => HOUR,
            'd' => DAY,
            'w' => 7 * DAY,
            _ => return Err(bad()),
        };
        let n: u64 = digits.parse().map_err(|_| bad())?;
        digits.clear();
        total = n
            .checked_mul(unit)
            .and_then(|v| total.checked_add(v))
            .ok_or_else(bad)?;
    }
    if !digits.is_empty() || total == 0 {
        return Err(bad());
    }
    Ok(total)
}

/// Days since 1970-01-01 for a date in the proleptic Gregorian calendar.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `HH:MM` or `HH:MM:SS` as milliseconds after midnight.
fn parse_clock(text: &str) -> Option<u64> {
    let parts: Vec<&str> = text.split(':').collect();
    let num = |s: &str, max: u64| s.parse::<u64>().ok().filter(|n| *n <= max);
    let (h, m, s) = match parts.as_slice() {
        [h, m] => (num(h, 23)?, num(m, 59)?, 0),
        [h, m, s] => (num(h, 23)?, num(m, 59)?, num(s, 59)?),
        _ => return None,
    };
    Some(h * HOUR + m * MINUTE + s * SECOND)
}

fn offset_ms(offset_minutes: i32) -> i64 {
    i64::from(offset_minutes) * MINUTE as i64
}

/// The next moment after `now_ms` (UTC) at which the local clock reads
/// `clock_ms` after midnight.
fn next_clock(now_ms: u64, clock_ms: u64, offset_minutes: i32) -> u64 {
    let local_now = now_ms as i64 + offset_ms(offset_minutes);
    let midnight = local_now - local_now.rem_euclid(DAY as i64);
    let mut at = midnight + clock_ms as i64;
    if at <= local_now {
        at += DAY as i64;
    }
    (at - offset_ms(offset_minutes)).max(0) as u64
}

/// When a schedule given `at` fires first, in UTC milliseconds. `at` is
/// `in <duration>`, `HH:MM` (the next time the clock reads that), or
/// `YYYY-MM-DD HH:MM` (also with `T`, and optional seconds), in local time.
pub fn parse_at(at: &str, now_ms: u64, offset_minutes: i32) -> Result<u64, String> {
    let at = at.trim();
    if let Some(rest) = at.strip_prefix("in ") {
        return Ok(now_ms + parse_duration(rest)?);
    }
    if let Some(clock) = parse_clock(at) {
        return Ok(next_clock(now_ms, clock, offset_minutes));
    }
    let bad = || {
        format!(
            "not a time: {at:?} (use \"in 20m\", \"09:00\" or \"2026-11-13 09:00\", local time)"
        )
    };
    let (date, clock) = at.split_once([' ', 'T']).ok_or_else(bad)?;
    let d: Vec<&str> = date.split('-').collect();
    let [y, m, day] = d.as_slice() else {
        return Err(bad());
    };
    let y: i64 = y.parse().map_err(|_| bad())?;
    let m: i64 = m
        .parse()
        .ok()
        .filter(|m| (1..=12).contains(m))
        .ok_or_else(bad)?;
    let day: i64 = day
        .parse()
        .ok()
        .filter(|d| (1..=31).contains(d))
        .ok_or_else(bad)?;
    let clock = parse_clock(clock.trim()).ok_or_else(bad)?;
    let local = days_from_civil(y, m, day) * DAY as i64 + clock as i64;
    let utc = local - offset_ms(offset_minutes);
    if utc < 0 {
        return Err(bad());
    }
    Ok(utc as u64)
}

/// When a recurring schedule fires first: at the next `time` (`HH:MM`,
/// local) if given, else one interval from now.
pub fn first_every(
    every_ms: u64,
    time: Option<&str>,
    now_ms: u64,
    offset_minutes: i32,
) -> Result<u64, String> {
    match time {
        None => Ok(now_ms + every_ms),
        Some(t) => {
            let clock = parse_clock(t.trim())
                .ok_or_else(|| format!("not a time of day: {t:?} (use HH:MM)"))?;
            Ok(next_clock(now_ms, clock, offset_minutes))
        }
    }
}

/// What to do with a schedule that is due.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Due {
    /// Post its prompt; then keep it until `next` (recurring), or remove it.
    Fire { next: Option<u64> },
    /// Too late to be useful: remove it without posting.
    Drop,
}

/// Decides a due schedule (`next_at_ms <= now_ms`). A one-shot fires if it
/// is at most [`MAX_LATE_MS`] late, else is dropped. A recurring one fires
/// once however many times it was missed, and its next time moves forward
/// by whole intervals to the first one after now.
pub fn on_due(next_at_ms: u64, every_ms: Option<u64>, now_ms: u64) -> Due {
    match every_ms {
        None if now_ms.saturating_sub(next_at_ms) > MAX_LATE_MS => Due::Drop,
        None => Due::Fire { next: None },
        Some(every) => {
            let every = every.max(1);
            let missed = now_ms.saturating_sub(next_at_ms) / every + 1;
            Due::Fire {
                next: Some(next_at_ms + missed * every),
            }
        }
    }
}

/// A UTC time in milliseconds as local `YYYY-MM-DD HH:MM` with its offset.
pub fn local(ms: u64, offset_minutes: i32) -> String {
    let secs = (ms as i64 + offset_ms(offset_minutes)).max(0) as u64 / 1000;
    let full = crate::builtin::utc(secs);
    let stamp = &full[..16];
    if offset_minutes == 0 {
        format!("{stamp} UTC")
    } else {
        let sign = if offset_minutes < 0 { '-' } else { '+' };
        let m = offset_minutes.unsigned_abs();
        format!("{stamp} UTC{sign}{:02}:{:02}", m / 60, m % 60)
    }
}

/// A UTC time in milliseconds as local `YYYY-MM-DD HH:MM`, without the
/// zone; [`parse_at`] reads it back.
pub fn local_stamp(ms: u64, offset_minutes: i32) -> String {
    local(ms, offset_minutes)[..16].to_string()
}

/// The zone [`local`] names: `UTC` or `UTC+01:00`.
pub fn zone(offset_minutes: i32) -> String {
    local(0, offset_minutes)[17..].to_string()
}

/// An interval as the shortest of `5m`, `2h`, `1d`, `1w`, or a sum.
pub fn duration_text(ms: u64) -> String {
    let mut rest = ms;
    let mut out = String::new();
    for (unit, label) in [
        (7 * DAY, "w"),
        (DAY, "d"),
        (HOUR, "h"),
        (MINUTE, "m"),
        (SECOND, "s"),
    ] {
        if rest >= unit {
            out.push_str(&format!("{}{label}", rest / unit));
            rest %= unit;
        }
    }
    if out.is_empty() { "0s".into() } else { out }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-11-12 15:04:05 UTC.
    const NOW: u64 = 1_794_495_845_000;

    #[test]
    fn durations() {
        assert_eq!(parse_duration("5m").unwrap(), 5 * MINUTE);
        assert_eq!(parse_duration("1h30m").unwrap(), 90 * MINUTE);
        assert_eq!(parse_duration("1 d").unwrap(), DAY);
        assert_eq!(parse_duration("2s").unwrap(), 2 * SECOND);
        for bad in ["", "5", "m", "5x", "0m", "99999999999999999999d"] {
            assert!(parse_duration(bad).is_err(), "{bad}");
        }
        assert_eq!(duration_text(DAY + 90 * MINUTE), "1d1h30m");
        assert_eq!(duration_text(7 * DAY), "1w");
    }

    #[test]
    fn times() {
        assert_eq!(parse_at("in 2s", NOW, 0).unwrap(), NOW + 2000);
        // The next 09:00 UTC is tomorrow; the next 16:00 is today.
        let tomorrow_nine = parse_at("2026-11-13 09:00", NOW, 0).unwrap();
        assert_eq!(parse_at("09:00", NOW, 0).unwrap(), tomorrow_nine);
        assert_eq!(local(tomorrow_nine, 0), "2026-11-13 09:00 UTC");
        assert_eq!(
            local(parse_at("16:00", NOW, 0).unwrap(), 0),
            "2026-11-12 16:00 UTC"
        );
        // At UTC-5 it is 10:04, so 09:00 local is tomorrow at 14:00 UTC.
        let east = parse_at("09:00", NOW, -300).unwrap();
        assert_eq!(local(east, 0), "2026-11-13 14:00 UTC");
        assert_eq!(local(east, -300), "2026-11-13 09:00 UTC-05:00");
        assert_eq!(
            parse_at("2026-11-13T09:00:30", NOW, 60).unwrap(),
            parse_at("2026-11-13 08:00:30", NOW, 0).unwrap()
        );
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 2, 29), 11_016);
        for bad in [
            "tomorrow",
            "25:00",
            "2026-13-01 09:00",
            "2026-11-12",
            "in soon",
        ] {
            assert!(parse_at(bad, NOW, 0).is_err(), "{bad}");
        }
        assert_eq!(first_every(HOUR, None, NOW, 0).unwrap(), NOW + HOUR);
        assert_eq!(
            first_every(DAY, Some("09:00"), NOW, 0).unwrap(),
            tomorrow_nine
        );
    }

    #[test]
    fn due_schedules() {
        assert_eq!(on_due(NOW - HOUR, None, NOW), Due::Fire { next: None });
        assert_eq!(on_due(NOW - DAY - 1, None, NOW), Due::Drop);
        // Missed three and a half times: fires once, next is the first
        // interval after now.
        let start = NOW - 7 * HOUR / 2;
        assert_eq!(
            on_due(start, Some(HOUR), NOW),
            Due::Fire {
                next: Some(start + 4 * HOUR)
            }
        );
        assert_eq!(
            on_due(NOW, Some(HOUR), NOW),
            Due::Fire {
                next: Some(NOW + HOUR)
            }
        );
    }
}
