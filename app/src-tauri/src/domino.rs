//! HCL Domino 계정 연동(사용자 요청 2026-10-21): 설정에서 켜고 로그인하면 웹메일의 메일을 'HCL Domino 메일' 아래 로컬로 가져온다.
//!
//! - 아이디·비밀번호는 메모리에만 둔다(세션이 끝나면 다시 로그인하려고). 앱을 켤 때마다 다시 로그인한다.
//! - 확인 주기: 받은 메일 최근 100통은 받은 편지함 주기(30초 이상), 모든 메일함은 나머지 폴더 주기(1분 이상)마다.
//!   "지금 동기화"는 모든 메일함을 바로 본다.
//! - 받은 메일 자동 정리(Outlook 받은 편지함 정리와 따로, 서버에서만 삭제): 모든 메일함을 확인한 뒤 30분에 한 번까지.
//!   로컬 사본(원문까지)이 있는 메일만 지운다. 웹메일의 삭제(지운 메일로)와 지운 메일에서의 삭제를 차례로 한다.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eo_owa::domino::Mailbox;
use eo_owa::{Credentials, Password};
use eo_store::{Store, Touched};
use eo_sync::{DominoOptions, DominoScan, ListedMail};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::watch;

use crate::applog;
use crate::cleanup::{Picked, pick};
use crate::commands::{MessagesChanged, bump_list_gen, now_ms, update_status};
use crate::settings::{Cleanup, CleanupMode};
use crate::state::{AppState, DominoRunner, DominoStatus, lock};

type CmdResult<T> = Result<T, String>;

const QUICK_MIN: Duration = Duration::from_secs(30);
const FULL_MIN: Duration = Duration::from_secs(60);
const CLEANUP_EVERY: Duration = Duration::from_secs(30 * 60);
const CLEANUP_MAX: usize = 1000;

/// 메일함 사용량만 바꾼다(지운 뒤 다시 잰 값).
pub fn set_quota_used(app: &AppHandle, used: i64) {
    update(app, |b| b.quota_used = Some(used));
}

fn update(app: &AppHandle, f: impl FnOnce(&mut DominoStatus)) {
    update_status(app, |s| f(&mut s.domino));
}

/// 확인 작업을 멈춘다(로그아웃·연동 끄기).
fn stop_runner(st: &AppState) {
    if let Some(r) = lock(&st.domino_runner).take() {
        r.cancel.store(true, Ordering::Relaxed);
        let _ = r.stop.send(true);
    }
}

/// 로그인(아이디·비밀번호는 메모리에만). 성공하면 확인을 시작한다.
#[tauri::command]
pub async fn domino_login(app: AppHandle, username: String, password: String) -> CmdResult<()> {
    let username = username.trim().to_owned();
    let password = Password::new(password);
    if username.is_empty() || password.is_empty() {
        return Err("아이디와 비밀번호를 입력하세요.".into());
    }
    let st = app.state::<AppState>();
    stop_runner(&st);
    update(&app, |b| {
        b.connecting = true;
        b.last_error = None;
    });
    let t = Instant::now();
    let r = st
        .domino
        .login(Credentials {
            username: username.clone(),
            password,
        })
        .await;
    let info = match r {
        Ok(info) => info,
        Err(e) => {
            update(&app, |b| {
                b.connecting = false;
                b.logged_in = false;
            });
            return Err(match e {
                eo_owa::Error::BadCredentials { .. } => {
                    applog::warn("HCL Domino 로그인 실패: 아이디 또는 비밀번호 오류");
                    "아이디 또는 비밀번호가 맞지 않습니다.".into()
                }
                other => {
                    applog::warn(format!("HCL Domino 로그인 실패: {other}"));
                    format!("HCL Domino에 연결할 수 없습니다: {other}")
                }
            });
        }
    };
    applog::info(format!(
        "HCL Domino 로그인 성공 ({}ms, 웹메일 {}, 한도 {})",
        t.elapsed().as_millis(),
        info.location.host,
        info.quota_limit
            .map_or("모름".to_owned(), |b| format!("{}MB", b / 1_048_576))
    ));
    // 주소 모양 아이디면 전체 회신에서 나로 뺀다.
    st.add_alias(&username);
    crate::commands::remember_user(&st, true, Some(username.clone()));
    update(&app, |b| {
        b.connecting = false;
        b.logged_in = true;
        b.login_required = false;
        b.username = Some(username);
        b.quota_limit = info.quota_limit;
    });
    // 연동 직후 처음 확인에서 가져오는 메일은 알리지 않는다.
    crate::notify::reset(crate::notify::Source::Domino);
    start_runner(&app);
    Ok(())
}

