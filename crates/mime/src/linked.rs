//! HCL Domino 알림 메일의 `[연결문서]`(1.2.0 사용자 요청): 링크가 가리키는 업무 문서를 가져와 메일 본문 아래에 붙인다.
//!
//! 형식은 2026-11-11 실서버 조사:
//! - 메일 링크: `https://{앱 서버}/common/commonlib.nsf/fmSSORouting?openform&UURL=…StartHome…&SysID=04_02_36_01&
//!   MenuID=…&DocID={UNID}`(SSO를 거쳐 포털이 문서를 연다).
//! - 문서: `https://{앱 서버}/{업무 DB}/0/{UNID}?opendocument`(웹메일 로그인 쿠키로 열린다). 업무 DB는 SysID로 정해지는데
//!   포털 코드에 대응표가 없어 확인한 것만 [`db_for_sys`]에 둔다.
//! - 문서는 표·클래스로 된 HTML + CSS 몇 개. 단추·스크립트를 빼고, 쓰는 CSS 규칙만 `#eo-linked` 아래로 묶어(메일 본문과
//!   섞이지 않게) 그림은 `data:`로 품은 한 덩어리로 만든다(읽기 창 CSP: 인라인 스타일·data: 그림만).

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use percent_encoding::percent_decode_str;

/// 메일 속 연결문서 링크.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkRef {
    /// 앱 서버(`hintapp02.skbroadband.com`).
    pub host: String,
    pub sys_id: String,
    /// 문서 UNID(대문자 32자리).
    pub unid: String,
}

/// SysID → 업무 DB 경로(실서버에서 확인한 것만).
pub fn db_for_sys(sys_id: &str) -> Option<&'static str> {
    match sys_id {
        // 내부 DNS작업요청
        "04_02_36_01" => Some("appl/applydomain.nsf"),
        _ => None,
    }
}

impl LinkRef {
    /// 문서 주소. 모르는 업무(SysID)면 없음.
    pub fn doc_url(&self) -> Option<String> {
        let db = db_for_sys(&self.sys_id)?;
        Some(format!(
            "https://{}/{db}/0/{}?opendocument",
            self.host, self.unid
        ))
    }
}

fn is_unid(s: &str) -> bool {
    s.len() == 32 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// 질의 값(`name=값`, 대소문자 무시). `&`나 끝까지.
fn param<'a>(s: &'a str, name: &str) -> Option<&'a str> {
    let lower = s.to_ascii_lowercase();
    let key = format!("{}=", name.to_ascii_lowercase());
    let mut from = 0;
    while let Some(i) = lower[from..].find(&key) {
        let at = from + i;
        let ok = at == 0 || matches!(lower.as_bytes()[at - 1], b'&' | b'?' | b';');
        if ok {
            let v = &s[at + key.len()..];
            return Some(v.split(['&', '#', '"', '\'']).next().unwrap_or(""));
        }
        from = at + key.len();
    }
    None
}

/// 본문(HTML)에서 첫 연결문서 링크를 찾는다.
pub fn find_link(html: &str) -> Option<LinkRef> {
    let lower = html.to_ascii_lowercase();
    let mut from = 0;
    while let Some(i) = lower[from..].find("href=") {
        let at = from + i + 5;
        from = at;
        let rest = &html[at..];
        let (quote, rest) = match rest.chars().next() {
            Some(q @ ('"' | '\'')) => (Some(q), &rest[1..]),
            _ => (None, rest),
        };
        let end = match quote {
            Some(q) => rest.find(q),
            None => rest.find([' ', '>']),
        }
        .unwrap_or(rest.len());
        let href = rest[..end].replace("&amp;", "&");
        if !href.to_ascii_lowercase().contains("docid") {
            continue;
        }
        // UURL 안이 몇 번 인코딩되어 있다: 풀어 가며 찾는다.
        let mut text = href;
        for _ in 0..4 {
            let decoded = percent_decode_str(&text).decode_utf8_lossy().into_owned();
            if decoded == text {
                break;
            }
            text = decoded;
        }
        let (Some(unid), Some(sys)) = (param(&text, "DocID"), param(&text, "SysID")) else {
            continue;
        };
        if !is_unid(unid)
            || sys.is_empty()
            || !sys.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            continue;
        }
        // 문서가 있는 서버: UURL의 서버, 없으면 링크의 서버.
        let host_of = |s: &str| {
            s.split_once("://").map(|(_, r)| {
                r.split(['/', '?', '&'])
                    .next()
                    .unwrap_or("")
                    .to_ascii_lowercase()
            })
        };
        let host = param(&text, "UURL")
            .and_then(host_of)
            .or_else(|| host_of(&text))
            .filter(|h| !h.is_empty())?;
        return Some(LinkRef {
            host,
            sys_id: sys.to_owned(),
            unid: unid.to_ascii_uppercase(),
        });
    }
    None
}

