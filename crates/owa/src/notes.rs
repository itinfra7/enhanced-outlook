//! HCL Domino 웹메일(HCL Domino, 자체 양식) 응답 해석과 요청 허용 목록. 모두 순수 함수다.
//!
//! 형식은 2026-10-21 조사(사용자가 직접 로그인한 브라우저에서 읽기만)에서 확인했다.
//! - 목록: `ReadViewEntries` XML. `<viewentry position unid noteid>` 안에 `<entrydata name>`(`<text>`·`<number>`).
//! - 메일: Memo 양식 HTML. 머리글은 숨은 입력란(`From`·`SendTo`·`CopyTo`…, RFC 2047 그대로),
//!   MIME 조각 목록은 `BodyContentType`(`M1^multipart/related; M1.1.1^text/html; M1.2^image/png`),
//!   첨부는 `…/$FILE/이름` 링크.
//! - 시작 페이지(`home.nsf/StartHome`): `mail_server`·`mailFile` 변수(그날의 웹메일 서버, 메일 파일).

use percent_encoding::percent_decode_str;

/// 읽기만 하는 보기: 메일함 목록·개인 폴더 트리·용량 합계.
const READ_VIEWS: &[&str] = &[
    "XML_Inbox",
    "XML_Sent",
    "XML_POP3",
    "XML_Draft",
    "XML_Trash",
    // 미확인('new') 메일: 가져온 뒤 'new'를 되돌릴 메일을 알려고(개인 폴더 보기에는 'new' 열이 없을 수 있다).
    "XML_Unread",
    // 모든 메일: 웹메일에서 지운(어느 메일함에도 없는) 메일을 알아보려고.
    "XML_All",
    "XML_Category",
    "FolderTree",
    "Size_Sum",
];

/// 웹메일 요청 허용 목록(서버 기준 상대 경로, 질의 포함). 웹메일의 동작(삭제·이동·읽음 표시·휴지통 비우기)은
/// 모두 `?OpenAgent` GET 주소라, 여기 없는 것은 보내지 않는다.
///
/// - POST `names.nsf?Login`(로그인), GET `home.nsf/StartHome?openform`(웹메일 서버 찾기),
///   `{메일 파일}/mail_size?openform`(웹메일의 "메일용량확인" 창)
/// - `{메일 파일}/{읽기 보기}?ReadViewEntries…`, `all/{UNID}?OpenDocument&Charset=UTF-8`,
///   `{0|all}/{unid}/Body/{조각}?OpenElement…`, `{0|all}/{unid}/$FILE/{이름}`
/// - 바꾸기는 사용자가 정한 것뿐(2026-10-21: 'new' 되돌리기·자동 정리, 0.6.0: 읽음·별·삭제·이동 양방향 동기화).
///   웹메일 화면 스크립트가 보내는 모양 그대로만 연다.
///   - `wMarkRead?OpenAgent&&docid=…&type=1|2`(읽음·안읽음)
///   - `agt_impo?OpenAgent&sid={노트}&type=N|H`(★ 켜기·끄기)
///   - `WebDelDoc2?OpenAgent&category={메일함}&docid=…`(받은 메일 등 → 지운 메일, 지운 메일 → 완전 삭제)
///   - `RecoverDelDoc?OpenAgent&RTView=($Inbox)&DocList=…`(지운 메일 → 받은 메일)
///   - `folder_move?OpenAgent&docid=…&fn={개인 폴더}`, `wFolderMoveCancel?OpenAgent&&docid=…`(원래 메일함으로)
///
///   휴지통 비우기(`Trash_delete_all`)·폴더 만들기·발송 등 나머지는 열지 않는다.
pub fn allowed(post: bool, path: &str) -> bool {
    if post {
        return path == "names.nsf?Login";
    }
    if path == "home.nsf/StartHome?openform" {
        return true;
    }
    let Some(i) = path.find(".nsf/") else {
        return false;
    };
    let (db, rest) = (&path[..i + 4], &path[i + 5..]);
    if !valid_db(db) {
        return false;
    }
    // 웹메일 "환경 설정 → 메일용량확인" 창(한도·메일함별·총 사용량, 사용자 안내 2026-10-21).
    if rest == "mail_size?openform" {
        return true;
    }
    if let Some(ids) = rest.strip_prefix("wMarkRead?OpenAgent&&docid=") {
        return ids
            .strip_suffix("&type=2")
            .or_else(|| ids.strip_suffix("&type=1"))
            .is_some_and(valid_note_ids);
    }
    if let Some(q) = rest.strip_prefix("agt_impo?OpenAgent&sid=") {
        let id = q
            .strip_suffix("&type=N")
            .or_else(|| q.strip_suffix("&type=H"));
        return id.is_some_and(|id| valid_note_ids(id) && !id.contains(','));
    }
    if let Some(q) = rest.strip_prefix("WebDelDoc2?OpenAgent&category=") {
        return q
            .split_once("&docid=")
            .is_some_and(|(c, ids)| valid_category(c) && valid_note_ids(ids));
    }
    if let Some(ids) = rest.strip_prefix("RecoverDelDoc?OpenAgent&RTView=($Inbox)&DocList=") {
        return valid_note_ids(ids);
    }
    if let Some(q) = rest.strip_prefix("folder_move?OpenAgent&docid=") {
        return q
            .split_once("&fn=")
            .is_some_and(|(ids, f)| valid_note_ids(ids) && valid_encoded(f));
    }
    if let Some(ids) = rest.strip_prefix("wFolderMoveCancel?OpenAgent&&docid=") {
        return valid_note_ids(ids);
    }
    if let Some((view, query)) = rest.split_once('?')
        && READ_VIEWS.contains(&view)
    {
        return query.starts_with("ReadViewEntries")
            && !query.contains('?')
            && !query.to_ascii_lowercase().contains("agent");
    }
    if let Some(unid) = rest
        .strip_prefix("all/")
        .and_then(|r| r.strip_suffix("?OpenDocument&Charset=UTF-8"))
    {
        return is_unid(unid);
    }
    let Some((unid, tail)) = rest
        .strip_prefix("0/")
        .or_else(|| rest.strip_prefix("all/"))
        .and_then(|r| r.split_once('/'))
    else {
        return false;
    };
    if !is_unid(unid) {
        return false;
    }
    if let Some(e) = tail.strip_prefix("Body/") {
        let (elem, query) = e.split_once('?').unwrap_or((e, ""));
        let query_ok = query
            .strip_prefix("OpenElement")
            .is_some_and(|q| q.split('&').filter(|p| !p.is_empty()).all(element_param));
        return !elem.is_empty()
            && elem.chars().all(|c| c.is_ascii_alphanumeric() || c == '.')
            && query_ok;
    }
    if let Some(name) = tail.strip_prefix("$FILE/") {
        let name = name.strip_suffix("?OpenElement").unwrap_or(name);
        return !name.is_empty() && !name.contains(['?', '#']);
    }
    false
}

