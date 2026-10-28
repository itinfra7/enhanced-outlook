//! 서버 반영 대기 작업 큐(계획서 7장). 로컬에 먼저 반영한 조작을 여기 쌓고 동기화 엔진이 처리한다.
//!
//! - 같은 메일의 읽음·플래그 연속 작업은 최종값 1회로 합친다. 원래 값으로 되돌아가면 작업을 없앤다.
//! - 실패하면 지수 백오프(1s, 2s, 4s… 최대 5분). 네트워크 오류는 계속 재시도하고(오프라인),
//!   서버가 항목을 거부한 횟수(`item_errors`)가 [`MAX_ITEM_ERRORS`]에 이르면 포기하고 되돌린다.
//!
//! 작업 내용(`payload`):
//! - `set_read` `{"read": bool, "prev": bool}`, `set_flag` `{"flag": 0|1|2, "prev": ..}`
//! - `move` `{"to": 폴더, "from": 폴더}`(지운 편지함으로 삭제도 이동이다)
//! - `copy` `{"to": 폴더, "source": 원본 메일}`(작업의 메일 = 새 사본 행)
//! - `delete` `{"mode": "soft"}`(서버에서만 삭제, 로컬 보관) 또는
//!   `{"mode": "hard", "server_id": ".."}`(완전 삭제, 로컬 행은 이미 지움)

use rusqlite::{OptionalExtension, params};
use serde_json::Value;

use crate::{BatchProgress, OpRow, OpSummary, PendingGuard, Result, Store, Tx};

pub const OP_SET_READ: &str = "set_read";
pub const OP_SET_FLAG: &str = "set_flag";
pub const OP_MOVE: &str = "move";
pub const OP_COPY: &str = "copy";
pub const OP_DELETE: &str = "delete";
/// 발송(`{"draft": 작성 중 메일 번호}`). 메일 행이 아니라 작성 중 메일을 가리킨다.
pub const OP_SEND: &str = "send";
/// 서버 임시 보관함에 올리기(`{"draft": 작성 중 메일 번호}`, 0.7.0).
pub const OP_DRAFT_SAVE: &str = "draft_save";

/// 서버가 항목을 이만큼 거부하면 포기하고 로컬을 되돌린다.
pub const MAX_ITEM_ERRORS: i64 = 5;

/// 최종값만 의미 있는 작업(대기 중이면 덮어쓴다)과 그 값 필드.
const COALESCED: &[(&str, &str)] = &[(OP_SET_READ, "read"), (OP_SET_FLAG, "flag")];

const MAX_BACKOFF_MS: i64 = 5 * 60 * 1000;

/// `attempts`번째 실패 뒤 다음 시도까지 기다릴 시간.
pub fn op_backoff_ms(attempts: i64) -> i64 {
    let exp = u32::try_from(attempts.clamp(1, 20) - 1).unwrap_or(0);
    1000_i64.saturating_mul(1 << exp).min(MAX_BACKOFF_MS)
}

const STATE_PENDING: i64 = 0;
const STATE_DONE: i64 = 1;
const STATE_ABANDONED: i64 = 2;

const OP_COLS: &str = "id, kind, message_id, payload, created_at, attempts, next_attempt_at, \
     last_error, item_errors, batch";

fn op_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<(OpRow, String)> {
    Ok((
        OpRow {
            id: r.get(0)?,
            kind: r.get(1)?,
            message_id: r.get(2)?,
            payload: Value::Null,
            created_at: r.get(4)?,
            attempts: r.get(5)?,
            next_attempt_at: r.get(6)?,
            last_error: r.get(7)?,
            item_errors: r.get(8)?,
            batch: r.get(9)?,
        },
        r.get(3)?,
    ))
}

fn parse_rows(rows: impl Iterator<Item = rusqlite::Result<(OpRow, String)>>) -> Result<Vec<OpRow>> {
    let mut out = Vec::new();
    for row in rows {
        let (mut op, payload) = row?;
        op.payload = serde_json::from_str(&payload)?;
        out.push(op);
    }
    Ok(out)
}