/// 로그아웃: 메모리의 아이디·비밀번호와 세션을 지운다(가져온 메일은 그대로).
#[tauri::command]
pub async fn domino_logout(app: AppHandle) -> CmdResult<()> {
    forget(&app);
    applog::info("HCL Domino 로그아웃");
    Ok(())
}

/// 연동을 끄거나 로그아웃할 때.
pub fn forget(app: &AppHandle) {
    let st = app.state::<AppState>();
    stop_runner(&st);
    st.domino.forget();
    *lock(&st.domino_inbox) = None;
    let pending = st.store.domino_pending_count().unwrap_or(0);
    update(app, |b| {
        *b = DominoStatus {
            pending,
            ..DominoStatus::default()
        }
    });
}

/// 지금 확인(모든 메일함).
#[tauri::command]
pub fn domino_sync_now(app: AppHandle) -> CmdResult<()> {
    let st = app.state::<AppState>();
    match lock(&st.domino_runner).as_ref() {
        Some(r) => {
            r.wake.notify_one();
            Ok(())
        }
        None => Err("HCL Domino에 로그인하지 않았습니다.".into()),
    }
}

/// 설정 화면: 이 기준이면 지금 서버에서 지울 HCL Domino 받은 메일 수(모르면 없음).
#[tauri::command]
pub async fn domino_cleanup_preview(app: AppHandle, cleanup: Cleanup) -> CmdResult<Option<usize>> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let Some(inbox) = lock(&st.domino_inbox).clone() else {
            return Ok(None);
        };
        let quota = {
            let s = lock(&st.status);
            s.domino.quota_used.zip(s.domino.quota_limit)
        };
        if cleanup.mode == CleanupMode::Quota && quota.is_none() {
            return Ok(None);
        }
        let rows = candidates(&st, &inbox).map_err(|e| e.to_string())?;
        Ok(Some(
            pick(&rows, &cleanup, now_ms(), quota, usize::MAX).ids.len(),
        ))
    })
    .await
    .map_err(|e| e.to_string())?
}

fn start_runner(app: &AppHandle) {
    let st = app.state::<AppState>();
    let (stop, rx) = watch::channel(false);
    let wake = Arc::new(tokio::sync::Notify::new());
    let kick = Arc::new(tokio::sync::Notify::new());
    let cancel = Arc::new(AtomicBool::new(false));
    *lock(&st.domino_runner) = Some(DominoRunner {
        stop,
        wake: wake.clone(),
        kick: kick.clone(),
        cancel: cancel.clone(),
    });
    let app = app.clone();
    tauri::async_runtime::spawn(async move { run(app, rx, wake, kick, cancel).await });
}

/// 앱에서 HCL Domino 메일을 바꾼 뒤: 대기 수를 고치고, 로그인해 있으면 곧바로 웹메일에 보낸다.
pub fn kick(app: &AppHandle) {
    let st = app.state::<AppState>();
    let pending = st.store.domino_pending_count().unwrap_or(0);
    update(app, |b| b.pending = pending);
    if pending > 0
        && let Some(r) = lock(&st.domino_runner).as_ref()
    {
        r.kick.notify_one();
    }
}

/// 쌓인 작업을 웹메일로 보낸다. 끝내 거부된 것은 로컬을 되돌리고 알린다.
async fn push(app: &AppHandle) -> Result<(), eo_owa::Error> {
    let st = app.state::<AppState>();
    if st.store.domino_pending_count().unwrap_or(0) == 0 {
        return Ok(());
    }
    let r = match eo_sync::push_domino_ops(&st.domino, &st.store).await {
        Ok(r) => r,
        Err(eo_sync::Error::Owa(e)) => return Err(e),
        Err(e) => return Err(eo_owa::Error::Http(e.to_string())),
    };
    if !r.folders.is_empty() {
        refresh(app, &r.folders);
    }
    for (kind, error) in &r.abandoned {
        let what = format!("HCL Domino {}", op_name(kind));
        applog::warn(format!("웹메일이 {what}을(를) 거부해 되돌림: {error}"));
        let _ = app.emit(
            "op-failed",
            crate::commands::OpFailed {
                what,
                error: error.clone(),
            },
        );
    }
    let pending = st.store.domino_pending_count().unwrap_or(0);
    update(app, |b| b.pending = pending);
    Ok(())
}