fn element_param(p: &str) -> bool {
    matches!(
        p,
        "Charset=UTF-8" | "FieldElemFormat=gif" | "FieldElemFormat=jpg" | "FieldElemFormat=png"
    )
}

/// 메일 파일 경로(`mail9/u000001.nsf`).
pub fn valid_db(db: &str) -> bool {
    db.ends_with(".nsf")
        && db.len() > 4
        && !db.starts_with('/')
        && !db.contains("..")
        && db
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '-' | '.'))
}

/// 문서 고유 번호(UNID, 16진 32자, 대소문자 무관).
pub fn is_unid(s: &str) -> bool {
    s.len() == 32 && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// 삭제 주소의 메일함 이름: 웹메일의 기본 메일함 또는 개인 폴더(`$$Str{이름}`: 브라우저처럼 `$`는 그대로, 이름만 퍼센트 인코딩).
fn valid_category(c: &str) -> bool {
    matches!(c, "Inbox" | "Sent" | "POP3" | "Draft" | "Trash")
        || c.strip_prefix("$$Str")
            .is_some_and(|f| !f.is_empty() && valid_encoded(f))
}

/// 퍼센트 인코딩한 값(글자·숫자·`%`·`-`·`_`·`.`만: `&`·`?`·`/`가 끼면 다른 질의가 된다).
fn valid_encoded(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '%' | '-' | '_' | '.'))
}

/// 노트 번호 목록(`18B3A,18BCE`): 웹메일의 동작 주소가 받는 형식.
pub fn valid_note_ids(s: &str) -> bool {
    !s.is_empty()
        && s.split(',')
            .all(|id| (1..=8).contains(&id.len()) && id.chars().all(|c| c.is_ascii_hexdigit()))
}

// ---- 목록(ReadViewEntries) ----

/// 목록의 한 줄.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ViewEntry {
    /// `1`, `1.2`(계층 보기).
    pub position: String,
    /// 범주 줄에는 없다.
    pub unid: Option<String>,
    pub noteid: String,
    pub children: usize,
    /// (열 이름, 값). 값이 여럿이면 `; `로 잇는다.
    pub columns: Vec<(String, String)>,
}

