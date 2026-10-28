//! 작업 큐 실행(계획서 7장): 로컬에 먼저 반영한 조작을 서버에 보낸다.
//!
//! - 쌓인 순서대로, 같은 종류(·대상)끼리 묶어 보낸다(읽음·플래그 10건, 이동·복사·삭제 50건).
//! - 네트워크 오류: 그 회차를 멈추고 백오프로 계속 재시도(오프라인). 다시 연결되면 바로 시도.
//! - 서버에 항목이 없음(`ErrorItemNotFound`): 다른 곳에서 지워진 것. 로컬은 보관으로 둔다.
//! - 그 밖의 항목 오류: [`MAX_ITEM_ERRORS`]번이면 포기하고 로컬을 되돌린 뒤 알린다.
//! - 같은 회차에서 앞 작업이 실패한 메일의 뒤 작업은 다음으로 미룬다(순서 보장).

use std::collections::HashSet;

use eo_owa::{DeleteType, FolderRef, ItemError, ItemId};
use eo_store::{
    MAX_ITEM_ERRORS, MessageRow, OP_COPY, OP_DELETE, OP_DRAFT_SAVE, OP_MOVE, OP_SEND, OP_SET_FLAG,
    OP_SET_READ, OpRow, Touched, server_state,
};
use serde_json::Value;

use crate::engine::{Engine, Event, now_ms};
use crate::{Error, Result};

const NOT_FOUND: &str = "ErrorItemNotFound";

#[derive(Debug, Clone, Default)]
pub struct OpsReport {
    pub done: usize,
    pub retry: usize,
    /// 포기하고 되돌린 작업과 사유.
    pub abandoned: Vec<(OpRow, String)>,
    pub touched: Touched,
    /// 네트워크 오류로 멈췄다.
    pub offline: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Outcome {
    Done,
    /// 서버에 항목이 없다(다른 곳에서 지워짐).
    NotFound,
    /// 서버가 거부(횟수를 센다).
    Rejected(String),
    /// 아직 보낼 수 없다(예: 서버 반영 전 사본). 백오프 뒤 다시.
    Postpone(String),
    /// 다시 해도 소용없다(예: 대상 폴더가 서버에 없음). 바로 포기·되돌림.
    Fatal(String),
}

fn from_item<T>(r: std::result::Result<T, ItemError>) -> (Outcome, Option<T>) {
    match r {
        Ok(v) => (Outcome::Done, Some(v)),
        Err(e) if e.code == NOT_FOUND => (Outcome::NotFound, None),
        Err(e) => (Outcome::Rejected(e.to_string()), None),
    }
}

/// 묶어 보낼 수 있는 작업인가: 종류 + 대상 폴더/삭제 방식이 같아야 한다.
fn run_key(op: &OpRow) -> String {
    let p = &op.payload;
    match op.kind.as_str() {
        OP_MOVE | OP_COPY => format!(
            "{}:{}",
            op.kind,
            p.get("to").and_then(Value::as_i64).unwrap_or(0)
        ),
        OP_DELETE => format!(
            "delete:{}",
            p.get("mode").and_then(Value::as_str).unwrap_or("")
        ),
        // 발송·임시 보관함 저장은 한 통씩.
        OP_SEND | OP_DRAFT_SAVE => format!("{}:{}", op.kind, op.id),
        k => k.to_owned(),
    }
}

fn batch_size(kind: &str) -> usize {
    match kind {
        OP_SET_READ | OP_SET_FLAG => 10,
        OP_SEND | OP_DRAFT_SAVE => 1,
        _ => 50,
    }
}

/// 연결이 다시 되면 해결될 오류인가(나머지는 항목 오류처럼 센다).
pub(crate) fn transient(e: &Error) -> bool {
    matches!(
        e,
        Error::Owa(
            eo_owa::Error::Http(_) | eo_owa::Error::Status { .. } | eo_owa::Error::SessionExpired
        )
    )
}

fn bare(id: &str) -> ItemId {
    ItemId {
        id: id.to_owned(),
        change_key: None,
    }
}

impl Engine {
    fn row(&self, op: &OpRow) -> Result<Option<MessageRow>> {
        Ok(match op.message_id {
            Some(m) => self.store().message(m)?,
            None => None,
        })
    }