fn op_name(kind: &str) -> &'static str {
    use eo_store::domino_op as op;
    match kind {
        op::READ => "읽음 표시",
        op::STAR => "중요 표시",
        op::DELETE => "삭제",
        op::RECOVER => "복구",
        op::FOLDER_MOVE | op::MOVE_BACK => "이동",
        _ => "작업",
    }
}

// ---- 앱에서 바꾼 것을 웹메일로(0.6.0 양방향, 사용자 요청 2026-10-21) ----
// 같은 계정 안에서만 옮긴다(HCL Domino ↔ Outlook 이동·복사는 막음, 사용자 결정). HCL Domino 안에서는 웹메일이 하는
// 이동만: 지운 메일로(삭제), 지운 메일 → 받은 메일(복구), 개인 폴더로, 개인 폴더 → 받은 메일(원래 메일함으로).

const CROSS_ACCOUNT: &str = "같은 계정 안에서만 옮길 수 있습니다";

/// 연동한 HCL Domino 메일 하나: (메일 번호, 문서, 지금 있는 웹메일 메일함 열쇠).
pub struct Linked {
    pub id: i64,
    pub folder_id: i64,
    pub unid: String,
    pub from_box: String,
    pub archived: bool,
}

/// 폴더가 'HCL Domino 메일'이나 그 아래인가.
fn in_domino_tree(s: &Store, folder_id: i64) -> eo_store::Result<bool> {
    let folders = s.folders()?;
    let mut cur = folders.iter().find(|f| f.id == folder_id);
    for _ in 0..32 {
        let Some(f) = cur else { return Ok(false) };
        if f.parent_id.is_none() {
            return Ok(f.name == eo_sync::DOMINO_FOLDER);
        }
        cur = folders.iter().find(|p| Some(p.id) == f.parent_id);
    }
    Ok(false)
}

/// 고른 메일을 연동한 HCL Domino 메일과 나머지로 나눈다.
pub fn split_linked(s: &Store, ids: &[i64]) -> eo_store::Result<(Vec<Linked>, Vec<i64>)> {
    let items = s.domino_items_for(ids)?;
    let boxes = s.domino_folder_boxes()?;
    let (mut linked, mut others) = (Vec::new(), Vec::new());
    for &id in ids {
        let (Some(item), Some(row)) = (items.get(&id), s.message(id)?) else {
            others.push(id);
            continue;
        };
        let from_box = boxes
            .get(&row.folder_id)
            .cloned()
            .or_else(|| item.srv_box.clone())
            .unwrap_or_else(|| Mailbox::Inbox.key());
        linked.push(Linked {
            id,
            folder_id: row.folder_id,
            unid: item.unid.clone(),
            from_box,
            archived: item.server_deleted || row.server_state == eo_store::server_state::ARCHIVED,
        });
    }
    Ok((linked, others))
}

/// 읽음·플래그를 바꾼(로컬에서 실제로 바뀐) 연동 메일의 작업을 쌓는다.
pub fn queue_changed(
    s: &Store,
    t: &Touched,
    now: i64,
    kind: &str,
    arg: &str,
) -> eo_store::Result<()> {
    let ids: Vec<i64> = t.folders.values().flatten().copied().collect();
    for (id, item) in s.domino_items_for(&ids)? {
        if !item.server_deleted {
            s.domino_enqueue(&item.unid, Some(id), kind, arg, now)?;
        }
    }
    Ok(())
}

