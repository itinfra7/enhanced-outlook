//! 회신·전달 작성(계획서 5.8, 9.3): 로컬 원문(EML)으로 받는 사람·제목·인용문·스레드 머리글을 만든다.
//!
//! - 인용 머리글은 웹메일과 같은 형식(보낸 사람/보낸 날짜/받는 사람/참조/제목).
//! - 보낼 원문 HTML은 정화하되 `cid:` 참조와 외부 이미지는 원문 그대로 둔다(인라인 이미지는 같은 Content-ID로
//!   다시 붙이고, 서버 전달(ForwardItem)은 서버가 원본 첨부를 함께 넣는다).

use std::collections::{HashMap, HashSet};

use mail_parser::{Address, MimeHeaders, PartType};
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};

use crate::RenderOptions;
use crate::render::{
    ImgMode, cid_links, clean_cid, escape_html, image_mime, mime_of, parse, render, sanitize, wrap,
};

/// 이름과 주소.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Person {
    pub name: String,
    pub email: String,
}

impl Person {
    /// "이름 <주소>" 또는 주소.
    pub fn display(&self) -> String {
        if self.name.is_empty() || self.name == self.email {
            self.email.clone()
        } else {
            format!("{} <{}>", self.name, self.email)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplyMode {
    Reply,
    ReplyAll,
    Forward,
}

/// 원문에서 다시 붙일 부분(인라인 이미지·첨부).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OriginalPart {
    pub name: String,
    pub mime: String,
    pub data: Vec<u8>,
    /// 인라인 이미지면 원문의 Content-ID(꺾쇠 없이, 원래 대소문자).
    pub content_id: Option<String>,
}

/// 회신·전달에 쓰는 원문 정보.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Original {
    pub message_id: Option<String>,
    pub references: Option<String>,
    pub subject: String,
    pub from: Vec<Person>,
    pub reply_to: Vec<Person>,
    pub to: Vec<Person>,
    pub cc: Vec<Person>,
    /// 보낸 날짜(unix ms).
    pub date: Option<i64>,
    /// 보낼 때 쓰는 원문 본문 HTML(정화, `cid:`·외부 이미지 유지).
    pub body_html: String,
    /// 본문이 쓰는 인라인 이미지.
    pub inline: Vec<OriginalPart>,
    /// 일반 첨부(로컬 보관본 전달 때 다시 붙인다).
    pub attachments: Vec<OriginalPart>,
}

/// 작성 창에 넣은 그림(`<img src="data:image/…;base64,…">`)을 본문 그림 인라인 첨부(`cid:`)로 바꾼다.
/// (바꾼 HTML, 붙일 그림들). 같은 그림을 여러 번 넣었으면 한 번만 붙인다. 해석할 수 없는 값은 그대로 둔다.
pub fn inline_data_images(html: &str) -> (String, Vec<OriginalPart>) {
    use base64::Engine as _;
    let mut out = String::with_capacity(html.len());
    let mut parts: Vec<OriginalPart> = Vec::new();
    let mut seen: std::collections::HashMap<&str, String> = std::collections::HashMap::new();
    let mut rest = html;
    while let Some(i) = rest.find("src=\"data:") {
        let v = i + "src=\"".len();
        let Some(len) = rest[v..].find('"') else {
            break;
        };
        let value = &rest[v..v + len];
        out.push_str(&rest[..v]);
        let parsed = value
            .strip_prefix("data:")
            .and_then(|x| x.split_once(','))
            .and_then(|(head, payload)| {
                let mime = head.strip_suffix(";base64")?.trim().to_ascii_lowercase();
                mime.starts_with("image/").then_some((mime, payload))
            });
        match parsed {
            Some((mime, payload)) => {
                let cid = match seen.get(payload) {
                    Some(c) => c.clone(),
                    None => {
                        let clean: String =
                            payload.chars().filter(|c| !c.is_whitespace()).collect();
                        match base64::engine::general_purpose::STANDARD.decode(clean) {
                            Ok(data) => {
                                let ext = match mime.as_str() {
                                    "image/jpeg" | "image/jpg" => "jpg",
                                    "image/gif" => "gif",
                                    "image/bmp" => "bmp",
                                    "image/webp" => "webp",
                                    _ => "png",
                                };
                                let name = format!("image{:03}.{ext}", parts.len() + 1);
                                let cid = format!("{name}@outlook");
                                parts.push(OriginalPart {
                                    name,
                                    mime,
                                    data,
                                    content_id: Some(cid.clone()),
                                });
                                seen.insert(payload, cid.clone());
                                cid
                            }
                            Err(_) => {
                                out.push_str(value);
                                rest = &rest[v + len..];
                                continue;
                            }
                        }
                    }
                };
                out.push_str("cid:");
                out.push_str(&cid);
            }
            None => out.push_str(value),
        }
        rest = &rest[v + len..];
    }
    out.push_str(rest);
    (out, parts)
}

fn people(a: Option<&Address<'_>>) -> Vec<Person> {
    let Some(a) = a else { return Vec::new() };
    a.iter()
        .filter_map(|x| {
            let email = x.address()?.trim().to_owned();
            let name = x.name().map(str::trim).unwrap_or("").to_owned();
            Some(Person { name, email })
        })
        .collect()
}

fn angle(id: &str) -> String {
    let id = id.trim();
    if id.starts_with('<') {
        id.to_owned()
    } else {
        format!("<{id}>")
    }
}

/// 원문을 해석한다. 해석할 수 없으면 `None`.
pub fn original(raw: &[u8]) -> Option<Original> {
    let msg = parse(raw)?;
    let is_html = msg
        .html_part(0)
        .is_some_and(|p| matches!(p.body, PartType::Html(_)));
    let html = if is_html {
        msg.body_html(0).unwrap_or_default()
    } else {
        Default::default()
    };
    let links = cid_links(&msg, &html);
    let cids: HashSet<String> = links.iter().map(|l| l.key.clone()).collect();
    let body_html = if is_html {
        sanitize(&html, ImgMode::Send, &cids).0
    } else {
        let mut text = String::new();
        for i in 0..msg.text_body_count() {
            if let Some(t) = msg.body_text(i) {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(&t);
            }
        }
        format!(
            "<div style=\"white-space:pre-wrap\">{}</div>",
            escape_html(&text)
        )
    };
    let lower_body = body_html.to_ascii_lowercase();
    let mut inline = Vec::new();
    // 본문이 쓰는 부분(같은 Content-ID로 다시 붙인다. 이름으로 이은 그림은 본문의 cid를 Content-ID로 준다).
    let mut used = HashSet::new();
    for l in &links {
        if !lower_body.contains(&format!("cid:{}", l.key)) {
            continue;
        }
        used.insert(l.part);
        let p = &msg.parts[l.part];
        inline.push(OriginalPart {
            name: p
                .attachment_name()
                .map_or_else(|| "image".to_owned(), str::to_owned),
            mime: image_mime(p).unwrap_or_else(|| mime_of(p)),
            data: p.contents().to_vec(),
            content_id: Some(l.cid.clone()),
        });
    }
    let attachments = msg
        .attachments
        .iter()
        .filter(|&&i| !used.contains(&(i as usize)))
        .filter_map(|&i| msg.parts.get(i as usize))
        .enumerate()
        .map(|(i, p)| OriginalPart {
            name: p
                .attachment_name()
                .map(str::to_owned)
                .or_else(|| {
                    p.message()
                        .and_then(|m| m.subject())
                        .map(|s| format!("{s}.eml"))
                })
                .unwrap_or_else(|| format!("첨부{}", i + 1)),
            mime: mime_of(p),
            data: p.contents().to_vec(),
            content_id: None,
        })
        .collect();
    let references = msg
        .header_raw("References")
        .map(|r| r.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|r| !r.is_empty());
    Some(Original {
        message_id: msg.message_id().map(angle),
        references,
        subject: msg.subject().unwrap_or_default().to_owned(),
        from: people(msg.from()),
        reply_to: people(msg.reply_to()),
        to: people(msg.to()),
        cc: people(msg.cc()),
        date: msg.date().map(|d| d.to_timestamp() * 1000),
        body_html,
        inline,
        attachments,
    })
}

/// 임시 보관함의 초안을 작성 화면으로 다시 열 때 쓰는 내용(0.7.0, 웹메일에서 쓰던 초안 등).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftParts {
    pub to: Vec<Person>,
    pub cc: Vec<Person>,
    pub bcc: Vec<Person>,
    pub subject: String,
    /// 본문 그림(`cid:`)을 `data:` 주소로 바꾼 HTML(작성 화면은 그림을 본문에 품는다).
    pub body_html: String,
    /// 일반 첨부.
    pub attachments: Vec<OriginalPart>,
    /// 0 낮음, 1 보통, 2 높음.
    pub importance: i64,
    /// 0 보통, 1 개인, 2 비공개, 3 비밀 우편.
    pub sensitivity: i64,
    pub read_receipt: bool,
    pub delivery_receipt: bool,
    pub plain_text: bool,
}

