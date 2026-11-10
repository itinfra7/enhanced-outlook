//! HCL Domino 리다이렉트 사본 자동 삭제(1.1.0 사용자 요청). 설정 → HCL Domino에서 켠다.
//!
//! 회사 정책으로 Outlook에 온 메일이 HCL Domino에도 똑같이 한 통 더 온다. 그 HCL Domino 사본만 서버와 로컬에서 지운다
//! (Outlook 메일은 읽기만 한다). 짝 찾기는 `eo_sync::find_redirect_copies`(제목·보낸 사람·2분·1:1·본문·첨부).
//! 지우기 직전에 HCL Domino 문서를 다시 열어 Notes 안에서 보낸 메일이 아님을 확인한다(`SenderText`가 비고
//! 보낸 사람이 `CN=…` 꼴이 아님, 2026-11-10 실서버 확인: 리다이렉트 사본은 비어 있고 Notes 메일은 `CN=이름 사번/O=…`).
//! 웹메일에서는 받은 메일(또는 외부 메일) → 지운 메일 → 완전 삭제. 로컬은 행·원문을 지우고, 가져온 기록은 남겨
//! 다시 가져오지 않는다.

use std::collections::HashSet;
use std::sync::Mutex;

use eo_owa::domino::Mailbox;
use serde::Serialize;
use tauri::{AppHandle, Manager};

use crate::applog;
use crate::commands::now_ms;
use crate::state::{AppState, lock};

type CmdResult<T> = Result<T, String>;

/// 한 번에 지울 최대 수(처음 켤 때 쌓인 사본도 몇 번에 나눠 지운다).
const MAX_PER_RUN: usize = 200;

/// 서버 확인에서 Notes 메일로 나와 건너뛴 문서(앱을 다시 켤 때까지 다시 열지 않는다).
static SKIPPED: Mutex<Option<HashSet<String>>> = Mutex::new(None);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RedirectItem {
    pub subject: String,
    pub from: String,
    pub date: i64,
    /// 받은 메일·외부 메일.
    pub mailbox: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RedirectPreview {
    /// 지금 지울 사본 수.
    pub count: usize,
    /// 앞쪽 몇 통(최신순).
    pub items: Vec<RedirectItem>,
    /// 짝 후보가 여럿이라 남기는 수.
    pub ambiguous: usize,
}

fn mailbox_label(key: &str) -> &'static str {
    if key == "XML_POP3" {
        "외부 메일"
    } else {
        "받은 메일"
    }
}

fn scan(st: &AppState) -> CmdResult<eo_sync::RedirectScan> {
    let mut s = eo_sync::find_redirect_copies(&st.store).map_err(|e| e.to_string())?;
    if let Some(skipped) = lock(&SKIPPED).as_ref() {
        s.copies.retain(|c| !skipped.contains(&c.domino.unid));
    }
    s.copies.sort_by_key(|c| std::cmp::Reverse(c.domino.date));
    Ok(s)
}

