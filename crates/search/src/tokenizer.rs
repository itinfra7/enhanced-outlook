//! `ko_bigram` 토크나이저(계획서 8.2). 색인과 질의에 같은 규칙을 쓴다.
//!
//! - 소문자화, NFC 정규화.
//! - 한글·한자·가나 연속 구간 → 겹치는 바이그램(1글자 구간은 그대로). "월간보고서" → 월간, 간보, 보고, 고서.
//! - 영문·숫자 → 단어 단위.
//! - 이메일 주소 → 통째 토큰 + `@`, `.` 등으로 나눈 조각(`kim.a@example.com` → 전체, kim, a, example, com).
//!   조각은 연속 위치라 "kim.a", "example.com" 같은 일부도 구문으로 찾힌다.
//! - 떨어진 단어·구간 사이에는 위치를 한 칸 비운다. 그래서 "보고 고서"가 "보고서" 구문으로 잘못 찾히지 않는다.
//! - 중간 글자 검색(0.7.0 사용자 요청: "검색어로 시작하지 않아도 찾아야"):
//!   - 색인: 영문·숫자 단어(4글자 이상)는 단어 토큰과 함께 세 글자 조각을 넣는다(첫 조각은 단어와 같은 위치,
//!     다음 조각부터 한 칸씩). 한중일 구간(2글자 이상)은 마지막 글자 하나를 마지막 바이그램과 같은 위치에 더 넣는다.
//!   - 질의([`tokenize_query`]): 영문·숫자 3글자 이상은 세 글자 조각만(연속 위치 → 구문이면 그 글자열이 단어 안
//!     어디에 있어도 찾힌다), 한중일 2글자 이상은 바이그램만. 1~2글자 영문·숫자와 한 글자 한중일은 앞부분 일치
//!     질의로 찾는다(조각·바이그램·마지막 글자 토큰 덕에 단어 중간·끝에 있어도 대부분 찾힌다).
//!   - 같은 자리에 겹쳐 넣으므로 색인과 질의의 다음 토큰 위치가 같다: 여러 단어 구문도 그대로 맞는다.

use tantivy::tokenizer::{Token, TokenStream, Tokenizer};
use unicode_normalization::UnicodeNormalization;

pub const TOKENIZER_NAME: &str = "ko_bigram";

/// 색인 형식. 토큰 규칙이 바뀌면 올린다(앱이 색인을 다시 만든다).
pub const INDEX_FORMAT: u32 = 2;

/// 질의 토큰: 앞부분 일치로 찾을 토큰인가(짧은 영문·숫자, 한 글자 한중일).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryToken {
    pub text: String,
    pub position: usize,
    pub prefix: bool,
}

/// 색인용인가 질의용인가.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Index,
    Query,
}

/// 이보다 긴 단어(base64 조각 등)는 색인하지 않는다.
const MAX_WORD_CHARS: usize = 48;

fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0xAC00..=0xD7A3   // 한글 음절
        | 0x1100..=0x11FF // 한글 자모
        | 0x3130..=0x318F // 호환용 자모
        | 0x3040..=0x30FF // 히라가나·가타카나
        | 0x3400..=0x4DBF // 한자 확장 A
        | 0x4E00..=0x9FFF // 한자
        | 0xF900..=0xFAFF // 호환 한자
    )
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() && !is_cjk(c)
}

/// 이메일·점 구분 단어(v2.1)에 들어가는 문자.
fn is_email_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '@' | '.' | '_' | '-' | '+' | '%')
}

struct Builder {
    tokens: Vec<Token>,
    position: usize,
    gap: bool,
    mode: Mode,
    /// 질의에서 앞부분 일치로 찾을 토큰의 순번.
    prefix: Vec<usize>,
}

impl Builder {
    /// 다음 토큰 앞에 위치 한 칸을 비운다(첫 토큰 앞은 비우지 않음).
    fn gap(&mut self) {
        self.gap = true;
    }