/// 정리한 문서.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CleanDoc {
    pub body: String,
    /// 문서가 쓰는 클래스(CSS 규칙 고르기용).
    pub classes: HashSet<String>,
    /// 스타일시트 주소(절대).
    pub stylesheets: Vec<String>,
    /// 그림 주소(절대, 본문 `src`에 그대로 남아 있다: [`assemble`]이 data:로 바꾼다).
    pub images: Vec<String>,
}

/// `base`(`https://서버/db/…`) 기준 절대 주소.
pub fn absolute(base: &str, url: &str) -> String {
    let url = url.trim();
    if url.starts_with("https://") || url.starts_with("http://") || url.starts_with("data:") {
        return url.to_owned();
    }
    let (scheme, rest) = base.split_once("://").unwrap_or(("https", base));
    let host = rest.split('/').next().unwrap_or(rest);
    if let Some(p) = url.strip_prefix("//") {
        return format!("{scheme}://{p}");
    }
    let joined = if url.starts_with('/') {
        url.to_owned()
    } else {
        // 상대 경로: base 경로의 마지막 `/`까지.
        let path = rest[host.len()..].split(['?', '#']).next().unwrap_or("");
        let dir = &path[..path.rfind('/').map_or(0, |i| i + 1)];
        format!("{}{url}", if dir.is_empty() { "/" } else { dir })
    };
    // `.`·`..` 풀기(질의는 그대로).
    let (path, query) = match joined.find(['?', '#']) {
        Some(q) => joined.split_at(q),
        None => (joined.as_str(), ""),
    };
    let mut segs: Vec<&str> = Vec::new();
    let parts: Vec<&str> = path.split('/').skip(1).collect();
    let n = parts.len();
    for (k, s) in parts.into_iter().enumerate() {
        match s {
            "." => {}
            ".." => {
                segs.pop();
            }
            _ => segs.push(s),
        }
        if k + 1 == n && matches!(s, "." | "..") {
            segs.push("");
        }
    }
    format!("{scheme}://{host}/{}{query}", segs.join("/"))
}

/// 태그의 속성 값(따옴표 안, 대소문자 무시).
fn attr_of(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let i = lower.find(&format!("{name}="))?;
    let rest = &tag[i + name.len() + 1..];
    let (q, rest) = match rest.chars().next()? {
        q @ ('"' | '\'') => (Some(q), &rest[1..]),
        _ => (None, rest),
    };
    let end = match q {
        Some(q) => rest.find(q)?,
        None => rest.find([' ', '>']).unwrap_or(rest.len()),
    };
    Some(rest[..end].to_owned())
}

/// `<div id="…">…</div>`(안에 div가 없는 것)을 뺀다.
fn remove_div_by_id(html: &str, id: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let pat = format!("id=\"{}\"", id.to_ascii_lowercase());
    let Some(i) = lower.find(&pat) else {
        return html.to_owned();
    };
    let Some(start) = lower[..i].rfind("<div") else {
        return html.to_owned();
    };
    let Some(end) = lower[i..].find("</div>").map(|e| i + e + 6) else {
        return html.to_owned();
    };
    format!("{}{}", &html[..start], &html[end..])
}

