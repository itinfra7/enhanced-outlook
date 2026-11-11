//! HCL Domino 알림 메일의 연결문서(1.2.0 사용자 요청). 설정 → HCL Domino → "연결문서 정보 가져오기".
//!
//! 본문의 `[연결문서]` 링크(`eo_mime::linked::find_link`)가 가리키는 업무 문서를 HCL Domino 로그인 세션으로 가져와
//! 메일 본문 맨 아래 구분선 밑에 원래 모양대로 붙인다(단추·스크립트는 뺀다). 메일을 열 때마다 화면이
//! [`linked_refresh`]로 새로 가져오고, 바뀌었으면 메일을 다시 연다. 가져온 것은 DB(`linked_docs`)에 두어 오프라인이면
//! 마지막 것을 보인다. 요청은 `eo_owa::domino::Domino::fetch_linked`의 허용 목록(문서·CSS·그림 GET)만.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use eo_mime::linked;
use eo_store::LinkedDoc;
use serde::Serialize;
use tauri::{AppHandle, Manager};

use crate::applog;
use crate::state::{AppState, lock};
use crate::view::{LinkedView, MessageView};

type CmdResult<T> = Result<T, String>;

/// 붙인 덩어리 맨 위의 안내.
const LABEL: &str = "연결문서";
/// 받을 문서·스타일시트 최대 크기.
const MAX_DOC: usize = 4 * 1024 * 1024;
/// 품을 그림 하나·전부의 최대 크기(본문 캐시·DB에 들어간다).
const MAX_IMAGE: usize = 256 * 1024;
const MAX_IMAGES_TOTAL: usize = 2 * 1024 * 1024;

/// 이번 실행 동안 받은 스타일시트(주소 → 글)·그림(주소 → data: 주소). 문서마다 같은 것을 쓴다.
static CSS: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);
static IMAGES: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);
/// 가져오는 중인 메일(같은 메일을 겹쳐 가져오지 않게).
static BUSY: Mutex<Option<HashSet<i64>>> = Mutex::new(None);

fn enabled(st: &AppState) -> bool {
    let s = lock(&st.settings);
    s.domino.enabled && s.domino.linked_docs
}