impl ViewEntry {
    pub fn col(&self, name: &str) -> Option<&str> {
        self.columns
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    /// 값이 있는 첫 열.
    pub fn first_value(&self) -> Option<&str> {
        self.columns
            .iter()
            .map(|(_, v)| v.trim())
            .find(|v| !v.is_empty() && *v != "&nbsp;")
    }

    /// 초까지 있는 날짜 열(`2026-10-21 10:09:45`). 보기마다 열 이름이 다르다(받은 메일 `$7`, 보낸 메일 `$10`).
    pub fn full_date(&self) -> Option<&str> {
        self.columns.iter().map(|(_, v)| v.trim()).find(|v| {
            let b = v.as_bytes();
            b.len() == 19
                && b[4] == b'-'
                && b[7] == b'-'
                && b[10] == b' '
                && b[13] == b':'
                && b[16] == b':'
                && v.chars()
                    .filter(|c| !matches!(c, '-' | ' ' | ':'))
                    .all(|c| c.is_ascii_digit())
        })
    }

    /// 크기(바이트, `$8` 열).
    pub fn size(&self) -> Option<i64> {
        self.col("$8")?.trim().parse().ok()
    }

    /// 'new' 표시(안 읽음). 받은 메일 보기에만 이 열이 있다.
    pub fn is_new(&self) -> bool {
        self.col("NewDocument").map(str::trim) == Some("1")
    }

    /// ★(중요 메일) 열: `★`면 켜짐, `☆`면 꺼짐, 그 열이 없으면 모름. 보기마다 열 이름이 다르다.
    pub fn star(&self) -> Option<bool> {
        self.columns.iter().find_map(|(_, v)| match v.trim() {
            "★" => Some(true),
            "☆" => Some(false),
            _ => None,
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ViewPage {
    pub toplevel: Option<usize>,
    pub entries: Vec<ViewEntry>,
}

/// `ReadViewEntries` XML. 목록 문서가 아니면(로그인 화면 등) `None`.
pub fn parse_view(xml: &str) -> Option<ViewPage> {
    let start = xml.find("<viewentries")?;
    let head_end = start + xml[start..].find('>')?;
    let head = attrs(&xml[start + "<viewentries".len()..head_end]);
    let mut page = ViewPage {
        toplevel: attr(&head, "toplevelentries").and_then(|v| v.parse().ok()),
        entries: Vec::new(),
    };
    let mut rest = &xml[head_end..];
    while let Some(i) = rest.find("<viewentry") {
        let after = &rest[i + "<viewentry".len()..];
        // `<viewentries`는 건너뛴다(이름이 앞부분을 공유).
        if !after.starts_with([' ', '>', '/', '\t', '\r', '\n']) {
            rest = after;
            continue;
        }
        let Some(tag_end) = after.find('>') else {
            break;
        };
        let tag = &after[..tag_end];
        let a = attrs(tag.trim_end_matches('/'));
        let mut entry = ViewEntry {
            position: attr(&a, "position").unwrap_or_default().to_owned(),
            unid: attr(&a, "unid").map(str::to_owned),
            noteid: attr(&a, "noteid").unwrap_or_default().to_owned(),
            children: attr(&a, "children")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0),
            columns: Vec::new(),
        };
        let body_start = tag_end + 1;
        if tag.ends_with('/') {
            page.entries.push(entry);
            rest = &after[body_start..];
            continue;
        }
        let body_end = after[body_start..]
            .find("</viewentry>")
            .map_or(after.len(), |j| body_start + j);
        let mut body = &after[body_start..body_end];
        while let Some(j) = body.find("<entrydata") {
            let ed = &body[j + "<entrydata".len()..];
            let Some(te) = ed.find('>') else { break };
            let name = attr(&attrs(&ed[..te]), "name")
                .unwrap_or_default()
                .to_owned();
            let inner_end = ed.find("</entrydata>").unwrap_or(ed.len());
            let inner = &ed[te + 1..inner_end.max(te + 1)];
            entry.columns.push((name, values(inner).join("; ")));
            body = &ed[inner_end.min(ed.len())..];
        }
        page.entries.push(entry);
        rest = &after[body_end.min(after.len())..];
    }
    Some(page)
}

/// `<text>`·`<number>`·`<datetime>` 값들.
fn values(inner: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = inner;
    while let Some(i) = rest.find('<') {
        let after = &rest[i + 1..];
        let Some(te) = after.find('>') else { break };
        let tag = &after[..te];
        let name = tag.split_whitespace().next().unwrap_or("");
        if matches!(name, "text" | "number" | "datetime") && !tag.ends_with('/') {
            let close = format!("</{name}>");
            let body = &after[te + 1..];
            let end = body.find(&close).unwrap_or(body.len());
            out.push(xml_unescape(&body[..end]));
            rest = &body[end..];
        } else {
            rest = &after[te + 1..];
        }
    }
    out
}

/// XML·HTML 문자 참조 풀기(`&amp;` `&lt;` `&gt;` `&quot;` `&apos;` `&nbsp;` `&#N;` `&#xH;`).
pub fn xml_unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_owned();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let semi = after.find(';').filter(|&j| j <= 10);
        let decoded = semi.and_then(|j| {
            let name = &after[..j];
            let c = match name {
                "amp" => '&',
                "lt" => '<',
                "gt" => '>',
                "quot" => '"',
                "apos" => '\'',
                "nbsp" => '\u{a0}',
                _ => {
                    let n = name.strip_prefix('#')?;
                    let code = match n.strip_prefix(['x', 'X']) {
                        Some(h) => u32::from_str_radix(h, 16).ok()?,
                        None => n.parse().ok()?,
                    };
                    char::from_u32(code)?
                }
            };
            Some((c, j))
        });
        match decoded {
            Some((c, j)) => {
                out.push(c);
                rest = &after[j + 1..];
            }
            None => {
                out.push('&');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// 태그 속성들(이름은 소문자). `a="1" b='2' c=3 d`.
fn attrs(s: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        while i < b.len() && (b[i].is_ascii_whitespace() || b[i] == b'/') {
            i += 1;
        }
        let ns = i;
        while i < b.len() && !b[i].is_ascii_whitespace() && b[i] != b'=' && b[i] != b'/' {
            i += 1;
        }
        if ns == i {
            i += 1;
            continue;
        }
        let name = s[ns..i].to_ascii_lowercase();
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        if i < b.len() && b[i] == b'=' {
            i += 1;
            while i < b.len() && b[i].is_ascii_whitespace() {
                i += 1;
            }
            let value = if i < b.len() && (b[i] == b'"' || b[i] == b'\'') {
                let q = b[i];
                let vs = i + 1;
                let ve = s[vs..]
                    .bytes()
                    .position(|c| c == q)
                    .map_or(s.len(), |p| vs + p);
                i = (ve + 1).min(s.len());
                &s[vs..ve]
            } else {
                let vs = i;
                while i < b.len() && !b[i].is_ascii_whitespace() {
                    i += 1;
                }
                &s[vs..i]
            };
            out.push((name, xml_unescape(value)));
        } else {
            out.push((name, String::new()));
        }
    }
    out
}

fn attr<'a>(a: &'a [(String, String)], name: &str) -> Option<&'a str> {
    a.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
}

// ---- 메일(Memo 양식) ----

/// MIME 조각 하나(`BodyContentType`의 한 항목).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MimePart {
    /// `M1.2`
    pub id: String,
    pub content_type: String,
    pub charset: Option<String>,
    /// 파일 이름(`Body_ContentName`에 있을 때).
    pub name: Option<String>,
}

impl MimePart {
    pub fn is_multipart(&self) -> bool {
        self.content_type.starts_with("multipart/")
    }
}

/// 열어 본 메일(Memo 양식 HTML에서 뽑은 것).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Memo {
    /// 제목(`<title>`).
    pub title: String,
    /// 숨은 입력란 등(이름, 값). 같은 이름은 처음 것.
    pub fields: Vec<(String, String)>,
    /// `<xmp id="xmpBody">` 안(MIME이 아닌 서식 있는 본문이면 본문 HTML).
    pub xmp_body: Option<String>,
    /// 이 메일의 첨부 링크(`…/$FILE/이름`, 서버 기준 경로나 절대 주소).
    pub files: Vec<String>,
}

impl Memo {
    /// 입력란 값(이름은 대소문자 무관). 비어 있으면 `None`.
    pub fn field(&self, name: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.trim())
            .filter(|v| !v.is_empty())
    }

