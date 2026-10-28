//! 가져오기(계획서 10장 7단계): EML 요약, EML 만들기, 유저스크립트 HTML 백업에서 EML 복원.
//!
//! 웹메일 유저스크립트 백업 형식:
//! `편지함\yyyy\mm\{yyyymmdd_HHMMSS_제목_해시}.html` + 같은 이름의 `_files\`(첨부·본문 이미지).
//! HTML은 `<div class="hdr"><h1>제목</h1><table>`(보낸 사람·받는 사람·참조·받은 날짜·보낸 날짜) +
//! `<div class="mailbody">본문</div>` + `<div class="atts">`(첨부 링크)로 고정이다. 원문(Message-ID, 숨은 참조,
//! 원래 MIME 구조)은 남아 있지 않은 손실 형식이라, 남은 것으로 EML을 다시 만들고 "HTML 백업에서 복원됨"을 표시한다.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use mail_parser::MessageParser;
use percent_encoding::percent_decode_str;

use crate::compose::{OriginalPart, Person};

/// 가져올 EML의 목록용 정보.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Summary {
    pub message_id: Option<String>,
    pub subject: String,
    pub from: Option<Person>,
    /// "이름 <주소>; ..." 형식.
    pub to: String,
    pub cc: String,
    pub date: Option<i64>,
    pub has_attachments: bool,
    /// 0 낮음, 1 보통, 2 높음.
    pub importance: i64,
    pub preview: String,
}

