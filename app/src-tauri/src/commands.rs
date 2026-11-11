//! 화면이 부르는 Tauri 명령(계획서 4.1-5: 필요한 열만, 200행 단위 창).
//! 화면 동작 중에는 네트워크를 기다리지 않는다(4.1-1). 로그인·지금 동기화만 서버와 관련된다.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use eo_mime::RenderOptions;
use eo_owa::{Credentials, Password};
use eo_search::{SearchOptions, Sort};
use eo_store::SortKey;
use eo_sync::{Engine, EngineConfig, Event};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_opener::OpenerExt;
use tokio::sync::{broadcast, watch};

use crate::applog;
use crate::files;
use crate::settings::{
    Cleanup, CleanupMode, INBOX_CHOICES, OTHERS_CHOICES, RemoteImages, Settings,
};
use crate::state::{AppState, Session, Status, lock};
use crate::view::{
    AttachmentView, FolderView, MessageView, RowView, UNIFIED_ID, folder_tree_for, unified_members,
};

type CmdResult<T> = Result<T, String>;

/// `--image-check`: 본문 그림 점검 모드(읽기만, 실데이터에서도 돈다). 메일을 열어 그려 보기만 하고 아무것도 바꾸지 않는다.
pub static IMAGE_CHECK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn image_check() -> bool {
    IMAGE_CHECK.load(std::sync::atomic::Ordering::Relaxed)
}

/// `--mem-test`: 같은 조작을 되풀이하며 메모리를 잰다(누수 점검, 성능 측정과 같은 환경).
pub static MEM_TEST: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 점검할 메일: 원문이 있고 본문에 그림 참조가 있는 것.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckItem {
    pub id: i64,
    pub date: i64,
    pub folder: String,
    pub origin: Option<String>,
}

/// `--image-check` 전용: 그림 참조가 있는 메일 목록(오래 걸린다: 원문을 모두 해석한다).
#[tauri::command]
pub async fn image_check_list(app: AppHandle) -> CmdResult<Vec<CheckItem>> {
    if !image_check() {
        return Err("점검 모드가 아닙니다".into());
    }
    blocking(move || {
        let st = app.state::<AppState>();
        let names: std::collections::HashMap<i64, String> = st
            .store
            .folders()
            .map_err(err)?
            .into_iter()
            .map(|f| (f.id, f.name))
            .collect();
        let mut out = Vec::new();
        for id in st.store.list_all_ids().map_err(err)? {
            let Some(m) = st.store.message(id).map_err(err)? else {
                continue;
            };
            let Some(hash) = m.eml_hash.as_deref() else {
                continue;
            };
            let Ok(raw) = st.store.eml().get(hash) else {
                continue;
            };
            if eo_mime::image_report(&raw).is_some_and(|r| !r.refs.is_empty()) {
                out.push(CheckItem {
                    id,
                    date: m.props.date_received,
                    folder: names.get(&m.folder_id).cloned().unwrap_or_default(),
                    origin: m.origin.clone(),
                });
            }
        }
        Ok(out)
    })
    .await
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// 무거운 일(DB·원문 해석)은 비동기 작업자를 막지 않게 별도 스레드에서.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> CmdResult<T> + Send + 'static,
) -> CmdResult<T> {
    tauri::async_runtime::spawn_blocking(f).await.map_err(err)?
}