/// 읽기 창 본문에 저장해 둔 연결문서를 붙이고 상태를 적는다(`commands::render_view`).
pub fn decorate(st: &AppState, id: i64, v: &mut MessageView) {
    if !enabled(st) {
        return;
    }
    let Some(link) = linked::find_link(&v.html) else {
        return;
    };
    let saved = st.store.linked_doc(id).ok().flatten();
    if let Some(d) = &saved {
        v.html = linked::append_to_document(&v.html, &d.html);
    }
    v.linked = Some(LinkedView {
        supported: link.doc_url().is_some(),
        fetched_at: saved.map(|d| d.fetched_at),
    });
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkedRefresh {
    /// 저장한 내용이 바뀌었다(화면이 메일을 다시 연다).
    pub changed: bool,
    /// 저장해 둔 것을 가져온 때(유닉스 초).
    pub fetched_at: Option<i64>,
    /// 못 가져온 까닭(저장해 둔 것은 그대로 보인다).
    pub error: Option<String>,
}

/// 겹쳐 가져오지 않게 메일 번호를 잡아 둔다.
struct Busy(i64);

impl Busy {
    fn take(id: i64) -> Option<Self> {
        lock(&BUSY)
            .get_or_insert_default()
            .insert(id)
            .then_some(Self(id))
    }
}

impl Drop for Busy {
    fn drop(&mut self) {
        if let Some(s) = lock(&BUSY).as_mut() {
            s.remove(&self.0);
        }
    }
}

/// 메일을 열 때마다 연결문서를 새로 가져온다. 링크가 없거나 설정이 꺼져 있으면 아무것도 하지 않는다.
#[tauri::command]
pub async fn linked_refresh(app: AppHandle, id: i64) -> CmdResult<LinkedRefresh> {
    let st = app.state::<AppState>();
    if !enabled(&st) {
        return Ok(LinkedRefresh {
            changed: false,
            fetched_at: None,
            error: None,
        });
    }
    let found = {
        let app = app.clone();
        tauri::async_runtime::spawn_blocking(move || {
            let st = app.state::<AppState>();
            let (v, _) = crate::commands::cached_or_render(&st, id, false)?;
            let saved = st.store.linked_doc(id).map_err(|e| e.to_string())?;
            Ok::<_, String>((linked::find_link(&v.html), saved.map(|d| d.fetched_at)))
        })
        .await
        .map_err(|e| e.to_string())??
    };
    let (Some(link), saved_at) = found else {
        return Ok(LinkedRefresh {
            changed: false,
            fetched_at: None,
            error: None,
        });
    };
    let fail = |e: String| LinkedRefresh {
        changed: false,
        fetched_at: saved_at,
        error: Some(e),
    };
    let Some(url) = link.doc_url() else {
        return Ok(fail(format!(
            "지원하지 않는 연결문서입니다(SysID {})",
            link.sys_id
        )));
    };
    if !st.domino.is_logged_in() {
        return Ok(fail("HCL Domino에 로그인하지 않았습니다".into()));
    }
    let Some(_busy) = Busy::take(id) else {
        return Ok(LinkedRefresh {
            changed: false,
            fetched_at: saved_at,
            error: None,
        });
    };
    let html = match build(&st, &url).await {
        Ok(h) => h,
        Err(e) => {
            applog::warn(format!("연결문서를 가져오지 못함(메일 {id}): {e}"));
            return Ok(fail(e));
        }
    };
    let doc = LinkedDoc {
        url,
        html,
        fetched_at: crate::commands::now_ms() / 1000,
    };
    let changed = st
        .store
        .put_linked_doc(id, &doc)
        .map_err(|e| e.to_string())?;
    // 캐시한 본문에는 앞 내용·가져온 때가 있다.
    lock(&st.views).remove(&id);
    Ok(LinkedRefresh {
        changed,
        fetched_at: Some(doc.fetched_at),
        error: None,
    })
}

/// 받은 글: UTF-8로 읽히면 그대로, 아니면 머리글의 문자셋(없으면 EUC-KR, 사내 업무 서버).
fn text_of(f: &eo_owa::domino::Fetched) -> String {
    match std::str::from_utf8(&f.data) {
        Ok(s) => s.to_owned(),
        Err(_) => eo_owa::domino::decode(&f.data, f.charset().or(Some("euc-kr"))),
    }
}

/// 문서를 받아 본문에 붙일 덩어리로 만든다.
async fn build(st: &AppState, url: &str) -> Result<String, String> {
    let d = &st.domino;
    let page = d.fetch_linked(url).await.map_err(|e| e.to_string())?;
    if page.data.len() > MAX_DOC {
        return Err("연결문서가 너무 큽니다".into());
    }
    let html = eo_owa::domino::decode(&page.data, page.charset().or(Some("euc-kr")));
    let doc = linked::clean_doc(&html, url);
    if doc.body.trim().is_empty() {
        return Err("연결문서 내용이 비어 있습니다".into());
    }
    let mut css = String::new();
    let mut urls = doc.images.clone();
    for sheet in &doc.stylesheets {
        let cached = lock(&CSS).get_or_insert_default().get(sheet).cloned();
        let text = match cached {
            Some(t) => t,
            None => match d.fetch_linked(sheet).await {
                Ok(f) if f.data.len() <= MAX_DOC && !f.data.starts_with(b"<") => {
                    let t = text_of(&f);
                    lock(&CSS)
                        .get_or_insert_default()
                        .insert(sheet.clone(), t.clone());
                    t
                }
                // 스타일시트 하나쯤 못 받아도 내용은 보인다.
                _ => continue,
            },
        };
        let (scoped, found) = linked::scope_css(&text, sheet, &doc.classes, "#eo-linked");
        css.push_str(&scoped);
        for u in found {
            if !urls.contains(&u) {
                urls.push(u);
            }
        }
    }
    let mut images = HashMap::new();
    let mut total = 0;
    for u in &urls {
        let cached = lock(&IMAGES).get_or_insert_default().get(u).cloned();
        let data = match cached {
            Some(d) => d,
            None => match d.fetch_linked(u).await {
                Ok(f) if f.data.len() <= MAX_IMAGE => {
                    let Some(uri) = linked::data_uri(&f.content_type, &f.data) else {
                        continue;
                    };
                    lock(&IMAGES)
                        .get_or_insert_default()
                        .insert(u.clone(), uri.clone());
                    uri
                }
                _ => continue,
            },
        };
        total += data.len();
        if total > MAX_IMAGES_TOTAL {
            break;
        }
        images.insert(u.clone(), data);
    }
    Ok(linked::assemble(&doc.body, &css, &images, LABEL, &urls))
}
