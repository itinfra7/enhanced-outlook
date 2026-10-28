//! HCL Domino 계정 연동(사용자 요청 2026-10-21)의 저장소 쪽: 가져온 웹메일 문서(UNID) 기록.
//!
//! 가져온 메일은 로컬 전용 메일(`origin = 'domino-sync'`)이다. 이 표는 같은 문서를 다시 가져오지 않으려고 둔다.
//! 로컬에서 지운 메일도 기록은 남는다(`message_id`만 비워진다).

use std::collections::{HashMap, HashSet};

use rusqlite::{OptionalExtension, Row, params};

use crate::{Result, Store, server_state};

/// 가져온 출처 표시(`messages.origin`).
pub const DOMINO_SYNC_ORIGIN: &str = "domino-sync";

/// 웹메일에 보낼 작업 종류(0.6.0 양방향 동기화).
pub mod domino_op {
    /// 읽음(`arg` "1")·안읽음("0").
    pub const READ: &str = "read";
    /// ★ 켜기("1")·끄기("0").
    pub const STAR: &str = "star";
    /// 웹메일 삭제. `arg`: 지울 때 있던 메일함 열쇠(지운 메일이면 완전 삭제).
    pub const DELETE: &str = "delete";
    /// 지운 메일 → 받은 메일.
    pub const RECOVER: &str = "recover";
    /// 개인 폴더로. `arg`: 폴더 이름.
    pub const FOLDER_MOVE: &str = "folder_move";
    /// 개인 폴더 → 원래 메일함.
    pub const MOVE_BACK: &str = "move_back";
}

/// 가져온 문서 하나와 마지막으로 본 서버 상태(없으면 아직 모름).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DominoItem {
    pub unid: String,
    pub noteid: String,
    pub message_id: Option<i64>,
    /// 메일함 열쇠(`XML_Inbox`, `XML_Category:이름`).
    pub srv_box: Option<String>,
    pub srv_new: Option<bool>,
    pub srv_star: Option<bool>,
    pub server_deleted: bool,
}

/// 웹메일에 보낼 작업 하나.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DominoOp {
    pub id: i64,
    pub unid: String,
    pub message_id: Option<i64>,
    pub kind: String,
    pub arg: String,
    pub attempts: i64,
}

const ITEM_COLS: &str =
    "unid, noteid, message_id, srv_box, srv_new, srv_star, server_deleted_at IS NOT NULL";

fn item_row(r: &Row<'_>) -> rusqlite::Result<DominoItem> {
    Ok(DominoItem {
        unid: r.get(0)?,
        noteid: r.get(1)?,
        message_id: r.get(2)?,
        srv_box: r.get(3)?,
        srv_new: r.get(4)?,
        srv_star: r.get(5)?,
        server_deleted: r.get(6)?,
    })
}

/// 자동 정리 후보 판단용: 가져온 문서와 그 로컬 사본.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DominoCopy {
    pub unid: String,
    pub noteid: String,
    pub message_id: i64,
    /// 로컬 사본에 원문이 있다.
    pub has_eml: bool,
}

impl Store {
    /// 이미 가져온(또는 같은 메일이 이미 있어 건너뛴) 문서들(UNID 대문자).
    pub fn domino_known(&self) -> Result<HashSet<String>> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT unid FROM domino_items")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// 가져온 문서를 적는다. `message_id`: 로컬 사본(새로 넣은 것, 또는 이미 있던 같은 메일).
    pub fn domino_record(
        &self,
        unid: &str,
        message_id: Option<i64>,
        noteid: &str,
        view: &str,
        at: i64,
    ) -> Result<()> {
        self.conn().execute(
            "INSERT INTO domino_items(unid, message_id, noteid, view, fetched_at) VALUES (?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT(unid) DO UPDATE SET message_id = excluded.message_id, noteid = excluded.noteid",
            params![unid.to_ascii_uppercase(), message_id, noteid, view, at],
        )?;
        Ok(())
    }