/// 문서 HTML을 정리한다: 스크립트·단추·입력란·이벤트를 빼고 표·글·스타일 속성은 그대로 둔다. `doc_url`: 문서 주소.
pub fn clean_doc(html: &str, doc_url: &str) -> CleanDoc {
    let lower = html.to_ascii_lowercase();
    // 스타일시트(<link href=…css>)
    let mut stylesheets = Vec::new();
    let mut from = 0;
    while let Some(i) = lower[from..].find("<link") {
        let at = from + i;
        let end = lower[at..].find('>').map_or(lower.len(), |e| at + e);
        let tag = &html[at..end];
        if tag.to_ascii_lowercase().contains("stylesheet")
            && let Some(href) = attr_of(tag, "href")
            && href
                .to_ascii_lowercase()
                .split('?')
                .next()
                .unwrap_or("")
                .ends_with(".css")
        {
            let u = absolute(doc_url, &href);
            if !stylesheets.contains(&u) {
                stylesheets.push(u);
            }
        }
        from = end;
    }
    // 본문만.
    let body_start = lower
        .find("<body")
        .and_then(|i| lower[i..].find('>').map(|e| i + e + 1))
        .unwrap_or(0);
    let body_end = lower.rfind("</body>").unwrap_or(html.len()).max(body_start);
    let mut body = html[body_start..body_end].to_owned();
    // 화면에는 안 보이게 되어 있던 인쇄 안내, 결재 현황은 단추로 펼치던 전체 기록을 보인다.
    body = remove_div_by_id(&body, "printMessageDiv");
    body = body
        .replace(
            "id=\"AppInfo_All\" style=\"display:none\"",
            "id=\"AppInfo_All\"",
        )
        .replace(
            "id=\"AppInfo_One\" style=\"display:\"",
            "id=\"AppInfo_One\" style=\"display:none\"",
        );

    let classes: Arc<Mutex<HashSet<String>>> = Arc::default();
    let images: Arc<Mutex<Vec<String>>> = Arc::default();
    let (cls, imgs) = (Arc::clone(&classes), Arc::clone(&images));
    let base = doc_url.to_owned();
    let mut b = ammonia::Builder::default();
    b.add_tags([
        "center", "font", "table", "thead", "tbody", "tfoot", "tr", "td", "th", "colgroup", "col",
        "caption", "span", "div", "img", "hr", "u", "s", "strike", "sup", "sub", "pre", "label",
    ])
    .add_generic_attributes([
        "style",
        "class",
        "id",
        "align",
        "valign",
        "width",
        "height",
        "bgcolor",
        "color",
        "border",
        "cellpadding",
        "cellspacing",
        "face",
        "size",
        "colspan",
        "rowspan",
        "nowrap",
    ])
    .add_tag_attributes("img", ["src", "alt", "title", "width", "height"])
    .add_tag_attributes("a", ["href", "title"])
    .add_url_schemes(["data"])
    .id_prefix(Some("eo-ld-"))
    .clean_content_tags(HashSet::from([
        "script", "style", "title", "button", "select", "option",
    ]))
    .strip_comments(true)
    .link_rel(Some("noopener noreferrer"))
    .attribute_filter(move |element, attribute, value| {
        if attribute == "class" {
            let mut set = cls.lock().unwrap_or_else(|e| e.into_inner());
            for c in value.split_whitespace() {
                set.insert(c.to_owned());
            }
        }
        if element == "img" && attribute == "src" {
            let lower = value.to_ascii_lowercase();
            // 펼치기·접기 단추 그림.
            if lower.contains("ic_collapse") || lower.contains("ic_expand") {
                return None;
            }
            let abs = absolute(&base, value);
            if !abs.starts_with("https://") && !abs.starts_with("data:image/") {
                return None;
            }
            if abs.starts_with("https://") {
                imgs.lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(abs.clone());
            }
            return Some(Cow::Owned(abs));
        }
        if element == "a" && attribute == "href" {
            let lower = value.trim().to_ascii_lowercase();
            if lower.starts_with("javascript:") || lower.starts_with('#') {
                return None;
            }
            return Some(Cow::Owned(absolute(&base, value)));
        }
        Some(Cow::Borrowed(value))
    });
    let body = b.clean(&body).to_string();
    drop(b);
    let mut images: Vec<String> = take(&images);
    images.dedup();
    CleanDoc {
        body,
        classes: take(&classes),
        stylesheets,
        images,
    }
}

fn take<T: Default>(m: &Mutex<T>) -> T {
    std::mem::take(&mut *m.lock().unwrap_or_else(|e| e.into_inner()))
}

