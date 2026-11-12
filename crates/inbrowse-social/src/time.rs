//! Dates without a date library: Unix seconds to RFC 3339 and back, plus the
//! one non-ISO format a source uses (X's `Wed Oct 10 20:19:24 +0000 2018`).
//! Used by every source module and by `lib.rs` for the search window.

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's
/// days_from_civil).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = i64::from(m);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// `2026-11-02T17:41:31Z`.
pub(crate) fn to_rfc3339(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// `YYYY-MM-DD` for search qualifiers.
pub(crate) fn to_date(secs: i64) -> String {
    to_rfc3339(secs)[..10].to_string()
}

/// Parse RFC 3339 / ISO 8601 (`2026-10-13T08:45:54Z`, fractional seconds,
/// `+hh:mm` offsets, or a bare date) to Unix seconds.
pub(crate) fn parse_rfc3339(s: &str) -> Option<i64> {
    let s = s.trim();
    let b = s.as_bytes();
    if b.len() < 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let num = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None;
    }
    let mut secs = days_from_civil(y, mo as u32, d as u32) * 86_400;
    if b.len() == 10 {
        return Some(secs);
    }
    if !matches!(b[10], b'T' | b't' | b' ') || b.len() < 19 {
        return None;
    }
    secs += num(11..13)? * 3600 + num(14..16)? * 60 + num(17..19)?;
    let mut rest = &s[19..];
    if let Some(frac) = rest.strip_prefix('.') {
        let digits = frac.bytes().take_while(u8::is_ascii_digit).count();
        rest = &frac[digits..];
    }
    match rest.as_bytes().first() {
        None | Some(b'Z' | b'z') => Some(secs),
        Some(&sign @ (b'+' | b'-')) => {
            let h = rest.get(1..3)?.parse::<i64>().ok()?;
            let m = rest
                .get(rest.len().saturating_sub(2)..)?
                .parse::<i64>()
                .ok()?;
            let offset = h * 3600 + m * 60;
            Some(if sign == b'+' {
                secs - offset
            } else {
                secs + offset
            })
        }
        _ => None,
    }
}

/// X's `created_at`: `Wed Oct 10 20:19:24 +0000 2018`.
pub(crate) fn parse_twitter(s: &str) -> Option<i64> {
    let parts: Vec<&str> = s.split_whitespace().collect();
    let [_, mon, day, hms, offset, year] = parts.as_slice() else {
        return None;
    };
    let months = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let m = months.iter().position(|x| x == mon)? as u32 + 1;
    let iso = format!(
        "{year}-{m:02}-{:02}T{hms}{}:{}",
        day.parse::<u32>().ok()?,
        &offset[..3],
        &offset[3..]
    );
    parse_rfc3339(&iso)
}

pub(crate) fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_formats() {
        assert_eq!(to_rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(to_rfc3339(1_791_887_220), "2026-10-13T10:27:00Z");
        assert_eq!(parse_rfc3339("2026-10-13T10:27:00Z"), Some(1_791_887_220));
        assert_eq!(
            parse_rfc3339("2026-10-13T10:27:00.573282Z"),
            Some(1_791_887_220)
        );
        assert_eq!(
            parse_rfc3339("2026-10-13T12:27:00+02:00"),
            Some(1_791_887_220)
        );
        assert_eq!(parse_rfc3339("2026-10-13"), Some(1_791_849_600));
        assert_eq!(
            parse_twitter("Tue Oct 13 10:27:00 +0000 2026"),
            Some(1_791_887_220)
        );
        assert_eq!(to_date(1_791_887_220), "2026-10-13");
        assert_eq!(parse_rfc3339("not a date"), None);
        for t in [-86_400 * 400, 951_782_400, 1_709_164_800, 4_102_444_800] {
            assert_eq!(parse_rfc3339(&to_rfc3339(t)), Some(t));
        }
    }
}
