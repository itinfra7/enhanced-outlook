//! `owa-cli store synth`: 화면 성능 측정용 합성 저장소(계획서 10장 4단계 "합성 10만 통 DB").
//!
//! 받은 편지함 하나에 `inbox`통(10만 행 목록 스크롤용), 나머지 폴더에 `others`통.
//! 내용은 `eo_search::synth`(3단계 말뭉치)로 만들고, 최근 `eml`통만 원문(HTML·첨부·인라인 이미지)을 둔다.
//! 전부 색인한다. 서버와는 통신하지 않고, 이미 DB가 있는 위치에는 만들지 않는다.

use std::path::Path;
use std::time::Instant;

use anyhow::{Result, bail};
use eo_search::{MailDoc, SearchIndex, synth};
use eo_store::{MessageProps, ServerFolder, Store};

use crate::util::fmt_bytes;

const ROOT: &str = "SYN-ROOT";

/// (서버 ID, 이름, 부모, 잘 알려진 이름)
const FOLDERS: &[(&str, &str, &str, Option<&str>)] = &[
    ("SYN-INBOX", "받은 편지함", ROOT, Some("inbox")),
    ("SYN-DRAFTS", "임시 보관함", ROOT, Some("drafts")),
    ("SYN-SENT", "보낸 편지함", ROOT, Some("sentitems")),
    ("SYN-DELETED", "지운 편지함", ROOT, Some("deleteditems")),
    ("SYN-JUNK", "정크 메일", ROOT, Some("junkemail")),
    ("SYN-C0", "프로젝트", "SYN-INBOX", None),
    ("SYN-C1", "회의", "SYN-INBOX", None),
    ("SYN-C2", "고객", "SYN-INBOX", None),
    ("SYN-C3", "보고", ROOT, None),
    ("SYN-C4", "공지", ROOT, None),
    ("SYN-C5", "거래처", ROOT, None),
    ("SYN-C6", "교육", ROOT, None),
    ("SYN-C7", "보안", ROOT, None),
    ("SYN-C8", "장애", "SYN-C7", None),
    ("SYN-C9", "기타", ROOT, None),
];

const WELL_KNOWN_ORDER: &[(&str, i64)] = &[
    ("inbox", 0),
    ("drafts", 1),
    ("sentitems", 2),
    ("deleteditems", 3),
    ("junkemail", 4),
];

/// 1×1 PNG(인라인 이미지 확인용).
const PNG_1X1: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";

fn split_person(s: &str) -> (Option<String>, Option<String>) {
    match (s.find('<'), s.rfind('>')) {
        (Some(a), Some(b)) if a < b => (
            Some(s[..a].trim().to_owned()),
            Some(s[a + 1..b].trim().to_owned()),
        ),
        _ => (None, Some(s.trim().to_owned())),
    }
}

/// RFC 2047 B 인코딩(UTF-8).
fn enc_word(s: &str) -> String {
    use base64::Engine as _;
    if s.is_ascii() {
        return s.to_owned();
    }
    format!(
        "=?UTF-8?B?{}?=",
        base64::engine::general_purpose::STANDARD.encode(s)
    )
}

fn enc_addrs(list: &str) -> String {
    list.split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|p| match split_person(p) {
            (Some(n), Some(a)) if !n.is_empty() => format!("{} <{a}>", enc_word(&n)),
            (_, Some(a)) => a,
            _ => String::new(),
        })
        .collect::<Vec<_>>()
        .join(",\r\n ")
}