pub fn update_status(app: &AppHandle, f: impl FnOnce(&mut Status)) {
    let st = app.state::<AppState>();
    let snapshot = {
        let mut s = lock(&st.status);
        f(&mut s);
        s.clone()
    };
    let _ = app.emit("status", snapshot);
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Bootstrap {
    pub folders: Vec<FolderView>,
    pub status: Status,
    /// 이번 실행 중 로그인한 아이디(메모리에만 있음).
    pub username: Option<String>,
    pub perf_test: bool,
    /// `--mem-test`(성능 측정 대신 메모리 반복 점검).
    pub mem_test: bool,
    pub root: String,
    pub indexed: u64,
    /// 폴더 트리·건수 계산 시간(ms).
    pub ms: f64,
    /// 시작 단계별 시각(프로세스 시작 후 ms, 측정용).
    pub marks: Vec<(String, f64)>,
    /// 받은 편지함(없으면 첫 폴더)의 첫 페이지. 첫 화면을 명령 한 번으로 그린다(계획서 4.1-3).
    pub first: Option<FirstPage>,
    /// `--ui-test`이고 합성 저장소일 때만(조작을 실제로 해 보므로 실데이터에서는 돌리지 않는다).
    pub ui_test: bool,
    /// `--image-check`(본문 그림 점검, 읽기만).
    pub image_check: bool,
    /// 폴더 목록·메일 목록 폭(px, 지난번에 끌어 바꾼 것. 측정·점검은 늘 기본).
    pub pane_widths: Option<(f64, f64)>,
    /// 메일 목록 정렬(지난번에 고른 것. 측정·점검은 늘 최신순).
    pub list_sort: Option<crate::settings::SavedSort>,
    /// 지난 실행이 비정상 종료됐다(한 번 알린다).
    pub crashed_before: bool,
    /// HCL Domino 계정 연동을 켜 두었다(로그인 뒤 HCL Domino 로그인 창을 띄운다).
    pub domino_enabled: bool,
    /// Outlook 웹메일 주소(비면 처음 실행: 주소부터 묻는다).
    pub outlook_url: String,
    /// 로그인 창에 채울 아이디(지난번에 로그인한 것, 측정·점검은 없음).
    pub remembered_user: Option<String>,
    pub remembered_domino_user: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FirstPage {
    pub folder_id: i64,
    pub page: Page,
}

#[tauri::command]
pub async fn bootstrap(app: AppHandle, state: State<'_, AppState>) -> CmdResult<Bootstrap> {
    let username = state.session.lock().await.username.clone();
    let status = lock(&state.status).clone();
    let (perf_test, root, indexed, marks) = (
        state.perf_test,
        state.root.display().to_string(),
        state.search.num_docs(),
        lock(&state.marks).clone(),
    );
    let t = Instant::now();
    // 늘 받은 편지함에서 시작한다(사용자 요청 2026-10-21; 전에는 마지막으로 본 폴더). 통합 편지함을 켰으면 통합 편지함,
    // 받은 편지함이 없으면 첫 폴더.
    let (folders, first) = blocking(move || {
        let st = app.state::<AppState>();
        let folders = folder_list(&st)?;
        let start = folders
            .iter()
            .find(|f| f.id == UNIFIED_ID)
            .or_else(|| {
                folders
                    .iter()
                    .find(|f| f.well_known.as_deref() == Some("inbox"))
            })
            .or_else(|| folders.iter().find(|f| f.id >= 0));
        let first = match start {
            Some(f) => {
                let t = Instant::now();
                let rows = if f.id == UNIFIED_ID {
                    let ids = sorted_ids_cached(&st, UNIFIED_ID, NEWEST)?;
                    st.store
                        .rows_by_ids(&ids[..ids.len().min(200)])
                        .map_err(err)?
                } else {
                    st.store.list_rows(f.id, 0, 200).map_err(err)?
                };
                Some(FirstPage {
                    folder_id: f.id,
                    page: Page {
                        total: usize::try_from(f.total).unwrap_or(0),
                        offset: 0,
                        rows: rows.into_iter().map(RowView::from).collect(),
                        ms: ms_since(t),
                    },
                })
            }
            None => None,
        };
        Ok((folders, first))
    })
    .await?;
    let ui_test = state.ui_test
        && state
            .store
            .folders()
            .map_err(err)?
            .iter()
            .any(|f| f.server_id.as_deref() == Some("SYN-INBOX"));
    Ok(Bootstrap {
        folders,
        status,
        username,
        perf_test,
        mem_test: perf_test && MEM_TEST.load(std::sync::atomic::Ordering::Relaxed),
        root,
        indexed,
        ms: ms_since(t),
        marks,
        first,
        ui_test,
        image_check: image_check(),
        pane_widths: if state.perf_test || state.ui_test {
            None
        } else {
            let ui = lock(&state.ui);
            ui.folders_width.zip(ui.list_width)
        },
        list_sort: if state.perf_test || state.ui_test {
            None
        } else {
            lock(&state.ui).list_sort
        },
        crashed_before: state.crashed_before,
        // 포털 주소가 없으면(앞 버전 설정) 로그인 창을 띄우지 않는다: 설정에서 주소를 넣으면 그때 뜬다.
        domino_enabled: !state.perf_test && !state.ui_test && {
            let s = lock(&state.settings);
            s.domino.enabled && !s.domino.portal.is_empty()
        },
        outlook_url: lock(&state.settings).outlook_url.clone(),
        remembered_user: (!state.perf_test && !state.ui_test)
            .then(|| lock(&state.ui).remembered_user.clone())
            .flatten(),
        remembered_domino_user: (!state.perf_test && !state.ui_test)
            .then(|| lock(&state.ui).remembered_domino_user.clone())
            .flatten(),
    })
}

/// 처음 실행 때 넣은 Outlook 웹메일 주소를 저장하고 그 주소로 연결한다. 고친 주소를 돌려준다.
#[tauri::command]
pub async fn set_outlook_url(app: AppHandle, url: String) -> CmdResult<String> {
    let st = app.state::<AppState>();
    let new = Settings {
        outlook_url: url,
        ..lock(&st.settings).clone()
    }
    .normalized();
    if new.outlook_url.is_empty() {
        return Err("Outlook 웹메일 주소를 넣어 주세요.".into());
    }
    // Outlook 주소만 본다(HCL Domino 포털 주소는 설정에서 따로 넣는다).
    new.check_outlook()?;
    crate::settings::save_settings(&st.root, &new)
        .map_err(|e| format!("설정을 저장하지 못했습니다: {e}"))?;
    st.set_outlook_url(&new.outlook_url);
    *lock(&st.settings) = new.clone();
    applog::info("Outlook 웹메일 주소 설정");
    Ok(new.outlook_url)
}

#[tauri::command]
pub async fn folders(app: AppHandle) -> CmdResult<Vec<FolderView>> {
    blocking(move || folder_list(&app.state::<AppState>())).await
}

/// 화면의 폴더 목록(통합 편지함·즐겨찾기 표시 포함).
pub fn folder_list(st: &AppState) -> CmdResult<Vec<FolderView>> {
    let tree = folder_tree_for(&st.store, unified_on(st)).map_err(err)?;
    let favorites = lock(&st.ui).favorites.clone();
    Ok(crate::view::mark_favorites(tree, &favorites))
}

/// 통합 편지함을 쓰는가(HCL Domino 연동을 켜고 통합 편지함도 켰을 때).
pub fn unified_on(st: &AppState) -> bool {
    let s = lock(&st.settings);
    !st.perf_test && s.domino.enabled && s.domino.unified
}

/// 기본 정렬(최신순).
const NEWEST: ListSort = ListSort {
    key: SortKey::Date,
    reverse: false,
};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Page {
    pub total: usize,
    pub offset: usize,
    pub rows: Vec<RowView>,
    /// Rust 쪽 처리 시간(ms).
    pub ms: f64,
}

fn ms_since(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

#[tauri::command]
pub async fn list_messages(
    app: AppHandle,
    folder_id: i64,
    offset: usize,
    limit: usize,
    sort: Option<ListSort>,
) -> CmdResult<Page> {
    blocking(move || {
        let t = Instant::now();
        let st = app.state::<AppState>();
        if folder_id == UNIFIED_ID {
            // 통합 편지함: 여러 폴더를 정렬해 기억해 둔 순서에서 쪽마다 자른다.
            let ids = sorted_ids_cached(&st, folder_id, sort.unwrap_or(NEWEST))?;
            let end = (offset + limit.min(1000)).min(ids.len());
            let page = ids.get(offset.min(end)..end).unwrap_or(&[]);
            return Ok(Page {
                total: ids.len(),
                offset,
                rows: st
                    .store
                    .rows_by_ids(page)
                    .map_err(err)?
                    .into_iter()
                    .map(RowView::from)
                    .collect(),
                ms: ms_since(t),
            });
        }
        let total = st.store.folder_total(folder_id).map_err(err)?;
        let rows = match sort.filter(|s| !(s.key == SortKey::Date && !s.reverse)) {
            // 기본(최신순)은 색인을 타는 빠른 길.
            None => st
                .store
                .list_rows(folder_id, offset, limit.min(1000))
                .map_err(err)?,
            Some(s) => {
                let ids = sorted_ids_cached(&st, folder_id, s)?;
                let end = (offset + limit.min(1000)).min(ids.len());
                let page = ids.get(offset.min(end)..end).unwrap_or(&[]);
                st.store.rows_by_ids(page).map_err(err)?
            }
        };
        let mut rows: Vec<RowView> = rows.into_iter().map(RowView::from).collect();
        // 임시 보관함: 보내는 중·발송 실패 표시(0.7.0).
        if st
            .store
            .folder_by_well_known("drafts")
            .ok()
            .flatten()
            .map(|f| f.id)
            == Some(folder_id)
        {
            let ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
            let states = st.store.draft_states(&ids).map_err(err)?;
            for r in &mut rows {
                r.draft = states.get(&r.id).map(|(_, s)| *s);
            }
        }
        Ok(Page {
            total: usize::try_from(total).unwrap_or(0),
            offset,
            rows,
            ms: ms_since(t),
        })
    })
    .await
}

/// 목록 정렬(사용자 요청 2026-10-21): 기준과, 기준의 기본 방향을 뒤집을지.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListSort {
    pub key: SortKey,
    pub reverse: bool,
}

/// 메일이 바뀔 때마다 올린다(정렬해 둔 순서를 다시 계산하게).
static LIST_GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub fn bump_list_gen() {
    LIST_GEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// (폴더, 정렬, 세대, 정렬한 메일 ID).
type SortedIds = (i64, ListSort, u64, Arc<Vec<i64>>);

/// 쪽마다 폴더 전체를 다시 정렬하지 않게 하나만 기억한다.
static SORTED: std::sync::Mutex<Option<SortedIds>> = std::sync::Mutex::new(None);

pub fn sorted_ids_cached(st: &AppState, folder_id: i64, s: ListSort) -> CmdResult<Arc<Vec<i64>>> {
    let generation = LIST_GEN.load(std::sync::atomic::Ordering::Relaxed);
    if let Some((f, k, g, ids)) = lock(&SORTED).as_ref()
        && (*f, *k, *g) == (folder_id, s, generation)
    {
        return Ok(ids.clone());
    }
    let folders = if folder_id == UNIFIED_ID {
        unified_members(&st.store).map_err(err)?
    } else {
        vec![folder_id]
    };
    let ids = Arc::new(
        st.store
            .sorted_ids_in(&folders, s.key, s.reverse)
            .map_err(err)?,
    );
    *lock(&SORTED) = Some((folder_id, s, generation, ids.clone()));
    Ok(ids)
}

/// 검색(계획서 8장): tantivy에서 ID만 받아 목록 행은 SQLite에서 채운다(8.3).
#[tauri::command]
pub async fn search(
    app: AppHandle,
    query: String,
    folder_id: Option<i64>,
    offset: usize,
    limit: usize,
) -> CmdResult<Page> {
    blocking(move || {
        let t = Instant::now();
        let st = app.state::<AppState>();
        let opts = SearchOptions {
            folder: folder_id.and_then(|f| u64::try_from(f).ok()),
            limit: limit.min(1000),
            offset,
            sort: Sort::Newest,
        };
        let r = st.search.search(&query, &opts).map_err(err)?;
        let ids: Vec<i64> = r
            .ids
            .iter()
            .filter_map(|&id| i64::try_from(id).ok())
            .collect();
        let rows = st.store.rows_by_ids(&ids).map_err(err)?;
        Ok(Page {
            total: r.total,
            offset,
            rows: rows.into_iter().map(RowView::from).collect(),
            ms: ms_since(t),
        })
    })
    .await
}

fn render_view(st: &AppState, id: i64, allow_remote: bool) -> CmdResult<MessageView> {
    let m = st
        .store
        .message(id)
        .map_err(err)?
        .ok_or("메일이 없습니다")?;
    let p = &m.props;
    let sender = p.from_addr.clone().unwrap_or_default();
    let always = lock(&st.settings).remote_images == RemoteImages::Always;
    let allow = allow_remote
        || always
        || (!sender.is_empty() && st.store.remote_images_allowed(&sender).unwrap_or(false));
    let from_db = match (&p.from_name, &p.from_addr) {
        (Some(n), Some(a)) if !n.is_empty() => format!("{n} <{a}>"),
        (n, a) => a.clone().or_else(|| n.clone()).unwrap_or_default(),
    };
    let mut v = MessageView {
        id,
        folder_id: m.folder_id,
        subject: p.subject.clone().unwrap_or_default(),
        from: from_db,
        sender,
        to: p.to_list.clone().unwrap_or_default(),
        cc: p.cc_list.clone().unwrap_or_default(),
        bcc: String::new(),
        reply_to: String::new(),
        date: p.date_sent.unwrap_or(p.date_received),
        html: String::new(),
        has_remote_images: false,
        remote_allowed: allow,
        attachments: Vec::new(),
        archived_at: m.archived_at,
        origin: m.origin.clone(),
        has_eml: m.eml_hash.is_some(),
        inline_images: 0,
        linked: None,
    };
    let preview = p.preview.clone().unwrap_or_default();
    let Some(raw) = st.raw(id, m.eml_hash.as_deref()) else {
        v.html = eo_mime::text_document(&format!("원문을 아직 받지 않았습니다.\n\n{preview}"));
        return Ok(v);
    };
    let opts = RenderOptions {
        cid_base: format!("http://app-cid.localhost/{id}/"),
        allow_remote: allow,
    };
    let Some(r) = eo_mime::render(&raw, &opts) else {
        v.html = eo_mime::text_document(&format!("원문을 해석할 수 없습니다.\n\n{preview}"));
        return Ok(v);
    };
    // 인라인 이미지는 지금 꺼내 둔다: 본문이 이미지마다 app-cid로 요청할 때 원문을 다시 해석하지 않게.
    v.inline_images = r.inline_images.len();
    if !r.inline_images.is_empty() {
        let map = r
            .inline_images
            .into_iter()
            .map(|i| (i.cid, (i.mime, i.data)))
            .collect();
        lock(&st.cids).put(id, Arc::new(map));
    }
    let keep = |s: String, fallback: String| if s.is_empty() { fallback } else { s };
    v.subject = keep(r.subject, v.subject);
    v.from = keep(r.from, v.from);
    v.to = keep(r.to, v.to);
    v.cc = keep(r.cc, v.cc);
    v.bcc = r.bcc;
    v.reply_to = r.reply_to;
    v.date = r.date.unwrap_or(v.date);
    v.html = r.html;
    v.has_remote_images = r.has_remote_images;
    v.attachments = r
        .attachments
        .into_iter()
        .filter(|a| !a.inline)
        .map(|a| AttachmentView {
            index: a.index,
            name: a.name,
            mime: a.mime,
            size: a.size,
        })
        .collect();
    crate::linked::decorate(st, id, &mut v);
    Ok(v)
}

/// 캐시 우선(계획서 4.1-6). 원문이 있는 것만 캐시한다(원문이 오면 다시 그려야 하므로).
pub(crate) fn cached_or_render(
    st: &AppState,
    id: i64,
    allow_remote: bool,
) -> CmdResult<(MessageView, bool)> {
    if let Some(v) = lock(&st.views).get(&id)
        && (!allow_remote || v.remote_allowed || !v.has_remote_images)
    {
        return Ok((v, true));
    }
    let v = render_view(st, id, allow_remote)?;
    if v.has_eml {
        lock(&st.views).put(id, v.clone());
    }
    Ok((v, false))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Opened {
    pub msg: MessageView,
    pub cached: bool,
    pub ms: f64,
}

#[tauri::command]
pub async fn open_message(app: AppHandle, id: i64, allow_remote: bool) -> CmdResult<Opened> {
    blocking(move || {
        let t = Instant::now();
        let st = app.state::<AppState>();
        let (msg, cached) = cached_or_render(&st, id, allow_remote)?;
        Ok(Opened {
            msg,
            cached,
            ms: ms_since(t),
        })
    })
    .await
}

/// 메일 한 통을 새 창에서 연다(두 번 누르기·"새 창에서 열기"). 이미 열려 있으면 앞으로.
#[tauri::command]
pub async fn open_message_window(app: AppHandle, id: i64) -> CmdResult<()> {
    let subject = {
        let st = app.state::<AppState>();
        st.store
            .message(id)
            .map_err(err)?
            .ok_or("메일이 없습니다")?
            .props
            .subject
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "(제목 없음)".into())
    };
    crate::compose::open_app_window(&app, &format!("message-{id}"), &subject)
}

/// 현재 상태(연결·동기화·용량). 메일 보기 창이 처음 열릴 때.
#[tauri::command]
pub fn app_status(app: AppHandle) -> Status {
    lock(&app.state::<AppState>().status).clone()
}

/// 선읽기(계획서 4.1-6): 위·아래 메일을 미리 해석해 둔다. 기다리지 않는다.
#[tauri::command]
pub fn prefetch(app: AppHandle, ids: Vec<i64>) {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        for id in ids {
            if !lock(&st.views).contains(&id) {
                let _ = cached_or_render(&st, id, false);
            }
        }
    });
}

/// 외부 이미지 허용 목록.
#[derive(Serialize)]
pub struct RemoteAllowlist {
    pub senders: Vec<String>,
    pub domains: Vec<String>,
}

/// 허용 목록에 넣을 값을 맞춘다: 보낸 사람은 `이름@도메인`, 도메인은 `@`를 뗀 `example.com`.
fn allow_value(domain: bool, value: &str) -> CmdResult<String> {
    let v = value
        .trim()
        .trim_start_matches(if domain { "@" } else { "" })
        .to_lowercase();
    let ok = if domain {
        v.contains('.') && !v.contains('@') && !v.contains(char::is_whitespace)
    } else {
        v.split_once('@')
            .is_some_and(|(a, d)| !a.is_empty() && d.contains('.'))
            && !v.contains(char::is_whitespace)
    };
    if ok {
        Ok(v)
    } else if domain {
        Err(format!("도메인 형식이 아닙니다: {value}"))
    } else {
        Err(format!("주소 형식이 아닙니다: {value}"))
    }
}

/// 외부 이미지를 늘 보여 줄 보낸 사람(`kind` = "sender") 또는 도메인("domain")을 더한다.
#[tauri::command]
pub async fn allow_remote(app: AppHandle, kind: String, value: String) -> CmdResult<String> {
    blocking(move || {
        let st = app.state::<AppState>();
        let domain = kind == "domain";
        let v = allow_value(domain, &value)?;
        if domain {
            st.store.allow_remote_domain(&v).map_err(err)?;
        } else {
            st.store.allow_remote_images(&v).map_err(err)?;
        }
        // 읽기 창 캐시에는 막은 채로 그린 본문이 있다.
        lock(&st.views).clear();
        Ok(v)
    })
    .await
}

#[tauri::command]
pub async fn remove_remote_allow(app: AppHandle, kind: String, value: String) -> CmdResult<()> {
    blocking(move || {
        let st = app.state::<AppState>();
        st.store
            .remove_remote_allow(kind == "domain", &value)
            .map_err(err)?;
        lock(&st.views).clear();
        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn remote_allowlist(app: AppHandle) -> CmdResult<RemoteAllowlist> {
    blocking(move || {
        let (senders, domains) = app
            .state::<AppState>()
            .store
            .remote_image_allowlist()
            .map_err(err)?;
        Ok(RemoteAllowlist { senders, domains })
    })
    .await
}

fn attachment_bytes(st: &AppState, id: i64, index: usize) -> CmdResult<(String, Vec<u8>)> {
    let raw = st.raw(id, None).ok_or("원문이 없습니다")?;
    let (name, _mime, bytes) = eo_mime::attachment(&raw, index).ok_or("첨부를 찾을 수 없습니다")?;
    Ok((files::safe_file_name(&name), bytes))
}

/// 첨부 저장: 저장 위치를 묻는다. 취소하면 `None`.
#[tauri::command]
pub async fn save_attachment(app: AppHandle, id: i64, index: usize) -> CmdResult<Option<String>> {
    blocking(move || {
        let st = app.state::<AppState>();
        let (name, bytes) = attachment_bytes(&st, id, index)?;
        let Some(picked) = app
            .dialog()
            .file()
            .set_file_name(&name)
            .blocking_save_file()
        else {
            return Ok(None);
        };
        let path = picked.into_path().map_err(err)?;
        files::write_marked(&path, &bytes).map_err(err)?;
        Ok(Some(path.display().to_string()))
    })
    .await
}

/// 모두 저장: 폴더를 묻고, 같은 이름이 있으면 번호를 붙인다. 저장한 수.
#[tauri::command]
pub async fn save_all_attachments(app: AppHandle, id: i64) -> CmdResult<Option<usize>> {
    blocking(move || {
        let st = app.state::<AppState>();
        let raw = st.raw(id, None).ok_or("원문이 없습니다")?;
        let opts = RenderOptions {
            cid_base: String::new(),
            allow_remote: false,
        };
        let r = eo_mime::render(&raw, &opts).ok_or("원문을 해석할 수 없습니다")?;
        let Some(dir) = app.dialog().file().blocking_pick_folder() else {
            return Ok(None);
        };
        let dir = dir.into_path().map_err(err)?;
        let mut n = 0;
        for a in r.attachments.iter().filter(|a| !a.inline) {
            let Some((name, _, bytes)) = eo_mime::attachment(&raw, a.index) else {
                continue;
            };
            let path = files::unique_path(&dir, &files::safe_file_name(&name));
            files::write_marked(&path, &bytes).map_err(err)?;
            n += 1;
        }
        Ok(Some(n))
    })
    .await
}

/// 첨부 열기: `EnhancedOutlook\tmp\{id}\`에 사본을 쓰고 연결 프로그램으로 연다. 실행 형식은 거부.
#[tauri::command]
pub async fn open_attachment(app: AppHandle, id: i64, index: usize) -> CmdResult<()> {
    blocking(move || {
        let st = app.state::<AppState>();
        let (name, bytes) = attachment_bytes(&st, id, index)?;
        if files::is_blocked(&name, &lock(&st.blocked_types)) {
            return Err(format!(
                "바로 열 수 없는 형식입니다(.{}). 저장해서 여세요.",
                files::extension(&name)
            ));
        }
        let dir = st.root.join("tmp").join(id.to_string());
        std::fs::create_dir_all(&dir).map_err(err)?;
        let path = dir.join(&name);
        files::write_marked(&path, &bytes).map_err(err)?;
        app.opener()
            .open_path(path.display().to_string(), None::<&str>)
            .map_err(err)
    })
    .await
}

// ---- 로그인·동기화 (계획서 9.5) ----

fn stop_engine(session: &mut Session) {
    if let Some(stop) = session.stop.take() {
        let _ = stop.send(true);
    }
    session.engine = None;
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MessagesChanged {
    pub folder_id: i64,
    pub ids: Vec<i64>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SendResult {
    draft_id: i64,
    error: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpFailed {
    /// "읽음 표시", "이동" 등.
    pub what: String,
    pub error: String,
}

/// 엔진 이벤트를 화면 이벤트로 옮긴다(계획서 4.2 `messages-changed`).
async fn forward_events(app: AppHandle, mut rx: broadcast::Receiver<Event>) {
    let mut last_progress = Instant::now();
    let mut sync_started: Option<Instant> = None;
    let mut last_error: Option<String> = None;
    let mut cycle_error = false;
    loop {
        match rx.recv().await {
            Ok(Event::FoldersChanged) => {
                bump_list_gen();
                let _ = app.emit("folders-changed", ());
            }
            Ok(Event::MessagesChanged {
                folder_id,
                message_ids,
            }) => {
                crate::notify::refresh_badge(&app);
                bump_list_gen();
                {
                    let st = app.state::<AppState>();
                    let mut views = lock(&st.views);
                    for id in &message_ids {
                        views.remove(id);
                    }
                }
                let _ = app.emit(
                    "messages-changed",
                    MessagesChanged {
                        folder_id,
                        ids: message_ids,
                    },
                );
            }
            Ok(Event::EmlProgress { done, total }) => {
                if done == total || last_progress.elapsed().as_millis() >= 250 {
                    last_progress = Instant::now();
                    update_status(&app, |s| {
                        s.eml_done = done;
                        s.eml_total = total;
                    });
                }
            }
            Ok(Event::Syncing(on)) => {
                // 주기는 10초마다 돌므로 오래 걸린 것(처음 동기화 등)만 남긴다.
                let mut recovered = false;
                if on {
                    // 오류는 주기가 끝난 뒤에 온다: 지난 주기에 오류가 없었으면 복구된 것이다.
                    if !cycle_error && last_error.take().is_some() {
                        applog::info("동기화 복구");
                        recovered = true;
                    }
                    cycle_error = false;
                    sync_started = Some(Instant::now());
                } else if let Some(t) = sync_started.take()
                    && t.elapsed().as_secs() >= 5
                {
                    applog::info(format!("동기화 주기 {:.1}초", t.elapsed().as_secs_f64()));
                }
                update_status(&app, |s| {
                    s.syncing = on;
                    // 복구되면 상태 표시줄의 지난 오류를 지운다(한 번 난 색인 오류가 계속 보이던 문제).
                    if recovered {
                        s.last_error = None;
                    }
                    if !on {
                        s.last_sync = Some(now_ms());
                        s.eml_done = 0;
                        s.eml_total = 0;
                    }
                });
                if !on {
                    crate::cleanup::maybe_run(&app);
                    // 임시 보관함 행이 없는 작성 중 메일·올릴 것이 남은 메일(0.7.0, 처음 동기화로 임시 보관함이 생긴 뒤 등).
                    crate::compose::prepare_drafts(&app);
                    // 새 메일 알림·작업 표시줄 배지(0.6.0).
                    crate::notify::check(&app, crate::notify::Source::Outlook);
                    crate::notify::refresh_badge(&app);
                    if lock(&QUOTA_AT).is_none_or(|t| t.elapsed() >= QUOTA_EVERY) {
                        refresh_quota_soon(&app, &[0]);
                    }
                }
            }
            Ok(Event::LoginRequired) => {
                applog::warn("재로그인 거부: 다시 로그인 필요");
                update_status(&app, |s| {
                    s.logged_in = false;
                    s.syncing = false;
                    s.login_required = true;
                });
                let _ = app.emit("login-required", ());
            }
            Ok(Event::Error(e)) => {
                // 연결이 끊긴 동안 같은 오류가 10초마다 오므로 바뀔 때만 남긴다.
                cycle_error = true;
                if last_error.as_deref() != Some(e.as_str()) {
                    applog::warn(format!("동기화 오류: {e}"));
                    last_error = Some(e.clone());
                }
                update_status(&app, |s| s.last_error = Some(e));
            }
            Ok(Event::OpsPending(n)) => {
                let was = lock(&app.state::<AppState>().status).pending_ops;
                update_status(&app, |s| s.pending_ops = n);
                // 서버 반영(지우기 등)이 다 끝났으면 메일함 용량을 다시 받는다.
                if was > 0 && n == 0 {
                    refresh_quota_soon(&app, &[3, 42]);
                }
            }
            Ok(Event::SendResult { draft_id, error }) => {
                match &error {
                    None => applog::info("발송 성공"),
                    Some(e) => applog::warn(format!("발송 실패: {e}")),
                }
                let _ = app.emit("send-result", SendResult { draft_id, error });
                let _ = app.emit("drafts-changed", ());
            }
            Ok(Event::DraftsChanged) => {
                let _ = app.emit("drafts-changed", ());
            }
            // 발송 실패는 위 SendResult로 알린다.
            Ok(Event::OpFailed { kind, .. }) if kind == eo_store::OP_SEND => {}
            Ok(Event::OpFailed { kind, error, .. }) => {
                applog::warn(format!(
                    "서버가 {}을(를) 거부해 되돌림: {error}",
                    crate::actions::kind_name(&kind)
                ));
                let _ = app.emit(
                    "op-failed",
                    OpFailed {
                        what: crate::actions::kind_name(&kind).to_owned(),
                        error,
                    },
                );
            }
            Err(broadcast::error::RecvError::Lagged(_)) => {
                let _ = app.emit("folders-changed", ());
            }
            Err(broadcast::error::RecvError::Closed) => return,
        }
    }
}

fn engine_config(s: &Settings) -> EngineConfig {
    EngineConfig {
        inbox_every: Duration::from_secs(s.inbox_every_secs),
        others_every: Duration::from_secs(s.others_every_secs),
        ..EngineConfig::default()
    }
}

fn start_engine(app: &AppHandle, st: &AppState, session: &mut Session) {
    let cfg = engine_config(&lock(&st.settings));
    let engine =
        Arc::new(Engine::new(st.client(), st.store.clone(), cfg).with_search(st.search.clone()));
    let (stop_tx, stop_rx) = watch::channel(false);
    tauri::async_runtime::spawn(forward_events(app.clone(), engine.subscribe()));
    let runner = engine.clone();
    tauri::async_runtime::spawn(async move { runner.run(stop_rx).await });
    session.engine = Some(engine);
    session.stop = Some(stop_tx);
}

/// 메일함 용량을 마지막으로 받은 때.
static QUOTA_AT: std::sync::Mutex<Option<Instant>> = std::sync::Mutex::new(None);
/// 용량 다시 받기가 예약돼 있다.
static QUOTA_SOON: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// 동기화 주기 사이에도 이만큼 지나면 용량을 다시 받는다(새 메일·다른 곳에서 지운 것).
const QUOTA_EVERY: Duration = Duration::from_secs(10 * 60);

/// 메일함 용량을 `delays`(초) 뒤마다 다시 받는다. 이미 예약돼 있으면 그대로 둔다.
/// 서버 반영(삭제·이동)이 끝나면 3초·45초 뒤 두 번(서버가 사용량을 늦게 고칠 수 있다), 주기적으로는 한 번.
fn refresh_quota_soon(app: &AppHandle, delays: &'static [u64]) {
    if QUOTA_SOON.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        for d in delays {
            tokio::time::sleep(Duration::from_secs(*d)).await;
            if !lock(&app.state::<AppState>().status).logged_in {
                break;
            }
            refresh_user_config(app.clone()).await;
        }
        QUOTA_SOON.store(false, std::sync::atomic::Ordering::SeqCst);
    });
}

/// 웹메일 사용자 설정(차단 형식·내 주소·메일함 용량)을 다시 받는다. 받았으면 true.
pub(crate) async fn refresh_user_config(app: AppHandle) -> bool {
    let st = app.state::<AppState>();
    match st.client().user_config().await {
        Ok(c) => {
            *lock(&QUOTA_AT) = Some(Instant::now());
            *lock(&st.blocked_types) = c.blocked_file_types.clone();
            if let Some(email) = &c.email_address {
                *lock(&st.me) = Some((c.display_name.clone().unwrap_or_default(), email.clone()));
            }
            for a in &c.other_addresses {
                st.add_alias(a);
            }
            *lock(&st.max_message_bytes) = c.max_message_size_kb.map(|kb| kb * 1024);
            *lock(&st.signature) = c.signature.clone();
            update_status(&app, |s| {
                s.quota_used = c.quota_used;
                s.quota_limit = c.quota_send;
            });
            true
        }
        Err(e) => {
            update_status(&app, |s| {
                s.last_error = Some(format!("메일함 용량 확인 실패: {e}"))
            });
            false
        }
    }
}

/// 앞 판에서 받아 둔 메일은 고정 여부를 모른다(1.1.0 전에는 받지 않았다): 로그인 뒤 한 번, 폴더마다 웹메일처럼
/// 고정한 메일이 먼저 오게 찾아 표시한다. 이후 바뀐 것은 동기화가 받는다.
const PIN_BACKFILL: &str = "backfill:pins-v1";

async fn backfill_pins(app: AppHandle) {
    let st = app.state::<AppState>();
    if st.perf_test || st.ui_test || st.store.meta(PIN_BACKFILL).ok().flatten().is_some() {
        return;
    }
    let Ok(folders) = st.store.syncable_folders() else {
        return;
    };
    let mut found = Vec::new();
    for f in folders {
        let Some(sid) = f.server_id else { continue };
        match st
            .client()
            .find_pinned(&eo_owa::FolderRef::Id(sid), 100)
            .await
        {
            Ok(ids) => found.extend(ids),
            Err(e) => {
                applog::warn(format!("고정 메일 확인 실패({}): {e}", f.name));
                return;
            }
        }
    }
    match st.store.pin_from_server(&found) {
        Ok(t) => {
            let _ = st.store.set_meta(PIN_BACKFILL, &now_ms().to_string());
            applog::info(format!(
                "고정 메일 확인: 서버 {}통, 새로 표시 {}통",
                found.len(),
                t.folders.values().map(Vec::len).sum::<usize>()
            ));
            crate::actions::after_local(&app, &t);
        }
        Err(e) => applog::warn(format!("고정 메일 표시 실패: {e}")),
    }
}

/// 로그인(계획서 9.5). 아이디·비밀번호는 메모리에만 둔다. 실패하면 사유를 돌려준다.
#[tauri::command]
pub async fn login(
    app: AppHandle,
    state: State<'_, AppState>,
    username: String,
    password: String,
) -> CmdResult<()> {
    let username = username.trim().to_owned();
    let password = Password::new(password);
    if username.is_empty() || password.is_empty() {
        return Err("아이디와 비밀번호를 입력하세요.".into());
    }
    let mut session = state.session.lock().await;
    stop_engine(&mut session);
    update_status(&app, |s| {
        s.connecting = true;
        s.last_error = None;
    });
    if lock(&state.settings).outlook_url.is_empty() {
        update_status(&app, |s| s.connecting = false);
        return Err("먼저 Outlook 웹메일 주소를 넣어 주세요.".into());
    }
    let t = Instant::now();
    let result = state
        .client()
        .login(Credentials {
            username: username.clone(),
            password,
        })
        .await;
    if let Err(e) = result {
        update_status(&app, |s| {
            s.connecting = false;
            s.logged_in = false;
        });
        // 아이디·비밀번호는 로그에 남기지 않는다(사유 종류만).
        return Err(match e {
            eo_owa::Error::BadCredentials { .. } => {
                applog::warn("로그인 실패: 아이디 또는 비밀번호 오류");
                "아이디 또는 비밀번호가 맞지 않습니다.".into()
            }
            other => {
                applog::warn(format!("로그인 실패: 서버 연결: {other}"));
                format!("서버에 연결할 수 없습니다: {other}")
            }
        });
    }
    applog::info(format!("로그인 성공 ({}ms)", t.elapsed().as_millis()));
    // 로그인 직후 첫 동기화로 밀려 들어온 메일은 알리지 않는다.
    crate::notify::reset(crate::notify::Source::Outlook);
    // 로그인 아이디가 주소 모양이면 나를 가리키는 주소다(대표 주소와 다를 수 있다, 1.0.5 사용자 지적).
    state.add_alias(&username);
    remember_user(&state, false, Some(username.clone()));
    session.username = Some(username);
    start_engine(&app, &state, &mut session);
    update_status(&app, |s| {
        s.connecting = false;
        s.logged_in = true;
        s.login_required = false;
    });
    tauri::async_runtime::spawn(refresh_user_config(app.clone()));
    tauri::async_runtime::spawn(backfill_pins(app.clone()));
    Ok(())
}

/// 로그인 창에 채울 아이디(`state.json`, 비밀번호는 저장하지 않는다). `None`이면 잊는다.
pub fn remember_user(st: &AppState, domino: bool, user: Option<String>) {
    let mut ui = lock(&st.ui);
    let slot = if domino {
        &mut ui.remembered_domino_user
    } else {
        &mut ui.remembered_user
    };
    if *slot != user {
        *slot = user;
        st.ui_dirty
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// 로그인 창 아이디 칸의 ✕: 기억한 아이디를 지운다(다시 로그인할 때까지 기억하지 않는다).
#[tauri::command]
pub fn forget_user(state: State<'_, AppState>, domino: bool) {
    remember_user(&state, domino, None);
}

/// 로그아웃: 메모리의 아이디·비밀번호와 세션을 지운다(로컬 메일은 유지).
#[tauri::command]
pub async fn logout(app: AppHandle, state: State<'_, AppState>) -> CmdResult<()> {
    let mut session = state.session.lock().await;
    stop_engine(&mut session);
    session.username = None;
    state.client().forget();
    applog::info("로그아웃");
    update_status(&app, |s| {
        *s = Status {
            quota_used: s.quota_used,
            quota_limit: s.quota_limit,
            pending_ops: s.pending_ops,
            // HCL Domino 연동은 따로 로그인한다.
            domino: s.domino.clone(),
            ..Status::default()
        };
    });
    Ok(())
}

/// 지금 동기화(다음 주기를 기다리지 않음).
#[tauri::command]
pub async fn sync_now(app: AppHandle, state: State<'_, AppState>) -> CmdResult<()> {
    let session = state.session.lock().await;
    let Some(engine) = &session.engine else {
        return Err("로그인하지 않았습니다.".into());
    };
    engine.wake();
    tauri::async_runtime::spawn(refresh_user_config(app.clone()));
    Ok(())
}

/// 창이 포커스를 얻으면 바로 확인(계획서 7장). 30초에 한 번까지.
pub async fn wake_on_focus(app: AppHandle) {
    let st = app.state::<AppState>();
    let mut session = st.session.lock().await;
    if session
        .last_wake
        .is_some_and(|t| t.elapsed().as_secs() < 30)
    {
        return;
    }
    if let Some(engine) = &session.engine {
        engine.wake();
        session.last_wake = Some(Instant::now());
    }
}

// ---- 설정(계획서 10장 8단계) ----

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsView {
    pub settings: Settings,
    pub inbox_choices: Vec<u64>,
    pub others_choices: Vec<u64>,
    /// 외부 이미지 허용 목록(보낸 사람·도메인) 수.
    pub remote_senders: i64,
    /// 데이터 위치(`EnhancedOutlook`).
    pub root: String,
    pub version: String,
}

#[tauri::command]
pub async fn get_settings(app: AppHandle) -> CmdResult<SettingsView> {
    blocking(move || {
        let st = app.state::<AppState>();
        Ok(SettingsView {
            settings: lock(&st.settings).clone(),
            inbox_choices: INBOX_CHOICES.to_vec(),
            others_choices: OTHERS_CHOICES.to_vec(),
            remote_senders: st.store.remote_image_senders().map_err(err)?,
            root: st.root.display().to_string(),
            version: app.package_info().version.to_string(),
        })
    })
    .await
}

/// 글자 크기를 열린 창 모두(주 창·작성 창·메일 창)에 적용하고 알린다(알림 창 제외).
fn apply_zoom(app: &AppHandle, zoom: u64) {
    for (label, w) in app.webview_windows() {
        if !label.starts_with("notify-") {
            let _ = w.set_zoom(zoom as f64 / 100.0);
        }
    }
    let _ = app.emit("zoom-changed", zoom);
}

/// 글자 크기만 바꿔 저장한다(Ctrl+휠, 0.7.0 사용자 요청: 설정과 같은 범위, 저장됨). 맞춘 값.
#[tauri::command]
pub async fn set_zoom(app: AppHandle, zoom: u64) -> CmdResult<u64> {
    let st = app.state::<AppState>();
    let new = {
        let mut s = lock(&st.settings);
        s.zoom = zoom;
        *s = s.clone().normalized();
        s.clone()
    };
    crate::settings::save_settings(&st.root, &new)
        .map_err(|e| format!("설정을 저장하지 못했습니다: {e}"))?;
    apply_zoom(&app, new.zoom);
    Ok(new.zoom)
}

/// 설정을 저장하고 바로 적용한다(동기화 주기는 다음 확인부터). 고를 수 없는 값은 가까운 값으로 바꿔 돌려준다.
#[tauri::command]
pub async fn set_settings(app: AppHandle, settings: Settings) -> CmdResult<Settings> {
    let new = settings.normalized();
    new.check()?;
    let st = app.state::<AppState>();
    let old = lock(&st.settings).clone();
    crate::settings::save_settings(&st.root, &new)
        .map_err(|e| format!("설정을 저장하지 못했습니다: {e}"))?;
    *lock(&st.settings) = new.clone();
    if old.outlook_url != new.outlook_url {
        // 다른 서버: 지금 로그인을 끝내고 새 주소로 연결한다(로그인 창이 다시 뜬다).
        {
            let mut session = st.session.lock().await;
            stop_engine(&mut session);
            session.username = None;
        }
        st.set_outlook_url(&new.outlook_url);
        update_status(&app, |s| {
            s.logged_in = false;
            s.login_required = true;
        });
        let _ = app.emit("login-required", ());
    }
    if old.domino.portal != new.domino.portal {
        if old.domino.enabled {
            crate::domino::forget(&app);
        }
        st.domino
            .configure(eo_owa::domino::DominoConfig::new(&new.domino.portal).ok());
    }
    if old.remote_images != new.remote_images || old.domino.linked_docs != new.domino.linked_docs {
        // 읽기 창 캐시에는 앞 정책으로 그린 본문이 있다(연결문서를 붙이거나 뺀다).
        lock(&st.views).clear();
    }
    if let Some(engine) = &st.session.lock().await.engine {
        engine.set_intervals(
            Duration::from_secs(new.inbox_every_secs),
            Duration::from_secs(new.others_every_secs),
        );
    }
    if old.zoom != new.zoom {
        apply_zoom(&app, new.zoom);
    }
    if old.cleanup != new.cleanup {
        crate::cleanup::reset();
        update_status(&app, |s| {
            if s.last_error
                .as_deref()
                .is_some_and(|e| e.starts_with("자동 정리 멈춤"))
            {
                s.last_error = None;
            }
        });
    }
    if old.domino.cleanup != new.domino.cleanup {
        crate::domino::reset_cleanup();
        update_status(&app, |s| {
            if s.domino
                .last_error
                .as_deref()
                .is_some_and(|e| e.starts_with("HCL Domino 자동 정리 멈춤"))
            {
                s.domino.last_error = None;
            }
        });
    }
    if old.domino.enabled && !new.domino.enabled {
        crate::domino::forget(&app);
    }
    if (old.domino.enabled && old.domino.unified) != (new.domino.enabled && new.domino.unified) {
        // 폴더 목록 모양이 바뀐다(통합 편지함·Outlook 메일 묶음).
        bump_list_gen();
        let _ = app.emit("folders-changed", ());
    }
    let cleanup_text = |c: &Cleanup| match c.mode {
        CleanupMode::Off => "끔".to_owned(),
        CleanupMode::Age => format!("{}일 지난 메일", c.days),
        CleanupMode::Quota => format!("용량 {}% 이상", c.percent),
    };
    applog::info(format!(
        "설정 변경: 받은 편지함 {}초, 나머지 폴더 {}초, 외부 이미지 {}, 받은 편지함 자동 정리 {}, HCL Domino 연동 {}{}",
        new.inbox_every_secs,
        new.others_every_secs,
        match new.remote_images {
            RemoteImages::Ask => "차단(보낸 사람별 허용)",
            RemoteImages::Always => "늘 표시",
        },
        cleanup_text(&new.cleanup),
        if new.domino.enabled { "켬" } else { "끔" },
        if new.domino.enabled {
            format!(
                "(받은 메일 자동 정리 {}, 리다이렉트 사본 자동 삭제 {})",
                cleanup_text(&new.domino.cleanup),
                if new.domino.remove_redirects {
                    "켬"
                } else {
                    "끔"
                }
            )
        } else {
            String::new()
        }
    ));
    Ok(new)
}

/// `--ui-test` 전용(합성 저장소): 로그인 없이 자동 정리를 한 번 해 본다. 합성 저장소는 최근 메일에만 원문이 있어
/// `later_days`일 뒤를 가정하고, 같은 저장소에서 여러 번 돌려도 남게 `max`통까지만. (고른 메일, 서버에 쌓인 작업 수)
#[tauri::command]
pub async fn ui_test_cleanup_run(
    app: AppHandle,
    cleanup: Cleanup,
    later_days: i64,
    max: usize,
) -> CmdResult<(Vec<i64>, usize)> {
    let t = blocking({
        let app = app.clone();
        move || {
            let st = app.state::<AppState>();
            if !st.ui_test {
                return Err("점검 모드에서만 됩니다".into());
            }
            let now = now_ms() + later_days * 86_400_000;
            let p = crate::cleanup::targets_at(&st, &cleanup, max, now)?;
            let batch = st.store.new_batch().map_err(err)?;
            let t = st
                .store
                .server_delete(&p.ids, now_ms(), Some(batch))
                .map_err(err)?;
            Ok((p.ids, t))
        }
    })
    .await?;
    crate::actions::after_local(&app, &t.1);
    Ok((t.0, t.1.queued))
}

/// 설정 화면: 이 기준이면 지금 서버에서 지울 받은 편지함 메일 수. 용량 기준인데 용량을 모르면 없음.
#[tauri::command]
pub async fn cleanup_preview(app: AppHandle, cleanup: Cleanup) -> CmdResult<Option<usize>> {
    blocking(move || {
        let st = app.state::<AppState>();
        let known = lock(&st.status).quota_used.is_some();
        if cleanup.mode == CleanupMode::Quota && !known {
            return Ok(None);
        }
        Ok(Some(
            crate::cleanup::targets(&st, &cleanup, usize::MAX)?
                .ids
                .len(),
        ))
    })
    .await
}

/// 외부 이미지 허용 목록(보낸 사람·도메인)을 모두 지운다. 지운 수.
#[tauri::command]
pub async fn forget_remote_images(app: AppHandle) -> CmdResult<usize> {
    blocking(move || {
        let st = app.state::<AppState>();
        let n = st.store.forget_remote_images().map_err(err)?;
        lock(&st.views).clear();
        applog::info(format!("외부 이미지 허용 목록 {n}개 지움"));
        Ok(n)
    })
    .await
}

/// 로그 폴더(`EnhancedOutlook\logs`)를 탐색기로 연다.
#[tauri::command]
pub fn open_logs(app: AppHandle) -> CmdResult<()> {
    let dir = app.state::<AppState>().root.join("logs");
    std::fs::create_dir_all(&dir).map_err(err)?;
    app.opener()
        .open_path(dir.display().to_string(), None::<&str>)
        .map_err(err)
}

/// 서버 이름(또는 inline·data) 목록을 "이름 N개, …"로 센다. 이름에는 영문·숫자·점·하이픈만 남긴다.
fn host_counts(hosts: Vec<String>) -> String {
    let mut n: std::collections::BTreeMap<String, usize> = Default::default();
    for h in hosts.into_iter().take(200) {
        let h: String = h
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-'))
            .take(80)
            .collect();
        *n.entry(if h.is_empty() { "?".into() } else { h })
            .or_default() += 1;
    }
    n.into_iter()
        .map(|(h, c)| format!("{h} {c}개"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// 읽기 창이 받지 못했거나 0폭으로 그린 그림을 로그에 남긴다(진단용). 서버 이름과 개수만, 주소·경로는 남기지 않는다.
#[tauri::command]
pub async fn report_broken_images(id: i64, failed: Vec<String>, collapsed: Vec<String>) {
    if !failed.is_empty() {
        applog::warn(format!("그림을 받지 못함: #{id} · {}", host_counts(failed)));
    }
    if !collapsed.is_empty() {
        applog::warn(format!(
            "그림이 0폭으로 그려짐: #{id} · {}",
            host_counts(collapsed)
        ));
    }
}

/// 마지막으로 본 폴더(다음 실행·비정상 종료 뒤 그 자리에서 시작). 폴더를 바꿀 때마다 불리므로
/// 주 스레드(동기 명령)가 아니라 비동기 작업자에서 돈다.
#[tauri::command]
pub async fn set_last_folder(app: AppHandle, folder_id: Option<i64>) {
    let st = app.state::<AppState>();
    let mut ui = lock(&st.ui);
    if ui.last_folder != folder_id {
        ui.last_folder = folder_id;
        st.ui_dirty
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// 메일 목록 정렬(다음 실행에도 그대로).
#[tauri::command]
pub async fn set_list_sort(app: AppHandle, sort: ListSort) {
    let st = app.state::<AppState>();
    let mut ui = lock(&st.ui);
    let new = Some(crate::settings::SavedSort {
        key: sort.key,
        reverse: sort.reverse,
    });
    if ui.list_sort != new {
        ui.list_sort = new;
        st.ui_dirty
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// 폴더 목록·메일 목록 폭(경계를 끌어 바꾼 것). 다음 실행에도 그 폭으로.
#[tauri::command]
pub async fn set_pane_widths(app: AppHandle, folders: f64, list: f64) {
    let st = app.state::<AppState>();
    let mut ui = lock(&st.ui);
    let new = (Some(folders.round()), Some(list.round()));
    if (ui.folders_width, ui.list_width) != new {
        (ui.folders_width, ui.list_width) = new;
        st.ui_dirty
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

// ---- 성능 측정(--perf-test, 계획서 4.1-4) ----

/// 프로세스 시작부터 지금까지(ms).
#[tauri::command]
pub fn perf_since_start(state: State<'_, AppState>) -> f64 {
    ms_since(state.started)
}

/// `EnhancedOutlook\logs\perf.log`(또는 `uitest.log`)에 덧붙인다.
#[tauri::command]
pub fn perf_write(
    state: State<'_, AppState>,
    lines: Vec<String>,
    file: Option<String>,
) -> CmdResult<String> {
    let dir = state.root.join("logs");
    std::fs::create_dir_all(&dir).map_err(err)?;
    let name = match file.as_deref() {
        Some("uitest.log") => "uitest.log",
        Some("imagecheck.log") => "imagecheck.log",
        Some("memtest.log") => "memtest.log",
        _ => "perf.log",
    };
    let path = dir.join(name);
    applog::append_rotated(&path, &lines).map_err(err)?;
    Ok(path.display().to_string())
}

#[tauri::command]
pub fn perf_exit(app: AppHandle) {
    app.exit(0);
}
