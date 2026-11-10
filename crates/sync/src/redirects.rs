//! HCL Domino 리다이렉트 사본 찾기(1.1.0 사용자 요청).
//!
//! 회사 정책으로 Outlook에 온 메일이 HCL Domino에도 그대로 한 통 더 온다. Outlook 메일과 짝이 확실한 HCL Domino
//! 사본만 고른다(Outlook 메일은 읽기만 한다). 짝 조건(모두 맞아야 함, 2026-11-10 실데이터 확인):
//! - 제목이 같다(공백만 정리). 보낸 사람 아이디(@ 앞)가 같다: 도메인은 다르게 온다(Outlook `x@p.skbroadband.com`,
//!   HCL Domino `x@skbroadband.com`).
//! - 받은 시각 차이가 [`WINDOW_MS`] 안(실측 0~11초).
//! - 서로 하나씩만 맞는다(1:1). 같은 제목·보낸 사람 메일이 몇 분 안에 여럿이면 건너뛴다.
//! - 두 원문의 본문 글(공백 제외)과 첨부 이름이 같다. 원문이 없으면 다음에 다시 본다.
//!
//! 서버에서 지우기 직전에 HCL Domino 문서를 다시 열어 Notes 안에서 보낸 메일(`SenderText`가 있음)이 아님을 한 번
//! 더 확인한다(앱 쪽).

use std::collections::HashMap;

use eo_store::{RedirectDomino, Store};

use crate::Result;

/// 받은 시각 차이 한도.
pub const WINDOW_MS: i64 = 120_000;

