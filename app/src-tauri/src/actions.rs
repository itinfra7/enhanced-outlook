//! 메일 조작(계획서 4.1-2 낙관적 반영, 10장 5단계)과 도구(9.4).
//! 로컬에 바로 반영하고 서버 작업을 쌓은 뒤 동기화 엔진을 깨운다. 로그인 전·오프라인이면 작업은
//! 쌓여 있다가 연결되면 반영된다. 사용자 결정(2026-10-16): Del은 웹메일처럼 지운 편지함으로 옮기고,
//! 지운 편지함에서는 서버에서만 지운다(로컬 보관 유지). 완전 삭제만 로컬에서도 지운다.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use eo_search::{SearchOptions, Sort};
use eo_store::{Store, Touched, domino_op, server_state};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_dialog::DialogExt;

use crate::commands::{MessagesChanged, Page, now_ms, update_status};
use crate::files;
use crate::state::{AppState, lock};
use crate::view::RowView;

type CmdResult<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

pub fn kind_name(kind: &str) -> &'static str {
    match kind {
        "set_read" => "읽음 표시",
        "set_flag" => "플래그",
        "move" => "이동",
        "copy" => "복사",
        "delete" => "삭제",
        _ => "서버 작업",
    }
}

/// 로컬 조작 뒤: 읽기 창 캐시·색인·화면 갱신, 엔진 깨우기, 대기 수 표시.
pub fn after_local(app: &AppHandle, t: &Touched) {
    crate::commands::bump_list_gen();
    let st = app.state::<AppState>();
    {
        let mut views = lock(&st.views);
        for id in t.folders.values().flatten().chain(&t.removed) {
            views.remove(id);
        }
    }
    if !t.removed.is_empty() {
        for id in &t.removed {
            let _ = st.search.delete(*id as u64);
        }
        let _ = st.search.commit();
    }
    for (folder_id, ids) in &t.folders {
        let _ = app.emit(
            "messages-changed",
            MessagesChanged {
                folder_id: *folder_id,
                ids: ids.clone(),
            },
        );
    }
    // 이동·복사한 메일의 색인 폴더 값(뒤에서).
    let handle = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let st = handle.state::<AppState>();
        let _ = eo_sync::index_pending(&st.store, &st.search, None);
    });
    if t.queued > 0 {
        let handle = app.clone();
        tauri::async_runtime::spawn(async move {
            let st = handle.state::<AppState>();
            if let Some(engine) = &st.session.lock().await.engine {
                engine.wake_ops();
            }
        });
    }
    let pending = st.store.pending_op_count().unwrap_or(0);
    update_status(app, |s| s.pending_ops = pending);
    // 연동한 HCL Domino 메일을 바꿨으면 웹메일로 바로 보낸다.
    crate::domino::kick(app);
    crate::notify::refresh_badge(app);
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionResult {
    /// 로컬에서 바뀐 메일 수.
    pub changed: usize,
    /// 서버 작업을 쌓은 수.
    pub queued: usize,
    pub note: Option<String>,
}

async fn run(
    app: AppHandle,
    f: impl FnOnce(&Store, i64) -> eo_store::Result<(Touched, Option<String>)> + Send + 'static,
) -> CmdResult<ActionResult> {
    let handle = app.clone();
    let (t, note) = tauri::async_runtime::spawn_blocking(move || {
        let st = handle.state::<AppState>();
        f(&st.store, now_ms())
    })
    .await
    .map_err(err)?
    .map_err(err)?;
    after_local(&app, &t);
    let mut ids: Vec<i64> = t.folders.values().flatten().copied().collect();
    ids.sort_unstable();
    ids.dedup();
    Ok(ActionResult {
        changed: ids.len(),
        queued: t.queued,
        note,
    })
}

// 연동한 HCL Domino 메일은 로컬에 바로 반영하고 웹메일 작업을 쌓는다(0.6.0 양방향, `crate::domino`).