    /// 서버 항목 ID가 있어야 하는 작업의 대상 행. 없으면 그 결과(완료·미룸).
    fn target(&self, op: &OpRow) -> Result<std::result::Result<(MessageRow, String), Outcome>> {
        let Some(row) = self.row(op)? else {
            return Ok(Err(Outcome::Done)); // 로컬에서 지워짐
        };
        Ok(match row.server_item_id.clone() {
            Some(sid) => Ok((row, sid)),
            None if row.server_state == server_state::PENDING => {
                Err(Outcome::Postpone("서버 반영을 기다리는 사본".into()))
            }
            None => Err(Outcome::Done), // 서버에 없는 메일: 로컬만
        })
    }

    async fn execute_run(&self, run: &[OpRow]) -> Result<Vec<Outcome>> {
        let mut out = vec![Outcome::Done; run.len()];
        let kind = run[0].kind.as_str();
        if kind == OP_SEND || kind == OP_DRAFT_SAVE {
            let draft = run[0]
                .payload
                .get("draft")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            return Ok(vec![if kind == OP_SEND {
                self.send_draft(draft).await?
            } else {
                self.save_draft_to_server(draft).await?
            }]);
        }
        let mut targets: Vec<(usize, MessageRow, String)> = Vec::new();
        let mut pick = |k: usize, op: &OpRow, out: &mut Vec<Outcome>| -> Result<()> {
            match self.target(op)? {
                Ok((row, sid)) => targets.push((k, row, sid)),
                Err(o) => out[k] = o,
            }
            Ok(())
        };
        match kind {
            OP_SET_READ | OP_SET_FLAG => {
                for (k, op) in run.iter().enumerate() {
                    pick(k, op, &mut out)?;
                }
                if targets.is_empty() {
                    return Ok(out);
                }
                let results = if kind == OP_SET_READ {
                    let items: Vec<(ItemId, bool)> = targets
                        .iter()
                        .map(|(k, _, sid)| {
                            (
                                bare(sid),
                                run[*k]
                                    .payload
                                    .get("read")
                                    .and_then(Value::as_bool)
                                    .unwrap_or(true),
                            )
                        })
                        .collect();
                    self.client().set_read(&items).await?
                } else {
                    let items: Vec<(ItemId, i64)> = targets
                        .iter()
                        .map(|(k, _, sid)| {
                            (
                                bare(sid),
                                run[*k]
                                    .payload
                                    .get("flag")
                                    .and_then(Value::as_i64)
                                    .unwrap_or(0),
                            )
                        })
                        .collect();
                    self.client().set_flag(&items).await?
                };
                for (j, (k, _, _)) in targets.iter().enumerate() {
                    out[*k] = match results.get(j) {
                        Some(r) => from_item(r.clone()).0,
                        None => Outcome::Rejected("응답에 결과가 없음".into()),
                    };
                }
            }
            OP_MOVE => {
                let to = run[0]
                    .payload
                    .get("to")
                    .and_then(Value::as_i64)
                    .unwrap_or(0);
                let Some(server_folder) = self.store().folder(to)?.and_then(|f| f.server_id) else {
                    return Ok(vec![
                        Outcome::Fatal("대상 폴더가 서버에 없습니다".into());
                        run.len()
                    ]);
                };
                for (k, op) in run.iter().enumerate() {
                    pick(k, op, &mut out)?;
                }
                if targets.is_empty() {
                    return Ok(out);
                }
                let ids: Vec<ItemId> = targets.iter().map(|(_, _, sid)| bare(sid)).collect();
                let results = self
                    .client()
                    .move_items(&ids, &FolderRef::Id(server_folder))
                    .await?;
                for (j, (k, row, _)) in targets.iter().enumerate() {
                    let (o, new_id) = match results.get(j) {
                        Some(r) => from_item(r.clone()),
                        None => (Outcome::Rejected("응답에 결과가 없음".into()), None),
                    };
                    if let Some(Some(new_id)) = new_id {
                        self.store().transaction(|tx| {
                            tx.set_server_item(row.id, &new_id.id, new_id.change_key.as_deref())
                        })?;
                    }
                    out[*k] = o;
                }
            }
            OP_COPY => {
                let to = run[0]
                    .payload
                    .get("to")
                    .and_then(Value::as_i64)
                    .unwrap_or(0);
                let Some(server_folder) = self.store().folder(to)?.and_then(|f| f.server_id) else {
                    return Ok(vec![
                        Outcome::Fatal("대상 폴더가 서버에 없습니다".into());
                        run.len()
                    ]);
                };
                // (작업 번호, 사본 행 ID, 원본 서버 ID)
                let mut copies: Vec<(usize, i64, String)> = Vec::new();
                for (k, op) in run.iter().enumerate() {
                    let Some(copy) = self.row(op)? else { continue };
                    if copy.server_state != server_state::PENDING || copy.server_item_id.is_some() {
                        continue; // 이미 서버에 있거나 올리지 않기로 함
                    }
                    let source = op
                        .payload
                        .get("source")
                        .and_then(Value::as_i64)
                        .map(|s| self.store().message(s))
                        .transpose()?
                        .flatten()
                        .and_then(|s| s.server_item_id);
                    match source {
                        Some(sid) => copies.push((k, copy.id, sid)),
                        None => {
                            // 원본이 서버에 없다: 사본은 로컬 보관으로 둔다.
                            let now = now_ms();
                            self.store()
                                .transaction(|tx| tx.archive_message(copy.id, now))?;
                            out[k] = Outcome::Done;
                        }
                    }
                }
                if copies.is_empty() {
                    return Ok(out);
                }
                let ids: Vec<ItemId> = copies.iter().map(|(_, _, sid)| bare(sid)).collect();
                let results = self
                    .client()
                    .copy_items(&ids, &FolderRef::Id(server_folder))
                    .await?;
                for (j, (k, copy_id, _)) in copies.iter().enumerate() {
                    let (o, new_id) = match results.get(j) {
                        Some(r) => from_item(r.clone()),
                        None => (Outcome::Rejected("응답에 결과가 없음".into()), None),
                    };
                    // 새 ID를 주지 않으면 다음 동기화가 같은 메일 사본과 잇는다(apply의 사본 연결).
                    if let Some(Some(new_id)) = new_id {
                        self.store().transaction(|tx| {
                            tx.set_server_item(*copy_id, &new_id.id, new_id.change_key.as_deref())
                        })?;
                    }
                    out[*k] = o;
                }
            }
            OP_DELETE => {
                let hard = run[0].payload.get("mode").and_then(Value::as_str) == Some("hard");
                // (작업 번호, 로컬 행(있으면), 서버 ID)
                let mut dels: Vec<(usize, Option<i64>, String)> = Vec::new();
                for (k, op) in run.iter().enumerate() {
                    if hard {
                        match op.payload.get("server_id").and_then(Value::as_str) {
                            Some(sid) => dels.push((k, None, sid.to_owned())),
                            None => out[k] = Outcome::Done,
                        }
                    } else {
                        match self.target(op)? {
                            Ok((row, sid)) => dels.push((k, Some(row.id), sid)),
                            Err(o) => out[k] = o,
                        }
                    }
                }
                if dels.is_empty() {
                    return Ok(out);
                }
                let ids: Vec<ItemId> = dels.iter().map(|(_, _, sid)| bare(sid)).collect();
                let t = if hard {
                    DeleteType::HardDelete
                } else {
                    DeleteType::SoftDelete
                };
                let results = self.client().delete_items(&ids, t).await?;
                let now = now_ms();
                for (j, (k, row, _)) in dels.iter().enumerate() {
                    let o = match results.get(j) {
                        Some(r) => from_item(r.clone()).0,
                        None => Outcome::Rejected("응답에 결과가 없음".into()),
                    };
                    if let (Outcome::Done | Outcome::NotFound, Some(row)) = (&o, row) {
                        self.store()
                            .transaction(|tx| tx.archive_message(*row, now))?;
                    }
                    out[*k] = o;
                }
            }
            other => {
                return Ok(vec![
                    Outcome::Fatal(format!("알 수 없는 작업 {other}"));
                    run.len()
                ]);
            }
        }
        Ok(out)
    }

