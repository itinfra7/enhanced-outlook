//! 질의 문법(계획서 8.3).
//!
//! - 띄어 쓴 단어는 AND, 각 단어는 모든 필드 OR. `"정확한 구문"`.
//! - 필드 지정: `제목:` `본문:` `보낸이:` `받는이:` `참조:` `숨은참조:` `첨부:`
//!   (영문 `subject: body: from: to: cc: bcc: attach:`). `제목:"월간 보고"`처럼 구문도 된다.
//! - 기간: `기간:2026-01-01..2026-03-31`, 한쪽 생략(`..2026-03-31`), 하루(`기간:2026-01-01`),
//!   `yyyymmdd`도 받는다. 날짜는 한국 시간 기준 하루 단위.

use crate::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FieldName {
    Subject,
    Body,
    From,
    To,
    Cc,
    Bcc,
    Attach,
}

impl FieldName {
    pub const ALL: [FieldName; 7] = [
        Self::Subject,
        Self::Body,
        Self::From,
        Self::To,
        Self::Cc,
        Self::Bcc,
        Self::Attach,
    ];

    pub fn schema_name(self) -> &'static str {
        match self {
            Self::Subject => "subject",
            Self::Body => "body",
            Self::From => "from",
            Self::To => "to",
            Self::Cc => "cc",
            Self::Bcc => "bcc",
            Self::Attach => "attach",
        }
    }

    fn from_prefix(p: &str) -> Option<Self> {
        Some(match p.to_lowercase().as_str() {
            "제목" | "subject" => Self::Subject,
            "본문" | "body" => Self::Body,
            "보낸이" | "보낸사람" | "from" => Self::From,
            "받는이" | "받는사람" | "to" => Self::To,
            "참조" | "cc" => Self::Cc,
            "숨은참조" | "bcc" => Self::Bcc,
            "첨부" | "attach" => Self::Attach,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Clause {
    /// 없으면 모든 필드.
    pub field: Option<FieldName>,
    pub text: String,
    /// 따옴표로 묶은 구문.
    pub phrase: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedQuery {
    pub clauses: Vec<Clause>,
    /// 받은 시각 하한(포함, unix ms).
    pub date_from: Option<i64>,
    /// 받은 시각 상한(미포함, unix ms).
    pub date_to: Option<i64>,
}

const KST_OFFSET_MS: i64 = 9 * 3600 * 1000;
const DAY_MS: i64 = 86_400_000;

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `2026-01-31` 또는 `20260131` → 그날 0시(한국 시간)의 unix ms.
fn parse_day(s: &str) -> Option<i64> {
    let digits: String = s.chars().filter(char::is_ascii_digit).collect();
    if digits.len() != 8
        || s.chars()
            .any(|c| !c.is_ascii_digit() && c != '-' && c != '.' && c != '/')
    {
        return None;
    }
    let (y, m, d) = (
        digits[..4].parse::<i64>().ok()?,
        digits[4..6].parse::<i64>().ok()?,
        digits[6..].parse::<i64>().ok()?,
    );
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    Some(days_from_civil(y, m, d) * DAY_MS - KST_OFFSET_MS)
}

fn parse_period(s: &str) -> Result<(Option<i64>, Option<i64>)> {
    let bad = || {
        Error::Query(format!(
            "기간 형식이 잘못됐습니다: {s} (예: 기간:2026-01-01..2026-03-31)"
        ))
    };
    match s.split_once("..") {
        Some((a, b)) => {
            let from = if a.is_empty() {
                None
            } else {
                Some(parse_day(a).ok_or_else(bad)?)
            };
            let to = if b.is_empty() {
                None
            } else {
                Some(parse_day(b).ok_or_else(bad)? + DAY_MS)
            };
            Ok((from, to))
        }
        None => {
            let day = parse_day(s).ok_or_else(bad)?;
            Ok((Some(day), Some(day + DAY_MS)))
        }
    }
}

/// 질의 문자열을 조항들로 나눈다.
pub fn parse_query(q: &str) -> Result<ParsedQuery> {
    let mut out = ParsedQuery::default();
    let chars: Vec<char> = q.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_whitespace() {
            i += 1;
            continue;
        }
        // 접두어(필드:) 후보를 읽는다.
        let start = i;
        let mut prefix: Option<String> = None;
        let mut j = i;
        while j < chars.len() && !chars[j].is_whitespace() && chars[j] != '"' {
            if chars[j] == ':' {
                prefix = Some(chars[start..j].iter().collect());
                break;
            }
            j += 1;
        }
        let field;
        let mut is_period = false;
        match prefix.as_deref() {
            Some(p) if FieldName::from_prefix(p).is_some() => {
                field = FieldName::from_prefix(p);
                i = j + 1;
            }
            Some("기간" | "period" | "date") => {
                field = None;
                is_period = true;
                i = j + 1;
            }
            _ => field = None,
        }
        // 값: 따옴표 구문 또는 공백까지.
        let (text, phrase) = if i < chars.len() && chars[i] == '"' {
            let mut k = i + 1;
            while k < chars.len() && chars[k] != '"' {
                k += 1;
            }
            let text: String = chars[i + 1..k].iter().collect();
            i = (k + 1).min(chars.len());
            (text, true)
        } else {
            let mut k = i;
            while k < chars.len() && !chars[k].is_whitespace() {
                k += 1;
            }
            let text: String = chars[i..k].iter().collect();
            i = k;
            (text, false)
        };
        if is_period {
            let (from, to) = parse_period(text.trim())?;
            out.date_from = from;
            out.date_to = to;
        } else if !text.trim().is_empty() {
            out.clauses.push(Clause {
                field,
                text,
                phrase,
            });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clause(field: Option<FieldName>, text: &str, phrase: bool) -> Clause {
        Clause {
            field,
            text: text.into(),
            phrase,
        }
    }

    #[test]
    fn words_fields_and_phrases() {
        let q = parse_query(r#"보고서  제목:"월간 회의" 숨은참조:audit from:kim 10:30"#).unwrap();
        assert_eq!(
            q.clauses,
            [
                clause(None, "보고서", false),
                clause(Some(FieldName::Subject), "월간 회의", true),
                clause(Some(FieldName::Bcc), "audit", false),
                clause(Some(FieldName::From), "kim", false),
                clause(None, "10:30", false),
            ]
        );
        let q = parse_query(r#""정확한 구문" 첨부:견적서"#).unwrap();
        assert_eq!(
            q.clauses,
            [
                clause(None, "정확한 구문", true),
                clause(Some(FieldName::Attach), "견적서", false)
            ]
        );
    }

    #[test]
    fn periods() {
        // 2026-01-01 0시 KST = 2025-12-31 15:00 UTC
        let jan1 = 1_767_225_600_000 - KST_OFFSET_MS;
        let q = parse_query("기간:2026-01-01..2026-01-31 회의").unwrap();
        assert_eq!(
            (q.date_from, q.date_to),
            (Some(jan1), Some(jan1 + 31 * DAY_MS))
        );
        assert_eq!(q.clauses.len(), 1);
        let q = parse_query("기간:20260101").unwrap();
        assert_eq!((q.date_from, q.date_to), (Some(jan1), Some(jan1 + DAY_MS)));
        let q = parse_query("기간:..2026-01-01").unwrap();
        assert_eq!((q.date_from, q.date_to), (None, Some(jan1 + DAY_MS)));
        assert!(parse_query("기간:어제").is_err());
    }
}
