//! 로컬 조작(계획서 4.1-2 낙관적 반영, 9.4 도구): 로컬에 바로 반영하고 같은 트랜잭션에서
//! 서버 작업을 쌓는다. 서버에 없는 메일(보관·로컬 전용)은 로컬만 바꾼다.
//! 서버가 끝내 거부한 작업은 [`Store::rollback_op`]로 되돌린다.

use std::collections::BTreeMap;

use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};

use crate::ops::{OP_COPY, OP_DELETE, OP_MOVE, OP_SET_FLAG, OP_SET_PIN, OP_SET_READ};
use crate::{
    Error, FOLDER_COLS, FolderRow, MESSAGE_COLS, MessageRow, OpRow, Result, RowLite, Store, Tx,
    folder_row, message_row, row_lite, server_state,
};

/// 조작으로 바뀐 것(화면 갱신·색인용).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Touched {
    /// 폴더별로 바뀐 메일.
    pub folders: BTreeMap<i64, Vec<i64>>,
    /// 로컬에서 지운 메일(색인에서도 지운다).
    pub removed: Vec<i64>,
    /// 새로 생긴 메일(복사본).
    pub created: Vec<i64>,
    /// 서버 작업을 쌓은 수.
    pub queued: usize,
}

impl Touched {
    pub fn add(&mut self, folder_id: i64, id: i64) {
        let v = self.folders.entry(folder_id).or_default();
        if !v.contains(&id) {
            v.push(id);
        }
    }

    pub fn merge(&mut self, other: Touched) {
        for (f, ids) in other.folders {
            for id in ids {
                self.add(f, id);
            }
        }
        self.removed.extend(other.removed);
        self.created.extend(other.created);
        self.queued += other.queued;
    }

    pub fn is_empty(&self) -> bool {
        self.folders.is_empty() && self.removed.is_empty()
    }
}

/// 서버에 반영할 수 있는(또는 곧 반영될) 메일인가.
fn on_server(row: &MessageRow) -> bool {
    row.server_item_id.is_some() || row.server_state == server_state::PENDING
}

