//! 폴더 오른쪽 클릭 메뉴(0.7.0 사용자 요청, 웹메일 메뉴 기준): 새 하위 폴더 만들기·이름 바꾸기·삭제·폴더 비우기·
//! 즐겨찾기·이동·모두 읽은 상태로 표시. 정책 할당·권한(관리용)은 넣지 않았다.
//!
//! - Outlook 서버 폴더: 웹메일에 바로 요청하고(로그인 필요) 계층 동기화가 로컬을 맞춘다. 잘 알려진 폴더(받은
//!   편지함 등)는 웹메일처럼 이름 바꾸기·삭제·이동이 없다.
//! - 가져온 로컬 폴더: 앱 안에서만(만들기·이름 바꾸기·지우기).
//! - HCL Domino 메일함: 모두 읽음만(웹메일에 반영).
//! - 통합 편지함: 모두 읽음(들어 있는 메일함마다).

use eo_owa::{DeleteType, FolderRef};
use eo_store::{FolderRow, Touched};
use tauri::{AppHandle, Emitter, Manager};

use crate::actions::{ActionResult, after_local};
use crate::commands::now_ms;
use crate::state::{AppState, lock};
use crate::view::{UNIFIED_ID, unified_members};

type CmdResult<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

fn folder(st: &AppState, id: i64) -> CmdResult<FolderRow> {
    st.store
        .folders()
        .map_err(err)?
        .into_iter()
        .find(|f| f.id == id)
        .ok_or_else(|| "폴더가 없습니다".to_owned())
}

/// 서버 폴더 ID(로그인 확인 포함).
fn server_folder(st: &AppState, f: &FolderRow) -> CmdResult<FolderRef> {
    let sid = f
        .server_id
        .clone()
        .ok_or_else(|| "서버 폴더가 아닙니다".to_owned())?;
    if !st.client().is_logged_in() {
        return Err("로그인하면 할 수 있습니다".into());
    }
    Ok(FolderRef::Id(sid))
}

/// 웹메일처럼 이름 바꾸기·삭제·이동이 없는 폴더.
fn fixed(f: &FolderRow) -> bool {
    f.well_known.is_some()
}

/// 서버 폴더를 바꿨다: 계층을 곧 다시 받는다.
async fn resync(app: &AppHandle) {
    let st = app.state::<AppState>();
    if let Some(engine) = &st.session.lock().await.engine {
        engine.wake();
    }
    let _ = app.emit("folders-changed", ());
}

/// 모두 읽은 상태로 표시.
#[tauri::command]
pub async fn folder_mark_read(app: AppHandle, folder_id: i64) -> CmdResult<ActionResult> {
    let st = app.state::<AppState>();
    let ids = if folder_id == UNIFIED_ID {
        unified_members(&st.store).map_err(err)?
    } else {
        vec![folder_id]
    };
    let mut t = Touched::default();
    let mut changed = 0;
    for id in ids {
        let f = folder(&st, id)?;
        // Outlook 서버 폴더이고 로그인했으면 한 번에(메일마다 작업을 쌓지 않는다).
        if let (Some(sid), true) = (f.server_id.clone(), st.client().is_logged_in()) {
            st.client()
                .mark_all_read(&[FolderRef::Id(sid)])
                .await
                .map_err(err)?;
            let r = st.store.mark_folder_read_synced(id).map_err(err)?;
            changed += r.folders.values().map(Vec::len).sum::<usize>();
            t.merge(r);
            continue;
        }
        // 로그인 전·로컬·HCL Domino: 안 읽은 메일마다(HCL Domino는 웹메일에도 반영).
        let unread = st.store.unread_ids_in(id).map_err(err)?;
        let now = now_ms();
        let r = st.store.mark_read(&unread, true, now).map_err(err)?;
        crate::domino::queue_changed(&st.store, &r, now, eo_store::domino_op::READ, "1")
            .map_err(err)?;
        changed += unread.len();
        t.merge(r);
    }
    after_local(&app, &t);
    Ok(ActionResult {
        changed,
        queued: t.queued,
        note: None,
    })
}