    fn push(&mut self, text: String, offset_from: usize, offset_to: usize) {
        if text.is_empty() || text.chars().count() > MAX_WORD_CHARS {
            return;
        }
        if self.gap && !self.tokens.is_empty() {
            self.position += 1;
        }
        self.gap = false;
        self.tokens.push(Token {
            offset_from,
            offset_to,
            position: self.position,
            text,
            position_length: 1,
        });
        self.position += 1;
    }

    /// 앞 토큰과 같은 위치에 겹쳐 넣는다(색인의 덧붙임 토큰).
    fn push_same(&mut self, text: String, offset_from: usize, offset_to: usize) {
        let position = self.position.saturating_sub(1);
        self.tokens.push(Token {
            offset_from,
            offset_to,
            position,
            text,
            position_length: 1,
        });
    }

    /// 영문·숫자 단어 하나. 색인: 단어 + 세 글자 조각(첫 조각은 단어 자리). 질의: 3글자 이상은 조각만,
    /// 짧으면 단어(앞부분 일치).
    fn word(&mut self, w: &str, from: usize) {
        if w.chars().count() > MAX_WORD_CHARS {
            return;
        }
        let chars: Vec<(usize, char)> = w.char_indices().collect();
        let tri = |k: usize| {
            let a = chars[k].0;
            let b = chars.get(k + 3).map_or(w.len(), |x| x.0);
            (w[a..b].to_owned(), from + a, from + b)
        };
        match (self.mode, chars.len()) {
            (Mode::Query, n) if n >= 3 => {
                for k in 0..=n - 3 {
                    let (t, a, b) = tri(k);
                    self.push(t, a, b);
                }
            }
            (Mode::Query, _) => {
                self.push(w.to_owned(), from, from + w.len());
                self.prefix.push(self.tokens.len() - 1);
            }
            (Mode::Index, n) => {
                self.push(w.to_owned(), from, from + w.len());
                if n >= 4 {
                    let (t, a, b) = tri(0);
                    self.push_same(t, a, b);
                    for k in 1..=n - 3 {
                        let (t, a, b) = tri(k);
                        self.push(t, a, b);
                    }
                }
            }
        }
    }

    /// 단어 단위 + 한중일 바이그램. `gaps`면 구간 사이에 위치를 비운다
    /// (이메일 문자로만 된 구간 안의 조각은 비우지 않는다).
    fn plain(&mut self, s: &str, base: usize, gaps: bool) {
        let chars: Vec<(usize, char)> = s.char_indices().collect();
        let mut i = 0;
        while i < chars.len() {
            let (start, c) = chars[i];
            if is_cjk(c) {
                if gaps {
                    self.gap();
                }
                let mut j = i;
                while j < chars.len() && is_cjk(chars[j].1) {
                    j += 1;
                }
                let run = &chars[i..j];
                if run.len() == 1 {
                    self.push(c.to_string(), base + start, base + start + c.len_utf8());
                    if self.mode == Mode::Query {
                        self.prefix.push(self.tokens.len() - 1);
                    }
                } else {
                    for w in run.windows(2) {
                        let end = w[1].0 + w[1].1.len_utf8();
                        self.push(format!("{}{}", w[0].1, w[1].1), base + w[0].0, base + end);
                    }
                    // 마지막 글자도(한 글자 검색이 낱말 끝에서도 찾히게).
                    if self.mode == Mode::Index {
                        let (at, last) = run[run.len() - 1];
                        self.push_same(last.to_string(), base + at, base + at + last.len_utf8());
                    }
                }
                i = j;
            } else if is_word(c) {
                if gaps {
                    self.gap();
                }
                let mut j = i;
                while j < chars.len() && is_word(chars[j].1) {
                    j += 1;
                }
                let end = chars.get(j).map_or(s.len(), |x| x.0);
                self.word(&s[start..end], base + start);
                i = j;
            } else {
                i += 1;
            }
        }
    }
}