impl Tx<'_> {
    pub fn message(&self, id: i64) -> Result<Option<MessageRow>> {
        Ok(self
            .tx
            .query_row(
                &format!("SELECT {MESSAGE_COLS} FROM messages WHERE id = ?1"),
                [id],
                message_row,
            )
            .optional()?)
    }

    pub fn folder(&self, id: i64) -> Result<Option<FolderRow>> {
        Ok(self
            .tx
            .query_row(
                &format!("SELECT {FOLDER_COLS} FROM folders WHERE id = ?1"),
                [id],
                folder_row,
            )
            .optional()?)
    }

    /// 로컬 폴더만 바꾼다(서버 항목 ID는 그대로). 색인의 폴더 값도 다시 넣게 한다.
    pub fn set_folder(&self, id: i64, folder_id: i64) -> Result<()> {
        self.tx.execute(
            "UPDATE messages SET folder_id = ?1, indexed = 0 WHERE id = ?2",
            params![folder_id, id],
        )?;
        Ok(())
    }

    pub fn set_flag(&self, id: i64, flag: i64) -> Result<()> {
        self.tx.execute(
            "UPDATE messages SET flag = ?1 WHERE id = ?2",
            params![flag, id],
        )?;
        Ok(())
    }

    pub fn set_pinned(&self, id: i64, pinned: bool) -> Result<()> {
        self.tx.execute(
            "UPDATE messages SET pinned = ?1 WHERE id = ?2",
            params![pinned, id],
        )?;
        Ok(())
    }

    /// 서버에서 삭제 대기: 보관으로 보이게 하되 서버 항목 ID는 작업이 끝날 때까지 둔다.
    pub fn set_archived_pending(&self, id: i64, now_ms: i64) -> Result<()> {
        self.tx.execute(
            "UPDATE messages SET server_state = 1, archived_at = ?1 WHERE id = ?2",
            params![now_ms, id],
        )?;
        Ok(())
    }

    pub fn unarchive(&self, id: i64) -> Result<()> {
        self.tx.execute(
            "UPDATE messages SET server_state = 0, archived_at = NULL WHERE id = ?1",
            [id],
        )?;
        Ok(())
    }

    /// 서버 작업이 돌려준 항목 ID로 바꾼다(이동·복사 뒤).
    pub fn set_server_item(
        &self,
        id: i64,
        server_item_id: &str,
        change_key: Option<&str>,
    ) -> Result<()> {
        self.tx.execute(
            "UPDATE messages SET server_item_id = ?1, change_key = ?2, server_state = 0, \
             archived_at = NULL WHERE id = ?3",
            params![server_item_id, change_key, id],
        )?;
        Ok(())
    }

    /// 원문을 떼어 다음 원문 받기에서 서버의 지금 내용으로 다시 받게 한다(내용이 바뀌는 초안, 1.2.1). 뗀 원문의
    /// 해시(쓰는 행이 없으면 호출한 쪽이 파일을 지운다).
    pub fn invalidate_eml(&self, id: i64) -> Result<Option<String>> {
        let old: Option<String> = self
            .tx
            .query_row("SELECT eml_hash FROM messages WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .optional()?
            .flatten();
        if old.is_some() {
            self.tx.execute(
                "UPDATE messages SET eml_hash = NULL WHERE id = ?1 AND server_item_id IS NOT NULL",
                [id],
            )?;
        }
        Ok(old.filter(|_| self.tx.changes() > 0))
    }

    /// 사본 행(원문 공유). 서버 ID는 없고 `state`(보류 또는 보관)로 만든다.
    pub fn insert_copy(
        &self,
        src: &MessageRow,
        folder_id: i64,
        state: i64,
        archived_at: Option<i64>,
    ) -> Result<i64> {
        self.tx.execute(
            &format!(
                "INSERT INTO messages(id, folder_id, server_item_id, change_key, server_state, \
                   internet_msg_id, conversation_id, subject, from_name, from_addr, to_list, cc_list, \
                   bcc_list, date_received, date_sent, size, has_attachments, is_read, flag, importance, \
                   is_draft, item_class, preview, eml_hash, indexed, archived_at, origin) \
                 SELECT {}, ?1, NULL, NULL, ?2, internet_msg_id, conversation_id, subject, from_name, \
                   from_addr, to_list, cc_list, bcc_list, date_received, date_sent, size, has_attachments, \
                   is_read, flag, importance, is_draft, item_class, preview, eml_hash, 0, ?3, origin \
                 FROM messages WHERE id = ?4",
                crate::schema::NEXT_MESSAGE_ID
            ),
            params![folder_id, state, archived_at, src.id],
        )?;
        Ok(self.tx.last_insert_rowid())
    }

    /// 이 폴더에 서버 반영을 기다리는 같은 메일 사본이 있으면 그 행(동기화가 서버의 새 항목과 잇는다).
    pub fn pending_copy_in(
        &self,
        folder_id: i64,
        imid: &str,
        date_received: i64,
    ) -> Result<Option<i64>> {
        Ok(self
            .tx
            .query_row(
                "SELECT id FROM messages WHERE folder_id = ?1 AND internet_msg_id = ?2 \
                 AND date_received = ?3 AND server_item_id IS NULL AND server_state = ?4 LIMIT 1",
                params![folder_id, imid, date_received, server_state::PENDING],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// 행을 지우고 원문 해시를 돌려준다(파일은 호출한 쪽이 더 쓰이지 않을 때 지운다).
    fn delete_row(&self, id: i64) -> Result<Option<String>> {
        let hash: Option<String> = self
            .tx
            .query_row("SELECT eml_hash FROM messages WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .optional()?
            .flatten();
        self.tx
            .execute("DELETE FROM messages WHERE id = ?1", [id])?;
        Ok(hash)
    }

    /// 행을 지우고(대기 작업도 정리) 원문 해시를 돌려준다. 파일은 [`Store::remove_eml_if_unused`]로.
    pub fn purge_row(&self, id: i64) -> Result<Option<String>> {
        self.drop_pending_ops_for(id, "로컬에서 지움")?;
        self.delete_row(id)
    }

    /// 잘 알려진 폴더(서버에 있는 것)의 로컬 번호.
    pub fn well_known_folder_id(&self, name: &str) -> Result<Option<i64>> {
        Ok(self
            .tx
            .query_row(
                "SELECT id FROM folders WHERE well_known = ?1 AND server_id IS NOT NULL",
                [name],
                |r| r.get(0),
            )
            .optional()?)
    }

    fn hash_in_use(&self, hash: &str) -> Result<bool> {
        Ok(self.tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM messages WHERE eml_hash = ?1)",
            [hash],
            |r| r.get(0),
        )?)
    }
}

impl Store {
    /// 이 원문을 쓰는 행이 없으면 파일을 지운다. 지웠으면 true.
    pub fn remove_eml_if_unused(&self, hash: &str) -> Result<bool> {
        let used: bool = self.conn().query_row(
            "SELECT EXISTS(SELECT 1 FROM messages WHERE eml_hash = ?1)",
            [hash],
            |r| r.get(0),
        )?;
        if used {
            return Ok(false);
        }
        self.eml().remove(hash)?;
        Ok(true)
    }

    fn remove_unused_eml(&self, hashes: Vec<String>) -> Result<()> {
        for h in hashes {
            self.eml().remove(&h)?;
        }
        Ok(())
    }

    /// 읽음·안읽음.
    pub fn mark_read(&self, ids: &[i64], read: bool, now_ms: i64) -> Result<Touched> {
        self.transaction(|tx| {
            let mut t = Touched::default();
            for &id in ids {
                let Some(row) = tx.message(id)? else { continue };
                if row.props.is_read == read {
                    continue;
                }
                tx.set_read(id, read)?;
                if on_server(&row) {
                    let q = tx.enqueue_op(
                        OP_SET_READ,
                        Some(id),
                        &json!({"read": read, "prev": row.props.is_read}),
                        now_ms,
                        None,
                    )?;
                    t.queued += usize::from(q.is_some());
                }
                t.add(row.folder_id, id);
            }
            Ok(t)
        })
    }

    /// 플래그: 0 없음, 1 플래그, 2 완료.
    pub fn mark_flag(&self, ids: &[i64], flag: i64, now_ms: i64) -> Result<Touched> {
        self.transaction(|tx| {
            let mut t = Touched::default();
            for &id in ids {
                let Some(row) = tx.message(id)? else { continue };
                if row.props.flag == flag {
                    continue;
                }
                tx.set_flag(id, flag)?;
                if on_server(&row) {
                    let q = tx.enqueue_op(
                        OP_SET_FLAG,
                        Some(id),
                        &json!({"flag": flag, "prev": row.props.flag}),
                        now_ms,
                        None,
                    )?;
                    t.queued += usize::from(q.is_some());
                }
                t.add(row.folder_id, id);
            }
            Ok(t)
        })
    }

    /// 고정·해제(1.1.0). 서버 메일은 웹메일의 고정과 같게 서버에도 반영한다(HCL Domino·가져온 메일은 로컬만).
    pub fn mark_pinned(&self, ids: &[i64], pinned: bool, now_ms: i64) -> Result<Touched> {
        self.transaction(|tx| {
            let mut t = Touched::default();
            for &id in ids {
                let Some(row) = tx.message(id)? else { continue };
                let prev = row.props.pinned.unwrap_or(false);
                if prev == pinned {
                    continue;
                }
                tx.set_pinned(id, pinned)?;
                if on_server(&row) {
                    let q = tx.enqueue_op(
                        OP_SET_PIN,
                        Some(id),
                        &json!({"pin": pinned, "prev": prev}),
                        now_ms,
                        None,
                    )?;
                    t.queued += usize::from(q.is_some());
                }
                t.add(row.folder_id, id);
            }
            Ok(t)
        })
    }

    /// 서버에서 고정한 것으로 확인한 메일을 고정으로 표시한다(업그레이드 뒤 한 번, 1.1.0). 바뀐 것.
    pub fn pin_from_server(&self, server_item_ids: &[String]) -> Result<Touched> {
        self.transaction(|tx| {
            let mut t = Touched::default();
            for sid in server_item_ids {
                let Some(row) = tx.message_by_server_id(sid)? else {
                    continue;
                };
                if row.props.pinned != Some(true) {
                    tx.set_pinned(row.id, true)?;
                    t.add(row.folder_id, row.id);
                }
            }
            Ok(t)
        })
    }

    /// 이동(지운 편지함으로 삭제도 이것). 서버에서 사라진 폴더로는 서버 메일을 옮길 수 없다.
    pub fn move_messages(
        &self,
        ids: &[i64],
        to: i64,
        now_ms: i64,
        batch: Option<i64>,
    ) -> Result<Touched> {
        self.transaction(|tx| {
            let target = tx
                .folder(to)?
                .ok_or_else(|| Error::Invalid("대상 폴더가 없습니다".into()))?;
            let mut t = Touched::default();
            for &id in ids {
                let Some(row) = tx.message(id)? else { continue };
                if row.folder_id == to {
                    continue;
                }
                if on_server(&row) {
                    if target.server_id.is_none() {
                        return Err(Error::Invalid(format!(
                            "'{}'은(는) 서버에 없는 폴더(보관·가져온 폴더)라 서버에 있는 메일을 옮길 수 없습니다",
                            target.name
                        )));
                    }
                    tx.enqueue_op(
                        OP_MOVE,
                        Some(id),
                        &json!({"to": to, "from": row.folder_id}),
                        now_ms,
                        batch,
                    )?;
                    t.queued += 1;
                }
                tx.set_folder(id, to)?;
                t.add(row.folder_id, id);
                t.add(to, id);
            }
            Ok(t)
        })
    }

    /// 복사: 사본 행을 만들고(원문 공유) 서버 복사를 쌓는다. 서버에 없는 메일은 로컬 사본(보관)만.
    pub fn copy_messages(
        &self,
        ids: &[i64],
        to: i64,
        now_ms: i64,
        batch: Option<i64>,
    ) -> Result<Touched> {
        self.transaction(|tx| {
            let target = tx
                .folder(to)?
                .ok_or_else(|| Error::Invalid("대상 폴더가 없습니다".into()))?;
            let mut t = Touched::default();
            for &id in ids {
                let Some(row) = tx.message(id)? else { continue };
                let new_id = if on_server(&row) && target.server_id.is_some() {
                    let new_id = tx.insert_copy(&row, to, server_state::PENDING, None)?;
                    tx.enqueue_op(
                        OP_COPY,
                        Some(new_id),
                        &json!({"to": to, "source": id}),
                        now_ms,
                        batch,
                    )?;
                    t.queued += 1;
                    new_id
                } else if row.server_state == server_state::LOCAL_ONLY {
                    // 가져온 메일의 사본은 가져온 메일 그대로(보관 아님).
                    tx.insert_copy(&row, to, server_state::LOCAL_ONLY, None)?
                } else {
                    tx.insert_copy(
                        &row,
                        to,
                        server_state::ARCHIVED,
                        Some(row.archived_at.unwrap_or(now_ms)),
                    )?
                };
                t.add(to, new_id);
                t.created.push(new_id);
            }
            Ok(t)
        })
    }

    /// 서버에서만 삭제(복구 가능 영역, 웹메일의 영구 삭제와 같음). 로컬은 그 자리에 보관으로 남는다.
    pub fn server_delete(&self, ids: &[i64], now_ms: i64, batch: Option<i64>) -> Result<Touched> {
        self.transaction(|tx| {
            let mut t = Touched::default();
            for &id in ids {
                let Some(row) = tx.message(id)? else { continue };
                // 이미 보관이거나, 서버에 올린 적 없는 가져온 메일은 서버에서 지울 것이 없다.
                if matches!(
                    row.server_state,
                    server_state::ARCHIVED | server_state::LOCAL_ONLY
                ) {
                    continue;
                }
                tx.set_archived_pending(id, now_ms)?;
                if row.server_item_id.is_some() {
                    tx.enqueue_op(OP_DELETE, Some(id), &json!({"mode": "soft"}), now_ms, batch)?;
                    t.queued += 1;
                } else {
                    // 서버 반영 전 사본: 서버에 올리지 않는다(복사 작업은 실행 때 건너뛴다).
                    tx.drop_pending_ops_for(id, "서버에서 삭제됨(올리기 전)")?;
                }
                t.add(row.folder_id, id);
            }
            Ok(t)
        })
    }

    /// 완전 삭제: 로컬 행·원문을 지우고, 서버에 있으면 서버에서도 영구 삭제를 쌓는다. 되돌릴 수 없다.
    pub fn purge_messages(&self, ids: &[i64], now_ms: i64, batch: Option<i64>) -> Result<Touched> {
        let (t, unused) = self.transaction(|tx| {
            let mut t = Touched::default();
            let mut hashes = Vec::new();
            for &id in ids {
                let Some(row) = tx.message(id)? else { continue };
                tx.drop_pending_ops_for(id, "완전 삭제됨")?;
                if let Some(sid) = &row.server_item_id {
                    tx.enqueue_op(
                        OP_DELETE,
                        Some(id),
                        &json!({"mode": "hard", "server_id": sid}),
                        now_ms,
                        batch,
                    )?;
                    t.queued += 1;
                }
                if let Some(h) = tx.delete_row(id)? {
                    hashes.push(h);
                }
                t.add(row.folder_id, id);
                t.removed.push(id);
            }
            hashes.sort();
            hashes.dedup();
            let mut unused = Vec::new();
            for h in hashes {
                if !tx.hash_in_use(&h)? {
                    unused.push(h);
                }
            }
            Ok((t, unused))
        })?;
        self.remove_unused_eml(unused)?;
        Ok(t)
    }

    /// 포기한 작업의 로컬 반영을 되돌린다(계획서 4.1-2 "실패하면 되돌리고 알린다").
    /// 발송은 작성 중 메일을 "발송 실패"로 돌려 작성 창을 다시 열 수 있게 한다.
    pub fn rollback_op(&self, op: &OpRow) -> Result<Touched> {
        let p = &op.payload;
        if op.kind == crate::ops::OP_SEND {
            if let Some(d) = p.get("draft").and_then(Value::as_i64) {
                self.mark_draft_failed(d, op.last_error.as_deref())?;
            }
            return Ok(Touched::default());
        }
        let (t, unused) = self.transaction(|tx| {
            let mut t = Touched::default();
            let mut unused = Vec::new();
            let Some(mid) = op.message_id else {
                return Ok((t, unused));
            };
            let Some(row) = tx.message(mid)? else {
                return Ok((t, unused));
            };
            match op.kind.as_str() {
                OP_SET_READ => {
                    if let Some(prev) = p.get("prev").and_then(Value::as_bool) {
                        tx.set_read(mid, prev)?;
                        t.add(row.folder_id, mid);
                    }
                }
                OP_SET_FLAG => {
                    if let Some(prev) = p.get("prev").and_then(Value::as_i64) {
                        tx.set_flag(mid, prev)?;
                        t.add(row.folder_id, mid);
                    }
                }
                OP_SET_PIN => {
                    if let Some(prev) = p.get("prev").and_then(Value::as_bool) {
                        tx.set_pinned(mid, prev)?;
                        t.add(row.folder_id, mid);
                    }
                }
                OP_MOVE => {
                    let (to, from) = (
                        p.get("to").and_then(Value::as_i64),
                        p.get("from").and_then(Value::as_i64),
                    );
                    if let (Some(to), Some(from)) = (to, from)
                        && row.folder_id == to
                        && tx.folder(from)?.is_some()
                    {
                        tx.set_folder(mid, from)?;
                        t.add(to, mid);
                        t.add(from, mid);
                    }
                }
                OP_COPY => {
                    if row.server_state == server_state::PENDING && row.server_item_id.is_none() {
                        if let Some(h) = tx.delete_row(mid)?
                            && !tx.hash_in_use(&h)?
                        {
                            unused.push(h);
                        }
                        t.add(row.folder_id, mid);
                        t.removed.push(mid);
                    }
                }
                OP_DELETE
                    if p.get("mode").and_then(Value::as_str) == Some("soft")
                        && row.server_item_id.is_some() =>
                {
                    tx.unarchive(mid)?;
                    t.add(row.folder_id, mid);
                }
                _ => {}
            }
            Ok((t, unused))
        })?;
        self.remove_unused_eml(unused)?;
        Ok(t)
    }

    pub fn folder(&self, id: i64) -> Result<Option<FolderRow>> {
        Ok(self
            .conn()
            .query_row(
                &format!("SELECT {FOLDER_COLS} FROM folders WHERE id = ?1"),
                [id],
                folder_row,
            )
            .optional()?)
    }

    /// 이 Message-ID를 가진 로컬 메일들(번호, 폴더). 스레드 확인용.
    pub fn message_ids_by_imid(&self, imid: &str) -> Result<Vec<(i64, i64)>> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare("SELECT id, folder_id FROM messages WHERE internet_msg_id = ?1 ORDER BY id")?;
        let rows = stmt.query_map([imid.trim()], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn folder_by_well_known(&self, name: &str) -> Result<Option<FolderRow>> {
        Ok(self
            .conn()
            .query_row(
                &format!("SELECT {FOLDER_COLS} FROM folders WHERE well_known = ?1 AND server_id IS NOT NULL"),
                [name],
                folder_row,
            )
            .optional()?)
    }

    /// 보관 메일(서버에서 지워져 로컬에만 있음) 수, 폴더 구분 없이(계획서 9.4).
    pub fn archived_total(&self) -> Result<i64> {
        Ok(self.conn().query_row(
            "SELECT COUNT(*) FROM messages WHERE server_state = ?1",
            [server_state::ARCHIVED],
            |r| r.get(0),
        )?)
    }

    /// 폴더(없으면 보관 메일 전체)의 메일 ID, 목록과 같은 순서(전체 선택용).
    pub fn list_ids(&self, folder_id: Option<i64>) -> Result<Vec<i64>> {
        let conn = self.conn();
        let rows = match folder_id {
            Some(f) => {
                let mut stmt = conn.prepare_cached(
                    "SELECT id FROM messages WHERE folder_id = ?1 \
                     ORDER BY pinned DESC, date_received DESC, id",
                )?;
                stmt.query_map([f], |r| r.get(0))?
                    .collect::<rusqlite::Result<Vec<i64>>>()?
            }
            None => {
                let mut stmt = conn.prepare_cached(
                    "SELECT id FROM messages WHERE server_state = ?1 ORDER BY date_received DESC, id",
                )?;
                stmt.query_map([server_state::ARCHIVED], |r| r.get(0))?
                    .collect::<rusqlite::Result<Vec<i64>>>()?
            }
        };
        Ok(rows)
    }

    /// 보관 메일 목록 창, 최신순.
    pub fn archived_rows(&self, offset: usize, limit: usize) -> Result<Vec<RowLite>> {
        let conn = self.conn();
        let mut stmt = conn.prepare_cached(&format!(
            "SELECT {} FROM messages WHERE server_state = ?1 ORDER BY date_received DESC, id LIMIT ?2 OFFSET ?3",
            crate::ROW_COLS
        ))?;
        let rows = stmt.query_map(
            params![
                server_state::ARCHIVED,
                i64::try_from(limit).unwrap_or(i64::MAX),
                i64::try_from(offset).unwrap_or(0)
            ],
            row_lite,
        )?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// 도구 대상(계획서 9.4): 폴더의 받은 시각 `[from, to)` 메일. (ID, 크기, 원문 있음, 서버에 있음), 오래된 것부터.
    pub fn messages_in_range(
        &self,
        folder_id: i64,
        from_ms: Option<i64>,
        to_ms: Option<i64>,
    ) -> Result<Vec<(i64, i64, bool, bool)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, COALESCE(size, 0), eml_hash IS NOT NULL, server_item_id IS NOT NULL FROM messages \
             WHERE folder_id = ?1 AND date_received >= ?2 AND date_received < ?3 \
             ORDER BY date_received, id",
        )?;
        let rows = stmt.query_map(
            params![
                folder_id,
                from_ms.unwrap_or(i64::MIN),
                to_ms.unwrap_or(i64::MAX)
            ],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// 이 폴더에서 서버에 있는 메일, 오래된 것부터(받은 편지함 자동 정리). (ID, 크기, 원문 있음, 받은 시각 ms)
    /// 보관(서버에서 지움)·가져온 메일(로컬 전용)은 빠진다.
    pub fn oldest_on_server_in(&self, folder_id: i64) -> Result<Vec<(i64, i64, bool, i64)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, COALESCE(size, 0), eml_hash IS NOT NULL, date_received FROM messages \
             WHERE folder_id = ?1 AND server_item_id IS NOT NULL AND server_state = 0 \
             ORDER BY date_received, id",
        )?;
        let rows = stmt.query_map([folder_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// 서버에 있는 메일 중 오래된 것부터(용량 확보 도우미). (ID, 크기, 원문 있음)
    pub fn oldest_on_server(&self, limit: usize) -> Result<Vec<(i64, i64, bool)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, COALESCE(size, 0), eml_hash IS NOT NULL FROM messages \
             WHERE server_item_id IS NOT NULL AND server_state = 0 ORDER BY date_received, id LIMIT ?1",
        )?;
        let rows = stmt.query_map([i64::try_from(limit).unwrap_or(i64::MAX)], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::ops::OP_DELETE;
    use crate::{MessageProps, ServerFolder, Store, server_state};

    fn setup() -> (tempfile::TempDir, Store, i64, i64, i64, i64) {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        let (inbox, other, a, b) = s
            .transaction(|tx| {
                let f = |id: &str| ServerFolder {
                    server_id: id.into(),
                    server_parent_id: Some("ROOT".into()),
                    name: id.into(),
                    folder_class: Some("IPF.Note".into()),
                };
                let inbox = tx.upsert_folder(&f("INBOX"))?;
                let other = tx.upsert_folder(&f("OTHER"))?;
                let p = |imid: &str| MessageProps {
                    internet_msg_id: Some(imid.into()),
                    date_received: 1000,
                    importance: 1,
                    ..MessageProps::default()
                };
                let a = tx.insert_message(inbox, "SA", None, &p("<a>"))?;
                let b = tx.insert_message(inbox, "SB", None, &p("<b>"))?;
                Ok((inbox, other, a, b))
            })
            .unwrap();
        (dir, s, inbox, other, a, b)
    }

    #[test]
    fn read_flag_move_queue_and_rollback() {
        let (_d, s, inbox, other, a, b) = setup();
        let t = s.mark_read(&[a, b], true, 1).unwrap();
        assert_eq!((t.queued, t.folders[&inbox].len()), (2, 2));
        assert!(s.message(a).unwrap().unwrap().props.is_read);
        assert_eq!(s.mark_read(&[a], true, 2).unwrap().queued, 0, "이미 읽음");
        s.mark_flag(&[a], 1, 3).unwrap();

        let t = s.move_messages(&[a], other, 4, None).unwrap();
        assert_eq!(t.folders.len(), 2);
        let row = s.message(a).unwrap().unwrap();
        assert_eq!(
            (row.folder_id, row.server_item_id.as_deref()),
            (other, Some("SA"))
        );

        // 서버가 끝내 거부한 이동·읽음을 되돌린다.
        for op in s.pending_ops().unwrap() {
            if op.message_id == Some(a) {
                s.rollback_op(&op).unwrap();
            }
        }
        let row = s.message(a).unwrap().unwrap();
        assert_eq!(
            (row.folder_id, row.props.is_read, row.props.flag),
            (inbox, false, 0)
        );
    }

    #[test]
    fn copy_server_delete_and_purge() {
        let (_d, s, inbox, other, a, b) = setup();
        assert!(s.attach_eml(a, "SA", b"raw").unwrap());
        let hash = s.message(a).unwrap().unwrap().eml_hash.unwrap();

        let t = s.copy_messages(&[a], other, 1, None).unwrap();
        let copy = t.created[0];
        let c = s.message(copy).unwrap().unwrap();
        assert_eq!(
            (
                c.folder_id,
                c.server_state,
                c.server_item_id,
                c.eml_hash.as_deref()
            ),
            (other, server_state::PENDING, None, Some(hash.as_str()))
        );
        assert_eq!(
            s.transaction(|tx| tx.pending_copy_in(other, "<a>", 1000))
                .unwrap(),
            Some(copy)
        );

        let t = s.server_delete(&[b], 2, None).unwrap();
        assert_eq!(t.queued, 1);
        let row = s.message(b).unwrap().unwrap();
        assert_eq!(
            (row.server_state, row.server_item_id.as_deref()),
            (1, Some("SB"))
        );
        let soft = s
            .pending_ops()
            .unwrap()
            .into_iter()
            .find(|o| o.kind == OP_DELETE)
            .unwrap();
        assert_eq!(soft.payload, json!({"mode": "soft"}));
        s.rollback_op(&soft).unwrap();
        assert_eq!(s.message(b).unwrap().unwrap().server_state, 0);

        // 원본을 완전 삭제해도 사본이 원문을 쓰므로 파일은 남는다.
        let t = s.purge_messages(&[a], 3, None).unwrap();
        assert_eq!((t.removed.as_slice(), t.queued), (&[a][..], 1));
        assert!(s.message(a).unwrap().is_none());
        assert!(s.eml().contains(&hash));
        assert!(s.pending_guard().unwrap().ignore_server_ids.contains("SA"));
        s.purge_messages(&[copy], 4, None).unwrap();
        assert!(
            !s.eml().contains(&hash),
            "마지막 행이 지워지면 원문도 지운다"
        );
        assert_eq!(s.archived_total().unwrap(), 0);
        assert_eq!(s.messages_in_range(inbox, None, None).unwrap().len(), 1);
    }
}
