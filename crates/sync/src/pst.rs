//! Outlook PST(HCL Domino 로컬 보관 형식, 사용자 요청 2026-10-21) → EML.
//!
//! Microsoft의 `outlook-pst`로 IPM 하위 폴더를 따라가며 메일 항목만 고른다(일정·연락처·작업 폴더와 항목은 뺀다).
//! 메일은 MAPI 속성으로 다시 만든다: 제목·보낸 사람·받는 사람(받는 사람·참조·숨은 참조)·보낸·받은 날짜·
//! Message-ID·In-Reply-To·References·중요도, 본문은 HTML(PR_HTML, 코드 페이지대로) 없으면 일반 글(PR_BODY),
//! 첨부는 파일(본문 그림은 Content-ID로)과 첨부된 메일(message/rfc822, 그 안의 첨부는 빠진다).
//! RTF만 있는 메일은 Outlook이 늘 함께 두는 PR_BODY를 쓴다.

use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;

use eo_mime::backup::{NewEml, build_eml};
use eo_mime::compose::{OriginalPart, Person};
use outlook_pst::UnicodePstFile;
use outlook_pst::ltp::prop_context::PropertyValue;
use outlook_pst::ltp::table_context::TableContext;
use outlook_pst::messaging::attachment::{Attachment, AttachmentData, UnicodeAttachment};
use outlook_pst::messaging::folder::{Folder, UnicodeFolder};
use outlook_pst::messaging::message::{Message, UnicodeMessage};
use outlook_pst::messaging::store::{Store, UnicodeStore};
use outlook_pst::ndb::node_id::NodeId;

// MAPI 속성 번호.
pub(crate) const PR_IMPORTANCE: u16 = 0x0017;
pub(crate) const PR_MESSAGE_CLASS: u16 = 0x001A;
pub(crate) const PR_SUBJECT: u16 = 0x0037;
pub(crate) const PR_CLIENT_SUBMIT_TIME: u16 = 0x0039;
pub(crate) const PR_SENT_REPRESENTING_NAME: u16 = 0x0042;
pub(crate) const PR_SENT_REPRESENTING_EMAIL: u16 = 0x0065;
pub(crate) const PR_SENDER_NAME: u16 = 0x0C1A;
pub(crate) const PR_SENDER_EMAIL: u16 = 0x0C1F;
pub(crate) const PR_RECIPIENT_TYPE: u16 = 0x0C15;
pub(crate) const PR_DELIVERY_TIME: u16 = 0x0E06;
pub(crate) const PR_BODY: u16 = 0x1000;
pub(crate) const PR_HTML: u16 = 0x1013;
pub(crate) const PR_INTERNET_MESSAGE_ID: u16 = 0x1035;
pub(crate) const PR_REFERENCES: u16 = 0x1039;
pub(crate) const PR_IN_REPLY_TO: u16 = 0x1042;
pub(crate) const PR_DISPLAY_NAME: u16 = 0x3001;
pub(crate) const PR_EMAIL_ADDRESS: u16 = 0x3003;
pub(crate) const PR_CREATION_TIME: u16 = 0x3007;
pub(crate) const PR_CONTAINER_CLASS: u16 = 0x3613;
pub(crate) const PR_ATTACH_FILENAME: u16 = 0x3704;
pub(crate) const PR_ATTACH_LONG_FILENAME: u16 = 0x3707;
pub(crate) const PR_ATTACH_MIME_TAG: u16 = 0x370E;
pub(crate) const PR_ATTACH_CONTENT_ID: u16 = 0x3712;
pub(crate) const PR_SMTP_ADDRESS: u16 = 0x39FE;
pub(crate) const PR_INTERNET_CPID: u16 = 0x3FDE;
pub(crate) const PR_SENDER_SMTP: u16 = 0x5D01;
pub(crate) const PR_SENT_REPRESENTING_SMTP: u16 = 0x5D02;

