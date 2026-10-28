//! 읽기 창 렌더링(계획서 9.2).
//!
//! - HTML 부분 우선, 없으면 텍스트를 `<pre>`로.
//! - `ammonia`로 정화(스크립트, `on*` 속성, form, iframe, object 제거). 결과는 샌드박스 iframe
//!   (`allow-scripts` 없음)의 `srcdoc`에 넣는 완성 문서이고, CSP를 `<meta>`로 함께 넣는다.
//! - `cid:` 이미지는 `cid_base` + 인코딩한 Content-ID로 바꾼다(앱의 `app-cid` 프로토콜이 원문에서 꺼내 준다).
//! - 외부 이미지는 기본 차단(`allow_remote`가 아니면 지우고 [`Rendered::has_remote_images`] 표시).

use std::borrow::Cow;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use mail_parser::{Address, Message, MessageParser, MimeHeaders, PartType};
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};

#[derive(Debug, Clone)]
pub struct RenderOptions {
    /// `cid:` 이미지 주소 앞부분, 예: `http://app-cid.localhost/123/`.
    pub cid_base: String,
    /// 외부(http/https) 이미지를 보여 줄까.
    pub allow_remote: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentInfo {
    /// [`attachment`]에 넘길 순번.
    pub index: usize,
    pub name: String,
    pub mime: String,
    pub size: usize,
    /// 본문에 `cid:`로 들어간 인라인 이미지(첨부 목록에는 보통 숨긴다).
    pub inline: bool,
}

/// 본문이 `cid:`로 쓰는 이미지(읽기 창 열 때 한 번에 꺼내 둔다).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineImage {
    /// 소문자, `<>` 없음(`cid_base` 뒤에 인코딩해 붙는 값).
    pub cid: String,
    pub mime: String,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rendered {
    pub subject: String,
    pub from: String,
    pub to: String,
    pub cc: String,
    pub bcc: String,
    pub reply_to: String,
    /// 보낸 날짜(unix ms).
    pub date: Option<i64>,
    /// iframe `srcdoc`에 그대로 넣는 문서.
    pub html: String,
    pub has_remote_images: bool,
    pub attachments: Vec<AttachmentInfo>,
    /// 본문이 실제로 쓰는 인라인 이미지.
    pub inline_images: Vec<InlineImage>,
}

pub(crate) fn mime_of(p: &mail_parser::MessagePart<'_>) -> String {
    p.content_type().map_or_else(
        || "application/octet-stream".to_owned(),
        |ct| match ct.subtype() {
            Some(sub) => format!("{}/{sub}", ct.ctype()),
            None => ct.ctype().to_owned(),
        },
    )
}

fn addresses(a: Option<&Address<'_>>) -> String {
    let Some(a) = a else {
        return String::new();
    };
    a.iter()
        .filter_map(|x| {
            match (
                x.name().map(str::trim),
                x.address().filter(|e| !e.trim().is_empty()),
            ) {
                (Some(n), Some(e)) if !n.is_empty() && n != e => Some(format!("{n} <{e}>")),
                (_, Some(e)) => Some(e.to_owned()),
                (Some(n), None) if !n.is_empty() => Some(n.to_owned()),
                _ => None,
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// 이 앱이 백업에서 복원한 메일인가: `X-EO-Restored` 머리글. 앞 버전들이 다른 이름(`X-…-Restored`)으로 붙인
/// 원문도 그대로 저장소에 있어 이름 모양으로 본다.
pub(crate) fn is_restored(msg: &Message<'_>) -> bool {
    msg.headers().iter().any(|h| {
        let n = h.name.as_str().to_ascii_lowercase();
        n.starts_with("x-") && n.ends_with("-restored") && n.len() > "x--restored".len()
    })
}

pub(crate) fn clean_cid(cid: &str) -> String {
    cid.trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
        .to_ascii_lowercase()
}

/// 본문 `cid:` 참조와 그것이 가리키는 부분.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CidLink {
    /// [`clean_cid`]한 값(소문자, `<>` 없음).
    pub key: String,
    /// 다시 붙일 때 쓸 Content-ID(`<>` 없음, 대소문자 그대로).
    pub cid: String,
    /// `msg.parts` 번호.
    pub part: usize,
}

/// 부분이 그림이면 그 MIME 형식(형식이 octet-stream 등이면 내용·이름으로 본다).
pub(crate) fn image_mime(p: &mail_parser::MessagePart<'_>) -> Option<String> {
    let m = mime_of(p);
    if m.starts_with("image/") {
        return Some(m);
    }
    if matches!(
        p.body,
        PartType::Text(_) | PartType::Html(_) | PartType::Multipart(_) | PartType::Message(_)
    ) {
        return None;
    }
    crate::domino::sniff_image(p.contents())
        .or_else(|| {
            p.attachment_name()
                .map(crate::backup::mime_by_name)
                .filter(|m| m.starts_with("image/"))
        })
        .map(str::to_owned)
}

/// 본문(HTML)의 `cid:` 참조를 원문 부분과 잇는다.
/// 1. Content-ID가 같은 부분.
/// 2. 없으면 파일 이름(또는 Content-Location)이 cid의 `@` 앞(`image001.png@01D…` → `image001.png`)이나 cid 전체와 같은 부분.
/// 3. 복원한 메일(`X-EO-Restored`)에서, 그래도 못 찾은 cid 수와 Content-ID 없는 그림 수가 같으면 나온 순서대로.
///
/// 2·3은 HCL Domino 백업처럼 본문 그림이 Content-ID 없이 첨부로만 남은 메일(`pastedImage.png`, `image001.png` 등)을 위한 것이다.
/// 3은 진짜 첨부 사진을 본문 그림으로 잘못 보일 수 있어 복원한 메일에만 쓴다.
pub(crate) fn cid_links(msg: &Message<'_>, html: &str) -> Vec<CidLink> {
    let mut out: Vec<CidLink> = Vec::new();
    for (i, p) in msg.parts.iter().enumerate() {
        if let Some(c) = p.content_id() {
            let key = clean_cid(c);
            if !key.is_empty() && !out.iter().any(|l| l.key == key) {
                let cid = c.trim().trim_matches(['<', '>']).to_owned();
                out.push(CidLink { key, cid, part: i });
            }
        }
    }
    // 본문이 쓰는 cid(나온 순서, 중복 없이)와 그중 Content-ID로 못 찾은 것.
    let lower = html.to_ascii_lowercase();
    let mut in_body: Vec<String> = Vec::new();
    let mut missing: Vec<(String, String)> = Vec::new();
    let mut at = 0;
    while let Some(i) = lower[at..].find("cid:") {
        let s = at + i + 4;
        let e = lower[s..]
            .find(['"', '\'', ' ', '>', ')', '\r', '\n', '\t'])
            .map_or(lower.len(), |x| s + x);
        at = e;
        let decoded = percent_encoding::percent_decode_str(&html[s..e])
            .decode_utf8_lossy()
            .into_owned();
        let key = clean_cid(&decoded);
        if key.is_empty() || in_body.contains(&key) {
            continue;
        }
        in_body.push(key.clone());
        if !out.iter().any(|l| l.key == key) {
            missing.push((key, decoded.trim().trim_matches(['<', '>']).to_owned()));
        }
    }
    if missing.is_empty() {
        return out;
    }
    // 이름으로 이을 후보: 본문이 쓰지 않는 그림(Content-ID가 없거나, 있어도 본문에 안 나오는 것).
    let free: Vec<usize> = msg
        .parts
        .iter()
        .enumerate()
        .filter(|(_, p)| {
            image_mime(p).is_some()
                && p.content_id()
                    .is_none_or(|c| !in_body.contains(&clean_cid(c)))
        })
        .map(|(i, _)| i)
        .collect();
    let loose: Vec<usize> = free
        .iter()
        .copied()
        .filter(|i| msg.parts[*i].content_id().is_none())
        .collect();
    let mut taken: Vec<usize> = Vec::new();
    let mut unresolved: Vec<(String, String)> = Vec::new();
    for (key, cid) in missing {
        let local = key.split('@').next().unwrap_or(&key).to_owned();
        let hit = free.iter().copied().find(|i| {
            let p = &msg.parts[*i];
            !taken.contains(i)
                && [p.attachment_name(), p.content_location()]
                    .into_iter()
                    .flatten()
                    .map(|n| n.trim().to_ascii_lowercase())
                    .any(|n| n == local || n == key)
        });
        match hit {
            Some(i) => {
                taken.push(i);
                out.push(CidLink { key, cid, part: i });
            }
            None => unresolved.push((key, cid)),
        }
    }
    let rest: Vec<usize> = loose.into_iter().filter(|i| !taken.contains(i)).collect();
    let restored = is_restored(msg);
    if restored && !unresolved.is_empty() && unresolved.len() == rest.len() {
        for ((key, cid), part) in unresolved.into_iter().zip(rest) {
            out.push(CidLink { key, cid, part });
        }
    }
    out
}

pub(crate) fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
    out
}

/// 큰 그림은 창 폭에 맞춘다. `max-width:100%`(칸 폭 기준)는 쓰지 않는다: WebView2(Chromium)는 그런 그림을
/// 표 칸 안에서 0폭까지 줄일 수 있는 것으로 보아, 옆 칸이 `width="100%"`면 그림이 사라진다(Jira 알림 바닥글 로고).
const BASE_STYLE: &str = "body{margin:0;padding:16px;font-family:'Malgun Gothic','Segoe UI',sans-serif;\
font-size:14px;line-height:1.5;color:#1f1f1f;background:#fff;word-wrap:break-word;overflow-wrap:anywhere}\
img{max-width:calc(100vw - 32px);height:auto}pre.text{white-space:pre-wrap;font-family:inherit;margin:0}\
blockquote{margin:0 0 0 .8em;padding-left:.8em;border-left:2px solid #c8c8c8}table{max-width:100%}";

/// 정화한 본문 조각을 CSP·기본 스타일이 든 완성 문서로 감싼다.
pub(crate) fn wrap(body: &str, allow_remote: bool) -> String {
    let img_src = if allow_remote {
        "http://app-cid.localhost data: https: http:"
    } else {
        "http://app-cid.localhost data:"
    };
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\">\
         <meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; img-src {img_src}; \
         style-src 'unsafe-inline'; font-src data:\">\
         <meta name=\"referrer\" content=\"no-referrer\">\
         <base target=\"_blank\"><style>{BASE_STYLE}</style></head><body>{body}</body></html>"
    )
}

/// 이미지 주소 처리 방식.
#[derive(Debug, Clone)]
pub(crate) enum ImgMode {
    /// 읽기 창: `cid:`는 `cid_base` 주소로, 외부 이미지는 `allow_remote`가 아니면 지운다.
    Display {
        cid_base: String,
        allow_remote: bool,
    },
    /// 회신·전달로 보낼 때: `cid:`는 원문 그대로(같은 Content-ID로 다시 붙인다), 외부 이미지도 그대로.
    Send,
}

pub(crate) fn sanitize(html: &str, mode: ImgMode, cids: &HashSet<String>) -> (String, bool) {
    let blocked = Arc::new(AtomicBool::new(false));
    let flag = blocked.clone();
    let (cid_base, allow_remote, send) = match mode {
        ImgMode::Display {
            cid_base,
            allow_remote,
        } => (cid_base, allow_remote, false),
        ImgMode::Send => (String::new(), true, true),
    };
    let known: HashSet<String> = cids.clone();
    let mut b = ammonia::Builder::default();
    b.add_tags([
        "style", "center", "font", "table", "thead", "tbody", "tfoot", "tr", "td", "th",
        "colgroup", "col", "caption", "span", "div", "img", "hr", "u", "s", "strike", "sup", "sub",
    ])
    .add_generic_attributes([
        "style",
        "class",
        "align",
        "valign",
        "width",
        "height",
        "bgcolor",
        "color",
        "border",
        "cellpadding",
        "cellspacing",
        "dir",
        "lang",
        "face",
        "size",
        "colspan",
        "rowspan",
    ])
    .add_tag_attributes("img", ["src", "alt", "title", "width", "height"])
    .add_tag_attributes("a", ["href", "title"])
    // cid:는 아래 필터가 app-cid 주소로 바꾼다(검사 단계에서 지워지지 않게 허용).
    .add_url_schemes(["data", "cid"])
    .clean_content_tags(HashSet::from(["script", "title"]))
    .strip_comments(true)
    .link_rel(Some("noopener noreferrer"))
    .attribute_filter(move |element, attribute, value| {
        if element != "img" || attribute != "src" {
            return Some(Cow::Borrowed(value));
        }
        let v = value.trim();
        let lower = v.to_ascii_lowercase();
        if let Some(cid) = lower.strip_prefix("cid:").map(|_| &v[4..]) {
            let key = clean_cid(&percent_encoding::percent_decode_str(cid).decode_utf8_lossy());
            if !known.contains(&key) {
                return None;
            }
            if send {
                return Some(Cow::Borrowed(value));
            }
            let enc = utf8_percent_encode(&key, NON_ALPHANUMERIC).to_string();
            return Some(Cow::Owned(format!("{cid_base}{enc}")));
        }
        if lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("//")
        {
            if allow_remote {
                return Some(Cow::Borrowed(value));
            }
            flag.store(true, Ordering::Relaxed);
            return None;
        }
        if lower.starts_with("data:image/") {
            return Some(Cow::Borrowed(value));
        }
        None
    });
    let out = b.clean(html).to_string();
    (out, blocked.load(Ordering::Relaxed))
}

/// 텍스트만으로 읽기 창 문서를 만든다(원문을 아직 받지 않았거나 해석하지 못했을 때).
pub fn text_document(text: &str) -> String {
    wrap(
        &format!("<pre class=\"text\">{}</pre>", escape_html(text)),
        false,
    )
}

pub(crate) fn parse(raw: &[u8]) -> Option<Message<'_>> {
    MessageParser::default().parse(raw)
}

/// 본문 그림 참조 진단(`owa-cli store images`): 내용·경로 없이 종류와 서버 이름만.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImageReport {
    /// (종류, 서버, 개수). 종류: "img https", "img http", "img //", "img data", "img cid(있음)", "img cid(없음)",
    /// "img 기타", "css url ..." 등.
    pub refs: Vec<(String, String, usize)>,
    /// (MIME 형식, 크기, Content-ID 있음, 첨부 표시) — 본문 아닌 부분들.
    pub parts: Vec<(String, usize, bool, bool)>,
    /// HTML 본문이 있는가.
    pub html: bool,
    /// 외부 그림 주소(http·https·//). 화면에 찍지 말고 접속 확인에만 쓴다.
    pub remote: Vec<String>,
    /// 원문에 없는 `cid:` 이름들.
    pub missing_cids: Vec<String>,
    /// 상대 경로 등 그 밖의 그림 주소들.
    pub others: Vec<String>,
    /// 본문 아닌 부분들의 파일 이름(소문자).
    pub names: Vec<String>,
    /// 본문 어디든(img 밖 포함) 나오는 서로 다른 `cid:` 값의 형태(위치 표시: img·background·a·기타).
    pub cid_contexts: Vec<String>,
    /// 복원한 메일(`X-EO-Restored`)인가.
    pub restored: bool,
    /// MIME 구조: (깊이, 형식).
    pub tree: Vec<(usize, String)>,
    /// 진단에 쓸 머리글(이름: 값) — 값에 개인 정보가 없는 것만.
    pub headers: Vec<(String, String)>,
}