/// `hay`에서 `needle`(ASCII 대소문자 무시)을 모두 `with`로 바꾼다.
fn replace_ignore_case(hay: &str, needle: &str, with: &str) -> String {
    if needle.is_empty() {
        return hay.to_owned();
    }
    let lower = hay.to_ascii_lowercase();
    let needle = needle.to_ascii_lowercase();
    let mut out = String::with_capacity(hay.len());
    let mut last = 0;
    for (i, _) in lower.match_indices(&needle) {
        out.push_str(&hay[last..i]);
        out.push_str(with);
        last = i + needle.len();
    }
    out.push_str(&hay[last..]);
    out
}

/// 초안 원문(EML)을 작성 화면 내용으로. 해석할 수 없으면 `None`.
pub fn draft_parts(raw: &[u8]) -> Option<DraftParts> {
    use base64::Engine as _;
    let o = original(raw)?;
    let msg = parse(raw)?;
    let plain_text = !msg
        .html_part(0)
        .is_some_and(|p| matches!(p.body, PartType::Html(_)));
    let mut body_html = o.body_html.clone();
    for p in &o.inline {
        if let Some(cid) = &p.content_id {
            let data = base64::engine::general_purpose::STANDARD.encode(&p.data);
            body_html = replace_ignore_case(
                &body_html,
                &format!("cid:{cid}"),
                &format!("data:{};base64,{data}", p.mime),
            );
        }
    }
    let header = |name: &str| msg.header_raw(name).map(|v| v.trim().to_ascii_lowercase());
    let importance = match (header("Importance").as_deref(), header("X-Priority")) {
        (Some("high"), _) => 2,
        (Some("low"), _) => 0,
        (_, Some(p)) if p.starts_with('1') || p.starts_with('2') => 2,
        (_, Some(p)) if p.starts_with('4') || p.starts_with('5') => 0,
        _ => 1,
    };
    let sensitivity = match header("Sensitivity").as_deref() {
        Some("personal") => 1,
        Some("private") => 2,
        Some("company-confidential" | "confidential") => 3,
        _ => 0,
    };
    Some(DraftParts {
        to: o.to,
        cc: o.cc,
        bcc: people(msg.bcc()),
        subject: o.subject,
        body_html,
        attachments: o.attachments,
        importance,
        sensitivity,
        read_receipt: msg.header_raw("Disposition-Notification-To").is_some(),
        delivery_receipt: msg.header_raw("Return-Receipt-To").is_some(),
        plain_text,
    })
}