#[tauri::command]
pub async fn mark_read(app: AppHandle, ids: Vec<i64>, read: bool) -> CmdResult<ActionResult> {
    run(app, move |s, now| {
        let t = s.mark_read(&ids, read, now)?;
        let arg = if read { "1" } else { "0" };
        crate::domino::queue_changed(s, &t, now, domino_op::READ, arg)?;
        Ok((t, None))
    })
    .await
}

/// 플래그. HCL Domino의 ★은 플래그와 맞춘다(완료는 웹메일에 없어 ★을 끈다).
#[tauri::command]
pub async fn set_flag(app: AppHandle, ids: Vec<i64>, flag: i64) -> CmdResult<ActionResult> {
    run(app, move |s, now| {
        let flag = flag.clamp(0, 2);
        let t = s.mark_flag(&ids, flag, now)?;
        let arg = if flag == 1 { "1" } else { "0" };
        crate::domino::queue_changed(s, &t, now, domino_op::STAR, arg)?;
        Ok((t, None))
    })
    .await
}

/// 고정·해제(1.1.0). Outlook 메일은 웹메일의 고정과 같게 서버에도, HCL Domino 메일은 웹메일에 고정이 없어 로컬만.
#[tauri::command]
pub async fn set_pinned(app: AppHandle, ids: Vec<i64>, pinned: bool) -> CmdResult<ActionResult> {
    run(app, move |s, now| {
        Ok((s.mark_pinned(&ids, pinned, now)?, None))
    })
    .await
}

/// 이동. 같은 계정 안에서만(사용자 결정 2026-10-21).
#[tauri::command]
pub async fn move_messages(app: AppHandle, ids: Vec<i64>, to: i64) -> CmdResult<ActionResult> {
    run(app, move |s, now| {
        let plan = crate::domino::plan_move(s, &ids, to)?;
        let (mut t, ids) = crate::compose::take_drafts(s, &ids, true)?;
        t.merge(s.move_messages(&ids, to, now, None)?);
        crate::domino::enqueue_plan(s, &plan, now)?;
        Ok((t, None))
    })
    .await
}

#[tauri::command]
pub async fn copy_messages(app: AppHandle, ids: Vec<i64>, to: i64) -> CmdResult<ActionResult> {
    run(app, move |s, now| {
        crate::domino::check_copy(s, &ids, to)?;
        crate::compose::check_draft_copy(s, &ids)?;
        Ok((s.copy_messages(&ids, to, now, None)?, None))
    })
    .await
}

/// Del(웹메일처럼): 일반 편지함 → 지운 편지함으로 이동, 지운 편지함 → 서버에서만 삭제(로컬 보관).
/// HCL Domino 메일은 HCL Domino의 지운 메일로(웹메일도 같이).
#[tauri::command]
pub async fn delete_messages(app: AppHandle, ids: Vec<i64>) -> CmdResult<ActionResult> {
    run(app, move |s, now| {
        let (linked, ids) = crate::domino::split_linked(s, &ids)?;
        let (mut t, mut note) = crate::domino::delete_linked(s, &linked, now)?;
        // 쓰던 메일: 작성 중 메일은 지우고, 서버에 있는 초안은 웹메일처럼 지운 편지함으로.
        let (drafts, ids) = crate::compose::take_drafts(s, &ids, false)?;
        t.merge(drafts);
        if ids.is_empty() {
            return Ok((t, note));
        }
        let trash = s
            .folder_by_well_known("deleteditems")?
            .ok_or_else(|| eo_store::Error::Invalid("로그인해 동기화한 뒤 다시 하세요.".into()))?;
        let (mut in_trash, mut others, mut local_only) = (Vec::new(), Vec::new(), 0);
        for id in ids {
            let Some(m) = s.message(id)? else { continue };
            if m.folder_id != trash.id {
                others.push(id);
            } else if m.server_state == server_state::ARCHIVED {
                local_only += 1;
            } else {
                in_trash.push(id);
            }
        }
        t.merge(s.move_messages(&others, trash.id, now, None)?);
        t.merge(s.server_delete(&in_trash, now, None)?);
        if local_only > 0 {
            note = Some(format!(
                "로컬 전용 {local_only}통은 완전 삭제로만 지울 수 있습니다"
            ));
        }
        Ok((t, note))
    })
    .await
}