/// 새 하위 폴더 만들기(`parent` 없으면 맨 위 로컬 폴더).
#[tauri::command]
pub async fn folder_create(app: AppHandle, parent: Option<i64>, name: String) -> CmdResult<()> {
    let st = app.state::<AppState>();
    let name = name.trim().to_owned();
    if name.is_empty() {
        return Err("이름을 적어 주세요".into());
    }
    match parent.map(|p| folder(&st, p)).transpose()? {
        Some(p) if p.server_id.is_some() => {
            let parent = server_folder(&st, &p)?;
            st.client()
                .create_folder(&parent, &name)
                .await
                .map_err(err)?;
            resync(&app).await;
        }
        p => {
            st.store
                .create_local_folder(p.map(|p| p.id), &name)
                .map_err(err)?;
            let _ = app.emit("folders-changed", ());
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn folder_rename(app: AppHandle, folder_id: i64, name: String) -> CmdResult<()> {
    let st = app.state::<AppState>();
    let f = folder(&st, folder_id)?;
    let name = name.trim().to_owned();
    if name.is_empty() || name == f.name {
        return Ok(());
    }
    if fixed(&f) {
        return Err("이 폴더는 이름을 바꿀 수 없습니다".into());
    }
    if f.server_id.is_some() {
        let id = server_folder(&st, &f)?;
        st.client().rename_folder(&id, &name).await.map_err(err)?;
        resync(&app).await;
    } else {
        st.store
            .rename_local_folder(folder_id, &name)
            .map_err(err)?;
        let _ = app.emit("folders-changed", ());
    }
    Ok(())
}

/// 폴더 삭제: 서버 폴더는 웹메일처럼 지운 편지함으로, 로컬 폴더는 안의 메일과 함께 앱에서 지운다. 알림 글.
#[tauri::command]
pub async fn folder_delete(app: AppHandle, folder_id: i64) -> CmdResult<String> {
    let st = app.state::<AppState>();
    let f = folder(&st, folder_id)?;
    if fixed(&f) {
        return Err("이 폴더는 지울 수 없습니다".into());
    }
    if f.server_id.is_some() {
        let id = server_folder(&st, &f)?;
        st.client().delete_folder(&id).await.map_err(err)?;
        resync(&app).await;
        return Ok(format!("'{}'을(를) 지운 편지함으로 옮겼습니다", f.name));
    }
    let (n, t) = st.store.delete_local_folder(folder_id).map_err(err)?;
    after_local(&app, &t);
    let _ = app.emit("folders-changed", ());
    Ok(format!("'{}'과(와) 메일 {n}통을 지웠습니다", f.name))
}

/// 폴더 이동(서버 폴더만, 다른 서버 폴더 아래로).
#[tauri::command]
pub async fn folder_move(app: AppHandle, folder_id: i64, to: i64) -> CmdResult<()> {
    let st = app.state::<AppState>();
    let f = folder(&st, folder_id)?;
    let target = folder(&st, to)?;
    if fixed(&f) {
        return Err("이 폴더는 옮길 수 없습니다".into());
    }
    if f.server_id.is_none() || target.server_id.is_none() {
        return Err("서버 폴더끼리만 옮길 수 있습니다".into());
    }
    let id = server_folder(&st, &f)?;
    let to = server_folder(&st, &target)?;
    st.client().move_folder(&id, &to).await.map_err(err)?;
    resync(&app).await;
    Ok(())
}

/// 폴더 비우기(서버 폴더): 지운 편지함은 복구 가능한 항목으로, 나머지는 지운 편지함으로. 로컬 사본은 동기화가
/// 서버 삭제 판정대로 맞춘다(지운 편지함 비우기는 로컬 보관으로 남는다).
#[tauri::command]
pub async fn folder_empty(app: AppHandle, folder_id: i64) -> CmdResult<()> {
    let st = app.state::<AppState>();
    let f = folder(&st, folder_id)?;
    let id = server_folder(&st, &f)?;
    let kind = if f.well_known.as_deref() == Some("deleteditems") {
        DeleteType::SoftDelete
    } else {
        DeleteType::MoveToDeletedItems
    };
    st.client().empty_folder(&id, kind).await.map_err(err)?;
    resync(&app).await;
    Ok(())
}

/// 즐겨찾기에 추가·제거(폴더 목록 맨 위에도 보인다, 다음 실행에도).
#[tauri::command]
pub async fn folder_favorite(app: AppHandle, folder_id: i64, on: bool) -> CmdResult<()> {
    let st = app.state::<AppState>();
    {
        let mut ui = lock(&st.ui);
        ui.favorites.retain(|f| *f != folder_id);
        if on {
            ui.favorites.push(folder_id);
        }
    }
    st.ui_dirty
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = app.emit("folders-changed", ());
    Ok(())
}