/// 메일을 다시 만들 때 읽는 속성(PST 메일·첨부·받는 사람 표에서 옮겨 둘 것).
const USED: &[u16] = &[
    PR_IMPORTANCE,
    PR_MESSAGE_CLASS,
    PR_SUBJECT,
    PR_CLIENT_SUBMIT_TIME,
    PR_SENT_REPRESENTING_NAME,
    PR_SENT_REPRESENTING_EMAIL,
    PR_SENDER_NAME,
    PR_SENDER_EMAIL,
    PR_RECIPIENT_TYPE,
    PR_DELIVERY_TIME,
    PR_BODY,
    PR_HTML,
    PR_INTERNET_MESSAGE_ID,
    PR_REFERENCES,
    PR_IN_REPLY_TO,
    PR_DISPLAY_NAME,
    PR_EMAIL_ADDRESS,
    PR_CREATION_TIME,
    PR_ATTACH_FILENAME,
    PR_ATTACH_LONG_FILENAME,
    PR_ATTACH_MIME_TAG,
    PR_ATTACH_CONTENT_ID,
    PR_SMTP_ADDRESS,
    PR_INTERNET_CPID,
    PR_SENDER_SMTP,
    PR_SENT_REPRESENTING_SMTP,
];

/// MAPI 속성 값. PST(`outlook-pst`)와 OST 4K(`ost`)에서 읽은 것을 같은 모양으로 맞춰 EML을 만든다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Val {
    Text(String),
    Bin(Vec<u8>),
    Int(i32),
    /// unix ms
    Time(i64),
}

pub(crate) type Props = HashMap<u16, Val>;

/// String8: UTF-8로 안 되면 EUC-KR로 읽는다.
pub(crate) fn string8(b: &[u8]) -> String {
    let b = b.strip_suffix(&[0]).unwrap_or(b);
    match std::str::from_utf8(b) {
        Ok(t) => t.to_owned(),
        Err(_) => encoding_rs::EUC_KR.decode(b).0.into_owned(),
    }
}

/// FILETIME(1601년부터 100ns) → unix ms.
pub(crate) fn filetime_ms(t: i64) -> Option<i64> {
    (t > 0).then(|| t / 10_000 - 11_644_473_600_000)
}

fn val(v: &PropertyValue) -> Option<Val> {
    match v {
        PropertyValue::Unicode(s) => Some(Val::Text(
            String::from_utf16_lossy(s.buffer())
                .trim_end_matches('\0')
                .to_owned(),
        )),
        PropertyValue::String8(s) => Some(Val::Text(string8(s.buffer()))),
        PropertyValue::Binary(b) => Some(Val::Bin(b.buffer().to_vec())),
        PropertyValue::Integer32(i) => Some(Val::Int(*i)),
        PropertyValue::Integer16(i) => Some(Val::Int(i32::from(*i))),
        PropertyValue::Time(t) => filetime_ms(*t).map(Val::Time),
        _ => None,
    }
}

fn props_of(get: impl Fn(u16) -> Option<PropertyValue>) -> Props {
    USED.iter()
        .filter_map(|&id| Some((id, val(&get(id)?)?)))
        .collect()
}

/// 열어 둔 PST 하나.
pub struct PstFile {
    store: Rc<UnicodeStore>,
}

/// PST 안의 메일 하나(폴더 경로, 노드 번호).
#[derive(Debug, Clone)]
pub struct PstItem {
    pub folder: Vec<String>,
    pub node: u32,
}

fn io(e: impl std::fmt::Display) -> String {
    e.to_string()
}

impl PstFile {
    /// Unicode PST(Outlook 2003 이후)만. ANSI PST는 거절한다.
    pub fn open(path: &Path) -> Result<Self, String> {
        let file = UnicodePstFile::open(path)
            .map_err(|e| format!("Outlook 2003 이후 형식(Unicode) PST가 아닙니다: {e}"))?;
        let store = UnicodeStore::read(Rc::new(file)).map_err(io)?;
        Ok(Self { store })
    }