/// 서버에서만 삭제(로컬 보관 유지).
#[tauri::command]
pub async fn server_delete(app: AppHandle, ids: Vec<i64>) -> CmdResult<ActionResult> {
    run(app, move |s, now| {
        let (linked, ids) = crate::domino::split_linked(s, &ids)?;
        let mut t = crate::domino::server_delete_linked(s, &linked, now, true)?;
        let (drafts, ids) = crate::compose::take_drafts(s, &ids, false)?;
        t.merge(drafts);
        t.merge(s.server_delete(&ids, now, None)?);
        Ok((t, None))
    })
    .await
}

/// 완전 삭제(로컬 + 서버). 화면에서 이중 확인을 받은 뒤에만 부른다.
#[tauri::command]
pub async fn purge_messages(app: AppHandle, ids: Vec<i64>) -> CmdResult<ActionResult> {
    run(app, move |s, now| {
        let (linked, _) = crate::domino::split_linked(s, &ids)?;
        crate::domino::server_delete_linked(s, &linked, now, false)?;
        let (mut t, ids) = crate::compose::take_drafts(s, &ids, false)?;
        t.merge(s.purge_messages(&ids, now, None)?);
        Ok((t, None))
    })
    .await
}

async fn blocking<T: Send + 'static>(
    app: &AppHandle,
    f: impl FnOnce(&AppState) -> CmdResult<T> + Send + 'static,
) -> CmdResult<T> {
    let handle = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let st = handle.state::<AppState>();
        f(&st)
    })
    .await
    .map_err(err)?
}

/// 보관 메일 모아 보기(계획서 9.4): 서버에서 지워져 로컬에만 있는 메일, 폴더 구분 없이.
#[tauri::command]
pub async fn list_archived(app: AppHandle, offset: usize, limit: usize) -> CmdResult<Page> {
    blocking(&app, move |st| {
        let t = std::time::Instant::now();
        let total = st.store.archived_total().map_err(err)?;
        let rows = st
            .store
            .archived_rows(offset, limit.min(1000))
            .map_err(err)?;
        Ok(Page {
            total: usize::try_from(total).unwrap_or(0),
            offset,
            rows: rows.into_iter().map(RowView::from).collect(),
            ms: t.elapsed().as_secs_f64() * 1000.0,
        })
    })
    .await
}

/// 전체 선택(Ctrl+A)용: 지금 목록의 메일 ID 전부.
#[tauri::command]
pub async fn list_ids(
    app: AppHandle,
    folder_id: Option<i64>,
    query: Option<String>,
    search_folder: Option<i64>,
) -> CmdResult<Vec<i64>> {
    blocking(&app, move |st| match query {
        Some(q) => {
            let opts = SearchOptions {
                folder: search_folder.and_then(|f| u64::try_from(f).ok()),
                limit: 1_000_000,
                offset: 0,
                sort: Sort::Newest,
            };
            let r = st.search.search(&q, &opts).map_err(err)?;
            Ok(r.ids
                .into_iter()
                .filter_map(|i| i64::try_from(i).ok())
                .collect())
        }
        None if folder_id == Some(crate::view::UNIFIED_ID) => {
            let ids = crate::commands::sorted_ids_cached(
                st,
                crate::view::UNIFIED_ID,
                crate::commands::ListSort {
                    key: eo_store::SortKey::Date,
                    reverse: false,
                },
            )?;
            Ok(ids.to_vec())
        }
        None => st.store.list_ids(folder_id).map_err(err),
    })
    .await
}

// ---- 도구(계획서 9.4) ----

/// 대상: 현재 폴더, 시작·마지막 날짜(yyyymmdd, 한국 시간, 마지막 날 포함) 또는 전체.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Range {
    pub folder_id: i64,
    pub from: Option<String>,
    pub to: Option<String>,
    pub all: bool,
}

