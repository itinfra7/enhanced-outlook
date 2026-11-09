//! 작성(계획서 9.3, 0.7.0): 웹메일처럼 본문 영역에서 쓰고 "별도 창에서 편집"으로 창을 띄운다. 로컬에 5초마다
//! 저장하고, 서버 임시 보관함에는 1분마다·닫을 때·Ctrl+S에 올린다(작업 큐 `draft_save`). 쓰던 메일은
//! 임시 보관함에 행으로 보이고, 웹메일에서 쓰던 초안도 열어 이어 쓸 수 있다. 받는 사람 자동완성, 첨부, 보낼 편지함.

use std::collections::HashMap;
use std::path::PathBuf;

use eo_mime::compose::{self, ReplyMode};
use eo_owa::ItemId;
use eo_store::{DraftContent, DraftOptions, Recipient, Touched, draft_state};
use serde::{Deserialize, Serialize};
use tauri::webview::NewWindowResponse;
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_dialog::DialogExt;

use crate::actions::after_local;
use crate::commands::now_ms;
use crate::files;
use crate::state::{AppState, lock};

type CmdResult<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// 계획서 9.3: 30MB를 넘으면 보내지 않는다(서버 한도가 더 작으면 그것).
const MAX_BYTES: i64 = 30 * 1024 * 1024;

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

fn mode_of(kind: &str) -> Option<ReplyMode> {
    match kind {
        "reply" => Some(ReplyMode::Reply),
        "reply_all" => Some(ReplyMode::ReplyAll),
        "forward" => Some(ReplyMode::Forward),
        _ => None,
    }
}

fn person(p: &compose::Person) -> Recipient {
    Recipient {
        name: p.name.clone(),
        email: p.email.clone(),
    }
}

/// 나의 주소: 로그인 때 받은 것, 없으면 보낸 편지함에서 추정.
fn my_address(st: &AppState) -> Option<String> {
    lock(&st.me)
        .as_ref()
        .map(|(_, e)| e.clone())
        .or_else(|| st.store.guess_me().ok().flatten())
}

/// 이 계정을 가리키는 모든 주소: 대표 주소와 로그인 아이디·로그온 주소 등([`AppState::my_aliases`]).
/// 전체 회신에서 나를 뺄 때 쓴다.
fn my_addresses(st: &AppState) -> Vec<String> {
    let mut all: Vec<String> = my_address(st).into_iter().collect();
    for a in lock(&st.my_aliases).iter() {
        if !all.iter().any(|b| b.eq_ignore_ascii_case(a)) {
            all.push(a.clone());
        }
    }
    all
}

/// 나(임시 보관함 행의 보낸 사람).
fn me_recipient(st: &AppState) -> Option<Recipient> {
    lock(&st.me)
        .as_ref()
        .map(|(name, email)| Recipient {
            name: name.clone(),
            email: email.clone(),
        })
        .or_else(|| {
            st.store.guess_me().ok().flatten().map(|email| Recipient {
                name: String::new(),
                email,
            })
        })
}

/// 임시 보관함 행을 작성 중 메일 내용으로 맞춘다. `upload`면 서버에 올릴 작업도 쌓는다. 바뀐 것.
fn refresh_draft(st: &AppState, id: i64, upload: bool) -> Touched {
    let mut t = Touched::default();
    if let Ok(Some(mid)) = st.store.sync_draft_row(id, me_recipient(st).as_ref())
        && let Ok(Some(row)) = st.store.message(mid)
    {
        t.add(row.folder_id, mid);
    }
    if upload && st.store.queue_draft_upload(id, now_ms()).unwrap_or(false) {
        t.queued += 1;
    }
    t
}

/// 시작할 때·동기화가 끝날 때: 행이 없는 작성 중 메일(예전 판)에 행을 만들고, 올릴 것이 남은 메일을 올린다
/// (기존 작성 중 메일도 임시 보관함에 올린다, 사용자 결정 2026-10-21).
pub fn prepare_drafts(app: &AppHandle) {
    let handle = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let st = handle.state::<AppState>();
        let mut t = Touched::default();
        for id in st.store.drafts_without_row().unwrap_or_default() {
            t.merge(refresh_draft(&st, id, false));
        }
        for id in st.store.dirty_drafts().unwrap_or_default() {
            if st.store.queue_draft_upload(id, now_ms()).unwrap_or(false) {
                t.queued += 1;
            }
        }
        if !t.folders.is_empty() || t.queued > 0 {
            after_local(&handle, &t);
        }
    });
}