fn list(a: Option<&mail_parser::Address<'_>>) -> String {
    let Some(a) = a else { return String::new() };
    a.iter()
        .filter_map(|x| {
            // 주소 없는 이름(`이름 <>`, HCL Domino의 노츠 이름)도 남긴다.
            let email = x.address().map(str::trim).filter(|e| !e.is_empty());
            let name = x.name().map(str::trim).filter(|n| !n.is_empty());
            match (name, email) {
                (Some(n), Some(e)) if n != e => Some(format!("{n} <{e}>")),
                (_, Some(e)) => Some(e.to_owned()),
                (Some(n), None) => Some(n.to_owned()),
                (None, None) => None,
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// EML을 해석해 목록용 정보를 뽑는다. 해석할 수 없으면 `None`.
pub fn summarize(raw: &[u8]) -> Option<Summary> {
    let msg = MessageParser::default().parse(raw)?;
    // 주소 없는 이름만 있는 보낸 사람도 남긴다(email은 빈 값).
    let from = msg.from().and_then(|a| a.first()).and_then(|x| {
        let p = Person {
            name: x.name().unwrap_or("").trim().to_owned(),
            email: x.address().unwrap_or("").trim().to_owned(),
        };
        (!p.name.is_empty() || !p.email.is_empty()).then_some(p)
    });
    // 노츠 형식 날짜(HCL Domino EML 백업)는 직접 읽는다. 아니면 mail-parser.
    let date = msg
        .header_raw("Date")
        .and_then(parse_notes_date)
        .or_else(|| msg.date().map(|d| d.to_timestamp() * 1000));
    let importance = match msg
        .header_raw("Importance")
        .or_else(|| msg.header_raw("X-Priority"))
        .map(|v| v.trim().to_ascii_lowercase())
    {
        Some(v) if v.starts_with("high") || v.starts_with('1') || v.starts_with('2') => 2,
        Some(v) if v.starts_with("low") || v.starts_with('4') || v.starts_with('5') => 0,
        _ => 1,
    };
    let preview: String = msg
        .body_text(0)
        .map(|t| t.split_whitespace().collect::<Vec<_>>().join(" "))
        .unwrap_or_default()
        .chars()
        .take(160)
        .collect();
    Some(Summary {
        message_id: msg
            .message_id()
            .map(|m| format!("<{}>", m.trim_matches(['<', '>']))),
        subject: msg.subject().unwrap_or_default().to_owned(),
        from,
        to: list(msg.to()),
        cc: list(msg.cc()),
        date,
        has_attachments: msg.attachment_count() > 0,
        importance,
        preview,
    })
}

/// 새로 만들 EML.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NewEml {
    pub from: Vec<Person>,
    pub to: Vec<Person>,
    pub cc: Vec<Person>,
    /// 내가 보낸 메일의 숨은 참조(HCL Domino 백업에는 남아 있다).
    pub bcc: Vec<Person>,
    pub reply_to: Vec<Person>,
    pub subject: String,
    pub date: Option<i64>,
    pub message_id: String,
    pub html: String,
    /// `content_id`가 있는 인라인 이미지.
    pub inline: Vec<OriginalPart>,
    pub attachments: Vec<OriginalPart>,
    /// 덧붙일 머리글(이름, 값).
    pub extra_headers: Vec<(String, String)>,
}

/// RFC 2047 B 인코딩(UTF-8). ASCII면 그대로.
pub(crate) fn word(s: &str) -> String {
    if s.is_ascii() && !s.contains(['\r', '\n']) {
        s.to_owned()
    } else {
        format!(
            "=?UTF-8?B?{}?=",
            STANDARD.encode(s.replace(['\r', '\n'], " "))
        )
    }
}

fn addr_header(list: &[Person]) -> String {
    list.iter()
        .map(|p| {
            if p.name.is_empty() {
                format!("<{}>", p.email)
            } else {
                format!("{} <{}>", word(&p.name), p.email)
            }
        })
        .collect::<Vec<_>>()
        .join(",\r\n ")
}

pub(crate) fn b64_lines(data: &[u8]) -> String {
    let s = STANDARD.encode(data);
    s.as_bytes()
        .chunks(76)
        .map(|c| String::from_utf8_lossy(c).into_owned())
        .collect::<Vec<_>>()
        .join("\r\n")
}

/// RFC 2822 날짜(한국 시간 표기).
fn rfc2822(ms: i64) -> String {
    const DAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let (y, m, d, h, mi, s, days) = kst_civil(ms);
    format!(
        "{}, {d} {} {y} {h:02}:{mi:02}:{s:02} +0900",
        DAYS[days.rem_euclid(7) as usize],
        MONTHS[(m - 1) as usize]
    )
}

/// unix ms → 한국 시간 (연, 월, 일, 시, 분, 초, 1970-01-01부터 날수).
pub(crate) fn kst_civil(ms: i64) -> (i64, i64, i64, i64, i64, i64, i64) {
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
    (y, m, d, rem / 3600, (rem / 60) % 60, rem % 60, days)
}

/// 노츠 형식 날짜 → unix ms: `09-Oct-2025 13:05:22 ZE9`(ZE9 = UTC+9, ZW5 = UTC−5). 시간대가 없으면 한국 시간.
/// HCL Domino EML 백업의 일부(실데이터 71통 중 27통)가 이 형식이다. 노츠 형식이 아니면 `None`.
pub fn parse_notes_date(s: &str) -> Option<i64> {
    const MONTHS: [&str; 12] = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    let mut parts = s.split_whitespace();
    let (date, time, zone) = (parts.next()?, parts.next()?, parts.next());
    if parts.next().is_some() {
        return None;
    }
    let mut d = date.split('-');
    let (day, mon, year) = (d.next()?, d.next()?, d.next()?);
    let month = MONTHS.iter().position(|m| m.eq_ignore_ascii_case(mon))? + 1;
    let (day, year): (u32, u32) = (day.parse().ok()?, year.parse().ok()?);
    // 동부 +, 서부 −. 시 다음 글자·숫자(분)는 있으면 분으로 본다(예: ZE5C처럼 알 수 없는 꼬리는 무시).
    let offset_min: i64 = match zone.map(str::to_ascii_uppercase).as_deref() {
        None | Some("KST") => 9 * 60,
        Some("GMT" | "UTC" | "Z" | "UT") => 0,
        Some(z) => {
            let sign = match z.get(..2)? {
                "ZE" => 1,
                "ZW" => -1,
                _ => return None,
            };
            let digits: String = z[2..].chars().take_while(char::is_ascii_digit).collect();
            let hours: i64 = digits.parse().ok()?;
            sign * hours * 60
        }
    };
    let kst = parse_kst(&format!("{year:04}-{month:02}-{day:02} {time}"))?;
    Some(kst + (9 * 60 - offset_min) * 60_000)
}

/// 한국 시간 "YYYY-MM-DD HH:MM:SS"(초는 없어도 됨) → unix ms.
pub fn parse_kst(s: &str) -> Option<i64> {
    let s = s.trim();
    let digits: Vec<i64> = s
        .split(|c: char| !c.is_ascii_digit())
        .filter(|p| !p.is_empty())
        .map(|p| p.parse().ok())
        .collect::<Option<_>>()?;
    let [y, m, d, rest @ ..] = digits.as_slice() else {
        return None;
    };
    let (h, mi, sec) = (
        rest.first().copied().unwrap_or(0),
        rest.get(1).copied().unwrap_or(0),
        rest.get(2).copied().unwrap_or(0),
    );
    if !(1..=12).contains(m) || !(1..=31).contains(d) || h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    let (yy, mm) = if *m <= 2 { (y - 1, m + 9) } else { (*y, m - 3) };
    let era = yy.div_euclid(400);
    let yoe = yy - era * 400;
    let doy = (153 * mm + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some((days * 86_400 + h * 3600 + mi * 60 + sec - 9 * 3600) * 1000)
}

pub(crate) fn filename_param(name: &str) -> String {
    let safe = name.replace(['"', '\r', '\n'], "_");
    format!("\"{}\"", word(&safe))
}

/// EML을 만든다: multipart/mixed(related(html + 인라인 이미지) + 첨부).
pub fn build_eml(e: &NewEml) -> Vec<u8> {
    let mut out = String::new();
    if !e.from.is_empty() {
        out.push_str(&format!("From: {}\r\n", addr_header(&e.from)));
    }
    if !e.to.is_empty() {
        out.push_str(&format!("To: {}\r\n", addr_header(&e.to)));
    }
    if !e.cc.is_empty() {
        out.push_str(&format!("Cc: {}\r\n", addr_header(&e.cc)));
    }
    if !e.bcc.is_empty() {
        out.push_str(&format!("Bcc: {}\r\n", addr_header(&e.bcc)));
    }
    if !e.reply_to.is_empty() {
        out.push_str(&format!("Reply-To: {}\r\n", addr_header(&e.reply_to)));
    }
    out.push_str(&format!("Subject: {}\r\n", word(&e.subject)));
    if let Some(d) = e.date {
        out.push_str(&format!("Date: {}\r\n", rfc2822(d)));
    }
    out.push_str(&format!("Message-ID: {}\r\n", e.message_id));
    for (k, v) in &e.extra_headers {
        out.push_str(&format!("{k}: {}\r\n", word(v)));
    }
    out.push_str("MIME-Version: 1.0\r\n");
    out.push_str("Content-Type: multipart/mixed; boundary=\"outlook-mixed\"\r\n\r\n");
    out.push_str(
        "--outlook-mixed\r\nContent-Type: multipart/related; boundary=\"outlook-related\"\r\n\r\n",
    );
    out.push_str(&format!(
        "--outlook-related\r\nContent-Type: text/html; charset=UTF-8\r\nContent-Transfer-Encoding: base64\r\n\r\n{}\r\n",
        b64_lines(e.html.as_bytes())
    ));
    for p in &e.inline {
        out.push_str(&format!(
            "--outlook-related\r\nContent-Type: {}; name={}\r\nContent-ID: <{}>\r\n\
             Content-Disposition: inline; filename={}\r\nContent-Transfer-Encoding: base64\r\n\r\n{}\r\n",
            p.mime,
            filename_param(&p.name),
            p.content_id.as_deref().unwrap_or("image"),
            filename_param(&p.name),
            b64_lines(&p.data)
        ));
    }
    out.push_str("--outlook-related--\r\n");
    for a in &e.attachments {
        out.push_str(&format!(
            "--outlook-mixed\r\nContent-Type: {}; name={}\r\nContent-Disposition: attachment; filename={}\r\n\
             Content-Transfer-Encoding: base64\r\n\r\n{}\r\n",
            a.mime,
            filename_param(&a.name),
            filename_param(&a.name),
            b64_lines(&a.data)
        ));
    }
    out.push_str("--outlook-mixed--\r\n");
    out.into_bytes()
}

/// 유저스크립트 HTML 백업 한 통.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackupMail {
    pub subject: String,
    pub from: Vec<Person>,
    pub to: Vec<Person>,
    pub cc: Vec<Person>,
    pub received: Option<i64>,
    pub sent: Option<i64>,
    /// 본문 HTML(이미지는 백업 안 상대 경로).
    pub body_html: String,
    /// 첨부(표시 이름, `_files` 안 상대 경로). 파일이 없는 첨부(항목 첨부 등)는 경로가 없다.
    pub attachments: Vec<(String, Option<String>)>,
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

/// 주소 머리글 값(RFC 2047 그대로, `;`로 이은 것도) → 사람들. 주소 없는 이름도 남긴다(email은 빈 값).
/// HCL Domino 웹메일의 `From`·`SendTo` 입력란 값(`"=?EUC-KR?B?…?=" <a@b>; c@d`).
pub fn parse_address_value(raw: &str) -> Vec<Person> {
    let v = raw.replace(['\r', '\n'], " ").replace(';', ",");
    if v.trim().is_empty() {
        return Vec::new();
    }
    let doc = format!("To: {v}\r\n\r\n");
    let Some(msg) = MessageParser::default().parse(doc.as_bytes()) else {
        return Vec::new();
    };
    msg.to()
        .map(|a| {
            a.iter()
                .filter_map(|x| {
                    let email = x.address().unwrap_or("").trim().to_owned();
                    let name = x.name().unwrap_or("").trim().to_owned();
                    (!name.is_empty() || !email.is_empty()).then(|| Person {
                        name: if name == email { String::new() } else { name },
                        email,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// "이름 <주소>; 주소2" → 사람들.
pub fn parse_people(s: &str) -> Vec<Person> {
    s.split(';')
        .map(str::trim)
        .filter(|x| x.contains('@'))
        .map(|x| match (x.rfind('<'), x.rfind('>')) {
            (Some(a), Some(b)) if a < b => Person {
                name: x[..a].trim().trim_matches('"').to_owned(),
                email: x[a + 1..b].trim().to_owned(),
            },
            _ => Person {
                name: String::new(),
                email: x.to_owned(),
            },
        })
        .collect()
}

fn between<'a>(s: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let i = s.find(start)? + start.len();
    let j = s[i..].find(end)? + i;
    Some(&s[i..j])
}

/// 유저스크립트 HTML 백업이면 해석한다(아니면 `None`).
pub fn parse_backup_html(doc: &str) -> Option<BackupMail> {
    let hdr = between(doc, "<div class=\"hdr\">", "</table></div>")?;
    let body_start = doc.find("<div class=\"mailbody\">")? + "<div class=\"mailbody\">".len();
    let mut m = BackupMail {
        subject: unescape(between(hdr, "<h1>", "</h1>").unwrap_or("")),
        ..BackupMail::default()
    };
    let mut rest = hdr;
    while let Some(i) = rest.find("<tr><td class=\"k\">") {
        rest = &rest[i + "<tr><td class=\"k\">".len()..];
        let Some(k_end) = rest.find("</td><td>") else {
            break;
        };
        let key = unescape(&rest[..k_end]);
        let after = &rest[k_end + "</td><td>".len()..];
        let Some(v_end) = after.find("</td></tr>") else {
            break;
        };
        let value = unescape(&after[..v_end]);
        match key.as_str() {
            "보낸 사람" => m.from = parse_people(&value),
            "받는 사람" => m.to = parse_people(&value),
            "참조" => m.cc = parse_people(&value),
            "받은 날짜" => m.received = parse_kst(&value),
            "보낸 날짜" => m.sent = parse_kst(&value),
            _ => {}
        }
        rest = &after[v_end..];
    }
    // 본문은 마지막 "</div>" 앞까지(그 뒤는 첨부 목록과 </body>).
    let tail = doc[body_start..]
        .rfind("<div class=\"atts\">")
        .or_else(|| doc[body_start..].rfind("</body>"))
        .map_or(doc.len(), |i| i + body_start);
    let body_end = doc[body_start..tail]
        .rfind("</div>")
        .map_or(tail, |i| i + body_start);
    m.body_html = doc[body_start..body_end]
        .trim_matches(['\n', '\r'])
        .to_owned();
    if let Some(atts) = doc[tail..].strip_prefix("<div class=\"atts\">") {
        let mut rest = atts;
        while let Some(i) = rest.find("<li>") {
            rest = &rest[i + 4..];
            let Some(end) = rest.find("</li>") else { break };
            let li = &rest[..end];
            if let Some(href) = between(li, "<a href=\"", "\"") {
                let name = between(li, "target=\"_blank\">", "</a>").unwrap_or(href);
                m.attachments.push((unescape(name), Some(href.to_owned())));
            } else {
                let name = li.split(" <span").next().unwrap_or(li);
                m.attachments.push((unescape(name.trim()), None));
            }
            rest = &rest[end..];
        }
    }
    Some(m)
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn kst_text(ms: i64) -> String {
    let (y, m, d, h, mi, s, _) = kst_civil(ms);
    format!("{y:04}-{m:02}-{d:02} {h:02}:{mi:02}:{s:02}")
}

fn people_text(list: &[Person]) -> String {
    list.iter()
        .map(|p| {
            if p.name.is_empty() || p.name == p.email {
                p.email.clone()
            } else {
                format!("{} <{}>", p.name, p.email)
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// 유저스크립트 `backupOne`과 같은 형식의 HTML(점검용 견본 만들기). 첨부 경로는 인코딩된 상대 경로.
pub fn to_backup_html(m: &BackupMail) -> String {
    let rows: Vec<(&str, String)> = [
        ("보낸 사람", people_text(&m.from)),
        ("받는 사람", people_text(&m.to)),
        ("참조", people_text(&m.cc)),
        ("받은 날짜", m.received.map(kst_text).unwrap_or_default()),
        ("보낸 날짜", m.sent.map(kst_text).unwrap_or_default()),
    ]
    .into_iter()
    .filter(|(_, v)| !v.is_empty())
    .collect();
    let hdr: String = rows
        .iter()
        .map(|(k, v)| {
            format!(
                "<tr><td class=\"k\">{}</td><td>{}</td></tr>",
                esc(k),
                esc(v)
            )
        })
        .collect();
    let atts = if m.attachments.is_empty() {
        String::new()
    } else {
        let items: String = m
            .attachments
            .iter()
            .map(|(name, rel)| match rel {
                Some(rel) => format!(
                    "<li><a href=\"{rel}\" target=\"_blank\">{}</a> <span class=\"sz\">(1 KB)</span></li>",
                    esc(name)
                ),
                None => format!(
                    "<li>{} <span class=\"sz\">(항목 첨부 - 자동 다운로드 불가)</span></li>",
                    esc(name)
                ),
            })
            .collect();
        format!(
            "<div class=\"atts\"><b>첨부파일 ({})</b><ul>{items}</ul></div>",
            m.attachments.len()
        )
    };
    format!(
        "<!DOCTYPE html>\n<html lang=\"ko\"><head><meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <title>{s}</title>\n<style>\n.mailbody img{{max-width:100%;height:auto}}\n</style></head><body>\n\
         <div class=\"hdr\"><h1>{s}</h1><table>{hdr}</table></div>\n<div class=\"mailbody\">\n{}\n</div>\n{atts}\n\
         </body></html>",
        m.body_html,
        s = esc(&m.subject)
    )
}

/// 유저스크립트와 같은 경로 조각 인코딩(`encodeURIComponent`).
pub fn enc_path(parts: &[&str]) -> String {
    parts
        .iter()
        .map(|p| encode_uri_component(p))
        .collect::<Vec<_>>()
        .join("/")
}

/// 백업 안 상대 경로(퍼센트 인코딩) → 실제 경로 조각.
pub fn decode_rel(rel: &str) -> String {
    percent_decode_str(rel).decode_utf8_lossy().into_owned()
}

pub fn mime_by_name(name: &str) -> &'static str {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "tif" | "tiff" => "image/tiff",
        "pdf" => "application/pdf",
        "txt" => "text/plain",
        "zip" => "application/zip",
        "eml" => "message/rfc822",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "hwp" => "application/x-hwp",
        _ => "application/octet-stream",
    }
}

/// SHA-256 앞 16바이트(32자리 16진수).
pub(crate) fn short_key(key: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(key.as_bytes())[..16]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// JavaScript `encodeURIComponent`와 같게: 영숫자와 `-_.!~*'()`만 그대로.
const URI_COMPONENT: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'!')
    .remove(b'~')
    .remove(b'*')
    .remove(b'\'')
    .remove(b'(')
    .remove(b')');

fn encode_uri_component(s: &str) -> String {
    percent_encoding::utf8_percent_encode(s, URI_COMPONENT).to_string()
}

/// HTML 백업 한 통을 EML로 복원한다.
/// - `files_dir`: HTML 옆 `{base}_files` 폴더 이름, `file_names`: 그 안 파일 이름들.
/// - `read(HTML 기준 상대 경로)`: 백업 안 파일 내용(없으면 `None`).
/// - `key`: 같은 백업을 다시 가져와도 같은 Message-ID가 되게 하는 값(HTML 내용 해시 등).
pub fn restore_eml(
    m: &BackupMail,
    files_dir: &str,
    file_names: &[String],
    mut read: impl FnMut(&str) -> Option<Vec<u8>>,
    key: &str,
) -> Vec<u8> {
    let mut html = m.body_html.clone();
    let mut inline: Vec<OriginalPart> = Vec::new();
    // 본문이 쓰는 `{base}_files/파일` 상대 경로(src·background·CSS url 모두) → cid:
    // 유저스크립트는 cid: 이미지와 내려받은 외부 이미지를 모두 encodeURIComponent 경로로 바꿔 두었다.
    // 그 경로에는 `( ) '`가 그대로 남아 경계를 짐작할 수 없으니, 폴더 안 실제 파일 이름으로 정확히 맞춘다.
    let mut names: Vec<&String> = file_names.iter().collect();
    names.sort_by_key(|n| std::cmp::Reverse(n.len()));
    for name in names {
        let rel = format!(
            "{}/{}",
            encode_uri_component(files_dir),
            encode_uri_component(name)
        );
        if !html.contains(&rel) {
            continue;
        }
        let Some(data) = read(&format!("{files_dir}/{name}")) else {
            continue;
        };
        let cid = format!("restored{}@eo.local", inline.len() + 1);
        html = html.replace(&rel, &format!("cid:{cid}"));
        inline.push(OriginalPart {
            mime: mime_by_name(name).to_owned(),
            name: name.clone(),
            data,
            content_id: Some(cid),
        });
    }
    let attachments = m
        .attachments
        .iter()
        .filter_map(|(name, rel)| {
            let data = read(&decode_rel(rel.as_deref()?))?;
            Some(OriginalPart {
                mime: mime_by_name(name).to_owned(),
                name: name.clone(),
                data,
                content_id: None,
            })
        })
        .collect();
    build_eml(&NewEml {
        from: m.from.clone(),
        to: m.to.clone(),
        cc: m.cc.clone(),
        subject: m.subject.clone(),
        date: m.sent.or(m.received),
        message_id: format!("<restored-{}@eo.local>", short_key(key)),
        html,
        inline,
        attachments,
        extra_headers: vec![("X-EO-Restored".into(), "html-backup".into())],
        ..NewEml::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 유저스크립트 backupOne이 만드는 형식 그대로.
    fn sample() -> String {
        concat!(
            "<!DOCTYPE html>\n<html lang=\"ko\"><head><meta charset=\"utf-8\">\n",
            "<title>주간 &amp; 보고</title>\n<style>body{}</style></head><body>\n",
            "<div class=\"hdr\"><h1>주간 &amp; 보고</h1><table>",
            "<tr><td class=\"k\">보낸 사람</td><td>김민수 &lt;minsu@x.com&gt;</td></tr>",
            "<tr><td class=\"k\">받는 사람</td><td>me@x.com; 이지영 &lt;jy@x.com&gt;</td></tr>",
            "<tr><td class=\"k\">받은 날짜</td><td>2026-10-15 16:34:05</td></tr>",
            "<tr><td class=\"k\">보낸 날짜</td><td>2026-10-15 16:34:00</td></tr>",
            "</table></div>\n<div class=\"mailbody\">\n",
            // 폴더 "20261015_163400_주간 (최종)_abc_files": encodeURIComponent는 ( )를 그대로 둔다.
            "<div><p>본문 <b>굵게</b></p>",
            "<img src=\"20261015_163400_%EC%A3%BC%EA%B0%84%20(%EC%B5%9C%EC%A2%85)_abc_files/image001%20(1).png\">",
            "<p style=\"background:url(20261015_163400_%EC%A3%BC%EA%B0%84%20(%EC%B5%9C%EC%A2%85)_abc_files/image001%20(1).png)\">x</p>",
            "<img src=\"https://cdn.example.com/a.png\"></div>\n",
            "</div>\n",
            "<div class=\"atts\"><b>첨부파일 (2)</b><ul>",
            "<li><a href=\"20261015_163400_%EC%A3%BC%EA%B0%84%20(%EC%B5%9C%EC%A2%85)_abc_files/%EB%B3%B4%EA%B3%A0%EC%84%9C.pdf\" target=\"_blank\">보고서.pdf</a> <span class=\"sz\">(13 KB)</span></li>",
            "<li>회의 초대 <span class=\"sz\">(항목 첨부 - 자동 다운로드 불가)</span></li>",
            "</ul></div>\n</body></html>"
        )
        .to_owned()
    }

    #[test]
    fn parses_userscript_backup_html() {
        let m = parse_backup_html(&sample()).unwrap();
        assert_eq!(m.subject, "주간 & 보고");
        assert_eq!(
            m.from,
            vec![Person {
                name: "김민수".into(),
                email: "minsu@x.com".into()
            }]
        );
        assert_eq!(m.to.len(), 2);
        assert_eq!(m.received, Some(1_792_049_645_000));
        assert_eq!(m.sent, Some(1_792_049_640_000));
        assert!(
            m.body_html.starts_with("<div><p>본문") && m.body_html.ends_with("</div>"),
            "{}",
            m.body_html
        );
        assert_eq!(m.attachments.len(), 2);
        assert_eq!(m.attachments[0].0, "보고서.pdf");
        assert_eq!(m.attachments[1], ("회의 초대".to_owned(), None));
        assert!(parse_backup_html("<html><body>그냥 HTML</body></html>").is_none());
    }

    #[test]
    fn restores_eml_with_inline_image_and_attachment() {
        let m = parse_backup_html(&sample()).unwrap();
        let png = STANDARD.decode("iVBORw0KGgo=").unwrap();
        let dir = "20261015_163400_주간 (최종)_abc_files";
        let files = [
            "image001 (1).png".to_owned(),
            "image001.png".to_owned(),
            "보고서.pdf".to_owned(),
        ];
        let raw = restore_eml(
            &m,
            dir,
            &files,
            |path| match path {
                "20261015_163400_주간 (최종)_abc_files/image001 (1).png" => Some(png.clone()),
                "20261015_163400_주간 (최종)_abc_files/image001.png" => {
                    Some(b"unused".to_vec())
                }
                "20261015_163400_주간 (최종)_abc_files/보고서.pdf" => Some(b"%PDF".to_vec()),
                _ => None,
            },
            "key-1",
        );
        let s = summarize(&raw).unwrap();
        assert_eq!(s.subject, "주간 & 보고");
        assert_eq!(s.from.unwrap().email, "minsu@x.com");
        assert_eq!(s.date, Some(1_792_049_640_000));
        assert!(s.has_attachments);
        let r = crate::render(
            &raw,
            &crate::RenderOptions {
                cid_base: "http://app-cid.localhost/1/".into(),
                allow_remote: false,
            },
        )
        .unwrap();
        assert!(
            r.html
                .contains("app-cid.localhost/1/restored1%40eo%2Elocal"),
            "{}",
            r.html
        );
        assert_eq!(r.inline_images.len(), 1, "같은 파일은 한 번만 붙인다");
        let body = MessageParser::default()
            .parse(&raw)
            .unwrap()
            .body_html(0)
            .unwrap()
            .into_owned();
        assert!(
            !body.contains("_files/"),
            "본문 경로가 모두 cid로 바뀜: {body}"
        );
        assert!(
            body.contains("https://cdn.example.com/a.png"),
            "못 받은 외부 이미지는 그대로"
        );
        let names: Vec<_> = r
            .attachments
            .iter()
            .filter(|a| !a.inline)
            .map(|a| a.name.as_str())
            .collect();
        assert_eq!(names, ["보고서.pdf"]);
        let again = restore_eml(&m, dir, &[], |_| None, "key-1");
        assert_eq!(
            summarize(&again).unwrap().message_id,
            s.message_id,
            "같은 키면 같은 Message-ID"
        );
        assert!(String::from_utf8_lossy(&raw).contains("X-EO-Restored: html-backup"));
    }

    #[test]
    fn writer_round_trips_through_parser() {
        let m = BackupMail {
            subject: "A&B <견본> \"따옴표\"".into(),
            from: vec![Person {
                name: "김민수".into(),
                email: "minsu@x.com".into(),
            }],
            to: vec![Person {
                name: String::new(),
                email: "me@x.com".into(),
            }],
            cc: vec![],
            received: Some(1_792_049_645_000),
            sent: Some(1_792_049_640_000),
            body_html: "<p>본문</p>\n<div>둘째 줄</div>".into(),
            attachments: vec![
                (
                    "a (1).pdf".into(),
                    Some(enc_path(&["x (최종)_files", "a (1).pdf"])),
                ),
                ("회의".into(), None),
            ],
        };
        assert_eq!(parse_backup_html(&to_backup_html(&m)).unwrap(), m);
        assert_eq!(
            enc_path(&["x (최종)_files", "a b.png"]),
            "x%20(%EC%B5%9C%EC%A2%85)_files/a%20b.png"
        );
    }

    #[test]
    fn kst_parsing() {
        assert_eq!(parse_kst("2026-10-15 16:34:00"), Some(1_792_049_640_000));
        assert_eq!(parse_kst("2026-10-15 16:34"), Some(1_792_049_640_000));
        assert_eq!(parse_kst("2026-13-01"), None);
        // 노츠 날짜: ZE9 = 한국 시간.
        assert_eq!(
            parse_notes_date("15-Oct-2026 16:34:00 ZE9"),
            Some(1_792_049_640_000)
        );
        assert_eq!(
            parse_notes_date("15-Oct-2026 16:34:00"),
            Some(1_792_049_640_000)
        );
        assert_eq!(
            parse_notes_date("15-Oct-2026 02:34:00 ZW5"),
            Some(1_792_049_640_000),
            "UTC−5 02:34 = 한국 16:34"
        );
        assert_eq!(
            parse_notes_date("Thu, 15 Oct 2026 16:34:00 +0900"),
            None,
            "노츠 형식만"
        );
        let eml = "From: a@x.com\r\nDate: 15-Oct-2026 16:34:00 ZE9\r\nSubject: s\r\n\r\nbody";
        assert_eq!(
            summarize(eml.as_bytes()).unwrap().date,
            Some(1_792_049_640_000)
        );
        assert_eq!(
            rfc2822(1_792_049_640_000),
            "Thu, 15 Oct 2026 16:34:00 +0900"
        );
    }

    #[test]
    fn address_values_from_domino_fields() {
        let p = |n: &str, e: &str| Person {
            name: n.into(),
            email: e.into(),
        };
        // HCL Domino 웹메일 입력란 값(EUC-KR 인코딩 이름, `;`로 이음).
        assert_eq!(
            parse_address_value("\"=?EUC-KR?B?sejDtrz2?=\" <U000002@inbound.example.com>"),
            [p("김철수", "U000002@inbound.example.com")]
        );
        assert_eq!(
            parse_address_value("user1@example.com; user2@example.com"),
            [p("", "user1@example.com"), p("", "user2@example.com")]
        );
        assert_eq!(
            parse_address_value("\"u000001@inbound.example.com\" <u000001@inbound.example.com>"),
            [p("", "u000001@inbound.example.com")],
            "이름이 주소와 같으면 이름 없음"
        );
        assert!(parse_address_value("  ").is_empty());
    }
}
