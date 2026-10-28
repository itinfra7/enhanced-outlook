//! EML 해석과 생성: 본문 추출, 한국어 문자셋 처리, 인용문·회신 생성, `cid:` 매핑.
//!
//! 계획서 5.8, 9.2, 9.3. 검색 색인용 추출([`extract`])과 읽기 창 렌더링([`render`])이 있다.
//! 문자셋은 mail-parser가 `ks_c_5601-1987`·`euc-kr`·`cp949` 등을 처리한다(`full_encoding`).

pub mod backup;
pub mod compose;
pub mod domino;
mod render;

pub use render::{
    AttachmentInfo, ImageReport, InlineImage, RenderOptions, Rendered, attachment, cid_link_parts,
    find_cid, image_report, render, text_document,
};

use mail_parser::{Address, MessageParser, MimeHeaders};

/// 검색 색인·목록 보강에 쓰는 원문 내용.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Extracted {
    pub subject: String,
    /// "이름 <주소>; ..." 형식.
    pub from: String,
    pub to: String,
    pub cc: String,
    /// 내가 보낸 메일의 원문에만 있다.
    pub bcc: String,
    /// 텍스트 본문(HTML뿐이면 텍스트로 바꾼 것).
    pub body_text: String,
    pub attachment_names: Vec<String>,
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

/// 원문(EML)을 해석해 검색에 쓸 내용을 뽑는다. 해석할 수 없으면 `None`.
pub fn extract(raw: &[u8]) -> Option<Extracted> {
    let msg = MessageParser::default().parse(raw)?;
    let mut body = String::new();
    for i in 0..msg.text_body_count() {
        if let Some(t) = msg.body_text(i) {
            if !body.is_empty() {
                body.push('\n');
            }
            body.push_str(&t);
        }
    }
    let attachment_names = msg
        .attachments()
        .filter_map(|p| {
            p.attachment_name().map(str::to_owned).or_else(|| {
                // 첨부된 메일은 그 제목을 이름으로 쓴다.
                p.message()
                    .and_then(|m| m.subject())
                    .map(|s| format!("{s}.eml"))
            })
        })
        .collect();
    Some(Extracted {
        subject: msg.subject().unwrap_or_default().to_owned(),
        from: addresses(msg.from()),
        to: addresses(msg.to()),
        cc: addresses(msg.cc()),
        bcc: addresses(msg.bcc()),
        body_text: body,
        attachment_names,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn korean_charset_html_body_attachments_and_bcc() {
        // 제목 "보고서"(ks_c_5601-1987 = EUC-KR, base64), HTML 본문, 첨부 1개, 숨은 참조.
        let eml = concat!(
            "From: =?UTF-8?B?6rmA66+87IiY?= <minsu.kim@example.com>\r\n",
            "To: a@x.com, =?UTF-8?B?7J207KeA7JiB?= <jiyoung.lee@partner.co.kr>\r\n",
            "Cc: c@x.com\r\n",
            "Bcc: audit.bcc@secure-partner.co.kr\r\n",
            "Subject: =?ks_c_5601-1987?B?uriw7byt?=\r\n",
            "MIME-Version: 1.0\r\n",
            "Content-Type: multipart/mixed; boundary=\"b\"\r\n",
            "\r\n",
            "--b\r\n",
            "Content-Type: text/html; charset=UTF-8\r\n",
            "\r\n",
            "<html><body><p>월간 <b>실적</b>입니다</p><script>x()</script></body></html>\r\n",
            "--b\r\n",
            "Content-Type: application/pdf\r\n",
            "Content-Disposition: attachment; filename=\"=?UTF-8?B?6rKs7KCB7IScLnBkZg==?=\"\r\n",
            "Content-Transfer-Encoding: base64\r\n",
            "\r\n",
            "JVBERg==\r\n",
            "--b--\r\n"
        );
        let x = extract(eml.as_bytes()).unwrap();
        assert_eq!(x.subject, "보고서");
        assert_eq!(x.from, "김민수 <minsu.kim@example.com>");
        assert_eq!(x.to, "a@x.com; 이지영 <jiyoung.lee@partner.co.kr>");
        assert_eq!(x.cc, "c@x.com");
        assert_eq!(x.bcc, "audit.bcc@secure-partner.co.kr");
        assert!(
            x.body_text.contains("월간") && x.body_text.contains("실적"),
            "{}",
            x.body_text
        );
        assert!(!x.body_text.contains("<b>"));
        assert_eq!(x.attachment_names, ["견적서.pdf"]);
    }

    #[test]
    fn garbage_is_tolerated() {
        // 머리글만 있어도 해석되고, 빈 입력은 None이어도 된다.
        assert!(extract(b"Subject: x\r\n\r\n").is_some());
        let _ = extract(b"");
    }
}