fn rfc2822(ms: i64) -> String {
    const DAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
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
    format!(
        "{}, {d} {} {y} {:02}:{:02}:{:02} +0900",
        DAYS[days.rem_euclid(7) as usize],
        MONTHS[(m - 1) as usize],
        rem / 3600,
        (rem / 60) % 60,
        rem % 60
    )
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// 결정적 의사 난수 바이트(첨부 내용).
fn fake_bytes(seed: u64, n: usize) -> Vec<u8> {
    let mut x = seed | 1;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect()
}

fn b64_lines(bytes: &[u8]) -> String {
    use base64::Engine as _;
    let s = base64::engine::general_purpose::STANDARD.encode(bytes);
    s.as_bytes()
        .chunks(76)
        .map(|c| String::from_utf8_lossy(c).into_owned())
        .collect::<Vec<_>>()
        .join("\r\n")
}

/// 합성 메일 원문: multipart/mixed(related(alternative(text, html), 인라인 이미지), 첨부들).
fn eml(i: u64, d: &MailDoc) -> Vec<u8> {
    use base64::Engine as _;
    let b64 = |s: &str| b64_lines(s.as_bytes());
    let inline = i.is_multiple_of(7);
    let remote = i.is_multiple_of(5);
    let mut html = String::from(
        "<html><head><style>p{margin:0 0 8px}td{border:1px solid #ccc;padding:4px}</style></head><body>",
    );
    for para in d.body.split('\n') {
        html.push_str(&format!("<p>{}</p>", html_escape(para)));
    }
    if i.is_multiple_of(3) {
        html.push_str("<table><tr><td>항목</td><td>수량</td><td>금액</td></tr>");
        for k in 0..8 {
            html.push_str(&format!(
                "<tr><td>{}</td><td>{}</td><td>{}원</td></tr>",
                synth::WORDS[(i as usize + k) % synth::WORDS.len()],
                k + 1,
                (k + 1) * 125_000
            ));
        }
        html.push_str("</table>");
    }
    if inline {
        html.push_str("<p><img src=\"cid:sig@synth.local\" alt=\"서명\"></p>");
    }
    if remote {
        html.push_str("<p><img src=\"https://www.example.org/banner.png\" alt=\"배너\"></p>");
    }
    html.push_str("</body></html>");

    let mut out = String::new();
    out.push_str(&format!("From: {}\r\n", enc_addrs(&d.from)));
    out.push_str(&format!("To: {}\r\n", enc_addrs(&d.to)));
    if !d.cc.is_empty() {
        out.push_str(&format!("Cc: {}\r\n", enc_addrs(&d.cc)));
    }
    if !d.bcc.is_empty() {
        out.push_str(&format!("Bcc: {}\r\n", enc_addrs(&d.bcc)));
    }
    out.push_str(&format!("Subject: {}\r\n", enc_word(&d.subject)));
    out.push_str(&format!("Date: {}\r\n", rfc2822(d.date)));
    out.push_str(&format!("Message-ID: <syn{i}@synth.local>\r\n"));
    out.push_str("MIME-Version: 1.0\r\n");
    out.push_str("Content-Type: multipart/mixed; boundary=\"MIX\"\r\n\r\n");
    out.push_str("--MIX\r\nContent-Type: multipart/related; boundary=\"REL\"\r\n\r\n");
    out.push_str("--REL\r\nContent-Type: multipart/alternative; boundary=\"ALT\"\r\n\r\n");
    out.push_str(&format!(
        "--ALT\r\nContent-Type: text/plain; charset=UTF-8\r\nContent-Transfer-Encoding: base64\r\n\r\n{}\r\n",
        b64(&d.body)
    ));
    out.push_str(&format!(
        "--ALT\r\nContent-Type: text/html; charset=UTF-8\r\nContent-Transfer-Encoding: base64\r\n\r\n{}\r\n--ALT--\r\n",
        b64(&html)
    ));
    if inline {
        let png = base64::engine::general_purpose::STANDARD
            .decode(PNG_1X1)
            .unwrap_or_default();
        out.push_str(&format!(
            "--REL\r\nContent-Type: image/png\r\nContent-ID: <sig@synth.local>\r\n\
             Content-Disposition: inline; filename=\"sig.png\"\r\nContent-Transfer-Encoding: base64\r\n\r\n{}\r\n",
            b64_lines(&png)
        ));
    }
    out.push_str("--REL--\r\n");
    for (k, name) in d.attach.split(' ').filter(|s| !s.is_empty()).enumerate() {
        // 20~120KB
        let size = 20_000 + ((i as usize * 31 + k * 17) % 100) * 1_000;
        out.push_str(&format!(
            "--MIX\r\nContent-Type: application/octet-stream; name=\"{n}\"\r\n\
             Content-Disposition: attachment; filename=\"{n}\"\r\nContent-Transfer-Encoding: base64\r\n\r\n{}\r\n",
            b64_lines(&fake_bytes(i * 10 + k as u64, size)),
            n = enc_word(name)
        ));
    }
    out.push_str("--MIX--\r\n");
    out.into_bytes()
}

pub fn synth(root: &Path, inbox: usize, others: usize, eml_count: usize) -> Result<()> {
    if root.join("db.sqlite").exists() {
        bail!(
            "{}에 이미 DB가 있습니다. 비어 있는 새 위치를 --root로 지정하세요(지우지 않습니다)",
            root.display()
        );
    }
    let started = Instant::now();
    let total = inbox + others;
    let n = total as u64;
    let store = Store::open(root)?;
    let (search, _) = SearchIndex::open_checked(&root.join("index"))?;

    let ids: std::collections::HashMap<&str, i64> = store.transaction(|tx| {
        let mut ids = std::collections::HashMap::new();
        for (sid, name, parent, wk) in FOLDERS {
            let id = tx.upsert_folder(&ServerFolder {
                server_id: (*sid).to_owned(),
                server_parent_id: Some((*parent).to_owned()),
                name: (*name).to_owned(),
                folder_class: Some("IPF.Note".into()),
            })?;
            if let Some(wk) = wk {
                let order = WELL_KNOWN_ORDER
                    .iter()
                    .find(|(n, _)| n == wk)
                    .map_or(100, |(_, o)| *o);
                tx.set_well_known(sid, wk, order)?;
            }
            ids.insert(*sid, id);
        }
        tx.rebuild_tree(ROOT)?;
        Ok(ids)
    })?;
    let folder_of = |i: usize| -> i64 {
        let step = if others == 0 {
            usize::MAX
        } else {
            total.div_ceil(others)
        };
        if !i.is_multiple_of(step) {
            return ids["SYN-INBOX"];
        }
        let k = i / step;
        match k % 10 {
            0..=4 => ids["SYN-SENT"],
            5 => ids["SYN-DELETED"],
            6 if k % 20 == 6 => ids["SYN-DRAFTS"],
            6 => ids["SYN-JUNK"],
            _ => ids[FOLDERS[5 + (k / 10) % 10].0],
        }
    };

    // 1) 목록 행(한 트랜잭션)
    let mut docs = Vec::with_capacity(total);
    let mut msg_ids = Vec::with_capacity(total);
    store.transaction(|tx| {
        for i in 0..total {
            let mut d = synth::doc(i as u64, n);
            let folder = folder_of(i);
            let (from_name, from_addr) = split_person(&d.from);
            let attach_n = d.attach.split(' ').filter(|s| !s.is_empty()).count() as i64;
            let props = MessageProps {
                internet_msg_id: Some(format!("<syn{i}@synth.local>")),
                conversation_id: None,
                subject: Some(d.subject.clone()),
                from_name,
                from_addr,
                to_list: Some(d.to.clone()),
                cc_list: Some(d.cc.clone()),
                date_received: d.date,
                date_sent: Some(d.date - 60_000),
                size: Some(d.body.len() as i64 * 3 + 2_000 + attach_n * 70_000),
                has_attachments: attach_n > 0,
                is_read: !i.is_multiple_of(9),
                flag: i64::from(i.is_multiple_of(53)),
                importance: if i.is_multiple_of(41) { 2 } else { 1 },
                is_draft: false,
                item_class: Some("IPM.Note".into()),
                preview: Some(d.body.chars().take(120).collect()),
            };
            let id = tx.insert_message(folder, &format!("SYN{i}"), None, &props)?;
            d.id = id as u64;
            d.folder = folder as u64;
            docs.push(d);
            msg_ids.push(id);
        }
        Ok(())
    })?;
    println!(
        "[1] 목록 행 {total}통 (받은 편지함 {inbox}, 나머지 {others}) {:.1}초",
        started.elapsed().as_secs_f64()
    );

    // 2) 최근 원문
    let t = Instant::now();
    let mut eml_bytes = 0u64;
    let mut with_eml = Vec::new();
    for i in total.saturating_sub(eml_count)..total {
        let raw = eml(i as u64, &docs[i]);
        eml_bytes += raw.len() as u64;
        store.attach_eml(msg_ids[i], &format!("SYN{i}"), &raw)?;
        with_eml.push((msg_ids[i], None, true));
    }
    println!(
        "[2] 원문 {}통 {} {:.1}초",
        with_eml.len(),
        fmt_bytes(eml_bytes),
        t.elapsed().as_secs_f64()
    );

    // 3) 색인(전부). 모두 색인 완료로 표시해 앱이 다시 색인하지 않게 한다(원문 없는 메일은 목록 속성만, 2).
    let t = Instant::now();
    for chunk in docs.chunks(10_000) {
        search.add(chunk)?;
        search.commit()?;
    }
    let done: std::collections::HashSet<i64> = with_eml.iter().map(|(id, _, _)| *id).collect();
    let mut marks = with_eml;
    marks.extend(
        msg_ids
            .iter()
            .filter(|id| !done.contains(id))
            .map(|id| (*id, None, false)),
    );
    store.mark_indexed(&marks)?;
    println!(
        "[3] 색인 {}통 {:.1}초",
        search.num_docs(),
        t.elapsed().as_secs_f64()
    );

    // 4) 서버에서 지워진 것처럼 보관 상태 몇 통(지운 편지함)
    let deleted = ids["SYN-DELETED"];
    let archived: Vec<i64> = docs
        .iter()
        .filter(|d| d.folder == deleted as u64)
        .step_by(20)
        .map(|d| d.id as i64)
        .collect();
    store.transaction(|tx| {
        for id in &archived {
            tx.archive_message(*id, docs.last().map_or(0, |d| d.date))?;
        }
        Ok(())
    })?;
    println!(
        "[4] 보관 상태 {}통. 합계 {:.1}초. 위치 {}",
        archived.len(),
        started.elapsed().as_secs_f64(),
        root.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_eml_renders_with_inline_and_attachments() {
        let d = synth::doc(35, 100);
        let raw = eml(35, &d);
        let x = eo_mime::extract(&raw).unwrap();
        assert_eq!(x.subject, d.subject);
        assert!(x.body_text.contains("안녕하세요"));
        let r = eo_mime::render(
            &raw,
            &eo_mime::RenderOptions {
                cid_base: "http://app-cid.localhost/1/".into(),
                allow_remote: false,
            },
        )
        .unwrap();
        // 35는 7과 5의 배수: 인라인 이미지와 외부 이미지가 모두 있다.
        assert!(
            r.html.contains("app-cid.localhost/1/sig%40synth%2Elocal"),
            "{}",
            r.html
        );
        assert!(r.has_remote_images);
        assert_eq!(
            r.attachments.iter().filter(|a| !a.inline).count(),
            d.attach.split(' ').filter(|s| !s.is_empty()).count()
        );
        assert!(rfc2822(1_792_040_798_000).starts_with("Thu, 15 Oct 2026 14:06:38"));
    }
}