/// 원문의 본문 그림 참조를 종류·서버별로 센다(진단용).
pub fn image_report(raw: &[u8]) -> Option<ImageReport> {
    let msg = parse(raw)?;
    let is_html = msg
        .html_part(0)
        .is_some_and(|p| matches!(p.body, PartType::Html(_)));
    let html = if is_html {
        msg.body_html(0).unwrap_or_default().into_owned()
    } else {
        String::new()
    };
    let cids: HashSet<String> = cid_links(&msg, &html).into_iter().map(|l| l.key).collect();
    let lower = html.to_ascii_lowercase();
    let mut counts: std::collections::BTreeMap<(String, String), usize> = Default::default();
    let mut remote = Vec::new();
    let mut missing_cids = Vec::new();
    let mut others = Vec::new();
    let mut classify = |v: &str| -> (String, String) {
        let v = v.trim().trim_matches(['\'', '"']);
        let l = v.to_ascii_lowercase();
        let host = |rest: &str| {
            rest.split(['/', '?', '#', ':'])
                .next()
                .unwrap_or("")
                .to_owned()
        };
        let kind = ["https://", "http://", "//"]
            .into_iter()
            .find(|p| l.starts_with(p));
        if let Some(p) = kind {
            remote.push(v.replace("&amp;", "&"));
            (p.trim_end_matches("://").into(), host(&l[p.len()..]))
        } else if l.starts_with("data:") {
            ("data".into(), String::new())
        } else if let Some(c) = l.strip_prefix("cid:") {
            let key = clean_cid(&percent_encoding::percent_decode_str(c).decode_utf8_lossy());
            if cids.contains(&key) {
                ("cid(있음)".into(), String::new())
            } else {
                missing_cids.push(key);
                ("cid(없음)".into(), String::new())
            }
        } else if l.is_empty() {
            ("빈 값".into(), String::new())
        } else {
            others.push(v.to_owned());
            ("기타(상대 경로 등)".into(), String::new())
        }
    };
    // <img ... src="..."> 와 CSS url(...).
    let mut at = 0;
    while let Some(i) = lower[at..].find("<img") {
        let start = at + i;
        let end = lower[start..].find('>').map_or(lower.len(), |e| start + e);
        let tag = &html[start..end];
        let tl = &lower[start..end];
        // 속성 이름 경계에서만(아웃룩의 `originalsrc="cid:…"` 안의 `src=`를 잡지 않게, HCL Domino 실메일 2026-10-21).
        let src_at = tl
            .match_indices("src=")
            .map(|(i, _)| i)
            .find(|&i| i > 0 && tl.as_bytes()[i - 1].is_ascii_whitespace());
        if let Some(s) = src_at {
            let rest = &tag[s + 4..];
            let quote = rest.chars().next().filter(|c| *c == '"' || *c == '\'');
            let v = match quote {
                Some(q) => rest[1..].split(q).next().unwrap_or(""),
                None => rest.split([' ', '>']).next().unwrap_or(""),
            };
            let (k, h) = classify(v);
            *counts.entry((format!("img {k}"), h)).or_default() += 1;
        } else {
            *counts
                .entry(("img src 없음".into(), String::new()))
                .or_default() += 1;
        }
        at = end;
    }
    let mut at = 0;
    while let Some(i) = lower[at..].find("url(") {
        let s = at + i + 4;
        let e = lower[s..].find(')').map_or(lower.len(), |x| s + x);
        let (k, h) = classify(&html[s..e]);
        *counts.entry((format!("css url {k}"), h)).or_default() += 1;
        at = e;
    }
    let parts = msg
        .parts
        .iter()
        .filter(|p| {
            !matches!(
                p.body,
                PartType::Text(_) | PartType::Html(_) | PartType::Multipart(_)
            )
        })
        .map(|p| {
            (
                mime_of(p),
                p.len(),
                p.content_id().is_some(),
                p.content_disposition()
                    .is_some_and(|d| d.ctype().eq_ignore_ascii_case("attachment")),
            )
        })
        .collect();
    let mut cid_contexts = Vec::new();
    let mut at = 0;
    while let Some(i) = lower[at..].find("cid:") {
        let s = at + i;
        let mut b = s.saturating_sub(400);
        while !lower.is_char_boundary(b) {
            b += 1;
        }
        let before = &lower[b..s];
        let tag = before.rfind('<').map_or("", |t| &before[t + 1..]);
        let tag = tag.split([' ', '\t', '\r', '\n', '>']).next().unwrap_or("");
        let tag = if tag.chars().all(|c| c.is_ascii_alphanumeric() || c == ':') {
            tag
        } else {
            "?"
        };
        let attr = before
            .rsplit(['"', '\'', '('])
            .nth(1)
            .map_or("", |a| a.trim_end_matches('=').trim());
        let attr = attr.rsplit([' ', '\t', '\r', '\n']).next().unwrap_or("");
        let attr = if attr.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            attr
        } else {
            "?"
        };
        cid_contexts.push(format!("<{tag} {attr}>"));
        at = s + 4;
    }
    let names = msg
        .parts
        .iter()
        .filter(|p| {
            !matches!(
                p.body,
                PartType::Text(_) | PartType::Html(_) | PartType::Multipart(_)
            )
        })
        .filter_map(|p| p.attachment_name().map(str::to_lowercase))
        .collect();
    let mut tree = Vec::new();
    fn walk(msg: &Message<'_>, i: usize, depth: usize, out: &mut Vec<(usize, String)>) {
        let Some(p) = msg.parts.get(i) else { return };
        out.push((depth, mime_of(p)));
        if let PartType::Multipart(ch) = &p.body {
            for c in ch {
                walk(msg, *c as usize, depth + 1, out);
            }
        }
    }
    walk(&msg, 0, 0, &mut tree);
    const SAFE: [&str; 6] = [
        "x-ms-has-attach",
        "x-mailer",
        "x-ms-exchange-organization-scl",
        "x-ms-exchange-organization-authas",
        "x-ms-tnef-correlator",
        "mime-version",
    ];
    let headers = msg
        .root_part()
        .headers()
        .iter()
        .filter(|h| SAFE.contains(&h.name().to_ascii_lowercase().as_str()))
        .map(|h| {
            let v = &raw[h.offset_start() as usize..h.offset_end() as usize];
            (
                h.name().to_owned(),
                String::from_utf8_lossy(v).trim().to_owned(),
            )
        })
        .collect();
    Some(ImageReport {
        refs: counts.into_iter().map(|((k, h), n)| (k, h, n)).collect(),
        parts,
        html: is_html,
        remote,
        missing_cids,
        others,
        names,
        cid_contexts,
        restored: is_restored(&msg),
        tree,
        headers,
    })
}