    fn settle(
        &self,
        op: &OpRow,
        outcome: Outcome,
        report: &mut OpsReport,
        blocked: &mut HashSet<i64>,
    ) -> Result<()> {
        let now = now_ms();
        match outcome {
            Outcome::Done => {
                self.store().op_done(op.id)?;
                report.done += 1;
            }
            Outcome::NotFound => {
                // 다른 곳(웹메일 등)에서 지워졌다: 로컬은 보관으로 남긴다.
                if let Some(row) = self.row(op)?
                    && row.server_item_id.is_some()
                {
                    self.store()
                        .transaction(|tx| tx.archive_message(row.id, now))?;
                    report.touched.merge(touched_one(row.folder_id, row.id));
                }
                self.store().op_done(op.id)?;
                report.done += 1;
            }
            Outcome::Postpone(why) => {
                self.store().op_failed(op.id, &why, now, false)?;
                report.retry += 1;
                blocked.extend(op.message_id);
            }
            Outcome::Rejected(why) => {
                let n = self.store().op_failed(op.id, &why, now, true)?;
                blocked.extend(op.message_id);
                if n >= MAX_ITEM_ERRORS {
                    self.abandon(op, why, report)?;
                } else {
                    report.retry += 1;
                }
            }
            Outcome::Fatal(why) => {
                blocked.extend(op.message_id);
                self.abandon(op, why, report)?;
            }
        }
        Ok(())
    }

