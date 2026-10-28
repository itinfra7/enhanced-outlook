//! 서버 날짜(`2026-10-15T14:06:38+09:00`, `...Z`, 소수 초 포함)를 unix ms로.

/// 1970-01-01부터의 일수(그레고리력, Howard Hinnant의 days_from_civil).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn num(s: &str, range: std::ops::Range<usize>) -> Option<i64> {
    let part = s.get(range)?;
    if part.bytes().all(|b| b.is_ascii_digit()) {
        part.parse().ok()
    } else {
        None
    }
}

/// RFC 3339 날짜를 unix ms로. 시간대가 없으면 UTC로 본다.
pub fn parse_rfc3339_ms(s: &str) -> Option<i64> {
    let s = s.trim();
    let (y, mo, d) = (num(s, 0..4)?, num(s, 5..7)?, num(s, 8..10)?);
    if !matches!(s.as_bytes().get(10), Some(b'T' | b't' | b' ')) {
        return None;
    }
    let (h, mi, sec) = (num(s, 11..13)?, num(s, 14..16)?, num(s, 17..19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    let mut rest = &s[19..];
    let mut millis = 0;
    if let Some(frac) = rest.strip_prefix('.') {
        let digits = frac.bytes().take_while(u8::is_ascii_digit).count();
        let mut ms_digits: String = frac[..digits].chars().take(3).collect();
        while ms_digits.len() < 3 {
            ms_digits.push('0');
        }
        millis = ms_digits.parse().ok()?;
        rest = &frac[digits..];
    }
    let offset_min = match rest {
        "" | "Z" | "z" => 0,
        _ => {
            let sign = match rest.as_bytes()[0] {
                b'+' => 1,
                b'-' => -1,
                _ => return None,
            };
            let oh = num(rest, 1..3)?;
            let om = num(rest, 4..6).or_else(|| num(rest, 3..5)).unwrap_or(0);
            sign * (oh * 60 + om)
        }
    };
    let days = days_from_civil(y, mo, d);
    let secs = days * 86_400 + h * 3600 + mi * 60 + sec - offset_min * 60;
    Some(secs * 1000 + millis)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_server_formats() {
        // 2026-10-15T05:06:38Z = 1792040798초
        assert_eq!(
            parse_rfc3339_ms("2026-10-15T05:06:38Z"),
            Some(1_792_040_798_000)
        );
        assert_eq!(
            parse_rfc3339_ms("2026-10-15T14:06:38+09:00"),
            Some(1_792_040_798_000)
        );
        assert_eq!(
            parse_rfc3339_ms("2026-10-15T14:06:38.5+09:00"),
            Some(1_792_040_798_500)
        );
        assert_eq!(parse_rfc3339_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            parse_rfc3339_ms("2000-03-01T00:00:00-01:00"),
            Some(951_872_400_000)
        );
        assert_eq!(parse_rfc3339_ms("nope"), None);
        assert_eq!(parse_rfc3339_ms("2026-13-01T00:00:00Z"), None);
    }
}