    /// MIME 조각들(순서대로). MIME이 아닌 메일이면 비어 있다.
    pub fn parts(&self) -> Vec<MimePart> {
        let types = part_list(self.field("BodyContentType").unwrap_or(""));
        let charsets = part_list(self.field("BodyCharset").unwrap_or(""));
        let names = part_list(
            self.field("Body_ContentName")
                .or_else(|| self.field("BodyContentName"))
                .unwrap_or(""),
        );
        types
            .into_iter()
            .map(|(id, ct)| {
                let find = |l: &[(String, String)]| {
                    l.iter()
                        .find(|(i, _)| *i == id)
                        .map(|(_, v)| v.clone())
                        .filter(|v| !v.is_empty())
                };
                MimePart {
                    charset: find(&charsets),
                    name: find(&names),
                    content_type: ct.to_ascii_lowercase(),
                    id,
                }
            })
            .collect()
    }
}

/// `; M1^multipart/related; M1.1.1^text/html` → [(M1, multipart/related), (M1.1.1, text/html)].
fn part_list(s: &str) -> Vec<(String, String)> {
    s.split(';')
        .filter_map(|x| {
            let (id, v) = x.trim().split_once('^')?;
            let id = id.trim();
            (id.starts_with('M') && id[1..].chars().all(|c| c.is_ascii_digit() || c == '.'))
                .then(|| (id.to_owned(), v.trim().to_owned()))
        })
        .collect()
}

/// Memo 양식 HTML을 해석한다. `unid`: 이 메일(첨부 링크를 고를 때).
pub fn parse_memo(html: &str, unid: &str) -> Memo {
    let lower = html.to_ascii_lowercase();
    let mut memo = Memo::default();
    if let (Some(a), Some(b)) = (lower.find("<title>"), lower.find("</title>"))
        && a + 7 <= b
    {
        memo.title = xml_unescape(html[a + 7..b].trim());
    }
    let mut from = 0;
    while let Some(i) = lower[from..].find("<input") {
        let s = from + i + "<input".len();
        let Some(e) = lower[s..].find('>') else { break };
        let a = attrs(&html[s..s + e]);
        if let Some(name) = attr(&a, "name").filter(|n| !n.is_empty())
            && !memo
                .fields
                .iter()
                .any(|(n, _)| n.eq_ignore_ascii_case(name))
        {
            let value = attr(&a, "value").unwrap_or_default().to_owned();
            memo.fields.push((name.to_owned(), value));
        }
        from = s + e;
    }
    if let Some(i) = lower.find("id=\"xmpbody\"")
        && let Some(gt) = lower[i..].find('>')
    {
        let s = i + gt + 1;
        let e = lower[s..].find("</xmp>").map_or(html.len(), |j| s + j);
        memo.xmp_body = Some(html[s..e].to_owned());
    }
    memo.files = file_links(html, unid);
    memo
}

/// `at` 위치를 품은 주소 조각의 (시작, 끝). 따옴표·공백·`<>`·제어 문자에서 끊는다.
/// 첨부가 여럿이면 주소들이 U+0002로 이어져 있고(실계정 2026-10-21), 파일 이름의 괄호는 인코딩하지 않아서
/// `)`는 조각이 `(` 바로 뒤에서 시작했을 때(`url(…)`)만 끝으로 본다.
pub fn url_token(s: &str, at: usize) -> (usize, usize) {
    let stop = |c: char| matches!(c, '"' | '\'' | ' ' | '<' | '>') || c.is_control();
    let start = s[..at]
        .rfind(|c: char| stop(c) || c == '(' || c == '=')
        .map_or(0, |p| p + 1);
    let paren = start > 0 && s.as_bytes()[start - 1] == b'(';
    let end = s[at..]
        .find(|c: char| stop(c) || (paren && c == ')'))
        .map_or(s.len(), |p| at + p);
    (start, end)
}

