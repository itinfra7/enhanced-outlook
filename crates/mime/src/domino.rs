//! 레거시 HCL Domino 메일 백업(HTML)에서 EML 복원(사용자 요청 2026-10-16).
//!
//! HCL Domino에서 **다운로드 형식 HTML, 이미지 다운로드 "다운로드 함", 첨부 다운로드 "다운로드 함", 연결하기 "지원함"**으로
//! 내려받은 백업의 형식:
//!
//! ```text
//! {메일함}\{기간}\{제목}_${번호}$_.html        기간은 "2021년 02월" 또는 "2018년"
//! {메일함}\{기간}\$$_Attach_$$\{원래 이름}_${번호}$_${순번}$_.{확장자}   첨부(본문이 쓰는 이미지도 일부)
//! {메일함}\{기간}\$$_Image_$$\{원래 이름}_${번호}$_${순번}$_.{확장자}    본문 이미지
//! ```
//!
//! HTML은 머리글 표(`class="tit"` 칸: 발신인·수신일·수신인·참조인·제목·첨부N·본문)와 숨은 입력값
//! (`BCC`, `REPLY_TO`, `IMPORTANCE`, `MESSAGE_ID`, `FILEID`)으로 되어 있다. 주소는 `이름 [주소]`를 `;`로 잇는다.
//! "연결하기"를 켜 두면 본문·첨부 칸이 위 파일을 `$$_Image_$$\…`·`$$_Attach_$$\…` 상대 경로(`\` 구분, 인코딩 없음)로 가리킨다.
//! 문자셋은 파일마다 UTF-8·EUC-KR·ks_c_5601-1987이다.
//!
//! 유저스크립트 백업과 달리 원래 Message-ID와 숨은 참조가 남아 있어, 그것으로 EML을 만든다.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

use crate::backup::{NewEml, build_eml, mime_by_name, parse_kst, short_key};
use crate::compose::{OriginalPart, Person};

/// 첨부·이미지 폴더 이름.
pub const ATTACH_DIR: &str = "$$_Attach_$$";
pub const IMAGE_DIR: &str = "$$_Image_$$";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DominoMail {
    pub subject: String,
    pub from: Vec<Person>,
    pub to: Vec<Person>,
    pub cc: Vec<Person>,
    pub bcc: Vec<Person>,
    pub reply_to: Vec<Person>,
    /// 수신일(한국 시간).
    pub received: Option<i64>,
    /// `<…>` 형식. 비어 있던 메일도 있다.
    pub message_id: Option<String>,
    pub file_id: Option<String>,
    /// 본문 HTML(이미지는 백업 안 상대 경로).
    pub body_html: String,
    /// 첨부N 칸: (표시 이름, 백업 안 상대 경로).
    pub attachments: Vec<(String, String)>,
}

/// 복원하면서 센 것(가져오기 보고용).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RestoreStats {
    pub inline_images: usize,
    pub attachments: usize,
    /// 본문·첨부 칸이 가리키는데 백업에 없는 파일.
    pub missing_files: usize,
    /// 이 메일 번호의 파일인데 아무도 가리키지 않아 첨부로 붙인 것(잃지 않게).
    pub extra_files: usize,
    /// 첨부 칸에 이름만 있고 백업 폴더를 가리키지 않는 첨부(내려받지 않고 저장한 것).
    pub not_downloaded: usize,
}

/// HTML 바이트 → 글자.
///
/// HCL Domino 백업은 한 파일 안에 문자셋이 섞여 있기도 하다(실데이터 약 27%). meta는 원래 메일의 `ks_c_5601-1987`인데
/// 머리글 표는 UTF-8이고 본문은 원래 EUC-KR 바이트 그대로인 식이다. 그래서
/// - 파일 전체가 올바른 UTF-8이면 UTF-8로 읽고,
/// - 아니면 ASCII가 아닌 바이트가 이어진 덩어리(대개 낱말 하나)마다, 올바른 UTF-8이면 UTF-8로, 아니면
///   `<meta charset>`(앞 4KB)이 가리키는 문자셋(없거나 UTF-8이라고 하면 EUC-KR=CP949)으로 읽는다.
///
/// EUC-KR 한글 바이트열이 UTF-8로 우연히 맞는 일은 거의 없어서 이 판정이 안전하다.
pub fn decode_html(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(rest).into_owned();
    }
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.to_owned();
    }
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(4096)]).to_ascii_lowercase();
    let declared = head.find("charset").and_then(|i| {
        let rest = head[i + 7..].trim_start_matches([' ', '=', '"', '\'']);
        let end = rest.find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))?;
        encoding_rs::Encoding::for_label(&rest.as_bytes()[..end])
    });
    let fallback = match declared {
        Some(e) if e != encoding_rs::UTF_8 => e,
        _ => encoding_rs::EUC_KR,
    };
    let mut out = String::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let ascii_end = bytes[i..]
            .iter()
            .position(|b| !b.is_ascii())
            .map_or(bytes.len(), |p| i + p);
        // ASCII 부분은 그대로.
        out.push_str(std::str::from_utf8(&bytes[i..ascii_end]).unwrap_or_default());
        if ascii_end == bytes.len() {
            break;
        }
        let run_end = bytes[ascii_end..]
            .iter()
            .position(u8::is_ascii)
            .map_or(bytes.len(), |p| ascii_end + p);
        let run = &bytes[ascii_end..run_end];
        let legacy = fallback.decode_without_bom_handling(run).0;
        match std::str::from_utf8(run) {
            // 짧은 EUC-KR 덩어리는 우연히 UTF-8로도 맞는다(예: "표" C7 A5 = "ǥ"). 둘 다 풀어 그럴듯한 쪽을 고른다.
            Ok(s) if plausibility(s) >= plausibility(&legacy) => out.push_str(s),
            _ => out.push_str(&legacy),
        }
        i = run_end;
    }
    out
}

/// 한국어 메일 글자로 그럴듯한 정도: 한글·한국어 문장부호 +1, 깨진 글자 −2,
/// 한국어 메일에 드문 문자(라틴 확장·그리스·키릴 등, EUC-KR을 UTF-8로 잘못 읽으면 나온다) −1, 한자·×·÷ 등은 0.
/// 같으면 UTF-8을 고른다(UTF-8 가운뎃점 "·"와 EUC-KR 한 글자가 같은 바이트일 때).
fn plausibility(s: &str) -> i32 {
    s.chars()
        .map(|c| match c as u32 {
            0xFFFD => -2,
            0xAC00..=0xD7A3 | 0x1100..=0x11FF | 0x3130..=0x318F => 1,
            0x00A0..=0x00BF => 1,
            0x00D7 | 0x00F7 => 0,
            0x2000..=0x206F
            | 0x2190..=0x23FF
            | 0x2460..=0x26FF
            | 0x3000..=0x303F
            | 0xFF00..=0xFFEF => 1,
            0x00C0..=0x07FF => -1,
            _ => 0,
        })
        .sum()
}