/// 설정 화면: 켜면 지금 지울 사본(읽기만).
#[tauri::command]
pub async fn domino_redirects_preview(app: AppHandle) -> CmdResult<RedirectPreview> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let s = scan(&st)?;
        Ok(RedirectPreview {
            count: s.copies.len(),
            items: s
                .copies
                .iter()
                .take(100)
                .map(|c| RedirectItem {
                    subject: c.domino.subject.clone(),
                    from: if c.domino.from_name.is_empty() {
                        c.domino.from_addr.clone()
                    } else {
                        c.domino.from_name.clone()
                    },
                    date: c.domino.date,
                    mailbox: mailbox_label(&c.domino.box_key).into(),
                })
                .collect(),
            ambiguous: s.ambiguous,
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Notes 안에서 보낸 메일인가(지우면 안 된다).
fn notes_internal(memo: &eo_owa::notes::Memo) -> bool {
    let sender_text = memo.field("SenderText").unwrap_or("").trim();
    let hiden_from = memo.field("hidenFrom").unwrap_or("").trim();
    !sender_text.is_empty() || hiden_from.to_ascii_uppercase().starts_with("CN=")
}

/// HCL Domino 확인이 끝날 때마다(설정을 켰을 때만): 짝이 확실한 사본을 서버·로컬에서 지운다.
pub async fn maybe_remove(app: &AppHandle) {
    let st = app.state::<AppState>();
    if !lock(&st.settings).domino.remove_redirects {
        return;
    }
    let handle = app.clone();
    let scanned = tauri::async_runtime::spawn_blocking(move || {
        let st = handle.state::<AppState>();
        scan(&st)
    })
    .await;
    let copies = match scanned {
        Ok(Ok(s)) => s.copies,
        Ok(Err(e)) => {
            applog::warn(format!("HCL Domino 리다이렉트 사본 찾기 실패: {e}"));
            return;
        }
        Err(e) => {
            applog::warn(format!("HCL Domino 리다이렉트 사본 찾기 실패: {e}"));
            return;
        }
    };
    if copies.is_empty() {
        return;
    }
    // 서버에서 한 통씩 다시 확인(Notes 안에서 보낸 메일은 남긴다).
    let mut sure = Vec::new();
    for c in copies.into_iter().take(MAX_PER_RUN) {
        match st.domino.memo(&c.domino.unid).await {
            Ok(memo) if !notes_internal(&memo) => sure.push(c),
            Ok(_) => {
                applog::warn(format!(
                    "HCL Domino 리다이렉트 사본 아님(Notes 메일, 남김): {}",
                    c.domino.subject
                ));
                // 문서를 열면 서버가 'new'를 지운다: 안읽음이었으면 되돌린다.
                if st
                    .store
                    .domino_item(&c.domino.unid)
                    .ok()
                    .flatten()
                    .and_then(|i| i.srv_new)
                    == Some(true)
                {
                    let _ = st
                        .domino
                        .mark_unread(std::slice::from_ref(&c.domino.noteid))
                        .await;
                }
                lock(&SKIPPED)
                    .get_or_insert_with(HashSet::new)
                    .insert(c.domino.unid.clone());
            }
            Err(e) => {
                applog::warn(format!("HCL Domino 리다이렉트 사본 확인 실패: {e}"));
                return;
            }
        }
    }
    if sure.is_empty() {
        return;
    }
    // 메일함별로 지운 메일로 → 지운 메일에서 완전 삭제.
    let mut done: Vec<&eo_sync::RedirectCopy> = Vec::new();
    for key in ["XML_Inbox", "XML_POP3"] {
        let group: Vec<&eo_sync::RedirectCopy> =
            sure.iter().filter(|c| c.domino.box_key == key).collect();
        if group.is_empty() {
            continue;
        }
        let Some(from) = Mailbox::from_key(key) else {
            continue;
        };
        let noteids: Vec<String> = group.iter().map(|c| c.domino.noteid.clone()).collect();
        let r = async {
            st.domino.delete_docs(&from, &noteids).await?;
            st.domino.delete_docs(&Mailbox::Trash, &noteids).await
        }
        .await;
        match r {
            Ok(()) => done.extend(group),
            Err(e) => {
                applog::warn(format!("HCL Domino 리다이렉트 사본 서버 삭제 실패: {e}"));
                break;
            }
        }
    }
    if done.is_empty() {
        return;
    }
    let unids: Vec<String> = done.iter().map(|c| c.domino.unid.clone()).collect();
    let ids: Vec<i64> = done.iter().map(|c| c.domino.message_id).collect();
    let subjects: Vec<String> = done
        .iter()
        .take(5)
        .map(|c| c.domino.subject.chars().take(40).collect())
        .collect();
    let now = now_ms();
    let _ = st.store.domino_mark_server_deleted(&unids, now);
    let _ = st.store.domino_drop_ops(&unids, "리다이렉트 사본 삭제");
    match st.store.purge_messages(&ids, now, None) {
        Ok(t) => {
            crate::actions::after_local(app, &t);
            crate::notify::refresh_badge(app);
        }
        Err(e) => applog::warn(format!("HCL Domino 리다이렉트 사본 로컬 삭제 실패: {e}")),
    }
    applog::info(format!(
        "HCL Domino 리다이렉트 사본 {}통 삭제(서버·로컬, Outlook 메일은 그대로): {}{}",
        done.len(),
        subjects.join(" · "),
        if done.len() > subjects.len() {
            " …"
        } else {
            ""
        }
    ));
    if let Ok((Some(u), _)) = st.domino.quota().await {
        crate::domino::set_quota_used(app, u);
    }
}