    /// 메일 폴더의 메일 항목들(폴더 경로는 IPM 하위 폴더 기준).
    pub fn items(&self) -> Result<Vec<PstItem>, String> {
        let props = self.store.properties();
        let root = props.ipm_sub_tree_entry_id().map_err(io)?;
        let root = UnicodeFolder::read(self.store.clone(), &root).map_err(io)?;
        let mut out = Vec::new();
        let mut stack: Vec<(Rc<UnicodeFolder>, Vec<String>)> = vec![(root, Vec::new())];
        while let Some((folder, path)) = stack.pop() {
            if let Some(table) = folder.hierarchy_table() {
                for row in table.rows_matrix() {
                    let node = NodeId::from(u32::from(row.id()));
                    let Ok(entry) = props.make_entry_id(node) else {
                        continue;
                    };
                    let Ok(sub) = UnicodeFolder::read(self.store.clone(), &entry) else {
                        continue;
                    };
                    let fp = sub.properties();
                    let class = match fp.get(PR_CONTAINER_CLASS).and_then(val) {
                        Some(Val::Text(c)) => c,
                        _ => String::new(),
                    };
                    if !is_mail_folder(&class) {
                        continue;
                    }
                    let name = fp.display_name().unwrap_or_else(|_| "폴더".into());
                    let mut p = path.clone();
                    p.push(name.trim().to_owned());
                    stack.push((sub, p));
                }
            }
            if path.is_empty() {
                continue;
            }
            if let Some(table) = folder.contents_table() {
                for row in table.rows_matrix() {
                    out.push(PstItem {
                        folder: path.clone(),
                        node: u32::from(row.id()),
                    });
                }
            }
        }
        Ok(out)
    }

    /// 메일 하나를 EML로. 메일 항목이 아니면(일정·연락처 등) `Ok(None)`.
    pub fn eml(&self, node: u32) -> Result<Option<Vec<u8>>, String> {
        let entry = self
            .store
            .properties()
            .make_entry_id(NodeId::from(node))
            .map_err(io)?;
        let msg = UnicodeMessage::read(self.store.clone(), &entry, None).map_err(io)?;
        let mp = msg.properties();
        let props = props_of(|id| mp.get(id).cloned());
        if !is_mail_class(&text(&props, PR_MESSAGE_CLASS).unwrap_or_default()) {
            return Ok(None);
        }
        let mut parts = Parts::default();
        if let Some(table) = msg.attachment_table() {
            for row in table.rows_matrix() {
                let sub = NodeId::from(u32::from(row.id()));
                let Ok(att) = UnicodeAttachment::read(msg.clone(), sub, None) else {
                    continue;
                };
                let ap = att.properties();
                let ap = props_of(|id| ap.get(id).cloned());
                match att.data() {
                    Some(AttachmentData::Binary(b)) => parts.file(&ap, b.buffer().to_vec()),
                    Some(AttachmentData::Message(m)) => {
                        let mp = m.properties();
                        let inner = props_of(|id| mp.get(id).cloned());
                        let raw = build(&inner, &recipients(m.recipient_table()), Parts::default());
                        parts.message(&inner, raw);
                    }
                    None => {}
                }
            }
        }
        Ok(Some(build(
            &props,
            &recipients(msg.recipient_table()),
            parts,
        )))
    }
}

/// 다시 만든 메일의 본문 그림(Content-ID 있는 첨부)과 첨부.
#[derive(Default)]
pub(crate) struct Parts {
    inline: Vec<OriginalPart>,
    attachments: Vec<OriginalPart>,
}

impl Parts {
    /// 파일 첨부(본문 그림이면 Content-ID로).
    pub(crate) fn file(&mut self, ap: &Props, data: Vec<u8>) {
        let name = text(ap, PR_ATTACH_LONG_FILENAME)
            .or_else(|| text(ap, PR_ATTACH_FILENAME))
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| format!("첨부{}", self.attachments.len() + self.inline.len() + 1));
        let cid = text(ap, PR_ATTACH_CONTENT_ID).filter(|c| !c.trim().is_empty());
        let mime = text(ap, PR_ATTACH_MIME_TAG)
            .filter(|m| m.contains('/'))
            .unwrap_or_else(|| outlook_mime_by_name(&name));
        let part = OriginalPart {
            name,
            mime,
            data,
            content_id: cid
                .clone()
                .map(|c| c.trim().trim_matches(['<', '>']).to_owned()),
        };
        if cid.is_some() {
            self.inline.push(part)
        } else {
            self.attachments.push(part)
        }
    }

    /// 첨부된 메일(message/rfc822, 이름은 제목).
    pub(crate) fn message(&mut self, inner: &Props, raw: Vec<u8>) {
        let subject = subject_text(inner).unwrap_or_default();
        let base = if subject.trim().is_empty() {
            "첨부된 메일".to_owned()
        } else {
            subject
        };
        self.attachments.push(OriginalPart {
            name: format!(
                "{}.eml",
                base.replace(['\\', '/', ':', '*', '?', '"', '<', '>', '|'], "_")
            ),
            mime: "message/rfc822".into(),
            data: raw,
            content_id: None,
        });
    }
}