/// HTML 문자 참조 풀기(`&lt;`, `&#44032;`, `&#xAC00;`, `&nbsp;` 등).
pub fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_owned();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        // 바이트로 찾는다: 12바이트에서 자르면 한글 글자 가운데일 수 있다(실데이터에서 멈춘 일이 있다).
        let Some(end) = rest.as_bytes()[..rest.len().min(12)]
            .iter()
            .position(|&b| b == b';')
        else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let name = &rest[1..end];
        let ch = match name {
            "lt" => Some('<'),
            "gt" => Some('>'),
            "amp" => Some('&'),
            "quot" => Some('"'),
            "apos" | "#39" => Some('\''),
            "nbsp" => Some(' '),
            _ => name
                .strip_prefix("#x")
                .or_else(|| name.strip_prefix("#X"))
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| name.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match ch {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

/// 칸 글자: 태그를 빼고 문자 참조를 풀고 공백을 하나로.
fn cell_text(s: &str) -> String {
    decode_entities(&strip_tags(s))
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn find_ci(hay: &str, needle: &str, from: usize) -> Option<usize> {
    // 찾는 것은 ASCII뿐이라 바이트 단위로 대소문자를 무시해도 글자 경계가 맞는다.
    let (h, n) = (hay.as_bytes(), needle.as_bytes());
    (from..=h.len().checked_sub(n.len())?).find(|&i| h[i..i + n.len()].eq_ignore_ascii_case(n))
}

fn is_email_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '%' | '+' | '-' | '\'')
}

/// 글 안의 첫 이메일 주소.
fn first_email(s: &str) -> Option<String> {
    let at = s.find('@')?;
    let start = s[..at]
        .char_indices()
        .rev()
        .take_while(|(_, c)| is_email_char(*c))
        .last()
        .map_or(at, |(i, _)| i);
    let end = s[at + 1..]
        .char_indices()
        .take_while(|(_, c)| is_email_char(*c))
        .last()
        .map_or(at + 1, |(i, c)| at + 1 + i + c.len_utf8());
    let email = s[start..end].trim_matches('.');
    (email.len() > 2 && email.contains('@') && !email.ends_with('@')).then(|| email.to_owned())
}

/// `이름 [주소]; 이름 [주소 [주소]]; 이름 <주소>; 주소` → 사람들.
/// 주소가 없는 노츠 이름(`CN=홍길동/OU=…/O=…`, 실데이터 17통의 발신인)은 이름만 남긴다(주소는 빈 값).
pub fn parse_people(s: &str) -> Vec<Person> {
    s.split(';')
        .filter_map(|part| {
            let part = part.trim().trim_matches('\'').trim();
            let Some(email) = first_email(part) else {
                let name = match find_ci(part, "CN=", 0) {
                    Some(i) => part[i + 3..].split('/').next().unwrap_or(""),
                    None => part,
                };
                let name = name.trim().trim_matches(['"', '\'']).trim();
                return (!name.is_empty()).then(|| Person {
                    name: name.to_owned(),
                    email: String::new(),
                });
            };
            let cut = part.find(['[', '<']).unwrap_or(part.len());
            let mut name = part[..cut]
                .trim()
                .trim_matches(['"', '\''])
                .trim()
                .to_owned();
            if name == email || name.contains('@') {
                name.clear();
            }
            Some(Person { name, email })
        })
        .collect()
}

fn hidden_value(doc: &str, name: &str) -> Option<String> {
    let key = format!("name=\"{name}\"");
    let i = find_ci(doc, &key, 0)?;
    let v = find_ci(doc, "value=\"", i)? + 7;
    // 값 안의 <, >는 이스케이프되지 않았다. 닫는 따옴표까지가 값이다.
    let end = doc[v..].find('"')? + v;
    let value = decode_entities(doc[v..end].trim());
    (!value.is_empty()).then_some(value)
}

/// 머리글 칸 이름인가(본문 안의 같은 class 이름과 헷갈리지 않게, 아는 이름만).
fn known_label(label: &str) -> bool {
    matches!(
        label,
        "발신인" | "수신일" | "수신인" | "참조인" | "제목" | "본문" | "내용"
    ) || label
        .strip_prefix("첨부")
        .is_some_and(|n| n.chars().all(|c| c.is_ascii_digit()))
}

/// `from` 뒤의 다음 머리글 칸: (`class="tit"` 위치, 공백을 뺀 칸 이름).
fn next_header(doc: &str, from: usize) -> Option<(usize, String)> {
    let mut pos = from;
    while let Some(i) = find_ci(doc, "class=\"tit\"", pos) {
        let label_start = doc[i..].find('>')? + i + 1;
        let label_end = find_ci(doc, "</td>", label_start)?;
        let label: String = cell_text(&doc[label_start..label_end])
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        if known_label(&label) {
            return Some((i, label));
        }
        pos = label_end;
    }
    None
}

/// 백업 프로그램이 칸 이름을 `???`로 써 버린 파일(실데이터 5통)이면, 늘 같은 칸 순서로 이름을 되돌린 사본.
fn repair_question_labels(doc: &str) -> Option<String> {
    const ORDER: [&str; 6] = ["발신인", "수신일", "수신인", "참조인", "제목", "본문"];
    let mut out = String::with_capacity(doc.len());
    let (mut last, mut pos, mut k, mut repaired) = (0, 0, 0, false);
    while let Some(i) = find_ci(doc, "class=\"tit\"", pos) {
        let label_start = doc[i..].find('>')? + i + 1;
        let label_end = find_ci(doc, "</td>", label_start)?;
        let text = cell_text(&doc[label_start..label_end]);
        pos = label_end;
        // `?`만 있는 칸만 센다(본문 안에 같은 class를 쓴 칸은 그대로 둔다).
        if text.is_empty() || !text.chars().all(|c| c == '?' || c.is_whitespace()) {
            continue;
        }
        let name = ORDER.get(k).map_or_else(
            || format!("첨부{}", k + 1 - ORDER.len()),
            |s| (*s).to_owned(),
        );
        out.push_str(&doc[last..label_start]);
        out.push_str(&name);
        last = label_end;
        k += 1;
        repaired = true;
    }
    out.push_str(&doc[last..]);
    repaired.then_some(out)
}

/// HCL Domino 백업 HTML이면 해석한다(아니면 `None`).
pub fn parse_domino_html(doc: &str) -> Option<DominoMail> {
    if find_ci(doc, "name=\"FILEID\"", 0).is_none()
        && find_ci(doc, "name=\"MESSAGE_ID\"", 0).is_none()
    {
        return None;
    }
    if next_header(doc, 0).is_none() {
        let fixed = repair_question_labels(doc)?;
        return parse_known_labels(&fixed);
    }
    parse_known_labels(doc)
}

fn parse_known_labels(doc: &str) -> Option<DominoMail> {
    let mut m = DominoMail::default();
    let mut seen_from = false;
    let mut pos = 0;
    while let Some((i, label)) = next_header(doc, pos) {
        let label_end = find_ci(doc, "</td>", i)?;
        let td = find_ci(doc, "<td", label_end)?;
        let value_start = doc[td..].find('>')? + td + 1;
        if label == "본문" || label == "내용" {
            // 본문 칸은 메일 HTML 문서를 통째로 품고 있다(안에 표가 있어 </td>로는 끝을 알 수 없다).
            // 뒤에 첨부 칸이 오기도 해서, 다음 머리글 칸이 있으면 그 앞, 없으면 숨은 입력값 앞의 마지막 </td>까지다.
            let stop = find_ci(doc, "<input type=\"hidden\"", value_start).unwrap_or(doc.len());
            let next = next_header(doc, value_start).filter(|(t, _)| *t < stop);
            let limit = next.as_ref().map_or(stop, |(t, _)| *t);
            let end = doc[value_start..limit]
                .to_ascii_lowercase()
                .rfind("</td>")
                .map_or(limit, |e| value_start + e);
            let mut body = doc[value_start..end].trim();
            if body.len() >= 3 && body[..3].eq_ignore_ascii_case("<p>") {
                body = body[3..].trim_start();
            }
            if body.len() >= 4 && body[body.len() - 4..].eq_ignore_ascii_case("</p>") {
                body = body[..body.len() - 4].trim_end();
            }
            m.body_html = body.to_owned();
            match next {
                Some((t, _)) => {
                    pos = t;
                    continue;
                }
                None => break,
            }
        }
        let value_end = find_ci(doc, "</td>", value_start)?;
        let raw = &doc[value_start..value_end];
        match label.as_str() {
            "발신인" => {
                m.from = parse_people(&cell_text(raw));
                seen_from = true;
            }
            "수신인" => m.to = parse_people(&cell_text(raw)),
            "참조인" => m.cc = parse_people(&cell_text(raw)),
            "수신일" => m.received = parse_kst(&cell_text(raw)),
            "제목" => m.subject = cell_text(raw),
            l if l.starts_with("첨부") => {
                // 칸 하나에 링크가 여럿일 수도 있다: <a href="경로">이름</a>마다.
                // 링크가 없으면(연결하기를 끄고 저장) 이름만 남긴다.
                if find_ci(raw, "href=\"", 0).is_none() {
                    let name = cell_text(raw);
                    if !name.is_empty() {
                        m.attachments.push((name, String::new()));
                    }
                }
                let mut at = 0;
                while let Some(h) = find_ci(raw, "href=\"", at) {
                    let h = h + 6;
                    let Some(e) = raw[h..].find('"') else { break };
                    let href = decode_entities(&raw[h..h + e]);
                    let text_start = raw[h + e..].find('>').map_or(h + e, |x| h + e + x + 1);
                    let text_end = find_ci(raw, "</a>", text_start).unwrap_or(raw.len());
                    let name = cell_text(&raw[text_start..text_end]);
                    m.attachments.push((name, href));
                    at = text_end;
                }
            }
            _ => {}
        }
        pos = value_end;
    }
    if !seen_from {
        return None;
    }
    m.bcc = hidden_value(doc, "BCC")
        .map(|v| parse_people(&v))
        .unwrap_or_default();
    m.reply_to = hidden_value(doc, "REPLY_TO")
        .map(|v| crate::backup::parse_people(&v))
        .unwrap_or_default();
    m.message_id = hidden_value(doc, "MESSAGE_ID").map(|v| {
        let v = v.trim_matches(['<', '>']);
        format!("<{v}>")
    });
    m.file_id = hidden_value(doc, "FILEID");
    Some(m)
}

/// 상대 경로 비교용: 구분자를 `\`로, 대소문자 무시.
fn norm_rel(s: &str) -> String {
    s.trim().replace('/', "\\").to_lowercase()
}

/// `이름_$12$_$3$_.확장자` → `이름.확장자`(백업이 붙인 번호를 뗀 원래 이름).
pub fn original_name(file: &str) -> String {
    let base = file.rsplit(['\\', '/']).next().unwrap_or(file);
    let Some(i) = base.find("_$") else {
        return base.to_owned();
    };
    let after = &base[i..];
    // 번호 덩어리(_$숫자$_$숫자$_) 뒤에 남는 것이 확장자다(".xlsx", 없을 수도 있음).
    let ext = after.rfind("$_").map_or("", |j| &after[j + 2..]);
    let ext = ext.trim();
    if ext.is_empty() || ext == "." {
        base[..i].to_owned()
    } else if let Some(e) = ext.strip_prefix('.') {
        format!("{}.{}", &base[..i], e.trim())
    } else {
        format!("{}{}", &base[..i], ext)
    }
}

/// 내용으로 그림 형식을 알아본다(백업의 이미지 확장자가 깨진 것이 있다).
pub(crate) fn sniff_image(data: &[u8]) -> Option<&'static str> {
    if data.starts_with(&[0x89, b'P', b'N', b'G']) {
        Some("image/png")
    } else if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if data.starts_with(b"GIF8") {
        Some("image/gif")
    } else if data.starts_with(b"BM") {
        Some("image/bmp")
    } else if data.len() > 12 && &data[..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

/// HCL Domino 메일 한 통을 EML로 복원한다.
/// - `files`: 이 메일 번호의 첨부·이미지 파일(메일 HTML 기준 상대 경로, 예: `$$_Attach_$$\보고서_$12$_$3$_.xlsx`).
/// - `read(상대 경로)`: 그 파일 내용.
/// - `key`: Message-ID가 없을 때 대신 만들 값의 재료(FILEID가 있으면 그것을 쓴다).
pub fn restore_domino_eml(
    m: &DominoMail,
    files: &[String],
    mut read: impl FnMut(&str) -> Option<Vec<u8>>,
    key: &str,
) -> (Vec<u8>, RestoreStats) {
    let mut stats = RestoreStats::default();
    let mut used = vec![false; files.len()];
    let find = |rel: &str| {
        let n = norm_rel(rel);
        files.iter().position(|f| norm_rel(f) == n)
    };
    let mut html = m.body_html.clone();
    let mut inline: Vec<OriginalPart> = Vec::new();
    // 본문 그림: src="…", background="…"가 백업 폴더를 가리키면 cid 인라인 이미지로.
    let mut replaced: Vec<(String, String)> = Vec::new();
    // 없는 그림은 경로마다 한 번만 센다(본문이 같은 그림을 여러 번 쓴다).
    let mut missing_refs: Vec<String> = Vec::new();
    for attr in ["src=\"", "background=\""] {
        let mut from = 0;
        while let Some(i) = find_ci(&html, attr, from) {
            let v = i + attr.len();
            let Some(len) = html[v..].find('"') else {
                break;
            };
            let value = html[v..v + len].to_owned();
            from = v + len;
            let lower = value.to_ascii_lowercase();
            if !(lower.starts_with("$$_image_$$") || lower.starts_with("$$_attach_$$")) {
                continue;
            }
            let cid = match replaced.iter().find(|(r, _)| *r == value) {
                Some((_, c)) => Some(c.clone()),
                None => {
                    let rel = decode_entities(&value);
                    let idx = find(&rel);
                    let path = idx.map_or(rel.clone(), |k| files[k].clone());
                    match read(&path) {
                        Some(data) => {
                            if let Some(k) = idx {
                                used[k] = true;
                            }
                            let name = original_name(&path);
                            let mime = sniff_image(&data).unwrap_or_else(|| mime_by_name(&name));
                            let cid = format!("domino{}@eo.local", inline.len() + 1);
                            inline.push(OriginalPart {
                                mime: mime.to_owned(),
                                name,
                                data,
                                content_id: Some(cid.clone()),
                            });
                            replaced.push((value.clone(), cid.clone()));
                            Some(cid)
                        }
                        None => {
                            if !missing_refs.contains(&value) {
                                missing_refs.push(value.clone());
                                stats.missing_files += 1;
                            }
                            None
                        }
                    }
                }
            };
            if let Some(cid) = cid {
                let replacement = format!("cid:{cid}");
                html.replace_range(v..v + len, &replacement);
                from = v + replacement.len();
            }
        }
    }
    stats.inline_images = inline.len();
    // 첨부N 칸. 백업 폴더를 가리키지 않는 첨부(첨부를 내려받지 않았거나 연결하기를 끄고 저장)는 원래 없는 것이다.
    let mut attachments: Vec<OriginalPart> = Vec::new();
    let mut absent: Vec<String> = Vec::new();
    for (display, href) in &m.attachments {
        let lower = href.trim().to_ascii_lowercase();
        let local = !lower.is_empty()
            && (lower.starts_with("$$_") || !(lower.contains(':') || lower.starts_with('#')));
        if !local {
            stats.not_downloaded += 1;
            if !display.trim().is_empty() {
                absent.push(display.trim().to_owned());
            }
            continue;
        }
        let idx = find(href);
        let path = idx.map_or(href.clone(), |k| files[k].clone());
        match read(&path) {
            Some(data) => {
                if let Some(k) = idx {
                    used[k] = true;
                }
                let name = if display.trim().is_empty() {
                    original_name(&path)
                } else {
                    display.trim().to_owned()
                };
                attachments.push(OriginalPart {
                    mime: mime_by_name(&name).to_owned(),
                    name,
                    data,
                    content_id: None,
                });
            }
            None => {
                stats.missing_files += 1;
                if !display.trim().is_empty() {
                    absent.push(display.trim().to_owned());
                }
            }
        }
    }
    // 백업에 없는 첨부의 이름이 사라지지 않게 본문 끝에 적는다(읽기 창에 보이고 검색된다).
    if !absent.is_empty() {
        html.push_str(&format!(
            "<hr><p style=\"color:#666;font-size:12px\">백업에 들어 있지 않은 첨부: {}</p>",
            esc(&absent.join(", "))
        ));
    }
    // 아무도 가리키지 않은 이 메일의 파일(본문이 cid:로 남은 그림 등)도 첨부로 남긴다.
    for (k, f) in files.iter().enumerate() {
        if used[k] {
            continue;
        }
        if let Some(data) = read(f) {
            let name = original_name(f);
            let mime = sniff_image(&data).unwrap_or_else(|| mime_by_name(&name));
            attachments.push(OriginalPart {
                mime: mime.to_owned(),
                name,
                data,
                content_id: None,
            });
            stats.extra_files += 1;
        }
    }
    stats.attachments = attachments.len();
    let message_id = match (&m.message_id, &m.file_id) {
        (Some(id), _) => id.clone(),
        (None, Some(f)) => format!("<domino-{f}@eo.local>"),
        (None, None) => format!("<domino-{}@eo.local>", short_key(key)),
    };
    let mut extra_headers = vec![("X-EO-Restored".to_owned(), "domino".to_owned())];
    if let Some(f) = &m.file_id {
        extra_headers.push(("X-Domino-FileId".to_owned(), f.clone()));
    }
    let eml = build_eml(&NewEml {
        from: m.from.clone(),
        to: m.to.clone(),
        cc: m.cc.clone(),
        bcc: m.bcc.clone(),
        reply_to: m.reply_to.clone(),
        subject: m.subject.clone(),
        date: m.received,
        message_id,
        html,
        inline,
        attachments,
        extra_headers,
    });
    (eml, stats)
}

fn decode_qp(b: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'=' {
            if b.get(i + 1) == Some(&b'\r') && b.get(i + 2) == Some(&b'\n') {
                i += 3;
                continue;
            }
            if b.get(i + 1) == Some(&b'\n') {
                i += 2;
                continue;
            }
            if let (Some(h), Some(l)) = (b.get(i + 1), b.get(i + 2))
                && let Ok(v) = u8::from_str_radix(&String::from_utf8_lossy(&[*h, *l]), 16)
            {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

/// 바이트에 UTF-8 한글 음절 같은 3바이트 묶음이 몇 개인가(EUC-KR이라는데 UTF-8이 섞였는지 보는 데 쓴다).
fn utf8_hangul_runs(b: &[u8]) -> usize {
    b.windows(3)
        .filter(|w| {
            matches!(w[0], 0xEA..=0xED)
                && (0x80..=0xBF).contains(&w[1])
                && (0x80..=0xBF).contains(&w[2])
        })
        .count()
}

/// 머리글 줄의 문자셋 표시에서 값만(소문자).
fn header_param(header: &str, name: &str) -> Option<String> {
    let lower = header.to_ascii_lowercase();
    let i = lower.find(&format!("{name}="))? + name.len() + 1;
    let v = header[i..].trim_start().trim_start_matches('"');
    let end = v.find(['"', ';', ' ', '\t', '\r', '\n']).unwrap_or(v.len());
    Some(v[..end].to_ascii_lowercase())
}

fn is_korean_charset(cs: &str) -> bool {
    matches!(
        cs,
        "euc-kr"
            | "ks_c_5601-1987"
            | "ks_c_5601"
            | "cp949"
            | "windows-949"
            | "x-windows-949"
            | "korean"
    )
}

/// HCL Domino EML 백업의 틀린 문자셋 표시를 바로잡는다(실데이터 71통 중 대부분이 깨져 보였다).
/// - 머리글에 인코딩하지 않은 EUC-KR 바이트(제목 32통): UTF-8로 바꾼다(메일 해석기는 날 바이트를 UTF-8로 읽는다).
/// - 글 부분의 선언과 실제가 다름: `charset=utf-8`인데 EUC-KR 바이트(14통), EUC-KR이라는데 UTF-8이거나 섞임.
///   이런 부분만 UTF-8로 다시 써서(base64) `charset=utf-8`로 표시한다. 바르게 표시된 부분은 손대지 않는다.
///
/// 판별은 [`decode_html`]과 같다(덩어리마다 한국어로 그럴듯한 쪽). 바꿀 것이 없으면 `None`(다시 해도 같다).
pub fn fix_eml_charsets(raw: &[u8]) -> Option<Vec<u8>> {
    let msg = mail_parser::MessageParser::default().parse(raw)?;
    // (시작, 끝, 새 바이트). 부분의 머리글 범위와 글 부분 본문 범위는 서로 겹치지 않는다.
    let mut edits: Vec<(usize, usize, Vec<u8>)> = Vec::new();
    for part in &msg.parts {
        let (hs, bs, be) = (
            part.offset_header as usize,
            part.offset_body as usize,
            part.offset_end as usize,
        );
        if hs > bs || bs > be || be > raw.len() {
            continue;
        }
        let head = &raw[hs..bs];
        let is_text = matches!(
            part.body,
            mail_parser::PartType::Text(_) | mail_parser::PartType::Html(_)
        );
        // 글 부분: 본문 바이트의 실제 문자셋.
        let mut new_body: Option<String> = None;
        let head_text = String::from_utf8_lossy(head).into_owned();
        if is_text {
            let body = &raw[bs..be];
            let bytes = match part.encoding {
                mail_parser::Encoding::Base64 => {
                    let clean: Vec<u8> = body
                        .iter()
                        .copied()
                        .filter(|c| !c.is_ascii_whitespace())
                        .collect();
                    STANDARD.decode(clean).unwrap_or_default()
                }
                mail_parser::Encoding::QuotedPrintable => decode_qp(body),
                mail_parser::Encoding::None => body.to_vec(),
            };
            if bytes.iter().any(|b| !b.is_ascii()) {
                let ct_line = head_text
                    .lines()
                    .find(|l| l.to_ascii_lowercase().starts_with("content-type:"))
                    .unwrap_or("");
                let declared = header_param(ct_line, "charset").unwrap_or_default();
                let valid = std::str::from_utf8(&bytes).is_ok();
                let wrong = if declared.is_empty()
                    || declared == "utf-8"
                    || declared == "utf8"
                    || declared == "us-ascii"
                {
                    !valid
                } else if is_korean_charset(&declared) {
                    valid || utf8_hangul_runs(&bytes) >= 3
                } else {
                    false
                };
                if wrong {
                    new_body = Some(decode_html(&bytes));
                }
            }
        }
        // 머리글: UTF-8이 아닌 날 바이트가 있는 줄을 바로잡고, 본문을 바꾸면 내용 머리글도 바꾼다.
        let bad_header = std::str::from_utf8(head).is_err();
        if !bad_header && new_body.is_none() {
            continue;
        }
        let mut out: Vec<u8> = Vec::with_capacity(head.len() + 64);
        let mut ct_written = false;
        let mut lines: Vec<Vec<u8>> = Vec::new();
        for line in head.split_inclusive(|&b| b == b'\n') {
            match (line.first(), lines.last_mut()) {
                (Some(b' ' | b'\t'), Some(last)) => last.extend_from_slice(line),
                _ => lines.push(line.to_vec()),
            }
        }
        for line in lines {
            let text = if std::str::from_utf8(&line).is_ok() {
                String::from_utf8_lossy(&line).into_owned()
            } else {
                decode_html(&line)
            };
            let lower = text.to_ascii_lowercase();
            if new_body.is_some() && lower.starts_with("content-transfer-encoding:") {
                continue;
            }
            // 머리글 끝의 빈 줄: 내용 머리글이 없던 글 부분이면 그 앞에 넣는다.
            if new_body.is_some() && !ct_written && text.trim().is_empty() {
                out.extend_from_slice(b"Content-Type: text/plain; charset=\"utf-8\"\r\nContent-Transfer-Encoding: base64\r\n");
                ct_written = true;
            }
            if new_body.is_some() && lower.starts_with("content-type:") {
                ct_written = true;
                let ct = text.trim_end_matches(['\r', '\n']);
                let fixed = match lower.find("charset=") {
                    Some(i) => {
                        let rest = &ct[i + 8..];
                        let quoted = rest.starts_with('"');
                        let v = rest.trim_start_matches('"');
                        let end = v.find(['"', ';', ' ', '\t']).unwrap_or(v.len());
                        let tail = &v[end..];
                        let tail = if quoted {
                            tail.strip_prefix('"').unwrap_or(tail)
                        } else {
                            tail
                        };
                        format!("{}charset=\"utf-8\"{}", &ct[..i], tail)
                    }
                    None => format!("{ct}; charset=\"utf-8\""),
                };
                out.extend_from_slice(fixed.as_bytes());
                out.extend_from_slice(b"\r\nContent-Transfer-Encoding: base64\r\n");
                continue;
            }
            out.extend_from_slice(text.as_bytes());
        }
        match &new_body {
            Some(text) => {
                // 머리글 끝의 빈 줄은 위 줄들에 들어 있다. 본문은 base64(UTF-8).
                edits.push((hs, bs, out));
                let mut b = crate::backup::b64_lines(text.as_bytes()).into_bytes();
                b.extend_from_slice(b"\r\n");
                edits.push((bs, be, b));
            }
            None => edits.push((hs, bs, out)),
        }
    }
    if edits.is_empty() {
        return None;
    }
    edits.sort_by_key(|e| std::cmp::Reverse(e.0));
    let mut fixed = raw.to_vec();
    for (s, e, bytes) in edits {
        fixed.splice(s..e, bytes);
    }
    Some(fixed)
}

/// EML 원문에 첨부를 덧붙인다(HCL Domino EML 백업에서 첨부·그림이 `$$_Attach_$$` 등에 따로 저장됐을 때).
/// 원래 본문 엔터티(내용 머리글 + 본문 바이트)는 손대지 않고 바깥을 multipart/mixed로 감싸 그 뒤에 첨부를 붙인다.
/// 나머지 머리글(보낸 사람·제목·Message-ID 등)은 그대로 둔다.
pub fn append_attachments(raw: &[u8], parts: &[OriginalPart]) -> Vec<u8> {
    if parts.is_empty() {
        return raw.to_vec();
    }
    let (head_end, sep) = match raw.windows(4).position(|w| w == b"\r\n\r\n") {
        Some(i) => (i, 4),
        None => match raw.windows(2).position(|w| w == b"\n\n") {
            Some(i) => (i, 2),
            None => (raw.len(), 0),
        },
    };
    let head = &raw[..head_end];
    let body = raw.get(head_end + sep..).unwrap_or_default();
    // 머리글을 논리 줄(접힌 줄 포함)로 나눠, 내용 머리글과 나머지로 가른다.
    let mut fields: Vec<Vec<u8>> = Vec::new();
    for line in head.split(|&b| b == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        match (line.first(), fields.last_mut()) {
            (Some(b' ' | b'\t'), Some(last)) => {
                last.extend_from_slice(b"\r\n");
                last.extend_from_slice(line);
            }
            _ => fields.push(line.to_vec()),
        }
    }
    let is_content = |f: &[u8]| {
        let lower = String::from_utf8_lossy(&f[..f.len().min(40)]).to_ascii_lowercase();
        lower.starts_with("content-") || lower.starts_with("mime-version:")
    };
    let boundary = {
        let mut k = 0;
        loop {
            let b = format!(
                "outlook-append-{}-{k}",
                short_key(&String::from_utf8_lossy(&raw[..raw.len().min(4096)]))
            );
            if !raw.windows(b.len()).any(|w| w == b.as_bytes()) {
                break b;
            }
            k += 1;
        }
    };
    let mut out = Vec::with_capacity(
        raw.len()
            + parts
                .iter()
                .map(|p| p.data.len() * 4 / 3 + 512)
                .sum::<usize>(),
    );
    for f in fields.iter().filter(|f| !f.is_empty() && !is_content(f)) {
        out.extend_from_slice(f);
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(
        format!(
            "MIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"{boundary}\"\r\n\r\n"
        )
        .as_bytes(),
    );
    out.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    let content: Vec<&Vec<u8>> = fields
        .iter()
        .filter(|f| {
            !f.is_empty()
                && is_content(f)
                && !String::from_utf8_lossy(&f[..f.len().min(13)])
                    .eq_ignore_ascii_case("mime-version:")
        })
        .collect();
    if content.is_empty() {
        out.extend_from_slice(b"Content-Type: text/plain; charset=us-ascii\r\n");
    }
    for f in content {
        out.extend_from_slice(f);
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b"\r\n");
    out.extend_from_slice(body);
    if !body.ends_with(b"\n") {
        out.extend_from_slice(b"\r\n");
    }
    for p in parts {
        out.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Type: {}; name={}\r\nContent-Disposition: attachment; filename={}\r\n\
                 Content-Transfer-Encoding: base64\r\n\r\n{}\r\n",
                p.mime,
                crate::backup::filename_param(&p.name),
                crate::backup::filename_param(&p.name),
                crate::backup::b64_lines(&p.data)
            )
            .as_bytes(),
        );
    }
    out.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    out
}

/// 이 메일 번호의 따로 저장된 파일들 → 첨부 부분(그림 형식은 내용으로 안다).
pub fn files_as_attachments(files: &[(String, Vec<u8>)]) -> Vec<OriginalPart> {
    files
        .iter()
        .map(|(path, data)| {
            let name = original_name(path);
            OriginalPart {
                mime: sniff_image(data)
                    .unwrap_or_else(|| mime_by_name(&name))
                    .to_owned(),
                name,
                data: data.clone(),
                content_id: None,
            }
        })
        .collect()
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn people_text(list: &[Person]) -> String {
    list.iter()
        .map(|p| {
            if p.name.is_empty() {
                format!("[{}]", p.email)
            } else {
                format!("{} [{}]", p.name, p.email)
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// HCL Domino 백업과 같은 형식의 HTML(점검용 견본 만들기). `charset`은 meta에 적을 값.
pub fn to_domino_html(m: &DominoMail, charset: &str) -> String {
    let received = m.received.map_or(String::new(), |ms| {
        let (y, mo, d, h, mi, s, _) = crate::backup::kst_civil(ms);
        format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}")
    });
    let row = |label: &str, value: &str| {
        format!(
            "<tr>\n  <td width=\"130\" class=\"tit\">{label}</td>\n  <td class=\"b_text\">{value}</td>\n</tr>\n"
        )
    };
    let mut rows = String::new();
    rows.push_str(&row("발신인", &esc(&people_text(&m.from))));
    rows.push_str(&row("수신일", &received));
    rows.push_str(&row("수신인", &esc(&people_text(&m.to))));
    rows.push_str(&row("참조인", &esc(&people_text(&m.cc))));
    rows.push_str(&row("제&nbsp;&nbsp;&nbsp;목", &esc(&m.subject)));
    // 실제 백업처럼 첨부 칸은 본문 칸 뒤에 둔다.
    let mut att_rows = String::new();
    for (k, (name, href)) in m.attachments.iter().enumerate() {
        att_rows.push_str(&row(
            &format!("첨&nbsp;부&nbsp;{}", k + 1),
            &format!("<a href=\"{href}\">{}</a>", esc(name)),
        ));
    }
    let hidden = |name: &str, value: &str| {
        format!("<input type=\"hidden\" name=\"{name}\" value=\"{value}\" />\n")
    };
    format!(
        "<!DOCTYPE HTML PUBLIC \"-//W3C//DTD HTML 4.01 Transitional//EN\">\n<html>\n<head>\n<title>{t}</title>\n\
         <meta http-equiv=\"Content-Type\" content=\"text/html; charset={charset}\">\n</head>\n<body topmargin=\"0\">\n\
         <table width=\"776\" border=\"0\" cellpadding=\"0\" cellspacing=\"1\" bgcolor=\"#d0d0d0\">\n{rows}\
         <tr>\n  <td class=\"tit\">본&nbsp;&nbsp;&nbsp;문</td>\n  <td colspan=\"2\" class=\"b_text2\">\n    <p>{body}</p>\n  </td>\n</tr>\n{att_rows}</table>\n\
         {bcc}{reply}{imp}{mid}{fid}</body>\n</html>\n",
        t = esc(&m.subject),
        body = m.body_html,
        bcc = hidden("BCC", &people_text(&m.bcc)),
        reply = hidden(
            "REPLY_TO",
            &m.reply_to
                .first()
                .map_or(String::new(), |p| format!("{} <{}>", p.name, p.email))
        ),
        imp = hidden("IMPORTANCE", ""),
        mid = hidden("MESSAGE_ID", m.message_id.as_deref().unwrap_or("")),
        fid = hidden("FILEID", m.file_id.as_deref().unwrap_or("")),
    )
}

/// EUC-KR(=CP949)로 바꾼다(견본 만들기용).
pub fn encode_korean(s: &str) -> Vec<u8> {
    encoding_rs::EUC_KR.encode(s).0.into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 실제 백업과 같은 모양의 견본(이름·주소·내용은 지어낸 것).
    pub(crate) fn sample(n: u32) -> String {
        format!(
            "<!DOCTYPE HTML PUBLIC \"-//W3C//DTD HTML 4.01 Transitional//EN\"><html><head><title>주간 보고 (최종)</title>\n\
             <meta http-equiv=\"Content-Type\" content=\"text/html; charset=euc-kr\"><style type=\"text/css\"><!-- .tit {{}} --></style></head>\n\
             <body topmargin=\"0\"><table width=\"776\"><tr><td><hr></td></tr></table>\n\
             <table width=\"776\" border=\"0\" cellpadding=\"0\" cellspacing=\"1\" bgcolor=\"#d0d0d0\">\n\
             <tr>\n <td width=\"130\" class=\"tit\">발신인</td>\n <td class=\"b_text\">김민수 [minsu@example.com]</td>\n</tr>\n\
             <tr><td class=\"tit\">수신일</td><td class=\"b_text\">2021-03-02 13:05:22</td></tr>\n\
             <tr><td class=\"tit\">수신인</td><td class=\"b_text\">이지영 [jy@example.com];\t박서연 [Z12@mail.example.org [Z12@mail.example.org]]; 주소없음</td></tr>\n\
             <tr><td class=\"tit\">참조인</td><td class=\"b_text\"></td></tr>\n\
             <tr><td class=\"tit\">제&nbsp;&nbsp;&nbsp;목</td><td class=\"b_text\">주간 보고 &amp; 계획 (최종)</td></tr>\n\
             <tr><td class=\"tit\">본&nbsp;&nbsp;&nbsp;문\n</td><td colspan=\"2\" class=\"b_text2\">\n    <p><!DOCTYPE HTML PUBLIC \"-//W3C//DTD HTML 4.0 Transitional//EN\"><HTML><HEAD>\
             <META http-equiv=\"content-type\" content=\"text/html; charset=euc-kr\"></HEAD><BODY><P>본문 HCL Domino점검</P>\
             <table><tr><td class=\"tit\">본문 안의 같은 class</td></tr></table>\
             <IMG src=\"$$_Image_$$\\$image_001_png__${n}$_$28$_.2\"><img src=\"$$_Attach_$$\\$att_image__${n}$_$61$_.2\">\
             <img src=\"$$_Image_$$\\$image_001_png__${n}$_$28$_.2\"><img src=\"cid:lost@x\"></BODY></HTML></p>\n</td>\n</tr>\n\
             <tr><td class=\"tit\">첨&nbsp;부&nbsp;1</td><td class=\"b_text\"><a href=\"$$_Attach_$$\\결산 (최종) & 계획_${n}$_$190$_.xlsx\">결산 (최종) & 계획.xlsx</a></td></tr>\n\
             <tr><td class=\"tit\">첨&nbsp;부&nbsp;2</td><td class=\"b_text\"><a href=\"$$_Attach_$$\\없는파일_${n}$_$191$_.pdf\">없는파일.pdf</a></td></tr>\n</table>\n\
             <input type=\"hidden\" name=\"BCC\" value=\"감사 [audit@example.com]\" />\
             <input type=\"hidden\" name=\"REPLY_TO\" value=\"김민수 <minsu@example.com>\" />\
             <input type=\"hidden\" name=\"IMPORTANCE\" value=\"\" />\
             <input type=\"hidden\" name=\"MESSAGE_ID\" value=\"<OFBED0A4C0.47791161@mail.example.org>\" />\
             <input type=\"hidden\" name=\"FILEID\" value=\"BED0A4C047791161492586A400135124\" /></body></html>"
        )
    }

    #[test]
    fn parses_domino_html() {
        let m = parse_domino_html(&sample(147)).unwrap();
        assert_eq!(m.subject, "주간 보고 & 계획 (최종)");
        assert_eq!(
            m.from,
            vec![Person {
                name: "김민수".into(),
                email: "minsu@example.com".into()
            }]
        );
        assert_eq!(
            m.to,
            vec![
                Person {
                    name: "이지영".into(),
                    email: "jy@example.com".into()
                },
                Person {
                    name: "박서연".into(),
                    email: "Z12@mail.example.org".into()
                },
                Person {
                    name: "주소없음".into(),
                    email: String::new()
                },
            ],
            "겹친 대괄호, 탭, 주소 없는 이름"
        );
        assert_eq!(
            parse_people("CN=홍길동 OU123456/O=Outlook; '이지영' [jy@example.com]"),
            vec![
                Person {
                    name: "홍길동 OU123456".into(),
                    email: String::new()
                },
                Person {
                    name: "이지영".into(),
                    email: "jy@example.com".into()
                },
            ],
            "노츠 이름(주소 없음)과 작은따옴표 이름"
        );
        assert!(m.cc.is_empty());
        assert_eq!(m.bcc[0].email, "audit@example.com");
        assert_eq!(m.reply_to[0].email, "minsu@example.com");
        assert_eq!(m.received, crate::backup::parse_kst("2021-03-02 13:05:22"));
        assert_eq!(
            m.message_id.as_deref(),
            Some("<OFBED0A4C0.47791161@mail.example.org>")
        );
        assert_eq!(
            m.file_id.as_deref(),
            Some("BED0A4C047791161492586A400135124")
        );
        assert_eq!(m.attachments.len(), 2);
        assert_eq!(m.attachments[0].0, "결산 (최종) & 계획.xlsx");
        assert!(
            m.body_html.starts_with("<!DOCTYPE") && m.body_html.ends_with("</HTML>"),
            "{}",
            m.body_html
        );
        assert!(
            m.body_html.contains("본문 안의 같은 class"),
            "본문 안의 표·같은 class 때문에 일찍 끊기지 않음"
        );
        assert!(
            !m.body_html.contains("결산"),
            "본문 뒤의 첨부 칸은 본문에 넣지 않음"
        );
        // 칸 이름이 ???로 깨진 파일은 칸 순서로 읽는다.
        let broken = sample(9)
            .replace("발신인", "???")
            .replace("수신일", "???")
            .replace("수신인", "???")
            .replace("참조인", "???")
            .replace("제&nbsp;&nbsp;&nbsp;목", "???")
            .replace("본&nbsp;&nbsp;&nbsp;문", "???")
            .replace("첨&nbsp;부&nbsp;1", "???")
            .replace("첨&nbsp;부&nbsp;2", "???");
        let b = parse_domino_html(&broken).unwrap();
        assert_eq!(
            (
                b.subject.as_str(),
                b.from.len(),
                b.to.len(),
                b.attachments.len()
            ),
            ("주간 보고 & 계획 (최종)", 1, 3, 2)
        );
        assert!(b.body_html.contains("본문 HCL Domino점검") && !b.body_html.contains("결산"));
        // 유저스크립트 백업이나 그냥 HTML은 아니다.
        assert!(
            parse_domino_html("<html><body><td class=\"tit\">발신인</td></body></html>").is_none()
        );
    }

    #[test]
    fn restores_inline_images_attachments_and_leftovers() {
        let m = parse_domino_html(&sample(147)).unwrap();
        let png = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A].to_vec();
        let files = vec![
            "$$_Image_$$\\$image_001_png__$147$_$28$_.2".to_owned(),
            "$$_Attach_$$\\$att_image__$147$_$61$_.2".to_owned(),
            "$$_Attach_$$\\결산 (최종) & 계획_$147$_$190$_.xlsx".to_owned(),
            "$$_Image_$$\\image002_$147$_$29$_.jpg".to_owned(),
        ];
        let (raw, stats) = restore_domino_eml(
            &m,
            &files,
            |p| match p {
                p if p.ends_with(".2") => Some(png.clone()),
                p if p.ends_with(".xlsx") => Some(b"PK-xlsx".to_vec()),
                p if p.ends_with(".jpg") => Some(vec![0xFF, 0xD8, 0xFF, 0xE0]),
                _ => None,
            },
            "k",
        );
        assert_eq!(
            stats,
            RestoreStats {
                inline_images: 2,
                attachments: 2,
                missing_files: 1,
                extra_files: 1,
                not_downloaded: 0
            },
            "같은 그림은 한 번, 없는 첨부 1, 아무도 안 가리킨 그림은 첨부로"
        );
        let r = crate::render(
            &raw,
            &crate::RenderOptions {
                cid_base: "http://app-cid.localhost/1/".into(),
                allow_remote: false,
            },
        )
        .unwrap();
        // 본문이 cid:로 남긴 그림(cid:lost@x) 1개와 아무도 안 가리킨 그림 1개는 읽기 창에서 순서로 잇는다.
        assert_eq!(r.inline_images.len(), 3);
        assert_eq!(
            r.inline_images[0].mime, "image/png",
            "깨진 확장자는 내용으로 형식을 안다"
        );
        assert_eq!(
            (
                r.inline_images[2].cid.as_str(),
                r.inline_images[2].mime.as_str()
            ),
            ("lost@x", "image/jpeg")
        );
        let names: Vec<_> = r
            .attachments
            .iter()
            .filter(|a| !a.inline)
            .map(|a| a.name.as_str())
            .collect();
        assert_eq!(names, ["결산 (최종) & 계획.xlsx"]);
        assert!(r.html.contains("본문 HCL Domino점검"));
        let x = crate::extract(&raw).unwrap();
        assert_eq!(x.bcc, "감사 <audit@example.com>");
        let s = crate::backup::summarize(&raw).unwrap();
        assert_eq!(
            s.message_id.as_deref(),
            Some("<OFBED0A4C0.47791161@mail.example.org>")
        );
        assert_eq!(s.date, m.received);
        assert!(String::from_utf8_lossy(&raw).contains("X-EO-Restored: domino"));

        // 주소 없는 노츠 이름 발신인도 이름으로 남는다(목록·검색).
        let notes = DominoMail {
            from: parse_people("CN=홍길동 OU123456/O=Outlook"),
            ..m.clone()
        };
        let (raw, _) = restore_domino_eml(&notes, &[], |_| None, "k2");
        let s = crate::backup::summarize(&raw).unwrap();
        assert_eq!(
            s.from,
            Some(Person {
                name: "홍길동 OU123456".into(),
                email: String::new()
            })
        );
        assert_eq!(crate::extract(&raw).unwrap().from, "홍길동 OU123456");
    }

    /// 첨부·그림을 내려받지 않았거나 연결하기를 끄고 저장한 HTML: 첨부 칸에 이름만 있거나 서버 주소다.
    #[test]
    fn html_saved_without_files_or_links() {
        let doc = sample(5)
            .replace(
                "<a href=\"$$_Attach_$$\\결산 (최종) & 계획_$5$_$190$_.xlsx\">결산 (최종) & 계획.xlsx</a>",
                "결산.xlsx (12KB)",
            )
            .replace(
                "<a href=\"$$_Attach_$$\\없는파일_$5$_$191$_.pdf\">없는파일.pdf</a>",
                "<a href=\"https://mail.example.com/att?id=1\">계약서.pdf</a>",
            );
        let m = parse_domino_html(&doc).unwrap();
        assert_eq!(
            m.attachments,
            [
                ("결산.xlsx (12KB)".to_owned(), String::new()),
                (
                    "계약서.pdf".to_owned(),
                    "https://mail.example.com/att?id=1".to_owned()
                )
            ]
        );
        let (raw, stats) = restore_domino_eml(&m, &[], |_| None, "k");
        assert_eq!((stats.not_downloaded, stats.attachments), (2, 0));
        assert_eq!(
            stats.missing_files, 2,
            "본문 그림 2개는 백업 폴더를 가리키는데 없다"
        );
        let x = crate::extract(&raw).unwrap();
        assert!(
            x.body_text
                .contains("백업에 들어 있지 않은 첨부: 결산.xlsx (12KB), 계약서.pdf"),
            "첨부 이름은 본문 끝에 남는다: {}",
            x.body_text
        );
    }

    /// 실데이터 HCL Domino EML의 틀린 문자셋 표시: 인코딩하지 않은 EUC-KR 제목, `charset=utf-8`인데 EUC-KR인 본문.
    #[test]
    fn fixes_mislabeled_domino_eml_charsets() {
        let mut eml: Vec<u8> = b"From: a@x.com\r\nSubject: ".to_vec();
        eml.extend(encode_korean("주간 보고서 제출"));
        eml.extend(
            b"\r\nDate: 15-Oct-2026 16:34:00 ZE9\r\nMessage-ID: <OF1@LocalDomain>\r\nMIME-Version: 1.0\r\n\
              Content-Type: multipart/mixed; boundary=\"-----------------------1a2b\"\r\n\r\n\
              -------------------------1a2b\r\nContent-Type: text/html; charset=\"utf-8\"\r\n\
              Content-Transfer-Encoding: 8bit\r\n\r\n<html><body><p>",
        );
        eml.extend(encode_korean("본문 한글 내용입니다"));
        eml.extend(b"</p></body></html>\r\n-------------------------1a2b\r\nContent-Type: text/plain; charset=euc-kr\r\n\
                     Content-Transfer-Encoding: base64\r\n\r\n");
        eml.extend(
            STANDARD
                .encode(encode_korean("바르게 표시된 부분"))
                .into_bytes(),
        );
        eml.extend(b"\r\n-------------------------1a2b--\r\n");

        // 고치기 전: 제목·본문이 깨진다.
        let before = crate::extract(&eml).unwrap();
        assert_ne!(before.subject, "주간 보고서 제출");
        assert!(!before.body_text.contains("본문 한글 내용입니다"));

        let fixed = fix_eml_charsets(&eml).expect("바꿀 것이 있다");
        let x = crate::extract(&fixed).unwrap();
        assert_eq!(x.subject, "주간 보고서 제출");
        assert!(
            x.body_text.contains("본문 한글 내용입니다"),
            "{}",
            x.body_text
        );
        assert!(
            x.body_text.contains("바르게 표시된 부분"),
            "바르게 표시된 부분은 그대로: {}",
            x.body_text
        );
        let r = crate::render(
            &fixed,
            &crate::RenderOptions {
                cid_base: "http://app-cid.localhost/1/".into(),
                allow_remote: false,
            },
        )
        .unwrap();
        assert!(r.html.contains("본문 한글 내용입니다"));
        let s = crate::backup::summarize(&fixed).unwrap();
        assert_eq!(s.message_id.as_deref(), Some("<OF1@LocalDomain>"));
        assert_eq!(s.date, Some(1_792_049_640_000));
        assert_eq!(fix_eml_charsets(&fixed), None, "다시 해도 같다");

        // 바르게 표시된 EML은 손대지 않는다.
        let good = "From: a@x.com\r\nSubject: =?utf-8?B?7KO86rCE?=\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n한글 본문\r\n";
        assert_eq!(fix_eml_charsets(good.as_bytes()), None);
        let mut euc = b"From: a@x.com\r\nSubject: s\r\nContent-Type: text/plain; charset=ks_c_5601-1987\r\n\r\n".to_vec();
        euc.extend(encode_korean("한글 본문 바르게"));
        assert_eq!(fix_eml_charsets(&euc), None);
        // 내용 머리글 없는 글 부분(기본 text/plain)인데 EUC-KR.
        let mut bare = b"From: a@x.com\r\nSubject: s\r\n\r\n".to_vec();
        bare.extend(encode_korean("머리글 없는 본문"));
        let fixed = fix_eml_charsets(&bare).unwrap();
        assert!(
            crate::extract(&fixed)
                .unwrap()
                .body_text
                .contains("머리글 없는 본문")
        );
    }

    #[test]
    fn appends_separately_saved_files_to_eml() {
        // HCL Domino EML(노츠 날짜, 빈 머리글, 접힌 To, HTML 본문).
        let eml = "Reply-To: a@x.com\r\nDate: 15-Oct-2026 16:34:00 ZE9\r\nContent-Type: text/html; charset=\"utf-8\"\r\n\
                   MIME-Version: 1.0\r\nFrom: =?utf-8?B?6rmA66+87IiY?= <minsu@x.com>\r\nX-Priority: \r\nImportance: \r\n\
                   Subject: =?utf-8?B?7KO86rCE?=\r\nTo: a@x.com,\r\n b@x.com\r\nMessage-ID: <OF1@LocalDomain>\r\nCc: \r\nBcc: \r\n\r\n\
                   <html><body><p>본문 EML점검</p><img src=\"cid:lost\"></body></html>\r\n";
        let parts = files_as_attachments(&[
            (
                "$$_Attach_$$\\보고서_$9$_$1$_.xlsx".to_owned(),
                b"PK-x".to_vec(),
            ),
            (
                "$$_Image_$$\\image001_$9$_$2$_.2".to_owned(),
                vec![0x89, b'P', b'N', b'G', 0, 0],
            ),
        ]);
        let raw = append_attachments(eml.as_bytes(), &parts);
        let s = crate::backup::summarize(&raw).unwrap();
        assert_eq!(s.message_id.as_deref(), Some("<OF1@LocalDomain>"));
        assert_eq!(s.date, Some(1_792_049_640_000));
        assert_eq!(s.to, "a@x.com; b@x.com", "접힌 머리글 그대로");
        assert!(s.has_attachments);
        let r = crate::render(
            &raw,
            &crate::RenderOptions {
                cid_base: "http://app-cid.localhost/1/".into(),
                allow_remote: false,
            },
        )
        .unwrap();
        assert!(r.html.contains("본문 EML점검"), "원래 본문 그대로");
        let names: Vec<_> = r
            .attachments
            .iter()
            .map(|a| (a.name.as_str(), a.mime.as_str()))
            .collect();
        assert_eq!(
            names,
            [
                ("보고서.xlsx", mime_by_name("보고서.xlsx")),
                ("image001.2", "image/png")
            ]
        );
        assert_eq!(
            append_attachments(eml.as_bytes(), &[]),
            eml.as_bytes(),
            "붙일 것이 없으면 원문 그대로"
        );
    }

    #[test]
    fn writer_round_trips() {
        let m = DominoMail {
            subject: "A & B <견본>".into(),
            from: vec![Person {
                name: "김민수".into(),
                email: "minsu@example.com".into(),
            }],
            to: vec![
                Person {
                    name: "이지영".into(),
                    email: "jy@example.com".into(),
                },
                Person {
                    name: String::new(),
                    email: "b@example.com".into(),
                },
            ],
            cc: vec![],
            bcc: vec![Person {
                name: "감사".into(),
                email: "audit@example.com".into(),
            }],
            reply_to: vec![Person {
                name: "김민수".into(),
                email: "minsu@example.com".into(),
            }],
            received: crate::backup::parse_kst("2021-03-02 13:05:22"),
            message_id: Some("<abc@mail.example.org>".into()),
            file_id: Some("FID1".into()),
            body_html: "<HTML><BODY><table><tr><td>표</td></tr></table></BODY></HTML>".into(),
            attachments: vec![(
                "결산 (최종).xlsx".into(),
                "$$_Attach_$$\\결산 (최종)_$1$_$2$_.xlsx".into(),
            )],
        };
        let html = to_domino_html(&m, "euc-kr");
        assert_eq!(
            parse_domino_html(&decode_html(&encode_korean(&html))).unwrap(),
            m
        );
    }

    #[test]
    fn charset_and_names() {
        let (euc, _, _) = encoding_rs::EUC_KR.encode("<meta charset=\"ks_c_5601-1987\">한글 메일");
        assert_eq!(
            decode_html(&euc),
            "<meta charset=\"ks_c_5601-1987\">한글 메일"
        );
        // UTF-8이라고 했는데 실제로는 EUC-KR인 파일.
        let (bad, _, _) = encoding_rs::EUC_KR.encode("<meta charset=utf-8>한글");
        assert_eq!(decode_html(&bad), "<meta charset=utf-8>한글");
        assert_eq!(
            decode_html("<meta charset=\"utf-8\">한글".as_bytes()),
            "<meta charset=\"utf-8\">한글"
        );
        // 한 파일에 섞임: 머리글은 UTF-8, 본문은 EUC-KR(실데이터에 많다).
        let mut mixed = "<meta charset=\"ks_c_5601-1987\"><td>발신인</td><p>"
            .as_bytes()
            .to_vec();
        mixed.extend(encode_korean("본문 한글 내용"));
        mixed.extend(b"</p>");
        assert_eq!(
            decode_html(&mixed),
            "<meta charset=\"ks_c_5601-1987\"><td>발신인</td><p>본문 한글 내용</p>"
        );
        // 한 글자 EUC-KR 낱말이 UTF-8로도 맞는 경우("표" = C7 A5, "첨" = C3 B7)와 UTF-8 가운뎃점(C2 B7).
        let mut tricky = encode_korean("표 첨부 ");
        tricky.extend(b"\xff");
        assert_eq!(decode_html(&tricky), "표 첨부 \u{FFFD}");
        let mut dot = "가·나 ".as_bytes().to_vec();
        dot.extend(encode_korean("다"));
        assert_eq!(decode_html(&dot), "가·나 다");
        // meta는 ks_c_5601-1987인데 파일 전체가 UTF-8.
        assert_eq!(
            decode_html("<meta charset=\"ks_c_5601-1987\">발신인".as_bytes()),
            "<meta charset=\"ks_c_5601-1987\">발신인"
        );
        assert_eq!(
            decode_entities("A&amp;B &lt;x&gt; &#44032;&#xAC01; &nbsp;&bogus; &"),
            "A&B <x> 가각  &bogus; &"
        );
        assert_eq!(
            decode_entities("R&D 반기 보고서&amp;"),
            "R&D 반기 보고서&",
            "& 뒤 한글"
        );
        assert_eq!(
            original_name("$$_Attach_$$\\결산 (최종)_$147$_$190$_.xlsx"),
            "결산 (최종).xlsx"
        );
        assert_eq!(
            original_name("image001_$9$_$1$_"),
            "image001",
            "확장자 없는 첨부"
        );
        assert_eq!(original_name("a b_$9$_$1$_. xlsx"), "a b.xlsx");
        assert_eq!(original_name("plain.txt"), "plain.txt");
    }
}