/// 이동 계획: 막을 것은 막고(오류 문장), 연동 메일은 보낼 작업을 돌려준다.
pub fn plan_move(
    s: &Store,
    ids: &[i64],
    to: i64,
) -> eo_store::Result<Vec<(String, i64, &'static str, String)>> {
    use eo_store::domino_op as op;
    let invalid = |m: &str| eo_store::Error::Invalid(m.to_owned());
    let target_box = s
        .domino_folder_boxes()?
        .get(&to)
        .and_then(|k| Mailbox::from_key(k));
    let target_in_bn = in_domino_tree(s, to)?;
    let (linked, others) = split_linked(s, ids)?;
    for id in others {
        // Outlook 서버 메일은 HCL Domino 쪽으로 옮길 수 없다.
        if let Some(row) = s.message(id)?
            && (row.server_item_id.is_some() || row.server_state == eo_store::server_state::PENDING)
            && target_in_bn
        {
            return Err(invalid(CROSS_ACCOUNT));
        }
    }
    let mut plan = Vec::new();
    for l in linked {
        if l.folder_id == to {
            continue;
        }
        let Some(to_box) = &target_box else {
            return Err(invalid(if target_in_bn {
                "HCL Domino 웹메일에 있는 메일함으로만 옮길 수 있습니다"
            } else {
                CROSS_ACCOUNT
            }));
        };
        if l.archived {
            return Err(invalid("웹메일에서 지운 메일은 옮길 수 없습니다"));
        }
        let from = Mailbox::from_key(&l.from_box).unwrap_or(Mailbox::Inbox);
        let (kind, arg) = match (&from, to_box) {
            (f, Mailbox::Trash) if *f != Mailbox::Trash => (op::DELETE, l.from_box.clone()),
            (Mailbox::Trash, Mailbox::Inbox) => (op::RECOVER, String::new()),
            (f, Mailbox::Folder(name)) if *f != Mailbox::Trash => (op::FOLDER_MOVE, name.clone()),
            (Mailbox::Folder(_), Mailbox::Inbox) => (op::MOVE_BACK, String::new()),
            _ => {
                return Err(invalid(
                    "HCL Domino 메일은 받은 메일·지운 메일·개인 폴더로만 옮길 수 있습니다",
                ));
            }
        };
        plan.push((l.unid, l.id, kind, arg));
    }
    Ok(plan)
}