/// 회신·전달의 처음 내용(받는 사람·제목·인용 여부).
fn prefill(st: &AppState, kind: &str, ref_id: Option<i64>) -> CmdResult<DraftContent> {
    let Some(mode) = mode_of(kind) else {
        return Ok(DraftContent {
            include_quote: false,
            ..DraftContent::default()
        });
    };
    let id = ref_id.ok_or("원본 메일을 고르세요")?;
    let row = st
        .store
        .message(id)
        .map_err(err)?
        .ok_or("원본 메일이 없습니다")?;
    let original = st
        .raw(id, row.eml_hash.as_deref())
        .and_then(|raw| compose::original(&raw));
    let me = my_addresses(st);
    Ok(match original {
        Some(o) => {
            let (to, cc) = compose::recipients(mode, &o, &me);
            let subject = if o.subject.is_empty() {
                row.props.subject.clone().unwrap_or_default()
            } else {
                o.subject.clone()
            };
            DraftContent {
                to: to.iter().map(person).collect(),
                cc: cc.iter().map(person).collect(),
                bcc: Vec::new(),
                subject: compose::prefixed_subject(mode, &subject),
                body_html: String::new(),
                include_quote: true,
                quote_html: None,
                options: DraftOptions::default(),
            }
        }
        // 원문을 아직 받지 않았다: 목록 정보로만(인용문 없이).
        None => DraftContent {
            to: match (mode, &row.props.from_addr) {
                (ReplyMode::Forward, _) | (_, None) => Vec::new(),
                (_, Some(a)) => vec![Recipient {
                    name: row.props.from_name.clone().unwrap_or_default(),
                    email: a.clone(),
                }],
            },
            subject: compose::prefixed_subject(mode, row.props.subject.as_deref().unwrap_or("")),
            include_quote: false,
            ..DraftContent::default()
        },
    })
}

fn window_title(kind: &str, subject: &str) -> String {
    let what = match kind {
        "reply" => "회신",
        "reply_all" => "전체 회신",
        "forward" => "전달",
        _ => "새 메일",
    };
    if subject.trim().is_empty() {
        format!("{what} - Enhanced Outlook")
    } else {
        format!("{subject} - {what}")
    }
}

/// 작성 창을 연다(이미 열려 있으면 앞으로).
fn show_window(app: &AppHandle, id: i64, title: &str) -> CmdResult<()> {
    open_app_window(app, &format!("compose-{id}"), title)
}

/// 앱 화면을 별도 창으로 연다(작성 창 `compose-*`, 메일 보기 창 `message-*`). 이미 열려 있으면 앞으로.
/// 주 창과 같은 WebView2 프로필·외부 링크 처리·글자 크기.
pub fn open_app_window(app: &AppHandle, label: &str, title: &str) -> CmdResult<()> {
    let label = label.to_owned();
    if let Some(w) = app.get_webview_window(&label) {
        let _ = w.unminimize();
        let _ = w.set_focus();
        return Ok(());
    }
    let root = app.state::<AppState>().root.clone();
    let zoom = crate::state::lock(&app.state::<AppState>().settings).zoom;
    let links = app.clone();
    let nav = app.clone();
    let mut builder =
        WebviewWindowBuilder::new(app, &label, WebviewUrl::App(PathBuf::from("index.html")))
            .title(title)
            .inner_size(900.0, 720.0)
            .min_inner_size(600.0, 420.0)
            // 주 창과 같은 WebView2 프로필(같은 브라우저 프로세스를 함께 쓴다).
            .data_directory(root.join("webview"))
            .general_autofill_enabled(false)
            .on_new_window(move |url, _| {
                crate::open_external(&links, &url);
                NewWindowResponse::Deny
            })
            .on_navigation(move |url| {
                if crate::is_app_url(url) {
                    return true;
                }
                crate::open_external(&nav, url);
                false
            });
    if let Some(args) = crate::memory::test_browser_args() {
        builder = builder.additional_browser_args(&args);
    }
    let window = builder.build().map_err(err)?;
    // 글자 크기(설정의 화면 확대).
    if zoom != 100 {
        let _ = window.set_zoom(zoom as f64 / 100.0);
    }
    Ok(())
}