/// 범위 안에 둘 기본 요소(클래스 없는 규칙은 이것들만).
const PLAIN_TAGS: &[&str] = &[
    "body", "html", "table", "tbody", "thead", "tr", "td", "th", "div", "span", "p", "a", "font",
    "img", "b", "strong", "pre", "hr", "ul", "ol", "li", "label", "u", "i",
];

/// 선택자 한 덩어리를 범위(`scope`) 아래로. 쓰지 않는 클래스·아이디가 있으면 `None`.
fn scope_selector(sel: &str, classes: &HashSet<String>, scope: &str) -> Option<String> {
    let sel = sel.trim();
    if sel.is_empty() || sel.contains('#') || sel.contains('*') {
        return None;
    }
    let mut out = String::new();
    let mut first = true;
    // 조합자(공백, >, +, ~)로 나눈 단위.
    let tokens: Vec<&str> = sel
        .split(|c: char| c.is_whitespace())
        .filter(|t| !t.is_empty())
        .collect();
    for tok in tokens {
        if matches!(tok, ">" | "+" | "~") {
            out.push(' ');
            out.push_str(tok);
            continue;
        }
        // 의사 클래스·속성 앞까지가 태그+클래스.
        let core_end = tok.find([':', '[']).unwrap_or(tok.len());
        let core = &tok[..core_end];
        let mut parts = core.split('.');
        let tag = parts.next().unwrap_or("").to_ascii_lowercase();
        for c in parts {
            if c.is_empty() || !classes.contains(c) {
                return None;
            }
        }
        if !tag.is_empty() && !PLAIN_TAGS.contains(&tag.as_str()) {
            return None;
        }
        // body·html은 범위 그 자체.
        let piece = if tag == "body" || tag == "html" {
            if !first {
                return None;
            }
            format!("{scope}{}", &tok[tag.len()..])
        } else if first {
            format!("{scope} {tok}")
        } else {
            tok.to_owned()
        };
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&piece);
        first = false;
    }
    (!out.is_empty()).then_some(out)
}

/// CSS에서 문서가 쓰는 규칙만 골라 `scope` 아래로 묶는다(@media·@font-face 등은 뺀다). `url(…)`은 `css_url` 기준 절대
/// 주소로 바꾸고 모은다. (CSS, 그림 주소들)
pub fn scope_css(
    css: &str,
    css_url: &str,
    classes: &HashSet<String>,
    scope: &str,
) -> (String, Vec<String>) {
    // 주석 빼기.
    let mut text = String::with_capacity(css.len());
    let mut rest = css;
    while let Some(i) = rest.find("/*") {
        text.push_str(&rest[..i]);
        rest = rest[i + 2..].split_once("*/").map_or("", |(_, r)| r);
    }
    text.push_str(rest);
    let mut out = String::new();
    let mut urls = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // 선택자: '{' 또는 ';'(@import·@charset)까지.
        let Some(open) = text[i..].find(['{', ';', '}']).map(|o| i + o) else {
            break;
        };
        if bytes[open] != b'{' {
            i = open + 1;
            continue;
        }
        let selector = text[i..open].trim();
        // 짝 맞는 '}'.
        let mut depth = 0;
        let mut close = open;
        for (k, &c) in bytes.iter().enumerate().skip(open) {
            if c == b'{' {
                depth += 1;
            } else if c == b'}' {
                depth -= 1;
                if depth == 0 {
                    close = k;
                    break;
                }
            }
        }
        if close == open {
            break;
        }
        let block = &text[open + 1..close];
        i = close + 1;
        if selector.starts_with('@') || block.contains('{') {
            continue;
        }
        let kept: Vec<String> = selector
            .split(',')
            .filter_map(|s| scope_selector(s, classes, scope))
            .collect();
        if kept.is_empty() {
            continue;
        }
        // url(…) → 절대 주소.
        let mut decl = String::new();
        let mut r = block;
        while let Some(u) = r.find("url(") {
            decl.push_str(&r[..u + 4]);
            let after = &r[u + 4..];
            let end = after.find(')').unwrap_or(after.len());
            let raw = after[..end].trim().trim_matches(['"', '\'']);
            let abs = absolute(css_url, raw);
            if abs.starts_with("https://") && !urls.contains(&abs) {
                urls.push(abs.clone());
            }
            decl.push('"');
            decl.push_str(&abs.replace('"', "%22"));
            decl.push('"');
            r = &after[end..];
        }
        decl.push_str(r);
        out.push_str(&kept.join(","));
        out.push('{');
        out.push_str(decl.trim());
        out.push_str("}\n");
    }
    (out, urls)
}