/// `$FILE/` 링크들(이 메일 것만, 이름이 같으면 하나).
fn file_links(html: &str, unid: &str) -> Vec<String> {
    let lower = html.to_ascii_lowercase();
    let unid = unid.to_ascii_lowercase();
    let mut out: Vec<String> = Vec::new();
    let mut names: Vec<String> = Vec::new();
    let mut from = 0;
    while let Some(i) = lower[from..].find("/$file/") {
        let at = from + i;
        let (start, end) = url_token(&lower, at);
        from = end.max(at + 1);
        let link = &html[start..end];
        let name = &link[at - start + "/$FILE/".len()..];
        if !lower[start..at].contains(&unid) || name.is_empty() {
            continue;
        }
        let name = file_name(name.strip_suffix("?OpenElement").unwrap_or(name));
        if !names.contains(&name) {
            names.push(name);
            out.push(link.to_owned());
        }
    }
    out
}

/// `$FILE/` 뒤의 이름(퍼센트 인코딩 풀기, UTF-8이 아니면 EUC-KR).
pub fn file_name(enc: &str) -> String {
    let bytes: Vec<u8> = percent_decode_str(enc).collect();
    match String::from_utf8(bytes) {
        Ok(s) => s,
        Err(e) => encoding_rs::EUC_KR.decode(e.as_bytes()).0.into_owned(),
    }
}

// ---- 시작 페이지·쿠키 ----

/// 자바스크립트 문자열 변수 값(`var  mail_server="…";`). 이름 앞이 글자·숫자면(다른 변수) 건너뛴다.
pub fn js_var(html: &str, name: &str) -> Option<String> {
    let mut from = 0;
    while let Some(i) = html[from..].find(name) {
        let at = from + i;
        from = at + name.len();
        let before_ok = html[..at]
            .chars()
            .next_back()
            .is_none_or(|c| !(c.is_alphanumeric() || c == '_' || c == '.'));
        let rest = html[from..].trim_start_matches([' ', '\t']);
        let Some(rest) = rest.strip_prefix('=') else {
            continue;
        };
        let rest = rest.trim_start_matches([' ', '\t']);
        let Some(q) = rest.chars().next().filter(|c| *c == '"' || *c == '\'') else {
            continue;
        };
        if !before_ok {
            continue;
        }
        let body = &rest[1..];
        let end = body.find(q)?;
        return Some(body[..end].to_owned());
    }
    None
}

/// 시작 페이지에서 (웹메일 서버, 메일 파일).
pub fn parse_start_home(html: &str) -> Option<(String, String)> {
    let host = js_var(html, "mail_server")?.trim().to_ascii_lowercase();
    let db = js_var(html, "mailFile")?
        .trim()
        .trim_start_matches('/')
        .to_owned();
    (!host.is_empty()
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':'))
        && valid_db(&db))
    .then_some((host, db))
}

/// "메일용량확인" 창의 값(바이트). 창은 `var max = "300"; var warning = "240"; var inbox = "211.71"; … var trash = "0";
/// var totvalue = "212.28";`(MB)를 그린다. 지운 메일도 총 사용량에 들어간다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MailSize {
    pub total: i64,
    pub limit: Option<i64>,
    pub warning: Option<i64>,
    pub trash: Option<i64>,
}

pub fn parse_mail_size(html: &str) -> Option<MailSize> {
    let mb = |name: &str| {
        let v: f64 = js_var(html, name)?.trim().parse().ok()?;
        (v >= 0.0).then(|| (v * 1024.0 * 1024.0).round() as i64)
    };
    Some(MailSize {
        total: mb("totvalue")?,
        limit: mb("max").filter(|v| *v > 0),
        warning: mb("warning").filter(|v| *v > 0),
        trash: mb("trash"),
    })
}

/// 시작 페이지의 메일함 한도(`var MailDbSize = "300"`, MB) → 바이트. 화면 스크립트가 이 값으로 `ckUserInfo` 쿠키
/// (`mq1`)를 만든다(실계정 2026-10-21: 로그인 응답에는 이 쿠키가 없다). "무제한"이면 없음.
pub fn quota_limit_from_start_home(html: &str) -> Option<i64> {
    let mb: i64 = js_var(html, "MailDbSize")?.trim().parse().ok()?;
    (mb > 0).then(|| mb * 1024 * 1024)
}