/// 제목 접두어(웹메일처럼 이미 붙어 있으면 다시 붙이지 않는다).
pub fn prefixed_subject(mode: ReplyMode, subject: &str) -> String {
    let s = subject.trim();
    let lower = s.to_lowercase();
    let (prefix, known): (&str, &[&str]) = match mode {
        ReplyMode::Reply | ReplyMode::ReplyAll => ("RE: ", &["re:", "회신:"]),
        ReplyMode::Forward => ("FW: ", &["fw:", "fwd:", "전달:"]),
    };
    if known.iter().any(|p| lower.starts_with(p)) {
        s.to_owned()
    } else {
        format!("{prefix}{s}")
    }
}

fn same(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

/// 받는 사람·참조. 회신: 회신 주소(없으면 보낸 사람), 내가 보낸 메일이면 원래 받는 사람.
/// 전체 회신: 거기에 원래 받는 사람을 더하고 참조는 원래 참조, 모두에서 나를 빼고 중복 제거. 전달: 비움.
/// `me`: 이 계정을 가리키는 모든 주소(대표 주소, 로그인 아이디 등. 1.0.5 사용자 지적: 대표 주소만 보면 로그인
/// 아이디 주소가 받는 사람인 메일의 전체 회신에 내가 들어갔다).
pub fn recipients(mode: ReplyMode, o: &Original, me: &[String]) -> (Vec<Person>, Vec<Person>) {
    if mode == ReplyMode::Forward {
        return (Vec::new(), Vec::new());
    }
    let is_me = |p: &Person| me.iter().any(|m| same(&p.email, m));
    let from_me = o.from.iter().any(is_me);
    // 내가 보낸 메일에 답하면(Outlook·웹메일처럼) 원래 받는 사람·참조를 그대로 쓴다. 나도 받는 사람이었으면 남긴다
    // (본인에게 보낸 메일의 전체 회신이 빈 받는 사람이 되던 문제, 6단계 실서버 확인).
    let keep_me = from_me;
    let push = |list: &mut Vec<Person>, p: &Person, others: &[Person]| {
        if (keep_me || !is_me(p)) && !list.iter().chain(others).any(|q| same(&q.email, &p.email)) {
            list.push(p.clone());
        }
    };
    let primary: Vec<Person> = if from_me {
        o.to.clone()
    } else if !o.reply_to.is_empty() {
        o.reply_to.clone()
    } else {
        o.from.clone()
    };
    let mut to: Vec<Person> = Vec::new();
    for p in &primary {
        push(&mut to, p, &[]);
    }
    let mut cc = Vec::new();
    if mode == ReplyMode::ReplyAll {
        if !from_me {
            for p in &o.to {
                push(&mut to, p, &[]);
            }
        }
        for p in &o.cc {
            push(&mut cc, p, &to);
        }
    }
    (to, cc)
}

/// (In-Reply-To, References): 로컬 보관본 회신의 스레드 머리글(계획서 5.8 확장 속성 값).
pub fn thread_headers(o: &Original) -> Option<(String, String)> {
    let mid = o.message_id.clone()?;
    let refs = match &o.references {
        Some(r) if !r.contains(&mid) => format!("{r} {mid}"),
        Some(r) => r.clone(),
        None => mid.clone(),
    };
    Some((mid, refs))
}

const DAYS: [&str; 7] = [
    "목요일",
    "금요일",
    "토요일",
    "일요일",
    "월요일",
    "화요일",
    "수요일",
];

/// unix ms → "2026년 10월 15일 목요일 오후 4:34"(한국 시간, 웹메일 인용 머리글 형식).
pub fn korean_datetime(ms: i64) -> String {
    let secs = ms.div_euclid(1000) + 9 * 3600;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    let (h, min) = (rem / 3600, (rem / 60) % 60);
    let (ampm, h12) = match h {
        0 => ("오전", 12),
        1..=11 => ("오전", h),
        12 => ("오후", 12),
        _ => ("오후", h - 12),
    };
    format!(
        "{y}년 {m}월 {d}일 {} {ampm} {h12}:{min:02}",
        DAYS[days.rem_euclid(7) as usize]
    )
}

fn join(list: &[Person]) -> String {
    list.iter()
        .map(|p| escape_html(&p.display()))
        .collect::<Vec<_>>()
        .join("; ")
}

/// HTML을 일반 텍스트로(웹메일 "일반 텍스트로 전환", 0.7.0). 문단·줄 바꿈·목록·표 칸을 살리고 태그·스타일은 뺀다.
pub fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 2);
    let mut rest = html;
    let mut skip_until: Option<&str> = None;
    while !rest.is_empty() {
        let Some(lt) = rest.find('<') else {
            if skip_until.is_none() {
                out.push_str(rest);
            }
            break;
        };
        if skip_until.is_none() {
            out.push_str(&rest[..lt]);
        }
        let tag_end = rest[lt..].find('>').map_or(rest.len(), |i| lt + i + 1);
        let tag = &rest[lt + 1..tag_end.saturating_sub(1).max(lt + 1)];
        rest = &rest[tag_end..];
        let closing = tag.starts_with('/');
        let name: String = tag
            .trim_start_matches('/')
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
            .to_ascii_lowercase();
        if let Some(end) = skip_until {
            if closing && name == end {
                skip_until = None;
            }
            continue;
        }
        match (name.as_str(), closing) {
            ("style" | "script" | "head" | "title", false) => {
                skip_until = Some(match name.as_str() {
                    "style" => "style",
                    "script" => "script",
                    "head" => "head",
                    _ => "title",
                });
            }
            ("br", _) => out.push('\n'),
            // 문단 끝은 늘 줄을 바꾼다(빈 문단 = 빈 줄).
            ("p", true) => out.push('\n'),
            ("p" | "div" | "tr" | "table" | "blockquote" | "ul" | "ol", _)
            | ("h1" | "h2" | "h3" | "h4" | "h5" | "h6", _) => {
                if !out.is_empty() && !out.ends_with('\n') {
                    out.push('\n');
                }
            }
            ("li", false) => {
                if !out.is_empty() && !out.ends_with('\n') {
                    out.push('\n');
                }
                out.push_str("- ");
            }
            ("td" | "th", true) => out.push('\t'),
            ("hr", _) => out.push_str("\n----------\n"),
            _ => {}
        }
    }
    let text = decode_entities(&out);
    // 세 줄 이상 빈 줄은 두 줄로, 줄 끝 공백·탭은 뺀다.
    let mut lines: Vec<&str> = Vec::new();
    let mut blank = 0;
    for line in text.lines().map(str::trim_end) {
        if line.is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        lines.push(line);
    }
    lines.join("\r\n").trim().to_owned()
}

fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let end = tail.bytes().take(12).position(|b| b == b';');
        let decoded = end.and_then(|e| {
            let ent = &tail[1..e];
            let c = match ent {
                "nbsp" => Some(' '),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "amp" => Some('&'),
                "quot" => Some('"'),
                "apos" | "#39" => Some('\''),
                _ => ent
                    .strip_prefix("#x")
                    .or_else(|| ent.strip_prefix("#X"))
                    .and_then(|h| u32::from_str_radix(h, 16).ok())
                    .or_else(|| ent.strip_prefix('#').and_then(|d| d.parse().ok()))
                    .and_then(char::from_u32),
            };
            c.map(|c| (c, e + 1))
        });
        match decoded {
            Some((c, len)) => {
                out.push(c);
                rest = &tail[len..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// 인용 머리글 + 원문 본문(보낼 본문의 뒷부분). 웹메일(Outlook on the web)과 같은 형식.
pub fn quote_html(o: &Original) -> String {
    format!("{}<div>{}</div>", quote_head_html(o), o.body_html)
}

/// 작성 창의 인용문 문서(0.9.0 사용자 요청: 회신·전달의 원문도 고칠 수 있게 작성 창에서 바로 고친다). 머리글 +
/// 화면용 원문. 본문 그림은 `cid_base` 주소로 보이고, 외부 그림 주소는 그대로 두되(고쳐서 보내도 남게) 불러오지는
/// 않는다(CSP). 해석할 수 없으면 `None`.
pub fn quote_doc(raw: &[u8], cid_base: &str, o: &Original) -> Option<String> {
    let r = render(
        raw,
        &RenderOptions {
            cid_base: cid_base.to_owned(),
            allow_remote: true,
        },
    )?;
    let start = r.html.find("<body>")? + "<body>".len();
    let end = r.html.rfind("</body>")?;
    Some(wrap(
        &format!("{}{}", quote_head_html(o), &r.html[start..end]),
        false,
    ))
}

/// 고친 인용문(작성 창 문서의 본문)을 다시 작성 창 문서로(임시 저장한 메일을 다시 열 때).
pub fn edited_quote_doc(edited: &str) -> String {
    wrap(edited, false)
}

/// 고친 인용문 → 보낼 HTML. 본문 그림 주소(`cid_base`)를 원문의 `cid:`로 되돌리고 보낼 때처럼 정화한다(원문에
/// 없는 그림 참조는 빠진다).
pub fn edited_quote(edited: &str, o: &Original, cid_base: &str) -> String {
    let back: HashMap<String, &str> = o
        .inline
        .iter()
        .filter_map(|p| p.content_id.as_deref())
        .map(|c| (clean_cid(c), c))
        .collect();
    let mut html = edited.to_owned();
    for (key, cid) in &back {
        let url = format!("{cid_base}{}", utf8_percent_encode(key, NON_ALPHANUMERIC));
        html = html.replace(&url, &format!("cid:{cid}"));
    }
    let known: HashSet<String> = back.into_keys().collect();
    sanitize(&html, ImgMode::Send, &known).0
}

/// 인용 머리글만(작성 창 미리 보기에서 화면용 원문 앞에 붙인다).
pub fn quote_head_html(o: &Original) -> String {
    let mut head = format!("<b>보낸 사람:</b> {}<br>", join(&o.from));
    if let Some(d) = o.date {
        head.push_str(&format!("<b>보낸 날짜:</b> {}<br>", korean_datetime(d)));
    }
    if !o.to.is_empty() {
        head.push_str(&format!("<b>받는 사람:</b> {}<br>", join(&o.to)));
    }
    if !o.cc.is_empty() {
        head.push_str(&format!("<b>참조:</b> {}<br>", join(&o.cc)));
    }
    head.push_str(&format!("<b>제목:</b> {}", escape_html(&o.subject)));
    format!(
        "<hr style=\"display:inline-block;width:98%\" tabindex=\"-1\">\
         <div id=\"divRplyFwdMsg\" dir=\"ltr\"><font face=\"Calibri, sans-serif\" style=\"font-size:11pt\" color=\"#000000\">{head}</font>\
         <div>&nbsp;</div></div>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draft_parts_inline_images_and_options() {
        let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";
        let eml = format!(
            "From: =?UTF-8?B?64KY?= <me@x.com>\r\nTo: 김 <kim@x.com>\r\nBcc: boss@x.com\r\nSubject: 초안\r\n\
             Importance: high\r\nSensitivity: Company-Confidential\r\nDisposition-Notification-To: me@x.com\r\n\
             MIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"M\"\r\n\r\n\
             --M\r\nContent-Type: multipart/related; boundary=\"R\"\r\n\r\n\
             --R\r\nContent-Type: text/html; charset=UTF-8\r\n\r\n<p>쓰던 글<img src=\"cid:Pic1@X\"></p>\r\n\
             --R\r\nContent-Type: image/png\r\nContent-ID: <pic1@x>\r\nContent-Transfer-Encoding: base64\r\n\r\n{png}\r\n\
             --R--\r\n\
             --M\r\nContent-Type: application/pdf; name=\"a.pdf\"\r\nContent-Disposition: attachment; filename=\"a.pdf\"\r\n\
             Content-Transfer-Encoding: base64\r\n\r\nJVBERi0=\r\n--M--\r\n"
        );
        let d = draft_parts(eml.as_bytes()).unwrap();
        assert_eq!(d.subject, "초안");
        assert_eq!(
            (d.to[0].email.as_str(), d.bcc[0].email.as_str()),
            ("kim@x.com", "boss@x.com")
        );
        assert!(
            d.body_html.contains("src=\"data:image/png;base64,iVBOR"),
            "{}",
            d.body_html
        );
        assert!(!d.body_html.to_ascii_lowercase().contains("cid:"));
        assert_eq!(d.attachments.len(), 1);
        assert_eq!(d.attachments[0].name, "a.pdf");
        assert_eq!(
            (
                d.importance,
                d.sensitivity,
                d.read_receipt,
                d.delivery_receipt,
                d.plain_text
            ),
            (2, 3, true, false, false)
        );
    }

    #[test]
    fn html_to_text_keeps_lines_lists_and_cells() {
        let html = "<style>p{color:red}</style><div style=\"font-family:x\"><p>안녕하세요&nbsp;김&amp;이</p>\
                    <p></p><p></p><p></p><ul><li>하나</li><li>둘</li></ul>\
                    <table><tr><td>가</td><td>나</td></tr></table>줄<br>바꿈 &#54620;&#x4E00; &bogus; 끝</div>";
        assert_eq!(
            html_to_text(html),
            "안녕하세요 김&이\r\n\r\n- 하나\r\n- 둘\r\n가\t나\r\n줄\r\n바꿈 한一 &bogus; 끝"
        );
    }

    #[test]
    fn pasted_images_become_cid_inline_parts() {
        // 1×1 PNG 두 번(같은 그림) + JPEG 하나 + 그림 아닌 data: + 깨진 base64.
        let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";
        let html = format!(
            "<p>가<img src=\"data:image/png;base64,{png}\" alt=\"a.png\"> 나 <img src=\"data:image/png;base64,{png}\">\
             <img src=\"data:image/jpeg;base64,/9j/4A==\"><img src=\"data:text/html;base64,PGI+\"><img src=\"data:image/png;base64,@@@\"></p>"
        );
        let (out, parts) = inline_data_images(&html);
        assert_eq!(parts.len(), 2, "같은 그림은 한 번");
        assert_eq!(
            parts
                .iter()
                .map(|p| (p.name.as_str(), p.mime.as_str(), p.content_id.as_deref()))
                .collect::<Vec<_>>(),
            [
                ("image001.png", "image/png", Some("image001.png@outlook")),
                ("image002.jpg", "image/jpeg", Some("image002.jpg@outlook"))
            ]
        );
        assert_eq!(&parts[0].data[..4], b"\x89PNG");
        assert_eq!(out.matches("src=\"cid:image001.png@outlook\"").count(), 2);
        assert!(out.contains("src=\"cid:image002.jpg@outlook\""));
        assert!(
            out.contains("data:text/html;base64,PGI+") && out.contains("base64,@@@"),
            "그 밖은 그대로"
        );
        assert!(out.contains("alt=\"a.png\"") && out.starts_with("<p>가<img"));
        assert_eq!(inline_data_images("<p>그림 없음</p>").0, "<p>그림 없음</p>");
    }

    const EML: &str = concat!(
        "From: =?UTF-8?B?6rmA66+87IiY?= <minsu.kim@example.com>\r\n",
        "To: me@x.com, other@x.com\r\n",
        "Cc: Boss <boss@x.com>, ME@x.com\r\n",
        "Subject: =?UTF-8?B?7KO86rCEIOuztOqzoA==?=\r\n",
        "Message-ID: <orig@x.com>\r\n",
        "References: <a@x.com>\r\n",
        "Date: Thu, 15 Oct 2026 16:34:00 +0900\r\n",
        "MIME-Version: 1.0\r\n",
        "Content-Type: multipart/mixed; boundary=\"m\"\r\n",
        "\r\n",
        "--m\r\n",
        "Content-Type: multipart/related; boundary=\"r\"\r\n",
        "\r\n",
        "--r\r\n",
        "Content-Type: text/html; charset=UTF-8\r\n",
        "\r\n",
        "<p>본문</p><img src=\"cid:Img1@X\"><img src=\"https://ex.org/a.png\"><script>x()</script>\r\n",
        "--r\r\n",
        "Content-Type: image/png\r\n",
        "Content-ID: <Img1@X>\r\n",
        "Content-Transfer-Encoding: base64\r\n",
        "\r\n",
        "iVBORw0KGgo=\r\n",
        "--r--\r\n",
        "--m\r\n",
        "Content-Type: application/pdf\r\n",
        "Content-Disposition: attachment; filename=\"a.pdf\"\r\n",
        "Content-Transfer-Encoding: base64\r\n",
        "\r\n",
        "JVBERg==\r\n",
        "--m--\r\n"
    );

    #[test]
    fn original_keeps_cid_and_remote_and_splits_parts() {
        let o = original(EML.as_bytes()).unwrap();
        assert_eq!(o.subject, "주간 보고");
        assert_eq!(o.message_id.as_deref(), Some("<orig@x.com>"));
        assert!(
            o.body_html.contains("src=\"cid:Img1@X\""),
            "{}",
            o.body_html
        );
        assert!(o.body_html.contains("https://ex.org/a.png"));
        assert!(!o.body_html.contains("<script"));
        assert_eq!(o.inline.len(), 1);
        assert_eq!(o.inline[0].content_id.as_deref(), Some("Img1@X"));
        assert_eq!(
            o.attachments
                .iter()
                .map(|a| a.name.as_str())
                .collect::<Vec<_>>(),
            ["a.pdf"]
        );
        assert_eq!(
            thread_headers(&o),
            Some(("<orig@x.com>".into(), "<a@x.com> <orig@x.com>".into()))
        );
    }

    #[test]
    fn edited_quote_round_trips_images() {
        let o = original(EML.as_bytes()).unwrap();
        let base = "http://app-cid.localhost/7/";
        let doc = quote_doc(EML.as_bytes(), base, &o).unwrap();
        assert!(doc.contains(&format!("{base}img1%40x")), "{doc}");
        assert!(
            doc.contains("https://ex.org/a.png"),
            "외부 그림 주소는 남는다"
        );
        assert!(
            doc.contains("img-src http://app-cid.localhost data:;"),
            "불러오지는 않는다"
        );
        assert!(doc.contains("보낸 사람:"), "머리글");
        // 작성 창에서 고친 본문(작성 창이 넘겨주는 body 안쪽).
        let body = &doc[doc.find("<body>").unwrap() + 6..doc.rfind("</body>").unwrap()];
        let edited = format!(
            "<p>앞에 덧붙임</p>{}<script>x</script>",
            body.replace("보낸 사람:", "From:")
        );
        let out = edited_quote(&edited, &o, base);
        assert!(out.contains("src=\"cid:Img1@X\""), "{out}");
        assert!(out.contains("https://ex.org/a.png"));
        assert!(out.contains("앞에 덧붙임") && out.contains("From:"));
        assert!(!out.contains("<script") && !out.contains("app-cid"));
        assert_eq!(edited_quote_doc(&out).matches("<body>").count(), 1);
    }

    #[test]
    fn recipients_subjects_and_quote() {
        let o = original(EML.as_bytes()).unwrap();
        let emails = |v: &[Person]| v.iter().map(|p| p.email.clone()).collect::<Vec<_>>();
        let me = ["me@x.com".to_owned()];
        let (to, cc) = recipients(ReplyMode::Reply, &o, &me);
        assert_eq!(
            (emails(&to), emails(&cc)),
            (vec!["minsu.kim@example.com".to_owned()], vec![])
        );
        let (to, cc) = recipients(ReplyMode::ReplyAll, &o, &me);
        assert_eq!(emails(&to), ["minsu.kim@example.com", "other@x.com"]);
        assert_eq!(emails(&cc), ["boss@x.com"], "나(대소문자 무관)는 뺀다");
        assert_eq!(recipients(ReplyMode::Forward, &o, &[]), (vec![], vec![]));

        assert_eq!(
            prefixed_subject(ReplyMode::Reply, "주간 보고"),
            "RE: 주간 보고"
        );
        assert_eq!(prefixed_subject(ReplyMode::Reply, "Re: 주간"), "Re: 주간");
        assert_eq!(
            prefixed_subject(ReplyMode::Forward, "RE: 주간"),
            "FW: RE: 주간"
        );

        assert_eq!(
            korean_datetime(1_792_049_640_000),
            "2026년 10월 15일 목요일 오후 4:34"
        );
        let q = quote_html(&o);
        assert!(
            q.contains("<b>보낸 사람:</b> 김민수 &lt;minsu.kim@example.com&gt;"),
            "{q}"
        );
        assert!(q.contains("<b>보낸 날짜:</b> 2026년 10월 15일 목요일 오후 4:34"));
        assert!(q.contains("<b>참조:</b> Boss &lt;boss@x.com&gt;; ME@x.com"));
        assert!(q.contains("<b>제목:</b> 주간 보고") && q.contains("<p>본문</p>"));
    }

    #[test]
    fn replying_to_my_own_mail_goes_to_original_recipients() {
        let me = ["me@x.com".to_owned()];
        let eml = "From: me@x.com\r\nTo: a@x.com\r\nCc: b@x.com\r\nSubject: s\r\n\r\nhi";
        let o = original(eml.as_bytes()).unwrap();
        let (to, cc) = recipients(ReplyMode::ReplyAll, &o, &me);
        assert_eq!(
            (to.len(), to[0].email.as_str(), cc[0].email.as_str()),
            (1, "a@x.com", "b@x.com")
        );
        assert!(o.body_html.contains("white-space:pre-wrap"));

        // 본인에게 보낸 메일: 회신·전체 회신 모두 받는 사람은 본인.
        let self_sent =
            original(b"From: me@x.com\r\nTo: Me <ME@x.com>\r\nSubject: s\r\n\r\nhi").unwrap();
        for mode in [ReplyMode::Reply, ReplyMode::ReplyAll] {
            let (to, cc) = recipients(mode, &self_sent, &me);
            assert_eq!(
                (
                    to.iter().map(|p| p.email.as_str()).collect::<Vec<_>>(),
                    cc.len()
                ),
                (vec!["ME@x.com"], 0),
                "{mode:?}"
            );
        }
    }

    /// 1.0.5 사용자 지적: 대표 주소(me@mail.x.com)만 나로 보면 로그인 아이디 주소(me@x.com)가 받는 사람인 메일의
    /// 전체 회신에 내가 들어갔다. 내 주소 모두를 뺀다.
    #[test]
    fn reply_all_leaves_out_every_address_of_mine() {
        let me = ["me@mail.x.com".to_owned(), "me@x.com".to_owned()];
        let emails = |v: &[Person]| v.iter().map(|p| p.email.clone()).collect::<Vec<_>>();
        for to_me in ["me@x.com", "ME@mail.x.com"] {
            let eml = format!(
                "From: a@y.com\r\nTo: {to_me}, b@y.com\r\nCc: Me <me@x.com>, c@y.com, me@mail.x.com\r\nSubject: s\r\n\r\nhi"
            );
            let o = original(eml.as_bytes()).unwrap();
            let (to, cc) = recipients(ReplyMode::ReplyAll, &o, &me);
            assert_eq!(emails(&to), ["a@y.com", "b@y.com"], "{to_me}");
            assert_eq!(emails(&cc), ["c@y.com"], "{to_me}");
        }
        // 다른 내 주소로 보낸 메일도 내가 보낸 메일이다(원래 받는 사람에게).
        let o = original(b"From: me@x.com\r\nTo: b@y.com\r\nSubject: s\r\n\r\nhi").unwrap();
        let (to, _) = recipients(ReplyMode::Reply, &o, &me);
        assert_eq!(emails(&to), ["b@y.com"]);
    }
}