    fn abandon(&self, op: &OpRow, why: String, report: &mut OpsReport) -> Result<()> {
        self.store().op_abandon(op.id, &why)?;
        report.touched.merge(self.store().rollback_op(op)?);
        report.abandoned.push((op.clone(), why));
        Ok(())
    }

    /// 지금 시도할 작업을 한 회차 처리한다.
    pub async fn process_ops(&self) -> Result<OpsReport> {
        let now = now_ms();
        let ops = self.store().due_ops(now, 200)?;
        let mut report = OpsReport::default();
        if ops.is_empty() {
            return Ok(report);
        }
        let mut blocked: HashSet<i64> = HashSet::new();
        let mut i = 0;
        while i < ops.len() {
            let key = run_key(&ops[i]);
            let max = batch_size(&ops[i].kind);
            let mut run = Vec::new();
            while i < ops.len() && run_key(&ops[i]) == key && run.len() < max {
                let op = &ops[i];
                i += 1;
                if op.message_id.is_some_and(|m| blocked.contains(&m)) {
                    continue;
                }
                run.push(op.clone());
            }
            if run.is_empty() {
                continue;
            }
            match self.execute_run(&run).await {
                Ok(outcomes) => {
                    for (op, outcome) in run.iter().zip(outcomes) {
                        self.settle(op, outcome, &mut report, &mut blocked)?;
                    }
                }
                Err(Error::Owa(eo_owa::Error::LoginRequired)) => {
                    self.emit(Event::LoginRequired);
                    return Err(Error::Owa(eo_owa::Error::LoginRequired));
                }
                Err(e) if transient(&e) => {
                    let why = e.to_string();
                    for op in &run {
                        self.store().op_failed(op.id, &why, now, false)?;
                        report.retry += 1;
                    }
                    report.offline = true;
                    break; // 연결이 안 되면 이번 회차는 멈춘다
                }
                Err(e) => {
                    let why = e.to_string();
                    for op in &run {
                        self.settle(
                            op,
                            Outcome::Rejected(why.clone()),
                            &mut report,
                            &mut blocked,
                        )?;
                    }
                }
            }
        }
        self.finish_ops(&report)?;
        Ok(report)
    }

    fn finish_ops(&self, report: &OpsReport) -> Result<()> {
        if let Some(search) = self.search()
            && !report.touched.removed.is_empty()
        {
            for id in &report.touched.removed {
                search.delete(*id as u64)?;
            }
            search.commit()?;
        }
        for (folder_id, ids) in &report.touched.folders {
            self.emit(Event::MessagesChanged {
                folder_id: *folder_id,
                message_ids: ids.clone(),
            });
        }
        for (op, why) in &report.abandoned {
            self.emit(Event::OpFailed {
                message_id: op.message_id,
                kind: op.kind.clone(),
                error: why.clone(),
            });
        }
        self.emit(Event::OpsPending(self.store().pending_op_count()?));
        Ok(())
    }
}

fn touched_one(folder_id: i64, id: i64) -> Touched {
    let mut t = Touched::default();
    t.folders.insert(folder_id, vec![id]);
    t
}