/// `ckUserInfo` 쿠키의 메일함 한도(`mq1`, MB) → 바이트.
pub fn quota_limit_from_cookie(value: &str) -> Option<i64> {
    let decoded =
        String::from_utf8_lossy(&percent_decode_str(value).collect::<Vec<u8>>()).into_owned();
    let i = decoded
        .match_indices("mq1=")
        .find(|(i, _)| *i == 0 || decoded.as_bytes()[i - 1] == b'&')?
        .0;
    let digits: String = decoded[i + 4..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    let mb: i64 = digits.parse().ok()?;
    (mb > 0).then(|| mb * 1024 * 1024)
}

/// 로그인 화면(세션 만료)인가: 로그인 폼이 있는 HTML.
pub fn is_login_page(html: &str) -> bool {
    html.contains("names.nsf?Login") && html.to_ascii_lowercase().contains("type=\"password\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    const DB: &str = "mail9/u000001.nsf";
    const UNID: &str = "98C9B1996C6C20DA49258E96000C0C4C";

    #[test]
    fn allowlist_opens_reads_and_agreed_changes_only() {
        let ok = [
            format!("{DB}/XML_Inbox?ReadViewEntries&start=1&count=500"),
            format!(
                "{DB}/XML_Category?ReadViewEntries&start=1&count=500&RestrictToCategory=%24%24Str%ED%8C%80"
            ),
            format!("{DB}/Size_Sum?ReadViewEntries&count=1"),
            format!("{DB}/mail_size?openform"),
            format!("{DB}/all/{UNID}?OpenDocument&Charset=UTF-8"),
            format!(
                "{DB}/0/{}/Body/M1.1.1?OpenElement&Charset=UTF-8",
                UNID.to_lowercase()
            ),
            format!("{DB}/0/{}/Body/M1.2?OpenElement", UNID.to_lowercase()),
            format!("{DB}/all/{UNID}/$FILE/%EC%8B%9D%EB%8B%A8.7z"),
            format!("{DB}/wMarkRead?OpenAgent&&docid=18B3A&type=2"),
            format!("{DB}/wMarkRead?OpenAgent&&docid=18B3A,18BCE&type=2"),
            format!("{DB}/WebDelDoc2?OpenAgent&category=Inbox&docid=18B3A"),
            format!("{DB}/WebDelDoc2?OpenAgent&category=Trash&docid=18B3A,1"),
            "home.nsf/StartHome?openform".to_owned(),
            // 0.6.0 양방향 동기화: 웹메일 화면 스크립트와 같은 모양만.
            format!("{DB}/XML_All?ReadViewEntries&start=1&count=500"),
            format!("{DB}/wMarkRead?OpenAgent&&docid=18B3A&type=1"),
            format!("{DB}/agt_impo?OpenAgent&sid=18B3A&type=N"),
            format!("{DB}/agt_impo?OpenAgent&sid=18B3A&type=H"),
            format!("{DB}/WebDelDoc2?OpenAgent&category=Sent&docid=18B3A"),
            format!("{DB}/WebDelDoc2?OpenAgent&category=POP3&docid=18B3A"),
            format!("{DB}/WebDelDoc2?OpenAgent&category=$$Str%ED%8C%80&docid=18B3A"),
            format!("{DB}/RecoverDelDoc?OpenAgent&RTView=($Inbox)&DocList=18B3A,18BCE"),
            format!("{DB}/folder_move?OpenAgent&docid=18B3A&fn=%ED%8C%80"),
            format!("{DB}/wFolderMoveCancel?OpenAgent&&docid=18B3A"),
        ];
        for p in &ok {
            assert!(allowed(false, p), "열어야 함: {p}");
        }
        assert!(allowed(true, "names.nsf?Login"));
        let blocked = [
            // 웹메일의 다른 동작과, 정한 동작이라도 다른 모양은 막는다.
            format!("{DB}/wMarkRead?OpenAgent&&docid=18B3A&type=3"),
            format!("{DB}/WebDelDoc2?OpenAgent&docid=18B3A"),
            format!("{DB}/WebDelDoc2?OpenAgent&category=Unread&docid=18B3A"),
            format!("{DB}/WebDelDoc2?OpenAgent&category=$$Str&docid=18B3A"),
            format!("{DB}/WebDelDoc2?OpenAgent&category=Inbox%26x&docid=18B3A"),
            format!("{DB}/Trash_delete_all?OpenAgent"),
            format!("{DB}/Spam_delete_all?OpenAgent"),
            format!("{DB}/folder_move?OpenAgent&docid=18B3A&fn=a&b=1"),
            format!("{DB}/folder_move?OpenAgent&docid=18B3A&fn="),
            format!("{DB}/RecoverDelDoc?OpenAgent&RTView=x&DocList=1"),
            format!("{DB}/agt_impo?OpenAgent&sid=18B3A,18BCE&type=N"),
            format!("{DB}/agt_impo?OpenAgent&sid=18B3A&type=X"),
            format!("{DB}/set_linecount?OpenAgent&count=30"),
            format!("{DB}/mail_size?openform&x=1"),
            format!("{DB}/Memo?OpenForm"),
            format!("{DB}/DocumentInheritedCopy?Openagent&NoteID=1&Action=Reply"),
            format!("{DB}/agCreateMemo?OpenAgent&sid=1"),
            format!("{DB}/XML_Inbox?ReadViewEntries&x=?OpenAgent"),
            format!("{DB}/XML_Approval?ReadViewEntries&count=1"),
            format!("{DB}/all/{UNID}?EditDocument"),
            format!("{DB}/all/{UNID}?OpenDocument"),
            format!("{DB}/all/{UNID}?DeleteDocument"),
            format!("{DB}/0/{UNID}/Body/M1?SaveDocument"),
            format!("{DB}/0/{UNID}/Body/M1?OpenElement&x=1"),
            format!("{DB}/all/{UNID}/$FILE/a?OpenAgent"),
            format!("{DB}/all/NOTAUNID/$FILE/a"),
            format!("{DB}/wMarkRead?OpenAgent&&docid=&type=2"),
            format!("{DB}/wMarkRead?OpenAgent&&docid=18B3A;x&type=2"),
            "../names.nsf/x.nsf/XML_Inbox?ReadViewEntries".to_owned(),
            "names.nsf?Logout".to_owned(),
            "home.nsf?open".to_owned(),
        ];
        for p in &blocked {
            assert!(!allowed(false, p), "막아야 함: {p}");
        }
        assert!(!allowed(true, &format!("{DB}/XML_Inbox?ReadViewEntries")));
        assert!(!allowed(false, "names.nsf?Login"), "로그인은 POST만");
    }

    #[test]
    fn view_xml_entries_and_columns() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<viewentries timestamp="20261021T013631,81Z" toplevelentries="323">
<viewentry position="1" unid="229185154069609749258E97000662F1" noteid="18BCE" siblings="323">
<entrydata columnnumber="0" name="ReceiverType">
<text>T</text></entrydata>
<entrydata columnnumber="3" name="$VC_Subject">
<text>&lt;b&gt; [내부 DNS작업요청]이 처리완료 되었습니다.&lt;/b&gt;</text></entrydata>
<entrydata columnnumber="7" name="$7">
<text>2026-10-21 10:09:45</text></entrydata>
<entrydata columnnumber="8" name="$8">
<number>1649</number></entrydata>
<entrydata columnnumber="9" name="NewDocument">
<text>1</text></entrydata>
</viewentry>
<viewentry position="2" unid="9A75D7CA327A89E649258E9600205558" noteid="18A00" siblings="323">
<entrydata columnnumber="2" name="$10"><text>2026-10-20 14:53:10</text></entrydata>
<entrydata columnnumber="3" name="List"><textlist><text>a</text><text>b</text></textlist></entrydata>
<entrydata columnnumber="9" name="NewDocument"><text>0</text></entrydata>
</viewentry>
</viewentries>
"#;
        let p = parse_view(xml).unwrap();
        assert_eq!(p.toplevel, Some(323));
        assert_eq!(p.entries.len(), 2);
        let e = &p.entries[0];
        assert_eq!(e.unid.as_deref(), Some("229185154069609749258E97000662F1"));
        assert_eq!(e.noteid, "18BCE");
        assert_eq!(
            e.col("$VC_Subject"),
            Some("<b> [내부 DNS작업요청]이 처리완료 되었습니다.</b>")
        );
        assert_eq!(e.full_date(), Some("2026-10-21 10:09:45"));
        assert_eq!(e.size(), Some(1649));
        assert!(e.is_new());
        let e = &p.entries[1];
        assert_eq!(
            e.full_date(),
            Some("2026-10-20 14:53:10"),
            "보낸 메일은 $10"
        );
        assert_eq!(e.col("List"), Some("a; b"));
        assert!(!e.is_new());
        assert!(parse_view("<html><form action=\"/names.nsf?Login\">").is_none());
        let empty = parse_view(
            "<?xml version=\"1.0\"?>\n<viewentries toplevelentries=\"0\">\n</viewentries>\n",
        )
        .unwrap();
        assert_eq!((empty.toplevel, empty.entries.len()), (Some(0), 0));
    }

    #[test]
    fn memo_fields_parts_and_files() {
        let html = r#"<html><head><title>기계실 장비 주황점등 발생해서 알려드립니다  </title></head><body>
<input type="button" name="ReplyBtn" value="회신">
<input name="From" type="hidden" value="&quot;=?EUC-KR?B?sejDtrz2?=&quot; &lt;U000002@inbound.example.com&gt;">
<input type="hidden" name="SendTo" value="user1@example.com; user2@example.com">
<input type="hidden" name="CopyTo" value="">
<input type="hidden" name="Importance" value="Normal">
<input type="hidden" name="BodyContentType" value="; M1^multipart/related; M1.1^multipart/alternative; M1.1.1^text/html; M1.2^image/png">
<input type="hidden" name="BodyCharset" value="; M1^; M1.1^; M1.1.1^UTF-8; M1.2^">
<input type="hidden" name="From" value="second">
<input name=tmpAttUrl type=hidden value="/mail9/u000001.nsf/all/09FFA558D814FC0249258D4600470AB8/$FILE/%EC%8B%9D%EB%8B%A8.7z">
<script>var AttachURL = "/mail9/u000001.nsf/all/09FFA558D814FC0249258D4600470AB8/$FILE/%EC%8B%9D%EB%8B%A8.7z";
var other = "/mail9/u000001.nsf/all/11111111111111111111111111111111/$FILE/x.txt";</script>
<a href='https://mail1.example.com/mail9/u000001.nsf/0/09ffa558d814fc0249258d4600470ab8/$FILE/b%20c.pdf?OpenElement'>b</a>
<div id="displayBody"><xmp id="xmpBody" style="display:none;">
<iframe src="/mail9/u000001.nsf/0/09ffa558d814fc0249258d4600470ab8/Body/M1.1.1?OpenElement"></iframe>
</xmp></div></body></html>"#;
        let m = parse_memo(html, "09FFA558D814FC0249258D4600470AB8");
        assert_eq!(m.title, "기계실 장비 주황점등 발생해서 알려드립니다");
        assert_eq!(
            m.field("from"),
            Some("\"=?EUC-KR?B?sejDtrz2?=\" <U000002@inbound.example.com>"),
            "같은 이름은 처음 것, 값의 문자 참조는 푼다"
        );
        assert_eq!(m.field("CopyTo"), None, "빈 값은 없음");
        assert_eq!(m.field("Importance"), Some("Normal"));
        let parts = m.parts();
        assert_eq!(
            parts
                .iter()
                .map(|p| (p.id.as_str(), p.content_type.as_str(), p.charset.as_deref()))
                .collect::<Vec<_>>(),
            [
                ("M1", "multipart/related", None),
                ("M1.1", "multipart/alternative", None),
                ("M1.1.1", "text/html", Some("UTF-8")),
                ("M1.2", "image/png", None),
            ]
        );
        assert_eq!(
            m.files,
            [
                "/mail9/u000001.nsf/all/09FFA558D814FC0249258D4600470AB8/$FILE/%EC%8B%9D%EB%8B%A8.7z",
                "https://mail1.example.com/mail9/u000001.nsf/0/09ffa558d814fc0249258d4600470ab8/$FILE/b%20c.pdf?OpenElement",
            ],
            "이 메일 것만, 같은 이름은 하나"
        );
        assert!(
            m.xmp_body
                .as_deref()
                .unwrap()
                .contains("Body/M1.1.1?OpenElement")
        );
        // 첨부가 여럿: 주소들이 U+0002로 이어지고, 이름의 괄호는 인코딩하지 않는다(실계정).
        let u = "96CC945DCBB2F5AB49258E96007E3A3A";
        let many = format!(
            "<script>var AttachURL = \"/mail9/b.nsf/all/{u}/$FILE/Citrix2610(%EA%B0%9C%EC%A0%95).xlsx\u{2}\
             /mail9/b.nsf/all/{u}/$FILE/TEAM%20BACKUP%2010%EC%9B%94%20(12).xlsx\u{2}/mail9/b.nsf/all/{u}/$FILE/r.html\";\
             </script><div style=\"background:url(/mail9/b.nsf/0/{u}/$FILE/bg.png)\"></div>"
        );
        assert_eq!(
            parse_memo(&many, u)
                .files
                .iter()
                .map(|l| file_name(l.rsplit("/$FILE/").next().unwrap()))
                .collect::<Vec<_>>(),
            [
                "Citrix2610(개정).xlsx",
                "TEAM BACKUP 10월 (12).xlsx",
                "r.html",
                "bg.png"
            ]
        );
        assert_eq!(file_name("%EC%8B%9D%EB%8B%A8.7z"), "식단.7z");
        assert_eq!(file_name("%BD%C4%B4%DC.7z"), "식단.7z", "EUC-KR 이름");
        let single = parse_memo(
            r#"<input name="BodyContentType" value="M1^text/plain"><input name="BodyCharset" value="M1^ks_c_5601-1987">"#,
            UNID,
        );
        assert_eq!(
            single.parts(),
            [MimePart {
                id: "M1".into(),
                content_type: "text/plain".into(),
                charset: Some("ks_c_5601-1987".into()),
                name: None
            }]
        );
    }

    #[test]
    fn start_home_cookie_and_login_page() {
        let html = "var  RefDBpath='';\nvar  cmail_server=\"evil.example.com\";\nvar  mail_server=\"mail1.example.com\";\n\
                    var  mailhost = \"mail1\";\nvar  mailFile = \"mail9/u000001.nsf\";\n";
        assert_eq!(
            parse_start_home(html),
            Some((
                "mail1.example.com".to_owned(),
                "mail9/u000001.nsf".to_owned()
            ))
        );
        assert_eq!(
            parse_start_home("var mail_server=\"h.example\"; var mailFile=\"../x.nsf\";"),
            None,
            "이상한 메일 파일 경로는 거절"
        );
        assert_eq!(
            quota_limit_from_cookie(
                "home_server%3Dapp1%26MailFile%3Dmail9/u000001.nsf%26mq1%3D300%26mq2%3D240"
            ),
            Some(300 * 1024 * 1024)
        );
        assert_eq!(quota_limit_from_cookie("xmq1%3D5"), None);
        assert_eq!(
            quota_limit_from_start_home("var MailDbSize = \"300\"\nvar MailWarningSize = \"240\""),
            Some(300 * 1024 * 1024)
        );
        assert_eq!(
            quota_limit_from_start_home("var MailDbSize = \"무제한\""),
            None
        );
        let size = parse_mail_size(
            "<script>var max = \"300\";\nvar warning = \"240\";\nvar inbox = \"211.71\";\nvar sent = \"0.54\";\n\
             var trash = \"0\";\nvar pop3 = \"0.03\";\nvar totvalue = \"212.28\";</script>",
        )
        .unwrap();
        assert_eq!(size.total, (212.28_f64 * 1_048_576.0).round() as i64);
        assert_eq!(size.limit, Some(300 * 1024 * 1024));
        assert_eq!(size.trash, Some(0));
        assert_eq!(parse_mail_size("<html>로그인</html>"), None);
        assert!(is_login_page(
            "<form method=\"post\" action=\"/names.nsf?Login\" name=\"_DominoForm\"><input type=\"password\" name=\"password\">"
        ));
        assert!(!is_login_page(
            "<viewentries toplevelentries=\"0\"></viewentries>"
        ));
        assert_eq!(xml_unescape("a&amp;b&#44;&#x41;&bogus;&"), "a&b,A&bogus;&");
    }
}