    /// 로컬 사본이 남아 있고 아직 서버에서 지우지 않은 문서들(자동 정리 후보를 고를 때).
    pub fn domino_copies(&self) -> Result<Vec<DominoCopy>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT b.unid, b.noteid, b.message_id, m.eml_hash IS NOT NULL FROM domino_items b \
             JOIN messages m ON m.id = b.message_id WHERE b.server_deleted_at IS NULL",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(DominoCopy {
                unid: r.get(0)?,
                noteid: r.get(1)?,
                message_id: r.get(2)?,
                has_eml: r.get(3)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// 자동 정리로 서버에서 지웠다고 적는다.
    pub fn domino_mark_server_deleted(&self, unids: &[String], at: i64) -> Result<()> {
        self.transaction(|tx| {
            for u in unids {
                tx.tx.execute(
                    "UPDATE domino_items SET server_deleted_at = ?2 WHERE unid = ?1",
                    params![u.to_ascii_uppercase(), at],
                )?;
            }
            Ok(())
        })
    }

    /// 가져온 문서 모두(서버에서 바뀐 것을 로컬에 반영할 때).
    pub fn domino_items(&self) -> Result<Vec<DominoItem>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!("SELECT {ITEM_COLS} FROM domino_items"))?;
        let rows = stmt.query_map([], item_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// 로컬 메일 번호 → 가져온 문서(연동한 HCL Domino 메일만).
    pub fn domino_items_for(&self, message_ids: &[i64]) -> Result<HashMap<i64, DominoItem>> {
        let conn = self.conn();
        let mut stmt = conn.prepare_cached(&format!(
            "SELECT {ITEM_COLS} FROM domino_items WHERE message_id = ?1"
        ))?;
        let mut out = HashMap::new();
        for &id in message_ids {
            if let Some(item) = stmt.query_row([id], item_row).optional()? {
                out.insert(id, item);
            }
        }
        Ok(out)
    }

    pub fn domino_item(&self, unid: &str) -> Result<Option<DominoItem>> {
        Ok(self
            .conn()
            .query_row(
                &format!("SELECT {ITEM_COLS} FROM domino_items WHERE unid = ?1"),
                [unid.to_ascii_uppercase()],
                item_row,
            )
            .optional()?)
    }

    /// 마지막으로 본 서버 상태를 적는다(`None`인 것은 그대로).
    pub fn domino_set_srv(
        &self,
        unid: &str,
        srv_box: Option<&str>,
        srv_new: Option<bool>,
        srv_star: Option<bool>,
    ) -> Result<()> {
        self.conn().execute(
            "UPDATE domino_items SET srv_box = COALESCE(?2, srv_box), srv_new = COALESCE(?3, srv_new), \
             srv_star = COALESCE(?4, srv_star) WHERE unid = ?1",
            params![unid.to_ascii_uppercase(), srv_box, srv_new, srv_star],
        )?;
        Ok(())
    }

    /// 웹메일 메일함(열쇠)이 어느 로컬 폴더인지 적는다.
    pub fn domino_set_folder_box(&self, folder_id: i64, box_key: &str) -> Result<()> {
        self.transaction(|tx| {
            tx.tx.execute(
                "DELETE FROM domino_folders WHERE box = ?2 AND folder_id <> ?1",
                params![folder_id, box_key],
            )?;
            tx.tx.execute(
                "INSERT INTO domino_folders(folder_id, box) VALUES (?1, ?2) \
                 ON CONFLICT(folder_id) DO UPDATE SET box = excluded.box",
                params![folder_id, box_key],
            )?;
            Ok(())
        })
    }

    /// 로컬 폴더 번호 → 웹메일 메일함 열쇠.
    pub fn domino_folder_boxes(&self) -> Result<HashMap<i64, String>> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT folder_id, box FROM domino_folders")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// 웹메일에 보낼 작업을 쌓는다. 읽음·★은 같은 메일의 앞 작업을 대신한다(마지막 상태만 보내면 된다).
    pub fn domino_enqueue(
        &self,
        unid: &str,
        message_id: Option<i64>,
        kind: &str,
        arg: &str,
        now_ms: i64,
    ) -> Result<()> {
        let unid = unid.to_ascii_uppercase();
        self.transaction(|tx| {
            if kind == domino_op::READ || kind == domino_op::STAR {
                tx.tx.execute(
                    "DELETE FROM domino_ops WHERE unid = ?1 AND kind = ?2 AND state = 0",
                    params![unid, kind],
                )?;
            }
            tx.tx.execute(
                "INSERT INTO domino_ops(unid, message_id, kind, arg, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![unid, message_id, kind, arg, now_ms],
            )?;
            Ok(())
        })
    }

    /// 지금 보낼 작업(쌓인 순서대로).
    pub fn domino_due_ops(&self, now_ms: i64, limit: usize) -> Result<Vec<DominoOp>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, unid, message_id, kind, arg, attempts FROM domino_ops \
             WHERE state = 0 AND next_attempt_at <= ?1 ORDER BY id LIMIT ?2",
        )?;
        let rows = stmt.query_map(
            params![now_ms, i64::try_from(limit).unwrap_or(i64::MAX)],
            |r| {
                Ok(DominoOp {
                    id: r.get(0)?,
                    unid: r.get(1)?,
                    message_id: r.get(2)?,
                    kind: r.get(3)?,
                    arg: r.get(4)?,
                    attempts: r.get(5)?,
                })
            },
        )?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn domino_op_done(&self, id: i64) -> Result<()> {
        self.conn()
            .execute("UPDATE domino_ops SET state = 1 WHERE id = ?1", [id])?;
        Ok(())
    }

    /// 실패: 다음 시도 시각을 미룬다.
    pub fn domino_op_retry(&self, id: i64, error: &str, next_at: i64) -> Result<()> {
        self.conn().execute(
            "UPDATE domino_ops SET attempts = attempts + 1, last_error = ?2, next_attempt_at = ?3 WHERE id = ?1",
            params![id, error, next_at],
        )?;
        Ok(())
    }

    /// 포기(로컬은 부른 쪽이 되돌린다).
    pub fn domino_op_abandon(&self, id: i64, error: &str) -> Result<()> {
        self.conn().execute(
            "UPDATE domino_ops SET state = 2, last_error = ?2 WHERE id = ?1",
            params![id, error],
        )?;
        Ok(())
    }

    /// 보낼 작업이 남은 문서(서버 상태로 로컬을 덮어쓰지 않는다).
    pub fn domino_pending_unids(&self) -> Result<HashSet<String>> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT DISTINCT unid FROM domino_ops WHERE state = 0")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn domino_pending_count(&self) -> Result<i64> {
        Ok(self
            .conn()
            .query_row("SELECT COUNT(*) FROM domino_ops WHERE state = 0", [], |r| {
                r.get(0)
            })?)
    }

    /// 진단용: 웹메일 작업 (종류, 상태, 수, 마지막으로 쌓인 시각). 상태 0 대기·1 완료·2 포기.
    pub fn domino_op_summary(&self) -> Result<Vec<(String, i64, i64, i64)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT kind, state, COUNT(*), MAX(created_at) FROM domino_ops GROUP BY kind, state ORDER BY kind, state",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// 웹메일에서 지운 연동 메일: 로컬은 그 자리에 보관으로 남긴다(Outlook의 "서버에서 삭제됨"과 같은 표시).
    pub fn domino_archive_local(&self, message_ids: &[i64], now_ms: i64) -> Result<Vec<i64>> {
        self.transaction(|tx| {
            let mut changed = Vec::new();
            for &id in message_ids {
                let n = tx.tx.execute(
                    "UPDATE messages SET server_state = ?3, archived_at = ?2 WHERE id = ?1 AND server_state = ?4",
                    params![id, now_ms, server_state::ARCHIVED, server_state::LOCAL_ONLY],
                )?;
                if n > 0 {
                    changed.push(id);
                }
            }
            Ok(changed)
        })
    }

    /// 보관을 풀어 다시 연동 메일로(웹메일에서 복구됐거나, 서버 삭제를 포기해 되돌릴 때).
    pub fn domino_unarchive_local(&self, message_id: i64) -> Result<bool> {
        let n = self.conn().execute(
            "UPDATE messages SET server_state = ?2, archived_at = NULL WHERE id = ?1 AND server_state = ?3",
            params![message_id, server_state::LOCAL_ONLY, server_state::ARCHIVED],
        )?;
        Ok(n > 0)
    }

    /// 새 메일 알림(0.6.0): 폴더들의 가장 큰 메일 번호(새로 들어온 메일일수록 크다. 옮긴 메일은 번호가 그대로).
    pub fn max_message_id_in(&self, folder_ids: &[i64]) -> Result<Option<i64>> {
        if folder_ids.is_empty() {
            return Ok(None);
        }
        let list = folder_ids
            .iter()
            .map(i64::to_string)
            .collect::<Vec<_>>()
            .join(",");
        Ok(self.conn().query_row(
            &format!("SELECT MAX(id) FROM messages WHERE folder_id IN ({list})"),
            [],
            |r| r.get(0),
        )?)
    }

    /// 새 메일 알림: 폴더들에서 `after_id`보다 나중에 들어온 안 읽은 메일(최근 것부터).
    pub fn unread_after(
        &self,
        folder_ids: &[i64],
        after_id: i64,
        limit: usize,
    ) -> Result<Vec<crate::RowLite>> {
        if folder_ids.is_empty() {
            return Ok(Vec::new());
        }
        let list = folder_ids
            .iter()
            .map(i64::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let ids: Vec<i64> = {
            let conn = self.conn();
            let mut stmt = conn.prepare(&format!(
                "SELECT id FROM messages WHERE folder_id IN ({list}) AND id > ?1 AND is_read = 0 \
                 ORDER BY date_received DESC, id DESC LIMIT ?2"
            ))?;
            let rows = stmt.query_map(
                params![after_id, i64::try_from(limit).unwrap_or(i64::MAX)],
                |r| r.get(0),
            )?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        self.rows_by_ids(&ids)
    }

    /// 폴더 표시 순서(HCL Domino 메일함을 웹메일 왼쪽 메뉴 순서로, 사용자 요청 2026-10-21). 바뀔 때만 쓴다.
    /// 바뀌었으면 true.
    pub fn set_folder_sort_order(&self, folder_id: i64, order: i64) -> Result<bool> {
        let n = self.conn().execute(
            "UPDATE folders SET sort_order = ?2 WHERE id = ?1 AND sort_order <> ?2",
            params![folder_id, order],
        )?;
        Ok(n > 0)
    }

    /// HCL Domino 백업(PST·HTML·EML)으로 가져온 로컬 메일들(번호, 제목, 받은 시각): 같은 메일을 알아보려고.
    /// 웹메일은 Message-ID를 주지 않아 제목과 받은 시각으로 맞춘다. 연동으로 가져온 것은 UNID로 알아보므로 뺀다.
    pub fn domino_local_mails(&self) -> Result<Vec<(i64, String, i64)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, COALESCE(subject, ''), date_received FROM messages \
             WHERE origin LIKE 'domino%' AND origin <> ?1",
        )?;
        let rows = stmt.query_map([DOMINO_SYNC_ORIGIN], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

#[cfg(test)]
mod tests {
    use crate::{ImportOutcome, MessageProps, Store};

    use super::DOMINO_SYNC_ORIGIN;

    fn imported(o: ImportOutcome) -> i64 {
        match o {
            ImportOutcome::Imported(id) => id,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn records_survive_local_delete_and_copies_need_message() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        let (folder, _) = s
            .find_or_create_folder_path(&["HCL Domino 메일".into(), "받은 메일".into()])
            .unwrap();
        let props = MessageProps {
            subject: Some("제목".into()),
            date_received: 1_000,
            ..MessageProps::default()
        };
        let id = imported(
            s.import_message(folder, b"raw-1", &props, DOMINO_SYNC_ORIGIN)
                .unwrap(),
        );
        let pst = imported(
            s.import_message(folder, b"raw-2", &props, "domino-pst")
                .unwrap(),
        );
        let unid = "98c9b1996c6c20da49258e96000c0c4c";
        s.domino_record(unid, Some(id), "18B3A", "XML_Inbox", 5)
            .unwrap();
        assert!(
            s.domino_known()
                .unwrap()
                .contains(&unid.to_ascii_uppercase())
        );
        let copies = s.domino_copies().unwrap();
        assert_eq!(copies.len(), 1);
        assert_eq!((copies[0].message_id, copies[0].has_eml), (id, true));
        assert_eq!(
            s.domino_local_mails().unwrap(),
            [(pst, "제목".to_owned(), 1_000)],
            "백업으로 가져온 것만(연동으로 가져온 것은 UNID로 안다)"
        );
        s.domino_mark_server_deleted(&[unid.to_owned()], 9).unwrap();
        assert!(
            s.domino_copies().unwrap().is_empty(),
            "서버에서 지운 것은 빠진다"
        );
        // 로컬에서 지워도 기록은 남는다(다시 가져오지 않는다).
        s.transaction(|tx| {
            tx.tx.execute("DELETE FROM messages WHERE id = ?1", [id])?;
            Ok(())
        })
        .unwrap();
        assert!(
            s.domino_known()
                .unwrap()
                .contains(&unid.to_ascii_uppercase())
        );
    }
}