impl Tx<'_> {
    /// 작업을 쌓는다. 읽음·플래그는 대기 중인 같은 작업에 합치고(원래 값은 처음 것을 유지),
    /// 원래 값으로 돌아갔으면 작업을 없앤다(`None`).
    pub fn enqueue_op(
        &self,
        kind: &str,
        message_id: Option<i64>,
        payload: &Value,
        now_ms: i64,
        batch: Option<i64>,
    ) -> Result<Option<i64>> {
        if let (Some((_, field)), Some(mid)) =
            (COALESCED.iter().find(|(k, _)| *k == kind), message_id)
        {
            let existing: Option<(i64, String)> = self
                .tx
                .query_row(
                    "SELECT id, payload FROM ops WHERE state = ?1 AND kind = ?2 AND message_id = ?3",
                    params![STATE_PENDING, kind, mid],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            if let Some((id, old)) = existing {
                let old: Value = serde_json::from_str(&old)?;
                let mut merged = payload.clone();
                if let Some(prev) = old.get("prev") {
                    merged["prev"] = prev.clone();
                }
                if merged.get(*field).is_some() && merged.get(*field) == merged.get("prev") {
                    // 서버에 반영하기 전에 원래 값으로 돌아왔다: 보낼 것이 없다.
                    self.tx.execute(
                        "UPDATE ops SET state = ?1, last_error = '취소(원래 값으로 돌아옴)' WHERE id = ?2",
                        params![STATE_DONE, id],
                    )?;
                    return Ok(None);
                }
                self.tx.execute(
                    "UPDATE ops SET payload = ?1, attempts = 0, item_errors = 0, next_attempt_at = ?2, \
                     last_error = NULL WHERE id = ?3",
                    params![serde_json::to_string(&merged)?, now_ms, id],
                )?;
                return Ok(Some(id));
            }
        }
        self.tx.execute(
            "INSERT INTO ops(kind, message_id, payload, created_at, next_attempt_at, batch) \
             VALUES (?1, ?2, ?3, ?4, ?4, ?5)",
            params![
                kind,
                message_id,
                serde_json::to_string(payload)?,
                now_ms,
                batch
            ],
        )?;
        Ok(Some(self.tx.last_insert_rowid()))
    }

    /// 이 메일의 대기 작업을 모두 끝난 것으로(메일을 로컬에서 지웠을 때).
    pub fn drop_pending_ops_for(&self, message_id: i64, reason: &str) -> Result<()> {
        self.tx.execute(
            "UPDATE ops SET state = ?1, last_error = ?2 WHERE state = ?3 AND message_id = ?4",
            params![STATE_DONE, reason, STATE_PENDING, message_id],
        )?;
        Ok(())
    }
}

impl Store {
    pub fn enqueue_op(
        &self,
        kind: &str,
        message_id: Option<i64>,
        payload: &Value,
        now_ms: i64,
    ) -> Result<Option<i64>> {
        self.transaction(|tx| tx.enqueue_op(kind, message_id, payload, now_ms, None))
    }