fn outlook_mime_by_name(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    let ext = lower.rsplit_once('.').map_or("", |x| x.1);
    match ext {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        "pdf" => "application/pdf",
        "txt" => "text/plain",
        "htm" | "html" => "text/html",
        "zip" => "application/zip",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "hwp" => "application/x-hwp",
        "eml" => "message/rfc822",
        _ => "application/octet-stream",
    }
    .to_owned()
}

/// 메일 항목: IPM.Note…, 회의 요청·응답(IPM.Schedule.Meeting…), 배달 보고(REPORT.…). 비어 있으면 메일로 본다.
pub(crate) fn is_mail_class(class: &str) -> bool {
    let c = class.to_ascii_lowercase();
    c.is_empty()
        || c.starts_with("ipm.note")
        || c.starts_with("ipm.schedule.meeting")
        || c.starts_with("report.")
        || c == "ipm"
}

/// 메일 폴더(클래스가 없거나 IPF.Note…). 일정·연락처·작업·메모·일지 폴더는 뺀다.
pub(crate) fn is_mail_folder(class: &str) -> bool {
    class.is_empty() || class.to_ascii_lowercase().starts_with("ipf.note")
}

/// 문자열 속성.
pub(crate) fn text(p: &Props, id: u16) -> Option<String> {
    match p.get(&id)? {
        Val::Text(s) => Some(s.clone()),
        _ => None,
    }
}

/// 제목: 앞의 정규화 표시(0x01 + 길이)를 뺀다.
pub(crate) fn subject_text(p: &Props) -> Option<String> {
    let t = text(p, PR_SUBJECT)?;
    let mut ch = t.chars();
    Some(if t.starts_with('\u{1}') {
        ch.next();
        ch.next();
        ch.as_str().to_owned()
    } else {
        t
    })
}

fn time_ms(p: &Props, id: u16) -> Option<i64> {
    match p.get(&id)? {
        Val::Time(t) => Some(*t),
        _ => None,
    }
}

pub(crate) fn int(p: &Props, id: u16) -> Option<i32> {
    match p.get(&id)? {
        Val::Int(i) => Some(*i),
        _ => None,
    }
}