/// 새 메일·회신·전달(`kind` new·reply·reply_all·forward)을 쓴다: 작성 중 메일을 만들고 임시 보관함에 행을
/// 둔다. 작성 화면은 본문 영역(또는 메일 창)이 연다.
#[tauri::command]
pub async fn compose_new(app: AppHandle, kind: String, ref_id: Option<i64>) -> CmdResult<i64> {
    let (id, t) = blocking(&app, move |st| {
        let mut content = prefill(st, &kind, ref_id)?;
        // 웹메일 설정의 서명을 자동으로 넣기로 했으면(새 메일·회신·전달 따로).
        if let Some(sig) = lock(&st.signature).clone()
            && (if kind == "new" {
                sig.auto_new
            } else {
                sig.auto_reply
            })
        {
            content.body_html = format!("<p></p><p></p>{}", sig.html);
        }
        let id = st
            .store
            .create_draft(&kind, ref_id, &content, now_ms())
            .map_err(err)?;
        Ok((id, refresh_draft(st, id, false)))
    })
    .await?;
    after_local(&app, &t);
    Ok(id)
}

/// 별도 창에서 편집(웹메일과 같은 이름).
#[tauri::command]
pub async fn compose_popout(app: AppHandle, draft_id: i64) -> CmdResult<()> {
    let title = blocking(&app, move |st| {
        let d = st
            .store
            .draft(draft_id)
            .map_err(err)?
            .ok_or("작성 중 메일이 없습니다")?;
        if d.state == draft_state::OUTBOX {
            return Err("보내는 중이라 고칠 수 없습니다.".into());
        }
        Ok(window_title(&d.kind, &d.subject))
    })
    .await?;
    show_window(&app, draft_id, &title)
}

/// 이 작성 중 메일이 별도 창에 열려 있는가(본문 영역에서 또 열지 않게).
#[tauri::command]
pub fn compose_in_window(app: AppHandle, draft_id: i64) -> bool {
    app.get_webview_window(&format!("compose-{draft_id}"))
        .is_some()
}