/// 짝을 찾은 HCL Domino 사본.
#[derive(Debug, Clone, PartialEq)]
pub struct RedirectCopy {
    pub domino: RedirectDomino,
    /// 짝인 Outlook 메일(지우지 않는다).
    pub outlook_message_id: i64,
    /// 본문이 비슷한 정도(0~1).
    pub similarity: f64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct RedirectScan {
    pub copies: Vec<RedirectCopy>,
    /// 살펴본 HCL Domino 메일 수.
    pub scanned: usize,
    /// 짝 후보가 여럿이라 건너뛴 수.
    pub ambiguous: usize,
    /// 머리글은 맞지만 본문·첨부가 달라 건너뛴 수.
    pub body_differs: usize,
    /// 그 짝들(HCL Domino 메일 번호, Outlook 메일 번호, 비슷한 정도, 첨부 이름 같음, 진단용).
    pub differing: Vec<(i64, i64, f64, bool)>,
    /// 원문이 아직 없어 비교하지 못한 수.
    pub without_body: usize,
}

fn subject_key(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn sender_key(addr: &str) -> String {
    addr.trim().split('@').next().unwrap_or("").to_lowercase()
}

/// 원문의 본문 비교값(글자·숫자만 소문자로 + 그림이 아닌 첨부 이름). HCL Domino 사본은 원문을 HTML에서 다시 만든
/// 것이라 줄바꿈·구분선·공백이 다르고, 이름 없는 본문 그림은 `ATT00001.gif`처럼 이름이 붙는다: 양쪽 모두 HTML 본문을
/// 글로 바꿔 문장 부호·공백을 빼고 견주고, 첨부는 그림을 빼고 이름만 견준다.
fn body_key(store: &Store, hash: Option<&str>) -> Option<(Vec<char>, Vec<String>)> {
    let raw = store.eml().get(hash?).ok()?;
    let o = eo_mime::compose::original(&raw)?;
    let text: Vec<char> = eo_mime::compose::html_to_text(&o.body_html)
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    let mut names: Vec<String> = o
        .attachments
        .iter()
        .filter(|a| !a.mime.to_ascii_lowercase().starts_with("image/"))
        .map(|a| a.name.clone())
        .collect();
    names.sort();
    Some((text, names))
}

/// 두 글의 비슷한 정도(0~1): 두 글자 묶음의 다이스 계수.
pub fn similarity(a: &[char], b: &[char]) -> f64 {
    if a == b {
        return 1.0;
    }
    if a.len() < 2 || b.len() < 2 {
        return 0.0;
    }
    let mut counts: HashMap<(char, char), i64> = HashMap::new();
    for w in a.windows(2) {
        *counts.entry((w[0], w[1])).or_default() += 1;
    }
    let mut common = 0_i64;
    for w in b.windows(2) {
        if let Some(n) = counts.get_mut(&(w[0], w[1]))
            && *n > 0
        {
            *n -= 1;
            common += 1;
        }
    }
    2.0 * common as f64 / (a.len() - 1 + b.len() - 1) as f64
}

/// 본문이 같은 메일로 볼 비슷한 정도(2026-11-10 실데이터: 짝 77통 모두 1.000).
pub const MIN_SIMILARITY: f64 = 0.98;

/// 리다이렉트 사본을 찾는다(읽기만).
pub fn find_redirect_copies(store: &Store) -> Result<RedirectScan> {
    let (domino, outlook) = store.redirect_rows(WINDOW_MS)?;
    let mut scan = RedirectScan {
        scanned: domino.len(),
        ..RedirectScan::default()
    };
    let key = |subject: &str, from: &str| (subject_key(subject), sender_key(from));
    let mut by_key: HashMap<(String, String), Vec<usize>> = HashMap::new();
    for (i, o) in outlook.iter().enumerate() {
        by_key
            .entry(key(&o.subject, &o.from_addr))
            .or_default()
            .push(i);
    }
    let mut dom_by_key: HashMap<(String, String), Vec<usize>> = HashMap::new();
    for (i, d) in domino.iter().enumerate() {
        dom_by_key
            .entry(key(&d.subject, &d.from_addr))
            .or_default()
            .push(i);
    }
    for d in &domino {
        let k = key(&d.subject, &d.from_addr);
        if k.0.is_empty() || k.1.is_empty() {
            continue;
        }
        let near = |a: i64, b: i64| (a - b).abs() <= WINDOW_MS;
        let partners: Vec<usize> = by_key
            .get(&k)
            .map(|v| {
                v.iter()
                    .copied()
                    .filter(|&i| near(outlook[i].date, d.date))
                    .collect()
            })
            .unwrap_or_default();
        if partners.is_empty() {
            continue;
        }
        // 1:1: 짝 Outlook 메일도 이 HCL Domino 메일 하나만 가리켜야 한다.
        let rivals = dom_by_key.get(&k).map_or(0, |v| {
            v.iter()
                .filter(|&&j| near(domino[j].date, outlook[partners[0]].date))
                .count()
        });
        if partners.len() != 1 || rivals != 1 {
            scan.ambiguous += 1;
            continue;
        }
        let o = &outlook[partners[0]];
        match (
            body_key(store, d.eml_hash.as_deref()),
            body_key(store, o.eml_hash.as_deref()),
        ) {
            (Some((ta, na)), Some((tb, nb))) => {
                let sim = similarity(&ta, &tb);
                if na == nb && sim >= MIN_SIMILARITY {
                    scan.copies.push(RedirectCopy {
                        domino: d.clone(),
                        outlook_message_id: o.message_id,
                        similarity: sim,
                    });
                } else {
                    scan.body_differs += 1;
                    scan.differing
                        .push((d.message_id, o.message_id, sim, na == nb));
                }
            }
            _ => scan.without_body += 1,
        }
    }
    Ok(scan)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chars(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    #[test]
    fn keys_and_similarity() {
        assert_eq!(sender_key(" YWChon@p.skbroadband.com"), "ywchon");
        assert_eq!(sender_key("ywchon@skbroadband.com"), "ywchon");
        assert_eq!(subject_key("RE:  [보안]   공지 "), "RE: [보안] 공지");
        let a = chars("안녕하세요전용우입니다아래vm요청건입니다");
        assert_eq!(similarity(&a, &a), 1.0);
        let b = chars("안녕하세요전용우입니다아래vm요청건입니다감사합니다");
        assert!(similarity(&a, &b) > 0.8 && similarity(&a, &b) < MIN_SIMILARITY);
        assert!(similarity(&a, &chars("전혀다른메일본문입니다")) < 0.5);
        assert_eq!(similarity(&chars("a"), &chars("b")), 0.0);
    }
}
