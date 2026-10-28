//! 작업 현황 창(0.7.1 사용자 요청: 상태 표시줄의 "동기화 중"·"서버 반영 대기"를 누르면 무엇을 하는지 보기).
//! 지금 하는 일은 화면이 상태(`status`)로 그리고, 여기서는 대기·실패 작업을 사람이 읽는 말로 바꿔 준다.

use std::collections::HashMap;
use std::sync::atomic::Ordering;

use eo_store::{
    ActivityOp, OP_COPY, OP_DELETE, OP_DRAFT_SAVE, OP_MOVE, OP_SEND, OP_SET_FLAG, OP_SET_READ,
    domino_op,
};
use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Manager};

use crate::state::AppState;

type CmdResult<T> = Result<T, String>;

/// 대기 작업은 앞의 이만큼만 한 줄씩 보인다(나머지는 종류별 수로).
const SHOW_PENDING: usize = 200;
/// 실패(포기)한 작업은 최근 7일 안의 이만큼.
const SHOW_FAILED: usize = 30;
const FAILED_DAYS: i64 = 7;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpLine {
    /// "Outlook" | "HCL Domino"
    pub source: &'static str,
    /// 무엇을(예: "읽음으로 표시", "이동")
    pub what: String,
    pub subject: Option<String>,
    /// 폴더 등(예: "받은 편지함 → 숙지")
    pub detail: String,
    pub created_at: i64,
    pub attempts: i64,
    /// 다음 시도 시각(기다리는 중일 때만)
    pub next_at: Option<i64>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KindCount {
    pub what: String,
    pub count: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityView {
    pub outlook: Vec<OpLine>,
    pub outlook_total: i64,
    pub outlook_kinds: Vec<KindCount>,
    pub domino: Vec<OpLine>,
    pub domino_total: i64,
    pub failed: Vec<OpLine>,
    /// 검색 색인을 기다리는 메일 수
    pub index_pending: i64,
    pub importing: bool,
}

fn outlook_kind(kind: &str) -> &str {
    match kind {
        OP_SET_READ => "읽음·안 읽음 표시",
        OP_SET_FLAG => "플래그",
        OP_MOVE => "이동·삭제",
        OP_COPY => "복사",
        OP_DELETE => "서버에서 삭제",
        OP_SEND => "보내기",
        OP_DRAFT_SAVE => "임시 보관함에 저장",
        k => k,
    }
}

fn outlook_line(o: ActivityOp, names: &HashMap<i64, (String, Option<String>)>, now: i64) -> OpLine {
    let p: Value = serde_json::from_str(&o.payload).unwrap_or(Value::Null);
    let folder = |key: &str| {
        p.get(key)
            .and_then(Value::as_i64)
            .and_then(|id| names.get(&id))
    };
    let name = |key: &str| folder(key).map_or_else(|| "?".to_owned(), |f| f.0.clone());
    let (what, detail) = match o.kind.as_str() {
        OP_SET_READ if p.get("read") == Some(&Value::Bool(true)) => {
            ("읽음으로 표시".into(), String::new())
        }
        OP_SET_READ => ("안 읽음으로 표시".into(), String::new()),
        OP_SET_FLAG => (
            match p.get("flag").and_then(Value::as_i64) {
                Some(1) => "플래그 표시",
                Some(2) => "완료로 표시",
                _ => "플래그 지우기",
            }
            .into(),
            String::new(),
        ),
        OP_MOVE if folder("to").is_some_and(|f| f.1.as_deref() == Some("deleteditems")) => {
            ("삭제(지운 편지함으로)".into(), name("from"))
        }
        OP_MOVE => ("이동".into(), format!("{} → {}", name("from"), name("to"))),
        OP_COPY => ("복사".into(), format!("→ {}", name("to"))),
        OP_DELETE if p.get("mode").and_then(Value::as_str) == Some("hard") => {
            ("완전 삭제".into(), String::new())
        }
        OP_DELETE => ("서버에서 삭제(로컬 보관)".into(), String::new()),
        k => (outlook_kind(k).to_owned(), String::new()),
    };
    let detail = if detail.is_empty() {
        o.folder_id
            .and_then(|f| names.get(&f))
            .map(|f| f.0.clone())
            .unwrap_or_default()
    } else {
        detail
    };
    OpLine {
        source: "Outlook",
        what,
        subject: o.subject,
        detail,
        created_at: o.created_at,
        attempts: o.attempts,
        next_at: (o.next_attempt_at > now).then_some(o.next_attempt_at),
        error: o.last_error,
    }
}

fn domino_line(o: ActivityOp, names: &HashMap<i64, (String, Option<String>)>, now: i64) -> OpLine {
    let on = o.payload == "1";
    let (what, detail) = match o.kind.as_str() {
        domino_op::READ if on => ("읽음으로 표시", String::new()),
        domino_op::READ => ("안 읽음으로 표시", String::new()),
        domino_op::STAR if on => ("★ 표시", String::new()),
        domino_op::STAR => ("★ 지우기", String::new()),
        domino_op::DELETE => ("삭제", String::new()),
        domino_op::RECOVER => ("복구(받은 메일로)", String::new()),
        domino_op::FOLDER_MOVE => ("개인 폴더로 이동", format!("→ {}", o.payload)),
        domino_op::MOVE_BACK => ("원래 메일함으로 이동", String::new()),
        k => (k, String::new()),
    };
    let detail = if detail.is_empty() {
        o.folder_id
            .and_then(|f| names.get(&f))
            .map(|f| f.0.clone())
            .unwrap_or_default()
    } else {
        detail
    };
    OpLine {
        source: "HCL Domino",
        what: what.to_owned(),
        subject: o.subject,
        detail,
        created_at: o.created_at,
        attempts: o.attempts,
        next_at: (o.next_attempt_at > now).then_some(o.next_attempt_at),
        error: o.last_error,
    }
}

#[tauri::command]
pub async fn activity_view(app: AppHandle) -> CmdResult<ActivityView> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let s = &st.store;
        let run = || -> eo_store::Result<ActivityView> {
            let now = crate::commands::now_ms();
            let since = now - FAILED_DAYS * 86_400_000;
            let names: HashMap<i64, (String, Option<String>)> = s
                .folders()?
                .into_iter()
                .map(|f| (f.id, (f.name, f.well_known)))
                .collect();
            let outlook = s
                .activity_ops(false, 0, SHOW_PENDING)?
                .into_iter()
                .map(|o| outlook_line(o, &names, now))
                .collect();
            let domino = s
                .activity_domino_ops(false, 0, SHOW_PENDING)?
                .into_iter()
                .map(|o| domino_line(o, &names, now))
                .collect();
            let mut failed: Vec<OpLine> = s
                .activity_ops(true, since, SHOW_FAILED)?
                .into_iter()
                .map(|o| outlook_line(o, &names, now))
                .chain(
                    s.activity_domino_ops(true, since, SHOW_FAILED)?
                        .into_iter()
                        .map(|o| domino_line(o, &names, now)),
                )
                .collect();
            failed.sort_by_key(|o| std::cmp::Reverse(o.created_at));
            failed.truncate(SHOW_FAILED);
            Ok(ActivityView {
                outlook,
                outlook_total: s.pending_op_count()?,
                outlook_kinds: s
                    .pending_op_kinds()?
                    .into_iter()
                    .map(|(k, count)| KindCount {
                        what: outlook_kind(&k).to_owned(),
                        count,
                    })
                    .collect(),
                domino,
                domino_total: s.domino_pending_count()?,
                failed,
                index_pending: s.unindexed_count()?,
                importing: st.import_running.load(Ordering::Relaxed),
            })
        };
        run().map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}