/// HTML 본문: 바이트를 PR_INTERNET_CPID 코드 페이지로 읽는다.
fn html_text(p: &Props) -> Option<String> {
    match p.get(&PR_HTML)? {
        Val::Bin(bytes) => {
            let enc = match int(p, PR_INTERNET_CPID) {
                Some(65001) | None => encoding_rs::UTF_8,
                Some(949) | Some(51949) => encoding_rs::EUC_KR,
                Some(932) => encoding_rs::SHIFT_JIS,
                Some(936) => encoding_rs::GBK,
                Some(950) => encoding_rs::BIG5,
                Some(50220) | Some(50221) | Some(50222) => encoding_rs::ISO_2022_JP,
                Some(1252) | Some(28591) | Some(20127) => encoding_rs::WINDOWS_1252,
                Some(_) => encoding_rs::UTF_8,
            };
            let (t, _, bad) = enc.decode(bytes);
            Some(if bad && enc == encoding_rs::UTF_8 {
                encoding_rs::EUC_KR.decode(bytes).0.into_owned()
            } else {
                t.into_owned()
            })
        }
        Val::Text(t) => Some(t.clone()),
        _ => None,
    }
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// 사람: 이름과 주소(Exchange 주소면 SMTP 주소를, 없으면 이름만).
fn person(name: Option<String>, addr: Option<String>, smtp: Option<String>) -> Option<Person> {
    let email = smtp
        .filter(|s| s.contains('@'))
        .or_else(|| addr.filter(|a| a.contains('@')))
        .unwrap_or_default();
    let name = name.unwrap_or_default();
    (!name.is_empty() || !email.is_empty()).then(|| Person {
        name: if name == email { String::new() } else { name },
        email,
    })
}

/// PST 받는 사람 표의 행들.
fn recipients(table: Option<&Rc<dyn TableContext>>) -> Vec<Props> {
    let Some(table) = table else {
        return Vec::new();
    };
    let context = table.context();
    let mut out = Vec::new();
    for row in table.rows_matrix() {
        let Ok(cols) = row.columns(context) else {
            continue;
        };
        out.push(props_of(|id| {
            context
                .columns()
                .iter()
                .zip(cols.iter())
                .find(|(c, _)| c.prop_id() == id)
                .and_then(|(c, v)| table.read_column(v.as_ref()?, c.prop_type()).ok())
        }));
    }
    out
}

/// MAPI 속성으로 EML을 다시 만든다. `recips`: 받는 사람 표의 행들.
pub(crate) fn build(p: &Props, recips: &[Props], parts: Parts) -> Vec<u8> {
    let s = |id: u16| text(p, id).filter(|t| !t.trim().is_empty());
    let from = person(
        s(PR_SENT_REPRESENTING_NAME),
        s(PR_SENT_REPRESENTING_EMAIL),
        s(PR_SENT_REPRESENTING_SMTP),
    )
    .or_else(|| person(s(PR_SENDER_NAME), s(PR_SENDER_EMAIL), s(PR_SENDER_SMTP)));
    let (mut to, mut cc, mut bcc) = (Vec::new(), Vec::new(), Vec::new());
    for r in recips {
        let Some(who) = person(
            text(r, PR_DISPLAY_NAME),
            text(r, PR_EMAIL_ADDRESS),
            text(r, PR_SMTP_ADDRESS),
        ) else {
            continue;
        };
        match int(r, PR_RECIPIENT_TYPE).unwrap_or(1) & 0xF {
            2 => cc.push(who),
            3 => bcc.push(who),
            _ => to.push(who),
        }
    }
    let date = time_ms(p, PR_DELIVERY_TIME)
        .or_else(|| time_ms(p, PR_CLIENT_SUBMIT_TIME))
        .or_else(|| time_ms(p, PR_CREATION_TIME));
    let html = html_text(p).unwrap_or_default();
    let html = if html.trim().is_empty() {
        let body = text(p, PR_BODY).unwrap_or_default();
        format!(
            "<div style=\"white-space:pre-wrap\">{}</div>",
            escape(&body)
        )
    } else {
        html
    };
    let mut extra = Vec::new();
    if let Some(v) = s(PR_IN_REPLY_TO) {
        extra.push(("In-Reply-To".to_owned(), v));
    }
    if let Some(v) = s(PR_REFERENCES) {
        extra.push((
            "References".to_owned(),
            v.split_whitespace().collect::<Vec<_>>().join(" "),
        ));
    }
    match int(p, PR_IMPORTANCE) {
        Some(2) => extra.push(("Importance".to_owned(), "high".to_owned())),
        Some(0) => extra.push(("Importance".to_owned(), "low".to_owned())),
        _ => {}
    }
    extra.push(("X-EO-Restored".to_owned(), "pst".to_owned()));
    let subject = subject_text(p).unwrap_or_default();
    let message_id = s(PR_INTERNET_MESSAGE_ID).unwrap_or_else(|| {
        // 없으면 내용으로 만든다(다시 가져올 때 중복으로 알아보게).
        use sha2::Digest;
        let mut h = sha2::Sha256::new();
        h.update(subject.as_bytes());
        h.update(date.unwrap_or(0).to_le_bytes());
        h.update(
            from.as_ref()
                .map(|f| f.email.clone())
                .unwrap_or_default()
                .as_bytes(),
        );
        let hex: String = h
            .finalize()
            .iter()
            .take(12)
            .map(|b| format!("{b:02x}"))
            .collect();
        format!("<pst-{hex}@eo.local>")
    });
    build_eml(&NewEml {
        from: from.into_iter().collect(),
        to,
        cc,
        bcc,
        reply_to: Vec::new(),
        subject,
        date,
        message_id,
        html,
        inline: parts.inline,
        attachments: parts.attachments,
        extra_headers: extra,
    })
}