/// 진단용: 본문 cid와 이어진 부분 번호와, Content-ID가 아니라 이름·순서로 이었는지(`owa-cli store images`).
pub fn cid_link_parts(raw: &[u8]) -> Vec<(String, usize, bool)> {
    let Some(msg) = parse(raw) else {
        return Vec::new();
    };
    let html = msg.body_html(0).unwrap_or_default();
    cid_links(&msg, &html)
        .into_iter()
        .map(|l| {
            let by_id =
                msg.parts[l.part].content_id().map(clean_cid).as_deref() == Some(l.key.as_str());
            (l.key, l.part, !by_id)
        })
        .collect()
}

/// 원문을 읽기 창 문서로 만든다. 해석할 수 없으면 `None`.
pub fn render(raw: &[u8], opts: &RenderOptions) -> Option<Rendered> {
    let msg = parse(raw)?;
    // mail-parser는 텍스트만 있어도 HTML 본문으로 세므로 실제 HTML 부분인지 본다.
    let is_html = msg
        .html_part(0)
        .is_some_and(|p| matches!(p.body, PartType::Html(_)));
    let html = if is_html {
        msg.body_html(0).unwrap_or_default()
    } else {
        Cow::Borrowed("")
    };
    let links = cid_links(&msg, &html);
    let cids: HashSet<String> = links.iter().map(|l| l.key.clone()).collect();
    let (body, has_remote) = if is_html {
        let mode = ImgMode::Display {
            cid_base: opts.cid_base.clone(),
            allow_remote: opts.allow_remote,
        };
        sanitize(&html, mode, &cids)
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
        (
            format!("<pre class=\"text\">{}</pre>", escape_html(&text)),
            false,
        )
    };
    let referenced =
        |cid: &str| body.contains(&utf8_percent_encode(cid, NON_ALPHANUMERIC).to_string());
    // 본문이 실제로 쓰는 부분들.
    let used: HashSet<usize> = links
        .iter()
        .filter(|l| referenced(&l.key))
        .map(|l| l.part)
        .collect();
    let attachments = msg
        .attachments
        .iter()
        .filter(|&&i| (i as usize) < msg.parts.len())
        .enumerate()
        .map(|(index, &part)| {
            let p = &msg.parts[part as usize];
            let mime = mime_of(p);
            let name = p
                .attachment_name()
                .map(str::to_owned)
                .or_else(|| {
                    p.message()
                        .and_then(|m| m.subject())
                        .map(|s| format!("{s}.eml"))
                })
                .unwrap_or_else(|| format!("첨부{}", index + 1));
            AttachmentInfo {
                index,
                name,
                mime,
                size: p.len(),
                inline: used.contains(&(part as usize)),
            }
        })
        .collect();
    let inline_images = links
        .iter()
        .filter(|l| referenced(&l.key))
        .filter_map(|l| {
            let p = &msg.parts[l.part];
            Some(InlineImage {
                cid: l.key.clone(),
                mime: image_mime(p)?,
                data: p.contents().to_vec(),
            })
        })
        .collect();
    Some(Rendered {
        subject: msg.subject().unwrap_or_default().to_owned(),
        from: addresses(msg.from()),
        to: addresses(msg.to()),
        cc: addresses(msg.cc()),
        bcc: addresses(msg.bcc()),
        reply_to: addresses(msg.reply_to()),
        date: msg.date().map(|d| d.to_timestamp() * 1000),
        html: wrap(&body, opts.allow_remote),
        has_remote_images: has_remote,
        attachments,
        inline_images,
    })
}