/// 받은 그림 → `data:` 주소. 그림이 아니면(로그인 화면 등) 없음.
pub fn data_uri(content_type: &str, data: &[u8]) -> Option<String> {
    use base64::Engine as _;
    let sniffed = if data.starts_with(b"GIF8") {
        "image/gif"
    } else if data.starts_with(b"\x89PNG") {
        "image/png"
    } else if data.starts_with(b"\xFF\xD8\xFF") {
        "image/jpeg"
    } else {
        return None;
    };
    let declared = content_type.split(';').next().unwrap_or("").trim();
    let mime = if declared.starts_with("image/") && !declared.contains(['"', '<', ' ']) {
        declared
    } else {
        sniffed
    };
    Some(format!(
        "data:{mime};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(data)
    ))
}

/// 메일 본문(완성 문서) 끝에 덩어리를 넣는다.
pub fn append_to_document(doc: &str, block: &str) -> String {
    match doc.to_ascii_lowercase().rfind("</body>") {
        Some(i) => format!("{}{block}{}", &doc[..i], &doc[i..]),
        None => format!("{doc}{block}"),
    }
}

/// 메일 본문 끝에 붙일 덩어리. `images`: 주소 → data: 주소(없는 그림은 빈 그림으로).
pub fn assemble(
    body: &str,
    css: &str,
    images: &HashMap<String, String>,
    label: &str,
    all_urls: &[String],
) -> String {
    let mut body = body.to_owned();
    let mut css = css.replace('<', "\\3C ");
    for u in all_urls {
        let to = images.get(u).map_or("data:,", String::as_str).to_owned();
        let esc = u.replace('&', "&amp;");
        body = body.replace(&format!("\"{esc}\""), &format!("\"{to}\""));
        body = body.replace(&format!("\"{u}\""), &format!("\"{to}\""));
        css = css.replace(&format!("\"{u}\""), &format!("\"{to}\""));
    }
    format!(
        "<div id=\"eo-linked-wrap\" style=\"margin-top:28px\">\
         <hr style=\"border:0;border-top:2px dashed #b8b8b8;margin:0 0 6px\">\
         <div style=\"font-size:12px;color:#6b6b70;margin:0 0 12px\">{label}</div>\
         <style>{css}</style><div id=\"eo-linked\">{body}</div></div>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAIL: &str = r#"<div>오른쪽[연결문서]를 클릭하면 문서로 이동합니다! ☞ <a href="https://hintapp02.skbroadband.com/common/commonlib.nsf/fmSSORouting?openform&amp;UURL=https://hintapp02.skbroadband.com/home.nsf/StartHome%3FOpenForm%26RightUrl%3Dloadview%25253Fopenpage%252526viewname%25253Dappok%26SysID%3D04_02_36_01%26MenuID%3DLM04%26DocID%3D80694E2C4AD74C2049258EA7002AE276">[연결문서]</a></div>"#;

    #[test]
    fn finds_link_in_mail() {
        let l = find_link(MAIL).unwrap();
        assert_eq!(l.host, "hintapp02.skbroadband.com");
        assert_eq!(l.sys_id, "04_02_36_01");
        assert_eq!(l.unid, "80694E2C4AD74C2049258EA7002AE276");
        assert_eq!(
            l.doc_url().unwrap(),
            "https://hintapp02.skbroadband.com/appl/applydomain.nsf/0/80694E2C4AD74C2049258EA7002AE276?opendocument"
        );
        assert!(find_link("<a href=\"https://x.example.com/a?b=1\">x</a>").is_none());
        let other = MAIL.replace("04_02_36_01", "09_99");
        assert!(
            find_link(&other).unwrap().doc_url().is_none(),
            "모르는 업무"
        );
    }

    #[test]
    fn cleans_document_and_scopes_css() {
        let doc = r#"<html><head><LINK REL=stylesheet TYPE="text/css" HREF="/HTMLPUB/css/skb_appr1.css"><script>alert(1)</script></head>
<body onload="init()"><form action=""><div id="btn_Div" class="NotPrint"><button type="button" onclick="closeDoc();"><span class="btn_txt">목록</span></button></div>
<table class="table_line_complex"><tr><td class="a_stitle100">요청회사</td><td class="a_fild_text">SK브로드밴드<input type="hidden" value="x"></td></tr></table>
<table><tr><td class="a_title100">접수정보 <img src="/img/ic_collapse.gif" onclick="x()"></td></tr></table><img src="/img/logo.gif">
<table><tr><td class="a_fild_title" id="AppInfo_All" style="display:none">전체</td><td class="a_fild_title" id="AppInfo_One" style="display:">하나</td></tr></table>
<div id="printMessageDiv"> 이문서는 Print할 수 없습니다. </div></form></body></html>"#;
        let url = "https://hintapp02.skbroadband.com/appl/applydomain.nsf/0/80694E2C4AD74C2049258EA7002AE276?opendocument";
        let c = clean_doc(doc, url);
        assert_eq!(
            c.stylesheets,
            ["https://hintapp02.skbroadband.com/HTMLPUB/css/skb_appr1.css"]
        );
        assert!(c.body.contains("SK브로드밴드") && c.body.contains("요청회사"));
        assert!(
            !c.body.contains("목록") && !c.body.contains("<script") && !c.body.contains("onclick")
        );
        assert!(!c.body.contains("<input") && !c.body.contains("Print할"));
        assert!(!c.body.contains("ic_collapse"));
        assert_eq!(c.images, ["https://hintapp02.skbroadband.com/img/logo.gif"]);
        assert!(c.body.contains("eo-ld-AppInfo_One") && c.body.contains("display:none"));
        assert!(c.classes.contains("a_fild_text"));
        let css = "/* x */ td{font-size:12px} .a_fild_text{border:1px solid #d8d8d8} .unused{color:red}\
            body{margin:0} body.community .x{a:b} @media screen and (min-width:1px){.a_fild_text{color:red}}\
            .page_title{background-image:url(../images/t.gif)} #id .a_fild_text{c:d} button:hover .btn_txt{e:f}";
        let mut classes = c.classes.clone();
        classes.insert("page_title".into());
        let (out, urls) = scope_css(
            css,
            "https://h.skbroadband.com/HTMLPUB/css/a.css",
            &classes,
            "#eo-linked",
        );
        assert!(out.contains("#eo-linked td{font-size:12px}"));
        assert!(out.contains("#eo-linked .a_fild_text{border:1px solid #d8d8d8}"));
        assert!(out.contains("#eo-linked{margin:0}"));
        assert!(
            !out.contains("unused") && !out.contains("community") && !out.contains("color:red")
        );
        assert!(!out.contains("#id") && !out.contains("button"));
        assert_eq!(urls, ["https://h.skbroadband.com/HTMLPUB/images/t.gif"]);
        let mut imgs = HashMap::new();
        imgs.insert(urls[0].clone(), "data:image/gif;base64,AAA".to_owned());
        let block = assemble(
            &c.body,
            &out,
            &imgs,
            "연결문서",
            &[urls.clone(), c.images.clone()].concat(),
        );
        assert!(block.contains("data:image/gif;base64,AAA") && block.contains("\"data:,\""));
        assert!(block.contains("<div id=\"eo-linked\">"));
    }

    #[test]
    fn absolute_urls() {
        let b = "https://h.example.com/appl/a.nsf/0/X?opendocument";
        assert_eq!(absolute(b, "/img/a.gif"), "https://h.example.com/img/a.gif");
        assert_eq!(
            absolute(b, "b.gif"),
            "https://h.example.com/appl/a.nsf/0/b.gif"
        );
        assert_eq!(
            absolute("https://h.example.com/css/x.css", "../i/t.gif"),
            "https://h.example.com/i/t.gif"
        );
        assert_eq!(
            absolute(b, "/a/./b/../c.css?x=1"),
            "https://h.example.com/a/c.css?x=1"
        );
    }
}
