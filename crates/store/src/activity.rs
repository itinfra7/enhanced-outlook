//! 작업 현황 창(0.7.1 사용자 요청: 상태 표시줄의 "서버 반영 대기"·"동기화 중"을 누르면 무엇을 하는지 보기)에
//! 보일 작업 목록. Outlook 작업 큐(`ops`)와 HCL Domino 작업 큐(`domino_ops`)를 메일 제목과 함께 읽기만 한다.

use rusqlite::params;

use crate::{Result, Store};

/// 작업 한 줄.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityOp {
    pub id: i64,
    pub kind: String,
    /// Outlook: 작업 내용 JSON, HCL Domino: `arg`.
    pub payload: String,
    /// 메일 제목(메일 행이 있으면), 보내기·임시 보관함 저장이면 작성 중 메일 제목.
    pub subject: Option<String>,
    pub folder_id: Option<i64>,
    pub created_at: i64,
    pub attempts: i64,
    pub next_attempt_at: i64,
    pub last_error: Option<String>,
}

/// 대기(0)와 포기(2)만 본다.
const PENDING: i64 = 0;
const ABANDONED: i64 = 2;

fn activity_op(r: &rusqlite::Row<'_>) -> rusqlite::Result<ActivityOp> {
    Ok(ActivityOp {
        id: r.get(0)?,
        kind: r.get(1)?,
        payload: r.get(2)?,
        subject: r.get(3)?,
        folder_id: r.get(4)?,
        created_at: r.get(5)?,
        attempts: r.get(6)?,
        next_attempt_at: r.get(7)?,
        last_error: r.get(8)?,
    })
}

impl Store {
    /// Outlook 작업: 대기 중이면 먼저 쌓인 순, 포기면 `since` 이후 쌓인 것을 최근 순으로 `limit`개.
    pub fn activity_ops(
        &self,
        abandoned: bool,
        since: i64,
        limit: usize,
    ) -> Result<Vec<ActivityOp>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT o.id, o.kind, o.payload, COALESCE(m.subject, d.subject), m.folder_id, o.created_at, \
               o.attempts, o.next_attempt_at, o.last_error \
             FROM ops o \
             LEFT JOIN messages m ON m.id = o.message_id \
             LEFT JOIN drafts d ON o.message_id IS NULL AND d.id = json_extract(o.payload, '$.draft') \
             WHERE o.state = ?1 AND o.created_at >= ?2 ORDER BY o.id {} LIMIT ?3",
            if abandoned { "DESC" } else { "ASC" }
        ))?;
        let rows = stmt.query_map(
            params![
                if abandoned { ABANDONED } else { PENDING },
                since,
                i64::try_from(limit).unwrap_or(i64::MAX)
            ],
            activity_op,
        )?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// 대기 중인 Outlook 작업의 종류별 수.
    pub fn pending_op_kinds(&self) -> Result<Vec<(String, i64)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT kind, COUNT(*) FROM ops WHERE state = ?1 GROUP BY kind ORDER BY COUNT(*) DESC",
        )?;
        let rows = stmt.query_map([PENDING], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// HCL Domino 작업(`activity_ops`와 같은 순서·조건).
    pub fn activity_domino_ops(
        &self,
        abandoned: bool,
        since: i64,
        limit: usize,
    ) -> Result<Vec<ActivityOp>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT o.id, o.kind, o.arg, m.subject, m.folder_id, o.created_at, o.attempts, \
               o.next_attempt_at, o.last_error \
             FROM domino_ops o LEFT JOIN messages m ON m.id = o.message_id \
             WHERE o.state = ?1 AND o.created_at >= ?2 ORDER BY o.id {} LIMIT ?3",
            if abandoned { "DESC" } else { "ASC" }
        ))?;
        let rows = stmt.query_map(
            params![
                if abandoned { ABANDONED } else { PENDING },
                since,
                i64::try_from(limit).unwrap_or(i64::MAX)
            ],
            activity_op,
        )?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::{DraftContent, MessageProps, OP_SEND, OP_SET_READ, ServerFolder, Store};

    #[test]
    fn lists_pending_and_abandoned_ops_with_subjects() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        let (inbox, mail) = s
            .transaction(|tx| {
                let inbox = tx.upsert_folder(&ServerFolder {
                    server_id: "INBOX".into(),
                    server_parent_id: Some("ROOT".into()),
                    name: "받은 편지함".into(),
                    folder_class: Some("IPF.Note".into()),
                })?;
                tx.rebuild_tree("ROOT")?;
                let mail = tx.insert_message(
                    inbox,
                    "S1",
                    None,
                    &MessageProps {
                        subject: Some("주간 보고".into()),
                        date_received: 1,
                        ..MessageProps::default()
                    },
                )?;
                Ok((inbox, mail))
            })
            .unwrap();
        let draft = s
            .create_draft(
                "new",
                None,
                &DraftContent {
                    subject: "보낼 메일".into(),
                    ..DraftContent::default()
                },
                5,
            )
            .unwrap();
        let read = s
            .enqueue_op(
                OP_SET_READ,
                Some(mail),
                &json!({"read": true, "prev": false}),
                10,
            )
            .unwrap()
            .unwrap();
        s.enqueue_op(OP_SEND, None, &json!({"draft": draft}), 20)
            .unwrap();
        let pending = s.activity_ops(false, 0, 10).unwrap();
        assert_eq!(
            pending
                .iter()
                .map(|o| (o.kind.as_str(), o.subject.as_deref(), o.folder_id))
                .collect::<Vec<_>>(),
            vec![
                ("set_read", Some("주간 보고"), Some(inbox)),
                ("send", Some("보낼 메일"), None)
            ]
        );
        let mut kinds = s.pending_op_kinds().unwrap();
        kinds.sort();
        assert_eq!(
            kinds,
            vec![("send".to_owned(), 1), ("set_read".to_owned(), 1)]
        );
        s.op_abandon(read, "서버가 거부").unwrap();
        let failed = s.activity_ops(true, 0, 10).unwrap();
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].last_error.as_deref(), Some("서버가 거부"));
        assert!(
            s.activity_ops(true, 11, 10).unwrap().is_empty(),
            "since 이전은 빼기"
        );
        s.domino_enqueue("abc", Some(mail), "read", "1", 30)
            .unwrap();
        let bn = s.activity_domino_ops(false, 0, 10).unwrap();
        assert_eq!(bn.len(), 1);
        assert_eq!(
            (bn[0].payload.as_str(), bn[0].subject.as_deref()),
            ("1", Some("주간 보고"))
        );
    }
}