/// 이메일처럼 보이는가: `로컬@도메인.최상위`.
fn looks_like_email(s: &str) -> bool {
    let Some((local, domain)) = s.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && domain.contains('.')
        && !domain.contains('@')
        && local.chars().any(|c| c.is_ascii_alphanumeric())
        && domain.split('.').all(|p| !p.is_empty())
}

/// 색인용 토큰(덧붙임 토큰 포함).
pub fn tokenize(text: &str) -> Vec<Token> {
    run(text, Mode::Index).tokens
}

/// 질의용 토큰: 영문·숫자 3글자 이상은 세 글자 조각, 짧은 것과 한 글자 한중일은 앞부분 일치.
pub fn tokenize_query(text: &str) -> Vec<QueryToken> {
    let b = run(text, Mode::Query);
    b.tokens
        .into_iter()
        .enumerate()
        .map(|(i, t)| QueryToken {
            prefix: b.prefix.contains(&i),
            text: t.text,
            position: t.position,
        })
        .collect()
}

fn run(text: &str, mode: Mode) -> Builder {
    let norm: String = text.nfc().collect::<String>().to_lowercase();
    let mut b = Builder {
        tokens: Vec::new(),
        position: 0,
        gap: false,
        mode,
        prefix: Vec::new(),
    };
    let chars: Vec<(usize, char)> = norm.char_indices().collect();
    let mut i = 0;
    while i < chars.len() {
        let email_seg = is_email_char(chars[i].1);
        let mut j = i;
        while j < chars.len() && is_email_char(chars[j].1) == email_seg {
            j += 1;
        }
        let start = chars[i].0;
        let end = chars.get(j).map_or(norm.len(), |x| x.0);
        let raw = &norm[start..end];
        if email_seg {
            b.gap();
            let seg = raw.trim_matches(|c: char| !c.is_ascii_alphanumeric());
            let seg_start = start + raw.find(seg).unwrap_or(0);
            if looks_like_email(seg) {
                b.push(seg.to_owned(), seg_start, seg_start + seg.len());
                let mut off = seg_start;
                for part in seg.split(|c: char| !c.is_ascii_alphanumeric()) {
                    if !part.is_empty() {
                        b.word(part, off);
                    }
                    off += part.len() + 1; // 구분 문자는 모두 ASCII 한 글자
                }
            } else {
                b.plain(raw, start, false);
            }
        } else {
            b.plain(raw, start, true);
        }
        i = j;
    }
    b
}

#[derive(Clone, Default)]
pub struct KoBigramTokenizer;

pub struct KoBigramStream {
    tokens: Vec<Token>,
    index: usize,
}

impl Tokenizer for KoBigramTokenizer {
    type TokenStream<'a> = KoBigramStream;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> KoBigramStream {
        KoBigramStream {
            tokens: tokenize(text),
            index: 0,
        }
    }
}

impl TokenStream for KoBigramStream {
    fn advance(&mut self) -> bool {
        if self.index < self.tokens.len() {
            self.index += 1;
            true
        } else {
            false
        }
    }

    fn token(&self) -> &Token {
        &self.tokens[self.index - 1]
    }

