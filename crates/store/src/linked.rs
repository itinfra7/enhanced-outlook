//! HCL Domino 메일의 연결문서(1.2.0, 마이그레이션 17). 메일을 열 때마다 새로 가져와 두고, 오프라인이면 마지막
//! 것을 보인다. 메일 행을 지우면 함께 지워진다.

use rusqlite::{OptionalExtension, params};

use crate::{Result, Store};

/// 저장한 연결문서.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkedDoc {
    pub url: String,
    /// 메일 본문 끝에 붙일 완성 조각.
    pub html: String,
    /// 가져온 때(유닉스 초).
    pub fetched_at: i64,
}

impl Store {
    pub fn linked_doc(&self, message_id: i64) -> Result<Option<LinkedDoc>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT url, html, fetched_at FROM linked_docs WHERE message_id = ?1",
                [message_id],
                |r| {
                    Ok(LinkedDoc {
                        url: r.get(0)?,
                        html: r.get(1)?,
                        fetched_at: r.get(2)?,
                    })
                },
            )
            .optional()?)
    }

    /// 새로 가져온 것을 둔다. 내용(`url`·`html`)이 바뀌었으면 참.
    pub fn put_linked_doc(&self, message_id: i64, doc: &LinkedDoc) -> Result<bool> {
        let conn = self.conn();
        let old: Option<(String, String)> = conn
            .query_row(
                "SELECT url, html FROM linked_docs WHERE message_id = ?1",
                [message_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        conn.execute(
            "INSERT INTO linked_docs(message_id, url, html, fetched_at) VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT(message_id) DO UPDATE SET url = excluded.url, html = excluded.html, \
               fetched_at = excluded.fetched_at",
            params![message_id, doc.url, doc.html, doc.fetched_at],
        )?;
        Ok(old.is_none_or(|(u, h)| u != doc.url || h != doc.html))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MessageProps, ServerFolder};

    #[test]
    fn keeps_latest_and_goes_with_message() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        let id = s
            .transaction(|tx| {
                let f = tx.upsert_folder(&ServerFolder {
                    server_id: "INBOX".into(),
                    server_parent_id: Some("ROOT".into()),
                    name: "INBOX".into(),
                    folder_class: Some("IPF.Note".into()),
                })?;
                tx.insert_message(f, "A", None, &MessageProps::default())
            })
            .unwrap();
        assert_eq!(s.linked_doc(id).unwrap(), None);
        let mut doc = LinkedDoc {
            url: "https://h.example.com/appl/a.nsf/0/X?opendocument".into(),
            html: "<div>1</div>".into(),
            fetched_at: 10,
        };
        assert!(s.put_linked_doc(id, &doc).unwrap());
        doc.fetched_at = 20;
        assert!(!s.put_linked_doc(id, &doc).unwrap(), "같은 내용");
        assert_eq!(s.linked_doc(id).unwrap().unwrap().fetched_at, 20);
        doc.html = "<div>2</div>".into();
        assert!(s.put_linked_doc(id, &doc).unwrap());
        assert_eq!(s.linked_doc(id).unwrap(), Some(doc));
        s.purge_message(id).unwrap();
        assert_eq!(s.linked_doc(id).unwrap(), None);
    }
}