/// 그레고리력 날짜 → 1970-01-01부터 날수(Howard Hinnant의 days_from_civil).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// yyyymmdd(한국 시간) 0시의 unix ms.
fn kst_day_ms(s: &str) -> CmdResult<i64> {
    let s = s.trim().replace('-', "");
    if s.len() != 8 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!("날짜는 yyyymmdd로 적어 주세요: {s}"));
    }
    let n = |r: std::ops::Range<usize>| s[r].parse::<i64>().unwrap_or(0);
    let (y, m, d) = (n(0..4), n(4..6), n(6..8));
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return Err(format!("없는 날짜입니다: {s}"));
    }
    Ok(days_from_civil(y, m, d) * 86_400_000 - 9 * 3_600_000)
}

fn range_ms(r: &Range) -> CmdResult<(Option<i64>, Option<i64>)> {
    if r.all {
        return Ok((None, None));
    }
    let from = r
        .from
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(kst_day_ms)
        .transpose()?;
    let to =
        r.to.as_deref()
            .filter(|s| !s.trim().is_empty())
            .map(|s| kst_day_ms(s).map(|ms| ms + 86_400_000))
            .transpose()?;
    if from.is_none() && to.is_none() {
        return Err("시작·마지막 날짜를 적거나 \"전체\"를 고르세요.".into());
    }
    if let (Some(a), Some(b)) = (from, to)
        && a >= b
    {
        return Err("시작 날짜가 마지막 날짜보다 늦습니다.".into());
    }
    Ok((from, to))
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Preview {
    pub count: usize,
    pub bytes: i64,
    /// 원문을 아직 받지 않은 메일(서버에서 삭제 대상에서 빠진다).
    pub without_eml: usize,
    /// 서버에 있는 메일.
    pub on_server: usize,
}

fn targets(st: &AppState, r: &Range) -> CmdResult<Vec<(i64, i64, bool, bool)>> {
    let (from, to) = range_ms(r)?;
    st.store
        .messages_in_range(r.folder_id, from, to)
        .map_err(err)
}

fn preview_of(t: &[(i64, i64, bool, bool)]) -> Preview {
    Preview {
        count: t.len(),
        bytes: t.iter().map(|x| x.1).sum(),
        without_eml: t.iter().filter(|x| !x.2).count(),
        on_server: t.iter().filter(|x| x.3).count(),
    }
}

#[tauri::command]
pub async fn tool_preview(app: AppHandle, range: Range) -> CmdResult<Preview> {
    blocking(&app, move |st| Ok(preview_of(&targets(st, &range)?))).await
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolStart {
    /// 서버 작업 진행을 볼 번호(서버 작업이 없으면 없음).
    pub batch: Option<i64>,
    pub count: usize,
    pub queued: usize,
    pub skipped: usize,
    pub note: Option<String>,
}

/// 도구 실행: `server_delete`(원문 있는 서버 메일만, 로컬 보관 유지), `purge`, `move`, `copy`.
#[tauri::command]
pub async fn tool_run(
    app: AppHandle,
    range: Range,
    action: String,
    to: Option<i64>,
) -> CmdResult<ToolStart> {
    let (t, start) = blocking(&app, move |st| {
        let all = targets(st, &range)?;
        let batch = st.store.new_batch().map_err(err)?;
        let now = now_ms();
        // 연동한 HCL Domino 메일(웹메일에도 반영, 0.6.0).
        let (linked, _) =
            crate::domino::split_linked(&st.store, &all.iter().map(|x| x.0).collect::<Vec<_>>())
                .map_err(err)?;
        let is_linked = |id: i64| linked.iter().any(|l| l.id == id);
        let (ids, skipped, note): (Vec<i64>, usize, Option<String>) = match action.as_str() {
            "server_delete" => {
                let ok = |x: &&(i64, i64, bool, bool)| x.2 && (x.3 || is_linked(x.0));
                let ids: Vec<i64> = all.iter().filter(ok).map(|x| x.0).collect();
                let no_eml = all.iter().filter(|x| !x.2 && x.3).count();
                let note = (no_eml > 0).then(|| format!("원문 없는 {no_eml}통 건너뜀"));
                (ids.clone(), all.len() - ids.len(), note)
            }
            _ => (all.iter().map(|x| x.0).collect(), 0, None),
        };
        let linked: Vec<_> = linked.into_iter().filter(|l| ids.contains(&l.id)).collect();
        let s = &st.store;
        let t = match action.as_str() {
            "server_delete" => {
                let bn: Vec<i64> = linked.iter().map(|l| l.id).collect();
                let rest: Vec<i64> = ids.iter().copied().filter(|i| !bn.contains(i)).collect();
                crate::domino::server_delete_linked(s, &linked, now, true).and_then(|mut t| {
                    t.merge(s.server_delete(&rest, now, Some(batch))?);
                    Ok(t)
                })
            }
            "purge" => crate::domino::server_delete_linked(s, &linked, now, false)
                .and_then(|_| s.purge_messages(&ids, now, Some(batch))),
            "move" => {
                let to = to.ok_or("대상 폴더를 고르세요")?;
                crate::domino::plan_move(s, &ids, to).and_then(|plan| {
                    let t = s.move_messages(&ids, to, now, Some(batch))?;
                    crate::domino::enqueue_plan(s, &plan, now)?;
                    Ok(t)
                })
            }
            "copy" => {
                let to = to.ok_or("대상 폴더를 고르세요")?;
                crate::domino::check_copy(s, &ids, to)
                    .and_then(|()| s.copy_messages(&ids, to, now, Some(batch)))
            }
            other => return Err(format!("알 수 없는 도구: {other}")),
        }
        .map_err(err)?;
        let start = ToolStart {
            batch: (t.queued > 0).then_some(batch),
            count: ids.len(),
            queued: t.queued,
            skipped,
            note,
        };
        Ok((t, start))
    })
    .await?;
    after_local(&app, &t);
    Ok(start)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub pending: i64,
    pub done: i64,
    pub abandoned: i64,
}

#[tauri::command]
pub async fn batch_progress(app: AppHandle, batch: i64) -> CmdResult<Progress> {
    blocking(&app, move |st| {
        let p = st.store.batch_progress(batch).map_err(err)?;
        Ok(Progress {
            pending: p.pending,
            done: p.done,
            abandoned: p.abandoned,
        })
    })
    .await
}

/// 일괄 작업 중지: 아직 서버에 보내지 않은 것은 로컬도 되돌린다. 되돌린 수.
#[tauri::command]
pub async fn cancel_batch(app: AppHandle, batch: i64) -> CmdResult<usize> {
    let t = blocking(&app, move |st| {
        let ops = st.store.cancel_batch(batch).map_err(err)?;
        let mut t = Touched::default();
        for op in &ops {
            t.merge(st.store.rollback_op(op).map_err(err)?);
        }
        Ok(t)
    })
    .await?;
    // 이동은 원래·대상 폴더 양쪽에 적히므로 메일 ID로 센다.
    let mut ids: Vec<i64> = t.folders.values().flatten().copied().collect();
    ids.sort_unstable();
    ids.dedup();
    after_local(&app, &t);
    Ok(ids.len())
}

// ---- 용량 확보 도우미(계획서 9.4) ----

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpacePreview {
    pub used: i64,
    pub limit: i64,
    /// 줄여야 할 양.
    pub need: i64,
    pub count: usize,
    pub bytes: i64,
    pub without_eml: usize,
    pub oldest: Option<i64>,
    pub newest: Option<i64>,
}

/// 오래된 서버 메일부터, 사용량이 `percent`% 이하가 될 만큼(원문 있는 것만 실제로 지운다).
fn space_targets(st: &AppState, percent: f64) -> CmdResult<(SpacePreview, Vec<i64>)> {
    let (used, limit) = {
        let s = lock(&st.status);
        (s.quota_used, s.quota_limit)
    };
    let (Some(used), Some(limit)) = (used, limit) else {
        return Err("메일함 용량을 아직 모릅니다. 로그인한 뒤 다시 하세요.".into());
    };
    if !(1.0..=100.0).contains(&percent) {
        return Err("목표 사용률은 1~100%로 적어 주세요.".into());
    }
    let goal = (limit as f64 * percent / 100.0) as i64;
    let need = (used - goal).max(0);
    let mut picked = Vec::new();
    let (mut bytes, mut without_eml) = (0, 0);
    if need > 0 {
        for (id, size, has_eml) in st.store.oldest_on_server(usize::MAX).map_err(err)? {
            if bytes >= need {
                break;
            }
            if has_eml {
                picked.push(id);
                bytes += size;
            } else {
                without_eml += 1;
            }
        }
    }
    let dates: Vec<i64> = picked
        .iter()
        .filter_map(|id| {
            st.store
                .message(*id)
                .ok()
                .flatten()
                .map(|m| m.props.date_received)
        })
        .collect();
    Ok((
        SpacePreview {
            used,
            limit,
            need,
            count: picked.len(),
            bytes,
            without_eml,
            oldest: dates.iter().min().copied(),
            newest: dates.iter().max().copied(),
        },
        picked,
    ))
}

#[tauri::command]
pub async fn space_preview(app: AppHandle, percent: f64) -> CmdResult<SpacePreview> {
    blocking(&app, move |st| Ok(space_targets(st, percent)?.0)).await
}

#[tauri::command]
pub async fn space_run(app: AppHandle, percent: f64) -> CmdResult<ToolStart> {
    let (t, start) = blocking(&app, move |st| {
        let (p, ids) = space_targets(st, percent)?;
        let batch = st.store.new_batch().map_err(err)?;
        let t = st
            .store
            .server_delete(&ids, now_ms(), Some(batch))
            .map_err(err)?;
        Ok((
            t.clone(),
            ToolStart {
                batch: (t.queued > 0).then_some(batch),
                count: ids.len(),
                queued: t.queued,
                skipped: p.without_eml,
                note: None,
            },
        ))
    })
    .await?;
    after_local(&app, &t);
    Ok(start)
}

// ---- 내보내기(계획서 9.4) ----

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportTarget {
    /// 고른 메일(목록 선택).
    pub ids: Option<Vec<i64>>,
    /// 또는 도구의 대상 범위.
    pub range: Option<Range>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ExportProgress {
    done: usize,
    total: usize,
    skipped: usize,
    finished: bool,
    error: Option<String>,
    path: String,
}

/// unix ms → 한국 시간 (yyyy, mm, "yyyymmdd_HHMMSS").
fn kst_parts(ms: i64) -> (String, String, String) {
    let secs = ms.div_euclid(1000) + 9 * 3600;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (
        format!("{y:04}"),
        format!("{m:02}"),
        format!(
            "{y:04}{m:02}{d:02}_{:02}{:02}{:02}",
            rem / 3600,
            (rem / 60) % 60,
            rem % 60
        ),
    )
}

/// `폴더명\yyyy\mm\yyyymmdd_HHMMSS_제목_해시.eml`(계획서 9.4).
fn export_path(folder: &str, subject: &str, received: i64, hash: &str) -> Vec<String> {
    let (y, m, stamp) = kst_parts(received);
    let title: String = subject.chars().take(60).collect();
    let title = if title.trim().is_empty() {
        "제목 없음".to_owned()
    } else {
        title
    };
    let name = files::safe_file_name(&format!(
        "{stamp}_{title}_{}.eml",
        &hash[..hash.len().min(8)]
    ));
    vec![files::safe_file_name(folder), y, m, name]
}

/// 내보내기: 폴더(또는 ZIP 파일) 위치를 묻고 뒤에서 쓴다. 진행은 `export-progress` 이벤트.
/// 취소하면 `None`.
#[tauri::command]
pub async fn export_mail(
    app: AppHandle,
    target: ExportTarget,
    zip: bool,
) -> CmdResult<Option<usize>> {
    let ids = blocking(&app, move |st| match (target.ids, target.range) {
        (Some(ids), _) => Ok(ids),
        (None, Some(r)) => Ok(targets(st, &r)?.into_iter().map(|x| x.0).collect()),
        (None, None) => Err("내보낼 메일이 없습니다".into()),
    })
    .await?;
    if ids.is_empty() {
        return Err("내보낼 메일이 없습니다".into());
    }
    let handle = app.clone();
    let dest: Option<PathBuf> = tauri::async_runtime::spawn_blocking(move || {
        let d = handle.dialog().file();
        let picked = if zip {
            d.set_file_name("메일 내보내기.zip")
                .add_filter("ZIP", &["zip"])
                .blocking_save_file()
        } else {
            d.blocking_pick_folder()
        };
        picked.and_then(|p| p.into_path().ok())
    })
    .await
    .map_err(err)?;
    let Some(dest) = dest else {
        return Ok(None);
    };
    let total = ids.len();
    let st = app.state::<AppState>();
    st.export_cancel.store(false, Ordering::Relaxed);
    let handle = app.clone();
    std::thread::spawn(move || {
        let st = handle.state::<AppState>();
        let r = write_export(&handle, &st, &ids, &dest, zip);
        let (done, skipped, error) = match r {
            Ok((d, s)) => (d, s, None),
            Err(e) => (0, 0, Some(e)),
        };
        match &error {
            None => crate::applog::info(format!(
                "내보내기({}): {done}통, 원문 없어 건너뜀 {skipped}통",
                if zip { "ZIP" } else { "폴더" }
            )),
            Some(e) => crate::applog::warn(format!("내보내기 실패·중지: {e}")),
        }
        let _ = handle.emit(
            "export-progress",
            ExportProgress {
                done,
                total,
                skipped,
                finished: true,
                error,
                path: dest.display().to_string(),
            },
        );
    });
    Ok(Some(total))
}

fn write_export(
    app: &AppHandle,
    st: &AppState,
    ids: &[i64],
    dest: &Path,
    zip: bool,
) -> CmdResult<(usize, usize)> {
    let folder_names: std::collections::HashMap<i64, String> = st
        .store
        .folders()
        .map_err(err)?
        .into_iter()
        .map(|f| (f.id, f.name))
        .collect();
    let mut writer = if zip {
        let f = std::fs::File::create(dest).map_err(err)?;
        Some(zip::ZipWriter::new(f))
    } else {
        None
    };
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .large_file(false);
    let mut used = std::collections::HashSet::new();
    let (mut done, mut skipped) = (0, 0);
    let mut last_emit = std::time::Instant::now();
    for (k, id) in ids.iter().enumerate() {
        if st.export_cancel.load(Ordering::Relaxed) {
            return Err(format!("중지했습니다({done}통 내보냄)"));
        }
        let Some(m) = st.store.message(*id).map_err(err)? else {
            skipped += 1;
            continue;
        };
        let Some(hash) = m.eml_hash.clone() else {
            skipped += 1;
            continue;
        };
        let raw = st.store.eml().get(&hash).map_err(err)?;
        let folder = folder_names
            .get(&m.folder_id)
            .map_or("폴더", String::as_str);
        let mut parts = export_path(
            folder,
            m.props.subject.as_deref().unwrap_or(""),
            m.props.date_received,
            &hash,
        );
        // 같은 이름이면 메일 번호를 붙인다.
        let mut key = parts.join("/");
        if !used.insert(key.clone()) {
            if let Some(last) = parts.last_mut() {
                *last = last.replacen(".eml", &format!("_{id}.eml"), 1);
            }
            key = parts.join("/");
            used.insert(key.clone());
        }
        match &mut writer {
            Some(w) => {
                w.start_file(key, opts).map_err(err)?;
                w.write_all(&raw).map_err(err)?;
            }
            None => {
                let mut path = dest.to_path_buf();
                for p in &parts[..parts.len() - 1] {
                    path.push(p);
                }
                std::fs::create_dir_all(&path).map_err(err)?;
                path.push(&parts[parts.len() - 1]);
                files::write_marked(&path, &raw).map_err(err)?;
            }
        }
        done += 1;
        if last_emit.elapsed().as_millis() >= 200 {
            last_emit = std::time::Instant::now();
            let _ = app.emit(
                "export-progress",
                ExportProgress {
                    done: k + 1,
                    total: ids.len(),
                    skipped,
                    finished: false,
                    error: None,
                    path: dest.display().to_string(),
                },
            );
        }
    }
    if let Some(w) = writer {
        w.finish().map_err(err)?;
        // ZIP 파일에도 "인터넷에서 받은 파일" 표시.
        let mut ads = dest.as_os_str().to_owned();
        ads.push(":Zone.Identifier");
        let _ = std::fs::write(PathBuf::from(ads), "[ZoneTransfer]\r\nZoneId=3\r\n");
    }
    Ok((done, skipped))
}

/// `--ui-test` 전용: 대화 창 없이 `EnhancedOutlook\dev\uitest-export(.zip)`에 내보낸다. (내보낸 수, 건너뛴 수)
#[tauri::command]
pub async fn ui_test_export(app: AppHandle, ids: Vec<i64>, zip: bool) -> CmdResult<(usize, usize)> {
    blocking(&app.clone(), move |st| {
        if !st.ui_test {
            return Err("점검 모드가 아닙니다".into());
        }
        let dev = st.root.join("dev");
        std::fs::create_dir_all(&dev).map_err(err)?;
        let dest = dev.join(if zip {
            "uitest-export.zip"
        } else {
            "uitest-export"
        });
        write_export(&app, st, &ids, &dest, zip)
    })
    .await
}

/// `--ui-test` 전용: 앞에서 내보낸 EML 파일 경로들(첨부 점검용).
#[tauri::command]
pub async fn ui_test_export_files(app: AppHandle) -> CmdResult<Vec<String>> {
    blocking(&app, move |st| {
        if !st.ui_test {
            return Err("점검 모드가 아닙니다".into());
        }
        fn walk(dir: &Path, out: &mut Vec<String>) {
            if let Ok(rd) = std::fs::read_dir(dir) {
                for e in rd.flatten() {
                    let p = e.path();
                    if p.is_dir() {
                        walk(&p, out);
                    } else {
                        out.push(p.display().to_string());
                    }
                }
            }
        }
        let mut out = Vec::new();
        walk(&st.root.join("dev").join("uitest-export"), &mut out);
        Ok(out)
    })
    .await
}

/// `--ui-test`·`--mem-test` 전용: 열려 있는 창 이름들.
#[tauri::command]
pub fn ui_test_window_labels(app: AppHandle) -> Vec<String> {
    let st = app.state::<AppState>();
    if !(st.ui_test || st.perf_test) {
        return Vec::new();
    }
    app.webview_windows().keys().cloned().collect()
}

/// `--ui-test`·`--mem-test` 전용: 점검이 연 작성 창·메일 창을 닫는다.
#[tauri::command]
pub async fn ui_test_close_window(app: AppHandle, label: String) -> CmdResult<()> {
    let st = app.state::<AppState>();
    if !(st.ui_test || st.perf_test)
        || !(label.starts_with("compose-") || label.starts_with("message-"))
    {
        return Err("점검 모드가 아닙니다".into());
    }
    if let Some(w) = app.get_webview_window(&label) {
        w.destroy().map_err(err)?;
    }
    Ok(())
}

#[tauri::command]
pub fn cancel_export(app: AppHandle) {
    app.state::<AppState>()
        .export_cancel
        .store(true, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kst_dates_and_ranges() {
        // 2026-10-16 00:00 KST = 2026-10-15T15:00:00Z
        assert_eq!(kst_day_ms("20261016").unwrap(), 1_792_076_400_000);
        let r = Range {
            folder_id: 1,
            from: Some("2026-10-01".into()),
            to: Some("20261016".into()),
            all: false,
        };
        let (a, b) = range_ms(&r).unwrap();
        assert_eq!(b.unwrap() - a.unwrap(), 16 * 86_400_000, "마지막 날 포함");
        assert!(kst_day_ms("2026101").is_err());
        assert!(kst_day_ms("20261341").is_err());
        assert!(
            range_ms(&Range {
                folder_id: 1,
                from: None,
                to: None,
                all: false
            })
            .is_err()
        );
        assert_eq!(kst_parts(1_792_040_798_000).2, "20261015_140638");
        let p = export_path(
            "받은 편지함",
            "보고서: 10월",
            1_792_040_798_000,
            "abcdef0123",
        );
        assert_eq!(
            p,
            [
                "받은 편지함",
                "2026",
                "10",
                "20261015_140638_보고서_ 10월_abcdef01.eml"
            ]
        );
    }
}