/// Content-ID로 원문 안의 부분을 찾는다(인라인 이미지). (MIME 형식, 내용)
pub fn find_cid(raw: &[u8], cid: &str) -> Option<(String, Vec<u8>)> {
    let msg = parse(raw)?;
    let want = clean_cid(cid);
    let html = msg.body_html(0).unwrap_or_default();
    let link = cid_links(&msg, &html).into_iter().find(|l| l.key == want)?;
    let part = &msg.parts[link.part];
    Some((
        image_mime(part).unwrap_or_else(|| mime_of(part)),
        part.contents().to_vec(),
    ))
}

/// [`AttachmentInfo::index`] 순번의 첨부. (이름, MIME 형식, 내용). 첨부된 메일은 원문 그대로.
pub fn attachment(raw: &[u8], index: usize) -> Option<(String, String, Vec<u8>)> {
    let msg = parse(raw)?;
    let p = msg.attachments().nth(index)?;
    let mime = mime_of(p);
    let name = p
        .attachment_name()
        .map(str::to_owned)
        .or_else(|| {
            p.message()
                .and_then(|m| m.subject())
                .map(|s| format!("{s}.eml"))
        })
        .unwrap_or_else(|| format!("첨부{}", index + 1));
    Some((name, mime, p.contents().to_vec()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const EML: &str = concat!(
        "From: =?UTF-8?B?6rmA66+87IiY?= <minsu.kim@example.com>\r\n",
        "To: me@x.com\r\n",
        "Subject: =?ks_c_5601-1987?B?uriw7byt?=\r\n",
        "Date: Thu, 15 Oct 2026 14:06:38 +0900\r\n",
        "MIME-Version: 1.0\r\n",
        "Content-Type: multipart/mixed; boundary=\"m\"\r\n",
        "\r\n",
        "--m\r\n",
        "Content-Type: multipart/related; boundary=\"r\"\r\n",
        "\r\n",
        "--r\r\n",
        "Content-Type: text/html; charset=UTF-8\r\n",
        "\r\n",
        "<html><head><title>숨김</title><style>p{color:red}</style></head><body onload=\"x()\">",
        "<p style=\"color:blue\">본문</p><script>alert(1)</script>",
        "<img src=\"cid:img1@x\"><img src=\"https://tracker.example/p.gif\">",
        "<form action=\"/x\"><input></form><iframe src=\"https://e.x\"></iframe>",
        "<a href=\"https://www.example.com\" onclick=\"x()\">링크</a></body></html>\r\n",
        "--r\r\n",
        "Content-Type: image/png\r\n",
        "Content-ID: <img1@x>\r\n",
        "Content-Transfer-Encoding: base64\r\n",
        "\r\n",
        "iVBORw0KGgo=\r\n",
        "--r--\r\n",
        "--m\r\n",
        "Content-Type: application/pdf\r\n",
        "Content-Disposition: attachment; filename=\"report.pdf\"\r\n",
        "Content-Transfer-Encoding: base64\r\n",
        "\r\n",
        "JVBERg==\r\n",
        "--m--\r\n"
    );

    fn opts(allow_remote: bool) -> RenderOptions {
        RenderOptions {
            cid_base: "http://app-cid.localhost/7/".into(),
            allow_remote,
        }
    }

    #[test]
    fn sanitizes_and_maps_cid_and_blocks_remote() {
        let r = render(EML.as_bytes(), &opts(false)).unwrap();
        assert_eq!(r.subject, "보고서");
        assert_eq!(r.from, "김민수 <minsu.kim@example.com>");
        assert_eq!(r.date, Some(1_792_040_798_000));
        let h = &r.html;
        assert!(h.contains("본문") && h.contains("color:blue"));
        for bad in [
            "<script",
            "alert(1)",
            "onload",
            "onclick",
            "<form",
            "<iframe",
            "tracker.example",
            "숨김",
        ] {
            assert!(!h.contains(bad), "{bad} 가 남음: {h}");
        }
        assert!(
            h.contains("src=\"http://app-cid.localhost/7/img1%40x\""),
            "{h}"
        );
        assert!(h.contains("p{color:red}"), "<style>은 유지");
        // 그림 폭 상한은 창 폭 기준(칸 폭 100%면 표 칸 안 그림이 0폭이 될 수 있다).
        assert!(
            h.contains("img{max-width:calc(100vw - 32px)") && !h.contains("img{max-width:100%")
        );
        assert!(h.contains("default-src 'none'"));
        assert!(r.has_remote_images);
        let names: Vec<(&str, bool)> = r
            .attachments
            .iter()
            .map(|a| (a.name.as_str(), a.inline))
            .collect();
        assert!(names.contains(&("report.pdf", false)), "{names:?}");
        assert!(
            r.attachments
                .iter()
                .any(|a| a.inline && a.mime == "image/png"),
            "{:?}",
            r.attachments
        );
        assert_eq!(r.inline_images.len(), 1);
        assert_eq!(
            (
                r.inline_images[0].cid.as_str(),
                &r.inline_images[0].data[..4]
            ),
            ("img1@x", &b"\x89PNG"[..])
        );

        let allowed = render(EML.as_bytes(), &opts(true)).unwrap();
        assert!(allowed.html.contains("tracker.example") && allowed.html.contains("https: http:"));
        assert!(!allowed.has_remote_images);
    }

    #[test]
    fn finds_inline_and_attachments() {
        let (mime, bytes) = find_cid(EML.as_bytes(), "<IMG1@x>").unwrap();
        assert_eq!(mime, "image/png");
        assert_eq!(&bytes[..4], b"\x89PNG");
        let r = render(EML.as_bytes(), &opts(false)).unwrap();
        let pdf = r
            .attachments
            .iter()
            .find(|a| a.name == "report.pdf")
            .unwrap();
        let (name, mime, data) = attachment(EML.as_bytes(), pdf.index).unwrap();
        assert_eq!(
            (name.as_str(), mime.as_str(), &data[..]),
            ("report.pdf", "application/pdf", &b"%PDF"[..])
        );
    }

    /// Content-ID 없이 첨부로만 남은 본문 그림(HCL Domino 백업 등). `restored`면 복원 표시 머리글을 넣는다.
    fn loose_eml(restored: bool, html: &str, files: &[(&str, &str)]) -> String {
        let mut s = String::from("Subject: t\r\nMIME-Version: 1.0\r\n");
        if restored {
            s.push_str("X-EO-Restored: domino\r\n");
        }
        s.push_str("Content-Type: multipart/mixed; boundary=\"m\"\r\n\r\n--m\r\n");
        s.push_str(&format!(
            "Content-Type: text/html; charset=UTF-8\r\n\r\n{html}\r\n"
        ));
        for (name, mime) in files {
            s.push_str(&format!(
                "--m\r\nContent-Type: {mime}; name=\"{name}\"\r\nContent-Disposition: attachment; filename=\"{name}\"\r\n\
                 Content-Transfer-Encoding: base64\r\n\r\niVBORw0KGgo=\r\n"
            ));
        }
        s.push_str("--m\r\nContent-Type: application/pdf\r\nContent-Disposition: attachment; filename=\"r.pdf\"\r\n\
                    Content-Transfer-Encoding: base64\r\n\r\nJVBERg==\r\n--m--\r\n");
        s
    }

    #[test]
    fn cid_falls_back_to_attachment_with_same_name() {
        // Outlook식 cid "image001.png@01D6…"와 이름이 같은 첨부(형식은 octet-stream이어도 내용으로 그림을 안다).
        let eml = loose_eml(
            false,
            "<p>a</p><img src=\"cid:image001.png@01D6A1B2.C3D4E5F0\">",
            &[("image001.png", "application/octet-stream")],
        );
        let r = render(eml.as_bytes(), &opts(false)).unwrap();
        assert!(
            r.html.contains(
                "src=\"http://app-cid.localhost/7/image001%2Epng%4001d6a1b2%2Ec3d4e5f0\""
            ),
            "{}",
            r.html
        );
        assert_eq!(r.inline_images.len(), 1);
        assert_eq!(r.inline_images[0].mime, "image/png");
        let inline: Vec<_> = r
            .attachments
            .iter()
            .map(|a| (a.name.as_str(), a.inline))
            .collect();
        assert_eq!(inline, [("image001.png", true), ("r.pdf", false)]);
        let (mime, data) = find_cid(eml.as_bytes(), "image001.png@01d6a1b2.c3d4e5f0").unwrap();
        assert_eq!((mime.as_str(), &data[..4]), ("image/png", &b"\x89PNG"[..]));
        // 회신·전달: 본문 cid를 Content-ID로 다시 붙이고 첨부에서는 뺀다.
        let o = crate::compose::original(eml.as_bytes()).unwrap();
        assert_eq!(o.inline.len(), 1);
        assert_eq!(
            o.inline[0].content_id.as_deref(),
            Some("image001.png@01D6A1B2.C3D4E5F0")
        );
        assert_eq!(
            o.attachments
                .iter()
                .map(|a| a.name.as_str())
                .collect::<Vec<_>>(),
            ["r.pdf"]
        );
    }

    #[test]
    fn restored_mail_links_unnamed_images_in_order_only_when_counts_match() {
        let html = "<img src=\"cid:aaa-111\"><img src=\"cid:bbb-222\"><img src=\"cid:aaa-111\">";
        let files = [
            ("pastedImage.png", "image/png"),
            ("pastedImage.png", "image/png"),
        ];
        let r = render(loose_eml(true, html, &files).as_bytes(), &opts(false)).unwrap();
        let cids: Vec<_> = r.inline_images.iter().map(|i| i.cid.as_str()).collect();
        assert_eq!(cids, ["aaa-111", "bbb-222"]);
        assert_eq!(r.attachments.iter().filter(|a| a.inline).count(), 2);
        // 복원한 메일이 아니면 순서로 잇지 않는다(진짜 첨부 사진일 수 있다).
        let r = render(loose_eml(false, html, &files).as_bytes(), &opts(false)).unwrap();
        assert!(r.inline_images.is_empty());
        assert!(!r.html.contains("src=\"http://app-cid"), "{}", r.html);
        // 수가 다르면 잇지 않는다.
        let r = render(loose_eml(true, html, &files[..1]).as_bytes(), &opts(false)).unwrap();
        assert!(r.inline_images.is_empty());
        // 앞 버전이 다른 이름으로 붙인 복원 표시도 복원한 메일이다.
        let old = loose_eml(true, html, &files).replace("X-EO-Restored", "X-Old-Name-Restored");
        let r = render(old.as_bytes(), &opts(false)).unwrap();
        assert_eq!(r.inline_images.len(), 2);
    }

    #[test]
    fn plain_text_is_escaped_in_pre() {
        let r = render(b"Subject: t\r\n\r\n<b>tag</b> & text", &opts(false)).unwrap();
        assert!(
            r.html
                .contains("<pre class=\"text\">&lt;b&gt;tag&lt;/b&gt; &amp; text"),
            "{}",
            r.html
        );
    }

    #[test]
    fn image_report_reads_src_not_originalsrc() {
        // 아웃룩이 남긴 originalsrc(없는 cid)는 그림 출처가 아니다.
        let raw = "Subject: t\r\nMIME-Version: 1.0\r\nContent-Type: multipart/related; boundary=\"b\"\r\n\r\n\
                   --b\r\nContent-Type: text/html; charset=UTF-8\r\n\r\n\
                   <img originalsrc=\"cid:gone@x\" src=\"cid:m2.u@domino\">\r\n\
                   --b\r\nContent-Type: image/png\r\nContent-ID: <m2.u@domino>\r\nContent-Transfer-Encoding: base64\r\n\r\n\
                   iVBORw0KGgo=\r\n--b--\r\n";
        let r = image_report(raw.as_bytes()).unwrap();
        assert!(r.missing_cids.is_empty(), "{:?}", r.missing_cids);
    }
}