pub fn enqueue_plan(
    s: &Store,
    plan: &[(String, i64, &'static str, String)],
    now: i64,
) -> eo_store::Result<()> {
    for (unid, id, kind, arg) in plan {
        s.domino_enqueue(unid, Some(*id), kind, arg, now)?;
    }
    Ok(())
}

/// 복사: 웹메일에 복사가 없고, 계정을 넘나드는 복사는 막는다.
pub fn check_copy(s: &Store, ids: &[i64], to: i64) -> eo_store::Result<()> {
    let (linked, others) = split_linked(s, ids)?;
    if !linked.is_empty() {
        return Err(eo_store::Error::Invalid(
            "HCL Domino 메일은 복사할 수 없습니다".into(),
        ));
    }
    if in_domino_tree(s, to)? {
        for id in others {
            if let Some(row) = s.message(id)?
                && (row.server_item_id.is_some()
                    || row.server_state == eo_store::server_state::PENDING)
            {
                return Err(eo_store::Error::Invalid(CROSS_ACCOUNT.into()));
            }
        }
    }
    Ok(())
}

/// 웹메일 지운 메일 폴더(로컬).
fn trash_folder(s: &Store) -> eo_store::Result<Option<i64>> {
    let key = Mailbox::Trash.key();
    Ok(s.domino_folder_boxes()?
        .into_iter()
        .find(|(_, k)| *k == key)
        .map(|(f, _)| f))
}

/// Del: 지운 메일이 아니면 지운 메일로(웹메일 삭제), 지운 메일이면 웹메일에서 완전히 지우고 로컬은 보관.
pub fn delete_linked(
    s: &Store,
    linked: &[Linked],
    now: i64,
) -> eo_store::Result<(Touched, Option<String>)> {
    use eo_store::domino_op as op;
    let trash_key = Mailbox::Trash.key();
    let mut t = Touched::default();
    let mut local_only = 0;
    for l in linked {
        if l.archived {
            local_only += 1;
        } else if l.from_box == trash_key {
            s.domino_enqueue(&l.unid, Some(l.id), op::DELETE, &trash_key, now)?;
            for id in s.domino_archive_local(&[l.id], now)? {
                t.folders.entry(l.folder_id).or_default().push(id);
            }
        } else {
            let trash = trash_folder(s)?.ok_or_else(|| {
                eo_store::Error::Invalid("HCL Domino를 한 번 확인한 뒤 다시 하세요.".into())
            })?;
            t.merge(s.move_messages(&[l.id], trash, now, None)?);
            s.domino_enqueue(&l.unid, Some(l.id), op::DELETE, &l.from_box, now)?;
        }
    }
    let note = (local_only > 0)
        .then(|| format!("로컬 전용 {local_only}통은 완전 삭제로만 지울 수 있습니다"));
    Ok((t, note))
}

/// 서버에서만 삭제(로컬 보관)·완전 삭제의 웹메일 쪽: 지운 메일로 보낸 뒤 지운 메일에서도 지운다.
/// `archive`면 로컬은 그 자리에 보관으로(완전 삭제는 부른 쪽이 로컬을 지운다).
pub fn server_delete_linked(
    s: &Store,
    linked: &[Linked],
    now: i64,
    archive: bool,
) -> eo_store::Result<Touched> {
    use eo_store::domino_op as op;
    let trash_key = Mailbox::Trash.key();
    let mut t = Touched::default();
    for l in linked.iter().filter(|l| !l.archived) {
        if l.from_box != trash_key {
            s.domino_enqueue(&l.unid, Some(l.id), op::DELETE, &l.from_box, now)?;
        }
        s.domino_enqueue(&l.unid, Some(l.id), op::DELETE, &trash_key, now)?;
        if archive {
            for id in s.domino_archive_local(&[l.id], now)? {
                t.folders.entry(l.folder_id).or_default().push(id);
            }
        }
    }
    Ok(t)
}

/// `--ui-test` 전용: 견본 HCL Domino 백업으로 만든 'HCL Domino 메일/받은 메일·외부 메일·지운 메일'을 웹메일 메일함으로
/// 짝지어 둔다(로그인 없이 통합 편지함·이동 규칙을 보려고).
#[tauri::command]
pub async fn ui_test_domino_boxes(app: AppHandle) -> CmdResult<usize> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        if !st.ui_test {
            return Err("점검 모드가 아닙니다".to_owned());
        }
        let mut n = 0;
        for (name, mailbox) in [
            ("받은 메일", Mailbox::Inbox),
            ("외부 메일", Mailbox::Pop3),
            ("지운 메일", Mailbox::Trash),
        ] {
            let (id, _) = st
                .store
                .find_or_create_folder_path(&[eo_sync::DOMINO_FOLDER.to_owned(), name.to_owned()])
                .map_err(|e| e.to_string())?;
            st.store
                .domino_set_folder_box(id, &mailbox.key())
                .map_err(|e| e.to_string())?;
            n += 1;
        }
        bump_list_gen();
        Ok(n)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// 폴더 목록·메일 목록을 다시 그리게 한다(읽기 창 캐시도 비운다).
fn refresh(app: &AppHandle, folders: &[i64]) {
    bump_list_gen();
    lock(&app.state::<AppState>().views).clear();
    let _ = app.emit("folders-changed", ());
    for folder_id in folders {
        let _ = app.emit(
            "messages-changed",
            MessagesChanged {
                folder_id: *folder_id,
                ids: Vec::new(),
            },
        );
    }
}

fn intervals(st: &AppState) -> (Duration, Duration) {
    let s = lock(&st.settings);
    (
        Duration::from_secs(s.inbox_every_secs).max(QUICK_MIN),
        Duration::from_secs(s.others_every_secs).max(FULL_MIN),
    )
}

async fn run(
    app: AppHandle,
    mut stop: watch::Receiver<bool>,
    wake: Arc<tokio::sync::Notify>,
    kick: Arc<tokio::sync::Notify>,
    cancel: Arc<AtomicBool>,
) {
    let mut last_full: Option<Instant> = None;
    'outer: loop {
        let (quick, full_every) = intervals(&app.state::<AppState>());
        let full = last_full.is_none_or(|t| t.elapsed() >= full_every);
        // 앱에서 바꾼 것을 먼저 보낸다(웹메일 상태로 확인하기 전에).
        let r = match push(&app).await {
            Ok(()) => cycle(&app, full, &cancel).await,
            Err(e) => Err(e),
        };
        match r {
            Ok(()) if full => last_full = Some(Instant::now()),
            Ok(()) => {}
            Err(eo_owa::Error::LoginRequired) => {
                if !*stop.borrow() {
                    applog::warn("HCL Domino 재로그인 거부: 다시 로그인 필요");
                    update(&app, |b| {
                        b.logged_in = false;
                        b.syncing = false;
                        b.login_required = true;
                    });
                    let _ = app.emit("domino-login-required", ());
                }
                break;
            }
            Err(e) => {
                let msg = format!("HCL Domino: {e}");
                if lock(&app.state::<AppState>().status)
                    .domino
                    .last_error
                    .as_deref()
                    != Some(msg.as_str())
                {
                    applog::warn(format!("HCL Domino 확인 실패: {e}"));
                }
                update(&app, |b| {
                    b.syncing = false;
                    b.last_error = Some(msg);
                });
            }
        }
        if *stop.borrow() {
            break;
        }
        let next = tokio::time::Instant::now() + quick;
        loop {
            tokio::select! {
                _ = stop.changed() => break 'outer,
                () = wake.notified() => {
                    last_full = None;
                    continue 'outer;
                }
                // 앱에서 바꿨다: 확인 주기를 기다리지 않고 보내기만.
                () = kick.notified() => {
                    if let Err(e) = push(&app).await {
                        applog::warn(format!("HCL Domino 작업 보내기 실패: {e}"));
                    }
                }
                () = tokio::time::sleep_until(next) => continue 'outer,
            }
        }
    }
}

async fn cycle(app: &AppHandle, full: bool, cancel: &AtomicBool) -> Result<(), eo_owa::Error> {
    let st = app.state::<AppState>();
    update(app, |b| b.syncing = true);
    let t = Instant::now();
    let mut last_progress = Instant::now();
    let r = eo_sync::sync_domino(
        &st.domino,
        &st.store,
        if full {
            DominoScan::Full
        } else {
            DominoScan::Quick
        },
        &DominoOptions::default(),
        cancel,
        |done, total| {
            if done == total || last_progress.elapsed() >= Duration::from_millis(250) {
                last_progress = Instant::now();
                update(app, |b| {
                    b.done = done;
                    b.total = total;
                });
            }
        },
    )
    .await;
    let report = match r {
        Ok(r) => r,
        Err(eo_sync::Error::Owa(e)) => return Err(e),
        Err(e) => return Err(eo_owa::Error::Http(e.to_string())),
    };
    if report.imported > 0 {
        let app2 = app.clone();
        let indexed = tauri::async_runtime::spawn_blocking(move || {
            let st = app2.state::<AppState>();
            eo_sync::index_pending(&st.store, &st.search, None)
        })
        .await;
        if let Ok(Err(e)) = indexed {
            applog::warn(format!("HCL Domino 메일 색인 실패: {e}"));
        }
        bump_list_gen();
        let _ = app.emit("folders-changed", ());
        for folder_id in &report.folders {
            let _ = app.emit(
                "messages-changed",
                MessagesChanged {
                    folder_id: *folder_id,
                    ids: Vec::new(),
                },
            );
        }
    } else if report.updated > 0 || report.folders_changed {
        // 웹메일에서 바뀐 것(읽음·★·메일함·삭제)을 반영했다.
        refresh(app, &report.folders);
    }
    if report.imported > 0 || report.failed > 0 || report.unread_restore_failed > 0 {
        applog::info(format!(
            "HCL Domino {}초: 새 메일 {}통 가져옴, 백업에 있던 같은 메일 {}, 실패 {}, 'new' 되돌림 {}(실패 {}), 그림·첨부 못 받은 메일 {}통({}개)",
            t.elapsed().as_secs(),
            report.imported,
            report.duplicates,
            report.failed,
            report.unread_restored,
            report.unread_restore_failed,
            report.with_missing,
            report.missing_parts
        ));
        for e in report.errors.iter().take(5) {
            applog::warn(format!("HCL Domino: {e}"));
        }
    }
    // 용량은 모든 메일함을 볼 때(웹메일의 "메일용량확인"과 같은 값).
    let (used, limit) = if full {
        st.domino.quota().await.unwrap_or((None, None))
    } else {
        (None, None)
    };
    update(app, |b| {
        b.syncing = false;
        b.done = 0;
        b.total = 0;
        b.last_sync = Some(now_ms());
        b.last_error =
            (report.failed > 0).then(|| format!("HCL Domino: {}통 가져오지 못함", report.failed));
        if used.is_some() {
            b.quota_used = used;
        }
        if limit.is_some() {
            b.quota_limit = limit;
        }
    });
    // 0.8.4까지 여기서 돌던 중복 전달 메일 자동 삭제는 0.9.0에서 뺐다(사용자 요청). 1.1.0에서 새로 설계해 다시 넣었다
    // (`crate::redirects`, 설정에서 켤 때만). 알림보다 먼저: 지울 사본을 새 메일로 알리지 않게.
    crate::redirects::maybe_remove(app).await;
    // 새 메일 알림·작업 표시줄 배지(0.6.0).
    crate::notify::check(app, crate::notify::Source::Domino);
    if report.imported > 0 || report.updated > 0 {
        crate::notify::refresh_badge(app);
    }
    if let Some(inbox) = report.inbox {
        *lock(&st.domino_inbox) = Some(inbox.clone());
        maybe_cleanup(app, &inbox).await;
    }
    Ok(())
}

/// 자동 정리 후보: 받은 메일 중 로컬 사본이 있는 것(`(목록 순번, 크기, 원문 있음, 받은 시각)`, 오래된 것부터).
fn candidates(
    st: &AppState,
    inbox: &[ListedMail],
) -> Result<Vec<(i64, i64, bool, i64)>, eo_store::Error> {
    let copies: HashMap<String, bool> = st
        .store
        .domino_copies()?
        .into_iter()
        .map(|c| (c.unid, c.has_eml))
        .collect();
    let mut rows: Vec<(i64, i64, bool, i64)> = inbox
        .iter()
        .enumerate()
        .filter_map(|(i, m)| {
            let has_eml = *copies.get(&m.unid)?;
            Some((
                i as i64,
                m.size.unwrap_or(0),
                has_eml,
                m.date.unwrap_or(i64::MAX),
            ))
        })
        .collect();
    rows.sort_by_key(|r| r.3);
    Ok(rows)
}

#[derive(Default)]
struct CleanupRunner {
    last: Option<Instant>,
    /// 용량 기준으로 지난번에 지운 것: (지우기 전 사용량, 지운 크기).
    last_quota: Option<(i64, i64)>,
    stopped: bool,
}

static CLEANUP: Mutex<CleanupRunner> = Mutex::new(CleanupRunner {
    last: None,
    last_quota: None,
    stopped: false,
});

/// 설정이 바뀌면 다음 확인에 바로 다시 본다(멈춤도 푼다).
pub fn reset_cleanup() {
    *lock(&CLEANUP) = CleanupRunner::default();
}

async fn maybe_cleanup(app: &AppHandle, inbox: &[ListedMail]) {
    let st = app.state::<AppState>();
    let c = lock(&st.settings).domino.cleanup;
    if c.mode == CleanupMode::Off {
        return;
    }
    let (used, limit) = {
        let s = lock(&st.status);
        (s.domino.quota_used, s.domino.quota_limit)
    };
    {
        let mut r = lock(&CLEANUP);
        if r.stopped || r.last.is_some_and(|t| t.elapsed() < CLEANUP_EVERY) {
            return;
        }
        if c.mode == CleanupMode::Quota
            && let (Some(used), Some((before, deleted))) = (used, r.last_quota.take())
            && before - used < deleted / 2
        {
            r.stopped = true;
            drop(r);
            applog::warn(format!(
                "HCL Domino 자동 정리 멈춤: 서버에서 {:.1}MB 지웠는데 사용량이 {:.1}MB 줄었음",
                deleted as f64 / 1_048_576.0,
                (before - used) as f64 / 1_048_576.0
            ));
            update(app, |b| {
                b.last_error = Some("HCL Domino 자동 정리 멈춤: 지운 만큼 용량이 줄지 않음".into())
            });
            return;
        }
        r.last = Some(Instant::now());
    }
    let rows = match candidates(&st, inbox) {
        Ok(x) => x,
        Err(e) => {
            applog::warn(format!("HCL Domino 자동 정리 실패: {e}"));
            return;
        }
    };
    let Picked {
        ids,
        bytes,
        without_eml,
    } = pick(&rows, &c, now_ms(), used.zip(limit), CLEANUP_MAX);
    if ids.is_empty() {
        return;
    }
    let picked: Vec<&ListedMail> = ids.iter().map(|i| &inbox[*i as usize]).collect();
    let noteids: Vec<String> = picked.iter().map(|m| m.noteid.clone()).collect();
    let unids: Vec<String> = picked.iter().map(|m| m.unid.clone()).collect();
    // 받은 메일 → 지운 메일 → 완전 삭제(용량에서 빠지게).
    let done = async {
        st.domino.delete_docs(&Mailbox::Inbox, &noteids).await?;
        st.domino.delete_docs(&Mailbox::Trash, &noteids).await
    }
    .await;
    if let Err(e) = done {
        applog::warn(format!("HCL Domino 자동 정리 실패: {e}"));
        update(app, |b| {
            b.last_error = Some(format!("HCL Domino 자동 정리 실패: {e}"))
        });
        return;
    }
    if let Err(e) = st.store.domino_mark_server_deleted(&unids, now_ms()) {
        applog::warn(format!("HCL Domino 자동 정리 기록 실패: {e}"));
    }
    if c.mode == CleanupMode::Quota
        && let Some(u) = used
    {
        lock(&CLEANUP).last_quota = Some((u, bytes));
    }
    applog::info(format!(
        "HCL Domino 자동 정리: 받은 메일 {}통({:.1}MB) 서버에서 삭제 · 기준 {}{}",
        ids.len(),
        bytes as f64 / 1_048_576.0,
        match c.mode {
            CleanupMode::Age => format!("{}일 지남", c.days),
            _ => format!("용량 {}% 이상", c.percent),
        },
        if without_eml > 0 {
            format!(" · 원문 없어 남김 {without_eml}통")
        } else {
            String::new()
        }
    ));
    if let Ok((Some(u), _)) = st.domino.quota().await {
        update(app, |b| b.quota_used = Some(u));
    }
}

#[cfg(test)]
mod tests {
    use eo_store::{ImportOutcome, MessageProps, Store, domino_op as op};

    use super::*;

    /// HCL Domino 받은 메일·지운 메일·보낸 메일(웹메일 메일함), HCL Domino 아래 백업 폴더, Outlook 쪽 로컬 폴더.
    fn setup() -> (tempfile::TempDir, Store, [i64; 5], i64) {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        let f = |p: &[&str]| {
            s.find_or_create_folder_path(&p.iter().map(|x| (*x).to_owned()).collect::<Vec<_>>())
                .unwrap()
                .0
        };
        let inbox = f(&["HCL Domino 메일", "받은 메일"]);
        let trash = f(&["HCL Domino 메일", "지운 메일"]);
        let sent = f(&["HCL Domino 메일", "보낸 메일"]);
        let pst = f(&["HCL Domino 메일", "받은 편지함"]);
        let outlook = f(&["가져온 메일"]);
        for (id, key) in [
            (inbox, "XML_Inbox"),
            (trash, "XML_Trash"),
            (sent, "XML_Sent"),
        ] {
            s.domino_set_folder_box(id, key).unwrap();
        }
        let props = MessageProps {
            subject: Some("시험".into()),
            date_received: 1,
            ..MessageProps::default()
        };
        let ImportOutcome::Imported(mid) = s
            .import_message(inbox, b"raw", &props, eo_store::DOMINO_SYNC_ORIGIN)
            .unwrap()
        else {
            panic!()
        };
        s.domino_record(
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            Some(mid),
            "A1",
            "XML_Inbox",
            1,
        )
        .unwrap();
        (dir, s, [inbox, trash, sent, pst, outlook], mid)
    }

    #[test]
    fn moves_stay_in_account_and_follow_webmail() {
        let (_d, s, [inbox, trash, sent, pst, outlook], mid) = setup();
        let plan = plan_move(&s, &[mid], trash).unwrap();
        assert_eq!(
            plan.iter().map(|p| (p.2, p.3.as_str())).collect::<Vec<_>>(),
            [(op::DELETE, "XML_Inbox")],
            "받은 메일 → 지운 메일 = 웹메일 삭제"
        );
        let err = |r: eo_store::Result<_>| match r {
            Err(eo_store::Error::Invalid(m)) => m,
            other => panic!("막아야 함: {:?}", other.map(|_: Vec<_>| ())),
        };
        assert_eq!(err(plan_move(&s, &[mid], outlook)), CROSS_ACCOUNT);
        assert!(err(plan_move(&s, &[mid], pst)).contains("웹메일에 있는 메일함"));
        assert!(err(plan_move(&s, &[mid], sent)).contains("받은 메일·지운 메일·개인 폴더"));
        assert!(
            check_copy(&s, &[mid], inbox).is_err(),
            "HCL Domino 메일은 복사 없음"
        );
        // 지운 메일에 있으면 받은 메일로는 복구.
        s.move_messages(&[mid], trash, 2, None).unwrap();
        let plan = plan_move(&s, &[mid], inbox).unwrap();
        assert_eq!(plan[0].2, op::RECOVER);
        // Del: 지운 메일에서는 웹메일에서 완전히 지우고 로컬은 보관.
        let (linked, rest) = split_linked(&s, &[mid]).unwrap();
        assert!(rest.is_empty());
        let (t, note) = delete_linked(&s, &linked, 3).unwrap();
        assert!(note.is_none() && t.folders.contains_key(&trash));
        assert_eq!(
            s.message(mid).unwrap().unwrap().server_state,
            eo_store::server_state::ARCHIVED
        );
        assert_eq!(s.domino_pending_count().unwrap(), 1);
    }
}