    fn token_mut(&mut self) -> &mut Token {
        &mut self.tokens[self.index - 1]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(s: &str) -> Vec<String> {
        tokenize(s).into_iter().map(|t| t.text).collect()
    }

    fn positions(s: &str) -> Vec<usize> {
        tokenize(s).into_iter().map(|t| t.position).collect()
    }

    fn query(s: &str) -> Vec<(String, usize, bool)> {
        tokenize_query(s)
            .into_iter()
            .map(|t| (t.text, t.position, t.prefix))
            .collect()
    }

    #[test]
    fn korean_bigrams() {
        assert_eq!(texts("월간보고서"), ["월간", "간보", "보고", "고서", "서"]);
        assert_eq!(
            positions("월간보고서"),
            [0, 1, 2, 3, 3],
            "마지막 글자는 마지막 바이그램 자리"
        );
        assert_eq!(texts("보고서"), ["보고", "고서", "서"]);
        assert_eq!(texts("팀 회의"), ["팀", "회의", "의"]);
    }

    #[test]
    fn separate_words_leave_a_gap() {
        assert_eq!(
            positions("보고 고서"),
            [0, 0, 2, 2],
            "떨어진 단어는 구문으로 이어지지 않는다"
        );
        assert_eq!(
            positions("월간, 회의"),
            positions("월간 회의"),
            "구분 문자 종류와 무관"
        );
    }

    #[test]
    fn words_numbers_mixed() {
        assert_eq!(
            texts("Q3 Report, 2026년"),
            [
                "q3", "report", "rep", "epo", "por", "ort", "2026", "202", "026", "년"
            ]
        );
        assert_eq!(
            positions("Q3 Report"),
            [0, 2, 2, 3, 4, 5],
            "세 글자 조각은 단어 자리부터"
        );
        assert_eq!(texts("PL팀"), ["pl", "팀"]);
        assert_eq!(texts("v2.1 a.b"), ["v2", "1", "a", "b"]);
        assert_eq!(positions("v2.1"), [0, 1], "점 구분 단어 안은 붙어 있다");
    }

    #[test]
    fn email_whole_and_parts() {
        assert_eq!(
            texts("받는이: kim.a@example.com,")[..7],
            [
                "받는",
                "는이",
                "이",
                "kim.a@example.com",
                "kim",
                "a",
                "example"
            ]
        );
        assert_eq!(
            positions("kim.a@xy.org"),
            [0, 1, 2, 3, 4],
            "조각은 연속 위치"
        );
        assert_eq!(
            positions("kim.a"),
            [0, 1],
            "질의 'kim.a'도 조각처럼 붙어 있다"
        );
    }

    #[test]
    fn query_tokens_find_inside_words() {
        // 3글자 이상 영문·숫자는 세 글자 조각(연속 위치), 짧으면 앞부분 일치.
        assert_eq!(
            query("broadband"),
            ["bro", "roa", "oad", "adb", "dba", "ban", "and"]
                .iter()
                .enumerate()
                .map(|(i, t)| ((*t).to_owned(), i, false))
                .collect::<Vec<_>>()
        );
        assert_eq!(query("ai"), [("ai".to_owned(), 0, true)]);
        assert_eq!(query("팀"), [("팀".to_owned(), 0, true)]);
        assert_eq!(
            query("보고서"),
            [("보고".to_owned(), 0, false), ("고서".to_owned(), 1, false)]
        );
        // 질의 조각의 위치는 색인 조각의 위치와 같은 간격이다(구문이 맞는다).
        let idx: Vec<(String, usize)> = tokenize("fastbroadband.com")
            .into_iter()
            .map(|t| (t.text, t.position))
            .collect();
        let at = |s: &str| idx.iter().find(|(t, _)| t == s).unwrap().1;
        let q = query("broadband.com");
        assert_eq!(at("bro") + 6, at("and"));
        assert_eq!(at("and") + 1, at("com"));
        assert_eq!((q[0].1 + 6, q[6].1 + 1), (q[6].1, q[7].1));
    }

    #[test]
    fn nfc_and_lowercase() {
        // 자모 조합형 "한" (ᄒ+ᅡ+ᆫ) → 완성형
        assert_eq!(texts("\u{1112}\u{1161}\u{11AB}글"), ["한글", "글"]);
        assert_eq!(texts("ABC"), ["abc"]);
    }

    #[test]
    fn long_garbage_is_skipped() {
        let long = "a".repeat(100);
        assert!(texts(&long).is_empty());
    }
}