    /// 지금 시도할 작업(먼저 쌓인 순).
    pub fn due_ops(&self, now_ms: i64, limit: usize) -> Result<Vec<OpRow>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {OP_COLS} FROM ops WHERE state = ?1 AND next_attempt_at <= ?2 ORDER BY id LIMIT ?3"
        ))?;
        let rows = stmt.query_map(
            params![
                STATE_PENDING,
                now_ms,
                i64::try_from(limit).unwrap_or(i64::MAX)
            ],
            op_row,
        )?;
        parse_rows(rows)
    }

    /// 대기 중인 작업 전부(먼저 쌓인 순).
    pub fn pending_ops(&self) -> Result<Vec<OpRow>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {OP_COLS} FROM ops WHERE state = ?1 ORDER BY id"
        ))?;
        let rows = stmt.query_map([STATE_PENDING], op_row)?;
        parse_rows(rows)
    }

    pub fn op_done(&self, id: i64) -> Result<()> {
        self.conn().execute(
            "UPDATE ops SET state = ?1 WHERE id = ?2",
            params![STATE_DONE, id],
        )?;
        Ok(())
    }

    /// 실패 기록 후 백오프. `item_error`면 서버가 항목을 거부한 것(횟수를 센다).
    /// 누적 항목 오류 수를 돌려준다.
    pub fn op_failed(&self, id: i64, error: &str, now_ms: i64, item_error: bool) -> Result<i64> {
        self.transaction(|tx| {
            let (attempts, item_errors): (i64, i64) = tx.tx.query_row(
                "SELECT attempts, item_errors FROM ops WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            let attempts = attempts + 1;
            let item_errors = item_errors + i64::from(item_error);
            tx.tx.execute(
                "UPDATE ops SET attempts = ?1, item_errors = ?2, next_attempt_at = ?3, last_error = ?4 \
                 WHERE id = ?5",
                params![
                    attempts,
                    item_errors,
                    now_ms + op_backoff_ms(attempts),
                    error,
                    id
                ],
            )?;
            Ok(item_errors)
        })
    }

    /// 더 시도하지 않는다(예: 서버가 계속 거부). 로컬 되돌리기는 [`Store::rollback_op`]로.
    pub fn op_abandon(&self, id: i64, error: &str) -> Result<()> {
        self.conn().execute(
            "UPDATE ops SET state = ?1, last_error = ?2 WHERE id = ?3",
            params![STATE_ABANDONED, error, id],
        )?;
        Ok(())
    }

    /// 다시 연결됐을 때: 기다리던 작업을 모두 지금 시도하게 한다(백오프 초기화).
    pub fn retry_ops_now(&self, now_ms: i64) -> Result<usize> {
        Ok(self.conn().execute(
            "UPDATE ops SET next_attempt_at = ?1 WHERE state = ?2 AND next_attempt_at > ?1",
            params![now_ms, STATE_PENDING],
        )?)
    }

    pub fn pending_op_count(&self) -> Result<i64> {
        Ok(self.conn().query_row(
            "SELECT COUNT(*) FROM ops WHERE state = ?1",
            [STATE_PENDING],
            |r| r.get(0),
        )?)
    }

    /// 동기화 반영 때 지킬 것(대기 작업이 있는 메일).
    pub fn pending_guard(&self) -> Result<PendingGuard> {
        let mut g = PendingGuard::default();
        for op in self.pending_ops()? {
            let mid = op.message_id;
            match (op.kind.as_str(), mid) {
                (OP_SET_READ, Some(m)) => {
                    g.keep_read.insert(m);
                }
                (OP_SET_FLAG, Some(m)) => {
                    g.keep_flag.insert(m);
                }
                (OP_MOVE, Some(m)) => {
                    g.keep_folder.insert(m);
                }
                (OP_DELETE, m) => match op.payload.get("mode").and_then(Value::as_str) {
                    Some("hard") => {
                        if let Some(sid) = op.payload.get("server_id").and_then(Value::as_str) {
                            g.ignore_server_ids.insert(sid.to_owned());
                        }
                    }
                    _ => {
                        if let Some(m) = m {
                            g.keep_state.insert(m);
                        }
                    }
                },
                _ => {}
            }
        }
        Ok(g)
    }

    /// 작업 큐 요약(종류·상태별 수와 마지막 오류 하나). 진단용.
    pub fn op_summary(&self) -> Result<Vec<OpSummary>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT kind, state, COUNT(*), MAX(last_error) FROM ops GROUP BY kind, state ORDER BY kind, state",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(OpSummary {
                kind: r.get(0)?,
                state: r.get(1)?,
                count: r.get(2)?,
                last_error: r.get(3)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// 새 일괄 작업 번호.
    pub fn new_batch(&self) -> Result<i64> {
        let n: i64 = self
            .meta("ops_batch_seq")?
            .and_then(|s| s.parse().ok())
            .unwrap_or(0)
            + 1;
        self.set_meta("ops_batch_seq", &n.to_string())?;
        Ok(n)
    }

    pub fn batch_progress(&self, batch: i64) -> Result<BatchProgress> {
        let conn = self.conn();
        let mut p = BatchProgress::default();
        let mut stmt =
            conn.prepare("SELECT state, COUNT(*) FROM ops WHERE batch = ?1 GROUP BY state")?;
        let rows = stmt.query_map([batch], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?;
        for row in rows {
            match row? {
                (STATE_PENDING, n) => p.pending = n,
                (STATE_DONE, n) => p.done = n,
                (STATE_ABANDONED, n) => p.abandoned = n,
                _ => {}
            }
        }
        Ok(p)
    }

    /// 일괄 작업 중지: 아직 서버에 보내지 않은 작업을 포기로 돌리고 돌려준다(호출한 쪽이 되돌린다).
    pub fn cancel_batch(&self, batch: i64) -> Result<Vec<OpRow>> {
        let pending: Vec<OpRow> = self
            .pending_ops()?
            .into_iter()
            .filter(|o| o.batch == Some(batch))
            .collect();
        self.transaction(|tx| {
            for o in &pending {
                tx.tx.execute(
                    "UPDATE ops SET state = ?1, last_error = '사용자가 중지' WHERE id = ?2",
                    params![STATE_ABANDONED, o.id],
                )?;
            }
            Ok(())
        })?;
        Ok(pending)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn read_toggles_coalesce_to_final_value_and_cancel_when_back() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        let a = s
            .enqueue_op(
                OP_SET_READ,
                Some(7),
                &json!({"read": true, "prev": false}),
                1,
            )
            .unwrap()
            .unwrap();
        let b = s
            .enqueue_op(
                OP_SET_READ,
                Some(7),
                &json!({"read": false, "prev": true}),
                2,
            )
            .unwrap();
        assert_eq!(b, None, "원래 값(안읽음)으로 돌아오면 작업이 없어진다");
        assert_eq!(s.pending_op_count().unwrap(), 0);
        let c = s
            .enqueue_op(
                OP_SET_READ,
                Some(7),
                &json!({"read": true, "prev": false}),
                3,
            )
            .unwrap()
            .unwrap();
        assert_ne!(a, c);
        let flag = s
            .enqueue_op(OP_SET_FLAG, Some(7), &json!({"flag": 1, "prev": 0}), 3)
            .unwrap()
            .unwrap();
        assert_eq!(
            s.enqueue_op(OP_SET_FLAG, Some(7), &json!({"flag": 2, "prev": 1}), 4)
                .unwrap(),
            Some(flag)
        );
        s.enqueue_op(OP_MOVE, Some(7), &json!({"to": 2, "from": 1}), 4)
            .unwrap();
        s.enqueue_op(OP_MOVE, Some(7), &json!({"to": 3, "from": 2}), 5)
            .unwrap();
        let due = s.due_ops(10, 100).unwrap();
        assert_eq!(due.len(), 4, "이동은 합치지 않는다");
        assert_eq!(
            due[1].payload,
            json!({"flag": 2, "prev": 0}),
            "원래 값은 처음 것"
        );
        let g = s.pending_guard().unwrap();
        assert!(g.keep_read.contains(&7) && g.keep_flag.contains(&7) && g.keep_folder.contains(&7));
    }

    #[test]
    fn failures_back_off_and_count_item_errors_separately() {
        assert_eq!(
            (1..=4).map(op_backoff_ms).collect::<Vec<_>>(),
            [1000, 2000, 4000, 8000]
        );
        assert_eq!(op_backoff_ms(30), 300_000);

        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        let id = s
            .enqueue_op(OP_DELETE, Some(1), &json!({"mode": "soft"}), 0)
            .unwrap()
            .unwrap();
        assert_eq!(s.op_failed(id, "연결 끊김", 100, false).unwrap(), 0);
        assert!(s.due_ops(1099, 10).unwrap().is_empty());
        assert_eq!(
            s.due_ops(1100, 10).unwrap()[0].last_error.as_deref(),
            Some("연결 끊김")
        );
        assert_eq!(s.op_failed(id, "ErrorX", 1100, true).unwrap(), 1);
        assert!(s.due_ops(3099, 10).unwrap().is_empty());
        assert_eq!(s.retry_ops_now(3000).unwrap(), 1, "다시 연결되면 바로 시도");
        assert_eq!(s.due_ops(3000, 10).unwrap().len(), 1);
        assert!(s.pending_guard().unwrap().keep_state.contains(&1));
        s.op_done(id).unwrap();
        assert_eq!(s.pending_op_count().unwrap(), 0);

        let hard = s
            .enqueue_op(
                OP_DELETE,
                None,
                &json!({"mode": "hard", "server_id": "S9"}),
                0,
            )
            .unwrap()
            .unwrap();
        assert!(s.pending_guard().unwrap().ignore_server_ids.contains("S9"));
        s.op_abandon(hard, "포기").unwrap();
        assert!(s.due_ops(i64::MAX, 10).unwrap().is_empty());
    }

    #[test]
    fn batches_report_progress_and_cancel() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        let b = s.new_batch().unwrap();
        assert_eq!(s.new_batch().unwrap(), b + 1);
        let ids: Vec<i64> = (0..3)
            .map(|m| {
                s.transaction(|tx| {
                    tx.enqueue_op(OP_DELETE, Some(m), &json!({"mode": "soft"}), 0, Some(b))
                })
                .unwrap()
                .unwrap()
            })
            .collect();
        s.op_done(ids[0]).unwrap();
        assert_eq!(
            s.batch_progress(b).unwrap(),
            BatchProgress {
                pending: 2,
                done: 1,
                abandoned: 0
            }
        );
        let cancelled = s.cancel_batch(b).unwrap();
        assert_eq!(cancelled.len(), 2);
        assert_eq!(s.batch_progress(b).unwrap().abandoned, 2);
        assert_eq!(s.pending_op_count().unwrap(), 0);
    }
}