/// 임시 보관함의 메일을 작성 화면으로 연다. 앱에서 쓰던 메일은 그대로, 웹메일에서 쓰던 초안은 서버에서
/// 최신 원문과 첨부 ID를 받아 가져온다. 작성 중 메일 번호.
#[tauri::command]
pub async fn open_draft(app: AppHandle, message_id: i64) -> CmdResult<i64> {
    let (row, linked) = blocking(&app, move |st| {
        let row = st
            .store
            .message(message_id)
            .map_err(err)?
            .ok_or("메일이 없습니다")?;
        Ok((row, st.store.draft_by_message(message_id).map_err(err)?))
    })
    .await?;
    if let Some(d) = linked {
        if d.state == draft_state::OUTBOX {
            return Err("보내는 중이라 고칠 수 없습니다.".into());
        }
        return Ok(d.id);
    }
    let sid = row
        .server_item_id
        .clone()
        .ok_or("서버에 없는 메일은 이어 쓸 수 없습니다")?;
    let st = app.state::<AppState>();
    if !st.client().is_logged_in() {
        return Err("로그인하면 이어 쓸 수 있습니다".into());
    }
    let item = ItemId {
        id: sid.clone(),
        change_key: None,
    };
    let raw = st
        .client()
        .get_mime(std::slice::from_ref(&item))
        .await
        .map_err(err)?
        .into_iter()
        .next()
        .ok_or("원문을 받지 못했습니다")?
        .map_err(|e| e.to_string())?;
    let server_files = st
        .client()
        .get_items(&[item], &["Attachments"])
        .await
        .map_err(err)?
        .into_iter()
        .next()
        .and_then(Result::ok)
        .and_then(|i| i.attachments)
        .unwrap_or_default();
    let handle = app.clone();
    let id = blocking(&app, move |st| {
        let parts = compose::draft_parts(&raw).ok_or("초안을 읽을 수 없습니다")?;
        let content = DraftContent {
            to: parts.to.iter().map(person).collect(),
            cc: parts.cc.iter().map(person).collect(),
            bcc: parts.bcc.iter().map(person).collect(),
            subject: parts.subject.clone(),
            body_html: parts.body_html.clone(),
            include_quote: false,
            quote_html: None,
            options: DraftOptions {
                importance: parts.importance,
                sensitivity: parts.sensitivity,
                read_receipt: parts.read_receipt,
                delivery_receipt: parts.delivery_receipt,
                plain_text: parts.plain_text,
            },
        };
        let id = st
            .store
            .import_server_draft(message_id, &sid, &content, now_ms())
            .map_err(err)?;
        // 첨부는 파일로 두고 서버 첨부 ID와 잇는다(그대로 두면 다시 올리지 않는다). 남은 서버 첨부(본문 그림 등)는
        // 작성 화면이 본문에 다시 품으므로 다음에 올릴 때 지운다.
        let mut by_name: HashMap<String, Vec<String>> = HashMap::new();
        let mut others = Vec::new();
        for a in server_files {
            let Some(aid) = a.attachment_id.map(|x| x.id) else {
                continue;
            };
            match (a.is_inline.unwrap_or(false), a.name) {
                (false, Some(name)) => by_name.entry(name).or_default().push(aid),
                _ => others.push(aid),
            }
        }
        for a in &parts.attachments {
            let name = files::safe_file_name(&a.name);
            let saved = st.store.add_draft_bytes(id, &name, &a.data).map_err(err)?;
            if let Some(aid) = by_name.get_mut(&a.name).and_then(Vec::pop) {
                st.store
                    .set_draft_upload(id, &format!("file:{saved}:{}", a.data.len()), Some(&aid))
                    .map_err(err)?;
            }
        }
        for aid in by_name.into_values().flatten().chain(others) {
            st.store
                .set_draft_upload(id, &format!("server:{aid}"), Some(&aid))
                .map_err(err)?;
        }
        Ok(id)
    })
    .await?;
    let _ = handle.emit("drafts-changed", ());
    Ok(id)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileView {
    pub name: String,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DraftView {
    pub id: i64,
    pub kind: String,
    pub to: Vec<Recipient>,
    pub cc: Vec<Recipient>,
    pub bcc: Vec<Recipient>,
    pub subject: String,
    pub body_html: String,
    pub include_quote: bool,
    /// 인용문 문서(머리글 + 화면용 원문, 작성 창에서 고칠 수 있다). 원문이 없으면 없음.
    pub quote_doc: Option<String>,
    /// 고친 인용문(고친 적이 있으면, 저장할 때 작성 창의 인용문을 함께 저장한다).
    pub quote_html: Option<String>,
    pub attachments: Vec<FileView>,
    /// 함께 전달되는 원본 첨부.
    pub forward_attachments: Vec<FileView>,
    pub state: i64,
    pub last_error: Option<String>,
    pub max_bytes: i64,
    /// 원본이 서버에 없음(로컬 보관본 회신·전달).
    pub local_original: bool,
    /// HCL Domino 메일에 회신·전달: "보내는 계정은 … 입니다."
    pub sender_note: Option<String>,
    pub options: DraftOptions,
    /// 보낸 사람(웹메일 "보낸 사람" 칸).
    pub from: Option<String>,
    /// 서버 임시 보관함에 지금 내용이 올라가 있다.
    pub server_saved: bool,
    /// 웹메일 설정의 서명("서명 삽입", 설정했을 때만).
    pub signature: Option<String>,
}

fn max_bytes(st: &AppState) -> i64 {
    lock(&st.max_message_bytes).map_or(MAX_BYTES, |m| m.min(MAX_BYTES))
}

fn files_of(st: &AppState, id: i64) -> CmdResult<Vec<FileView>> {
    Ok(st
        .store
        .draft_files(id)
        .map_err(err)?
        .into_iter()
        .map(|(name, size)| FileView { name, size })
        .collect())
}

#[tauri::command]
pub async fn compose_load(app: AppHandle, draft_id: i64) -> CmdResult<DraftView> {
    blocking(&app, move |st| {
        let d = st
            .store
            .draft(draft_id)
            .map_err(err)?
            .ok_or("작성 중 메일이 없습니다")?;
        let mut quote_doc = None;
        let mut forward_attachments = Vec::new();
        let mut local_original = false;
        let mut sender_note = None;
        if let (Some(_), Some(ref_id)) = (mode_of(&d.kind), d.ref_message_id)
            && let Some(row) = st.store.message(ref_id).map_err(err)?
        {
            local_original = row.server_item_id.is_none();
            // HCL Domino 메일에 회신·전달해도 Outlook 계정으로 나간다(사용자 요청 2026-10-21: 안내 줄).
            if row
                .origin
                .as_deref()
                .is_some_and(|o| o.starts_with("domino"))
            {
                let who = lock(&st.me)
                    .as_ref()
                    .map(|(_, email)| email.clone())
                    .unwrap_or_else(|| "Outlook 계정".into());
                sender_note = Some(format!("보내는 계정은 {who} 입니다."));
            }
            if let Some(raw) = st.raw(ref_id, row.eml_hash.as_deref())
                && let Some(o) = compose::original(&raw)
            {
                // 고친 인용문이 있으면 그것을, 아니면 원문으로(0.9.0: 작성 창에서 고칠 수 있다).
                quote_doc = match d.quote_html.as_deref() {
                    Some(edited) => Some(compose::edited_quote_doc(edited)),
                    None => compose::quote_doc(
                        &raw,
                        &format!("{}{ref_id}/", eo_sync::QUOTE_CID_BASE),
                        &o,
                    ),
                };
                if d.kind == "forward" {
                    forward_attachments = o
                        .attachments
                        .iter()
                        .map(|a| FileView {
                            name: a.name.clone(),
                            size: a.data.len() as u64,
                        })
                        .collect();
                }
            }
        }
        Ok(DraftView {
            id: d.id,
            kind: d.kind,
            to: d.to,
            cc: d.cc,
            bcc: d.bcc,
            subject: d.subject,
            body_html: d.body_html,
            include_quote: d.include_quote && quote_doc.is_some(),
            quote_html: d.quote_html.filter(|_| quote_doc.is_some()),
            quote_doc,
            attachments: files_of(st, draft_id)?,
            forward_attachments,
            state: d.state,
            last_error: d.last_error,
            max_bytes: max_bytes(st),
            local_original,
            sender_note,
            options: d.options,
            from: my_address(st),
            server_saved: d.server_draft_id.is_some() && !d.server_dirty,
            signature: lock(&st.signature).as_ref().map(|s| s.html.clone()),
        })
    })
    .await
}

/// 서버 임시 보관함에 지금 내용이 올라가 있는가(작성 화면의 "임시 보관함에 저장됨").
#[tauri::command]
pub async fn compose_server_saved(app: AppHandle, draft_id: i64) -> CmdResult<bool> {
    blocking(&app, move |st| {
        Ok(st
            .store
            .draft(draft_id)
            .map_err(err)?
            .is_some_and(|d| d.server_draft_id.is_some() && !d.server_dirty))
    })
    .await
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DraftSave {
    pub to: Vec<Recipient>,
    pub cc: Vec<Recipient>,
    pub bcc: Vec<Recipient>,
    pub subject: String,
    pub body_html: String,
    pub include_quote: bool,
    /// 작성 창에서 고친 인용문(고치지 않았으면 없음).
    #[serde(default)]
    pub quote_html: Option<String>,
    #[serde(default)]
    pub options: DraftOptions,
}

impl From<DraftSave> for DraftContent {
    fn from(s: DraftSave) -> Self {
        Self {
            to: s.to,
            cc: s.cc,
            bcc: s.bcc,
            subject: s.subject,
            body_html: s.body_html,
            include_quote: s.include_quote,
            quote_html: s.quote_html,
            options: s.options,
        }
    }
}

/// 저장(작성 화면이 5초마다 로컬에). `upload`면 서버 임시 보관함에도 올린다(1분마다·닫을 때·Ctrl+S).
#[tauri::command]
pub async fn compose_save(
    app: AppHandle,
    draft_id: i64,
    content: DraftSave,
    upload: bool,
) -> CmdResult<()> {
    let t = blocking(&app, move |st| {
        st.store
            .save_draft(draft_id, &content.into(), now_ms())
            .map_err(err)?;
        Ok(refresh_draft(st, draft_id, upload))
    })
    .await?;
    after_local(&app, &t);
    Ok(())
}

/// 바뀐 것 없이 서버에 올린다(닫을 때 등). 올릴 것이 없으면 아무것도 하지 않는다.
#[tauri::command]
pub async fn compose_upload(app: AppHandle, draft_id: i64) -> CmdResult<()> {
    let t = blocking(&app, move |st| {
        let mut t = Touched::default();
        let dirty = st
            .store
            .draft(draft_id)
            .map_err(err)?
            .is_some_and(|d| d.server_dirty || d.server_draft_id.is_none());
        if dirty
            && st
                .store
                .queue_draft_upload(draft_id, now_ms())
                .map_err(err)?
        {
            t.queued += 1;
        }
        Ok(t)
    })
    .await?;
    after_local(&app, &t);
    Ok(())
}

/// 첨부를 바꿨다: 서버에 올릴 것으로 표시하고 임시 보관함 행을 맞춘다.
fn files_changed(app: &AppHandle, st: &AppState, draft_id: i64) {
    let _ = st.store.touch_draft(draft_id, now_ms());
    let t = refresh_draft(st, draft_id, false);
    after_local(app, &t);
}

fn add_paths(st: &AppState, draft_id: i64, paths: &[PathBuf]) -> CmdResult<Vec<FileView>> {
    for p in paths {
        if !p.is_file() {
            return Err(format!("파일이 아닙니다: {}", p.display()));
        }
        let name = files::safe_file_name(
            &p.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
        );
        st.store.add_draft_file(draft_id, p, &name).map_err(err)?;
    }
    files_of(st, draft_id)
}

/// 끌어놓은 파일 첨부.
#[tauri::command]
pub async fn compose_add_files(
    app: AppHandle,
    draft_id: i64,
    paths: Vec<String>,
) -> CmdResult<Vec<FileView>> {
    let handle = app.clone();
    blocking(&app, move |st| {
        let paths: Vec<PathBuf> = paths.into_iter().map(PathBuf::from).collect();
        let v = add_paths(st, draft_id, &paths)?;
        files_changed(&handle, st, draft_id);
        Ok(v)
    })
    .await
}

/// "첨부" 단추: 파일을 고른다.
#[tauri::command]
pub async fn compose_pick_files(app: AppHandle, draft_id: i64) -> CmdResult<Vec<FileView>> {
    let handle = app.clone();
    let picked = tauri::async_runtime::spawn_blocking(move || {
        handle.dialog().file().blocking_pick_files().map(|v| {
            v.into_iter()
                .filter_map(|p| p.into_path().ok())
                .collect::<Vec<PathBuf>>()
        })
    })
    .await
    .map_err(err)?;
    let handle = app.clone();
    blocking(&app, move |st| match picked {
        Some(paths) => {
            let v = add_paths(st, draft_id, &paths)?;
            files_changed(&handle, st, draft_id);
            Ok(v)
        }
        None => files_of(st, draft_id),
    })
    .await
}

#[tauri::command]
pub async fn compose_remove_file(
    app: AppHandle,
    draft_id: i64,
    name: String,
) -> CmdResult<Vec<FileView>> {
    let handle = app.clone();
    blocking(&app, move |st| {
        st.store.remove_draft_file(draft_id, &name).map_err(err)?;
        files_changed(&handle, st, draft_id);
        files_of(st, draft_id)
    })
    .await
}

fn valid_email(e: &str) -> bool {
    let e = e.trim();
    match e.split_once('@') {
        Some((local, domain)) => {
            !local.is_empty()
                && domain.contains('.')
                && !domain.starts_with('.')
                && !domain.ends_with('.')
                && !e.contains(char::is_whitespace)
                && !e.contains(['<', '>', ',', ';'])
        }
        None => false,
    }
}

/// 보내기: 확인한 뒤 보낼 편지함에 넣고 작업을 쌓는다(창은 화면이 닫는다). 실패하면 사유.
#[tauri::command]
pub async fn compose_send(app: AppHandle, draft_id: i64, content: DraftSave) -> CmdResult<()> {
    let t = blocking(&app, move |st| {
        let all: Vec<&Recipient> = content
            .to
            .iter()
            .chain(&content.cc)
            .chain(&content.bcc)
            .collect();
        if all.is_empty() {
            return Err("받는 사람을 적어 주세요.".into());
        }
        if let Some(bad) = all.iter().find(|r| !valid_email(&r.email)) {
            return Err(format!("주소 형식이 올바르지 않습니다: {}", bad.email));
        }
        let d = st
            .store
            .draft(draft_id)
            .map_err(err)?
            .ok_or("작성 중 메일이 없습니다")?;
        let mut total: i64 = content.body_html.len() as i64;
        total += st
            .store
            .draft_files(draft_id)
            .map_err(err)?
            .iter()
            .map(|(_, s)| *s as i64)
            .sum::<i64>();
        if d.kind == "forward"
            && let Some(ref_id) = d.ref_message_id
            && let Some(row) = st.store.message(ref_id).map_err(err)?
        {
            total += row.props.size.unwrap_or(0);
        }
        let limit = max_bytes(st);
        if total > limit {
            return Err(format!(
                "메일 크기({:.1}MB)가 한도 {:.0}MB를 넘어 보낼 수 없습니다.",
                total as f64 / 1_048_576.0,
                limit as f64 / 1_048_576.0
            ));
        }
        let now = now_ms();
        st.store
            .save_draft(draft_id, &content.into(), now)
            .map_err(err)?;
        st.store.queue_send(draft_id, now).map_err(err)?;
        let mut t = refresh_draft(st, draft_id, false);
        t.queued += 1;
        Ok(t)
    })
    .await?;
    after_local(&app, &t);
    let _ = app.emit("drafts-changed", ());
    Ok(())
}

/// 버리기(웹메일 "취소 → 삭제"): 임시 보관함의 초안은 지운 편지함으로 옮기고, 작성 중 메일과 첨부를 지운다.
#[tauri::command]
pub async fn compose_discard(app: AppHandle, draft_id: i64) -> CmdResult<()> {
    let t = blocking(&app, move |st| {
        let Some(d) = st.store.draft(draft_id).map_err(err)? else {
            return Ok(Touched::default());
        };
        if d.state == draft_state::OUTBOX {
            return Err("보낼 편지함에 있는 메일은 버릴 수 없습니다.".into());
        }
        let t = discard_row(st, d.message_id)?;
        st.store.delete_draft(draft_id).map_err(err)?;
        Ok(t)
    })
    .await?;
    after_local(&app, &t);
    let _ = app.emit("drafts-changed", ());
    Ok(())
}

/// 버린 작성 중 메일의 임시 보관함 행: 서버에 있으면 지운 편지함으로(서버도), 없으면 그냥 지운다.
fn discard_row(st: &AppState, message_id: Option<i64>) -> CmdResult<Touched> {
    let mut t = Touched::default();
    let Some(row) = message_id.and_then(|m| st.store.message(m).ok().flatten()) else {
        return Ok(t);
    };
    if row.server_item_id.is_some()
        && let Some(trash) = st.store.folder_by_well_known("deleteditems").map_err(err)?
    {
        t.merge(
            st.store
                .move_messages(&[row.id], trash.id, now_ms(), None)
                .map_err(err)?,
        );
    } else {
        t.add(row.folder_id, row.id);
        t.removed.push(row.id);
    }
    Ok(t)
}

/// 임시 보관함에서 지우거나 옮기는 메일 가운데 작성 중 메일과 이어진 것을 정리한다(0.7.0). 서버에 아직 없는
/// 행은 여기서 지우고(남은 행 목록에서 뺀다), 서버에 있는 행은 작성 중 메일만 지우고 원래 처리에 맡긴다.
/// 보내는 중인 메일이 있으면 오류. `moving`(이동)이면 서버에 없는 행은 오류.
pub fn take_drafts(
    s: &eo_store::Store,
    ids: &[i64],
    moving: bool,
) -> eo_store::Result<(Touched, Vec<i64>)> {
    let mut t = Touched::default();
    let mut rest = Vec::with_capacity(ids.len());
    for &id in ids {
        let Some(d) = s.draft_by_message(id)? else {
            rest.push(id);
            continue;
        };
        if d.state == draft_state::OUTBOX {
            return Err(eo_store::Error::Invalid(
                "보내는 중인 메일은 지우거나 옮길 수 없습니다".into(),
            ));
        }
        let row = s.message(id)?;
        let on_server = row.as_ref().is_some_and(|r| r.server_item_id.is_some());
        if moving && !on_server {
            return Err(eo_store::Error::Invalid(
                "임시 보관함에 저장된 뒤 옮길 수 있습니다".into(),
            ));
        }
        if on_server {
            rest.push(id);
        } else if let Some(r) = &row {
            t.add(r.folder_id, id);
            t.removed.push(id);
        }
        s.delete_draft(d.id)?;
    }
    Ok((t, rest))
}

/// 복사: 서버에 아직 없는 작성 중 메일 행은 복사할 수 없다.
pub fn check_draft_copy(s: &eo_store::Store, ids: &[i64]) -> eo_store::Result<()> {
    for &id in ids {
        if s.draft_by_message(id)?.is_some()
            && s.message(id)?.is_some_and(|r| r.server_item_id.is_none())
        {
            return Err(eo_store::Error::Invalid(
                "임시 보관함에 저장된 뒤 복사할 수 있습니다".into(),
            ));
        }
    }
    Ok(())
}

/// 주소록(로컬, 빈도순). 처음 부를 때 메일에서 만든다.
#[tauri::command]
pub async fn contacts_local(app: AppHandle, query: String) -> CmdResult<Vec<Recipient>> {
    blocking(&app, move |st| {
        if st.store.search_contacts("", 1).map_err(err)?.is_empty() {
            st.store.rebuild_contacts().map_err(err)?;
        }
        let hidden = lock(&st.settings).hidden_domains.clone();
        Ok(receivable(
            st.store.search_contacts(&query, 24).map_err(err)?,
            &hidden,
        ))
    })
    .await
}

/// 설정의 숨길 도메인(하위 도메인 포함, 보내기 전용 주소 등)인가.
fn hidden(email: &str, domains: &[String]) -> bool {
    email.rsplit_once('@').is_some_and(|(_, d)| {
        let d = d.trim().trim_end_matches('>').to_ascii_lowercase();
        domains
            .iter()
            .any(|h| d == *h || d.ends_with(&format!(".{h}")))
    })
}

/// 자동 완성 후보에서 숨길 도메인의 주소를 빼고 8개까지.
fn receivable(v: Vec<Recipient>, domains: &[String]) -> Vec<Recipient> {
    v.into_iter()
        .filter(|r| !hidden(&r.email, domains))
        .take(8)
        .collect()
}

/// 서버 주소 검색(계획서 5.9 FindPeople). 로그인하지 않았으면 빈 목록.
#[tauri::command]
pub async fn contacts_server(app: AppHandle, query: String) -> CmdResult<Vec<Recipient>> {
    let st = app.state::<AppState>();
    if !st.client().is_logged_in() || query.trim().chars().count() < 2 {
        return Ok(Vec::new());
    }
    let domains = lock(&st.settings).hidden_domains.clone();
    match st.client().find_people(query.trim(), 16).await {
        Ok(v) => Ok(receivable(
            v.into_iter()
                .map(|a| Recipient {
                    name: a.name,
                    email: a.email,
                })
                .collect(),
            &domains,
        )),
        Err(_) => Ok(Vec::new()), // 자동완성은 실패해도 로컬 결과로 충분하다
    }
}

#[cfg(test)]
mod tests {
    use super::{Recipient, receivable, valid_email};

    #[test]
    fn hidden_domains_are_left_out_of_autocomplete() {
        let r = |e: &str| Recipient {
            name: String::new(),
            email: e.into(),
        };
        let mut v = vec![
            r("user1@p.example.com"),
            r("User1@P.Example.COM"),
            r("user1@example.com"),
            r("x@sub.p.example.com"),
            r("y@sp.example.com"),
        ];
        v.extend((0..10).map(|i| r(&format!("u{i}@example.com"))));
        assert_eq!(
            receivable(v.clone(), &[]).len(),
            8,
            "숨길 도메인이 없으면 그대로"
        );
        let out = receivable(v, &["p.example.com".to_owned()]);
        assert_eq!(out.len(), 8, "8개까지");
        assert_eq!(
            out.iter()
                .map(|x| x.email.as_str())
                .take(2)
                .collect::<Vec<_>>(),
            ["user1@example.com", "y@sp.example.com"]
        );
    }

    #[test]
    fn email_validation() {
        assert!(valid_email("minsu.kim@example.com"));
        assert!(!valid_email("minsu"));
        assert!(!valid_email("a@b"));
        assert!(!valid_email("a b@x.com"));
        assert!(!valid_email("<a@x.com>"));
    }
}
