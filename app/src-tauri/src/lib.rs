//! Enhanced Outlook 앱 셸. 화면은 로컬 저장소·색인만 보고(계획서 4.1), 서버와는 동기화 엔진만 이야기한다.
//!
//! - 데이터·WebView2 프로필은 모두 실행 파일 옆 `EnhancedOutlook\`(포터블).
//! - `app-cid` 프로토콜: 읽기 창의 `cid:` 이미지를 원문에서 바로 꺼내 준다(계획서 9.2).
//! - 메일 안 링크(새 창 요청)는 앱 안에서 열지 않고 기본 브라우저로 넘긴다.
//! - `--perf-test`: 화면이 정해진 측정 시나리오를 돌려 `EnhancedOutlook\logs\perf.log`에 쓰고 끝낸다.
//! - `--mem-test`: 같은 조작을 되풀이하며 메모리를 재 `EnhancedOutlook\logs\memtest.log`에 쓰고 끝낸다(누수 점검).
//! - 8단계: 로그(`EnhancedOutlook\logs\app.log`, 회전), 설정(`settings.json`), 창 위치·마지막 폴더(`state.json`),
//!   비정상 종료 감지(`running`). 포터블 폴더 밖에는 아무것도 쓰지 않는다.

mod actions;
mod activity;
mod applog;
mod cleanup;
mod commands;
mod compat;
mod compose;
mod domino;
mod dupes;
mod files;
mod folders;
mod import;
mod memory;
mod notify;
mod settings;
mod state;
mod templates;
mod view;

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tauri::http::{Response, StatusCode, header};
use tauri::webview::NewWindowResponse;
use tauri::{
    AppHandle, Emitter, Manager, RunEvent, Url, WebviewUrl, WebviewWindow, WebviewWindowBuilder,
    WindowEvent,
};
use tauri_plugin_opener::OpenerExt;

use crate::settings::WindowState;
use crate::state::{AppState, lock};

/// 창의 지금 위치·크기를 화면 상태에 적는다(최대화·최소화 중에는 원래 크기를 지킨다).
/// 주 스레드가 아닌 곳에서 부른다(창 값 읽기가 주 스레드에 물어보는 방식이다).
/// 창 이동·크기 이벤트로 하려 했으나, 이 앱의 창 이벤트 처리기에는 포커스 이벤트만 들어와서(8단계 확인) 1초마다 읽는다.
fn record_window(st: &AppState, w: &WebviewWindow) {
    // 창 값은 잠금 밖에서 읽는다. 하나라도 못 읽으면(창이 닫히는 중 등) 기록하지 않는다.
    let (Ok(false), Ok(maximized)) = (w.is_minimized(), w.is_maximized()) else {
        return;
    };
    let normal = if maximized {
        None
    } else {
        let (Ok(sf), Ok(pos), Ok(size)) = (w.scale_factor(), w.outer_position(), w.inner_size())
        else {
            return;
        };
        Some((pos.to_logical::<f64>(sf), size.to_logical::<f64>(sf)))
    };
    let mut ui = lock(&st.ui);
    let mut ws = ui.window.clone().unwrap_or_default();
    ws.maximized = maximized;
    if let Some((pos, size)) = normal {
        ws.x = pos.x;
        ws.y = pos.y;
        ws.width = size.width;
        ws.height = size.height;
    }
    if ui.window.as_ref() != Some(&ws) {
        ui.window = Some(ws);
        st.ui_dirty.store(true, Ordering::Relaxed);
    }
}

/// 바뀐 화면 상태를 저장한다.
fn save_ui(st: &AppState) {
    if st.ui_dirty.swap(false, Ordering::Relaxed) {
        let ui = lock(&st.ui).clone();
        if let Err(e) = settings::save_ui_state(&st.root, &ui) {
            applog::warn(format!("화면 상태 저장 실패: {e}"));
        }
    }
}

/// 저장한 창 위치가 지금 모니터 안에 보이는가(모니터 구성이 바뀌었을 수 있다).
fn on_some_monitor(w: &WebviewWindow) -> bool {
    let (Ok(pos), Ok(monitors)) = (w.outer_position(), w.available_monitors()) else {
        return true;
    };
    // 제목 표시줄 근처 한 점이 어느 모니터 안에 있으면 된다.
    let (x, y) = (pos.x + 60, pos.y + 15);
    monitors.iter().any(|m| {
        let (mp, ms) = (m.position(), m.size());
        x >= mp.x
            && y >= mp.y
            && x < mp.x + i32::try_from(ms.width).unwrap_or(i32::MAX)
            && y < mp.y + i32::try_from(ms.height).unwrap_or(i32::MAX)
    })
}

/// 메일 링크 중 기본 프로그램으로 넘겨도 되는 것.
pub(crate) fn open_external(app: &AppHandle, url: &Url) {
    if matches!(url.scheme(), "http" | "https" | "mailto") {
        let _ = app.opener().open_url(url.as_str(), None::<&str>);
    }
}

/// 앱 자신의 화면 주소인가(그 밖의 주 창 이동은 막는다).
pub(crate) fn is_app_url(url: &Url) -> bool {
    match url.host_str() {
        Some("tauri.localhost") => true,
        // 개발 서버(vite)
        Some("localhost") => cfg!(debug_assertions),
        _ => url.scheme() == "tauri",
    }
}

/// `http://app-cid.localhost/{메일 ID}/{Content-ID}` → 원문 속 이미지.
fn cid_response(app: &AppHandle, path: &str) -> Response<Vec<u8>> {
    let not_found = || {
        Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Vec::new())
            .unwrap_or_default()
    };
    let mut parts = path.trim_start_matches('/').splitn(2, '/');
    let (Some(id), Some(cid)) = (parts.next(), parts.next()) else {
        return not_found();
    };
    let Ok(id) = id.parse::<i64>() else {
        return not_found();
    };
    let cid = percent_encoding::percent_decode_str(cid).decode_utf8_lossy();
    let st = app.state::<AppState>();
    match st.cid_image(id, &cid) {
        Some((mime, bytes)) if mime.starts_with("image/") => Response::builder()
            .header(header::CONTENT_TYPE, mime)
            // 메일 이미지가 WebView2 디스크 캐시에 남지 않게.
            .header(header::CACHE_CONTROL, "no-store")
            .header("X-Content-Type-Options", "nosniff")
            .body(bytes)
            .unwrap_or_else(|_| not_found()),
        _ => not_found(),
    }
}

pub fn run() {
    let started = Instant::now();
    // --mem-test는 성능 측정과 같은 환경(합성 저장소, 고정 창, 알림 영역 없음)에서 메모리만 잰다.
    let mem_test = std::env::args().any(|a| a == "--mem-test");
    commands::MEM_TEST.store(mem_test, std::sync::atomic::Ordering::Relaxed);
    let perf_test = mem_test || std::env::args().any(|a| a == "--perf-test");
    let ui_test = std::env::args().any(|a| a == "--ui-test");
    let image_check = std::env::args().any(|a| a == "--image-check");
    commands::IMAGE_CHECK.store(image_check, std::sync::atomic::Ordering::Relaxed);
    let (root, moved_from) = eo_store::default_root_migrating();
    applog::init(&root);
    applog::install_panic_hook();
    if let Some(old) = moved_from {
        applog::info(format!(
            "앞 버전 데이터 폴더를 옮김: {} → {}",
            old.display(),
            root.display()
        ));
    }
    // setup 전 단계의 시각(상태가 생기면 옮겨 적는다).
    let marks: Arc<Mutex<Vec<(&'static str, Instant)>>> = Arc::new(Mutex::new(Vec::new()));
    let early_marks = marks.clone();
    let context = tauri::generate_context!();
    lock(&marks).push(("컨텍스트", Instant::now()));

    let mut builder = tauri::Builder::default();
    // 한 번만 실행: 두 번째 실행은 이미 떠 있는 창을 앞으로(동기화 엔진·색인을 둘이 쓰지 않게).
    // 먼저 등록해야 한다. 점검(--ui-test·--perf-test)은 실행 파일 옆 합성 저장소를 쓰므로 걸지 않는다(사용자 앱이
    // 떠 있으면 점검 앱이 뜨지 않고 사용자 창만 앞으로 나왔다, 0.8.4).
    if !(ui_test || perf_test) {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            // 알림 영역에 숨어 있어도 앞으로.
            notify::show_main(app);
        }));
    }
    let app = builder
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .register_asynchronous_uri_scheme_protocol("app-cid", |ctx, req, responder| {
            let app = ctx.app_handle().clone();
            let path = req.uri().path().to_owned();
            tauri::async_runtime::spawn_blocking(move || {
                responder.respond(cid_response(&app, &path));
            });
        })
        .invoke_handler(tauri::generate_handler![
            commands::bootstrap,
            commands::folders,
            commands::list_messages,
            commands::search,
            commands::open_message,
            commands::prefetch,
            commands::allow_remote,
            commands::remove_remote_allow,
            commands::remote_allowlist,
            commands::report_broken_images,
            commands::image_check_list,
            commands::cleanup_preview,
            commands::set_pane_widths,
            commands::set_list_sort,
            commands::open_message_window,
            commands::app_status,
            commands::ui_test_cleanup_run,
            commands::save_attachment,
            commands::save_all_attachments,
            commands::open_attachment,
            commands::login,
            commands::logout,
            commands::sync_now,
            domino::domino_login,
            domino::domino_logout,
            domino::domino_sync_now,
            domino::domino_cleanup_preview,
            notify::notify_payload,
            notify::notify_ready,
            notify::notify_close,
            notify::notify_open,
            notify::ui_test_notify,
            domino::ui_test_domino_boxes,
            commands::perf_since_start,
            commands::perf_write,
            commands::perf_exit,
            memory::perf_memory,
            memory::perf_background,
            commands::get_settings,
            commands::set_settings,
            commands::set_outlook_url,
            commands::forget_remote_images,
            commands::open_logs,
            commands::set_last_folder,
            actions::mark_read,
            actions::set_flag,
            actions::move_messages,
            actions::copy_messages,
            actions::delete_messages,
            actions::server_delete,
            actions::purge_messages,
            actions::list_archived,
            actions::list_ids,
            actions::tool_preview,
            actions::tool_run,
            actions::batch_progress,
            actions::cancel_batch,
            actions::space_preview,
            actions::space_run,
            actions::export_mail,
            actions::cancel_export,
            actions::ui_test_export,
            actions::ui_test_export_files,
            actions::ui_test_close_window,
            actions::ui_test_window_labels,
            import::import_pick,
            import::import_start,
            import::import_cancel,
            import::ui_test_sample_backup,
            import::ui_test_sample_domino,
            commands::set_zoom,
            folders::folder_mark_read,
            folders::folder_create,
            folders::folder_rename,
            folders::folder_delete,
            folders::folder_move,
            folders::folder_empty,
            folders::folder_favorite,
            activity::activity_view,
            dupes::duplicates_find,
            dupes::duplicates_exclude,
            dupes::duplicates_purge,
            templates::templates_list,
            templates::templates_set,
            compose::compose_new,
            compose::compose_popout,
            compose::compose_in_window,
            compose::open_draft,
            compose::compose_load,
            compose::compose_save,
            compose::compose_upload,
            compose::compose_server_saved,
            compose::compose_add_files,
            compose::compose_pick_files,
            compose::compose_remove_file,
            compose::compose_send,
            compose::compose_discard,
            compose::contacts_local,
            compose::contacts_server,
        ])
        .setup(move |app| {
            let setup_at = Instant::now();
            // 한 번만 실행 플러그인을 지난 뒤라 이 프로세스가 저장소를 쓰는 유일한 프로세스다.
            let crashed_before = settings::mark_running(&root);
            applog::info(format!(
                "시작 v{} · 데이터 {}{}{}",
                app.package_info().version,
                root.display(),
                if mem_test {
                    " · 메모리 점검"
                } else if perf_test {
                    " · 성능 측정"
                } else if ui_test {
                    " · 화면 점검"
                } else if image_check {
                    " · 본문 그림 점검"
                } else {
                    ""
                },
                if crashed_before {
                    " · 지난 실행이 비정상 종료됨"
                } else {
                    ""
                }
            ));
            // 가장 오래 걸리는 WebView2 창 만들기를 먼저 시작하고, 저장소·색인은 그동안 다른 스레드에서 연다.
            // 화면의 첫 명령은 setup이 끝난 뒤에 처리되므로 그 전에 상태를 등록하면 된다.
            let opener = {
                let root = root.clone();
                std::thread::spawn(move || {
                    (
                        AppState::open(root, perf_test, ui_test, started, crashed_before),
                        Instant::now(),
                    )
                })
            };
            // 지난번 창 위치·크기(측정·점검은 늘 같은 크기로).
            let saved = if perf_test || ui_test || image_check {
                None
            } else {
                settings::load_ui_state(&root).window
            };
            let links = app.handle().clone();
            let nav = app.handle().clone();
            let mut builder = WebviewWindowBuilder::new(app, "main", WebviewUrl::default())
                .title("Enhanced Outlook")
                .inner_size(1280.0, 800.0)
                .min_inner_size(800.0, 500.0);
            if let Some(WindowState {
                x,
                y,
                width,
                height,
                maximized,
            }) = saved.clone()
                && width >= 800.0
                && height >= 500.0
            {
                builder = builder
                    .inner_size(width, height)
                    .position(x, y)
                    .maximized(maximized);
            }
            if let Some(args) = memory::test_browser_args() {
                builder = builder.additional_browser_args(&args);
            }
            let window = builder
                .data_directory(root.join("webview"))
                // 입력값(아이디 등)을 WebView2 프로필에 저장하지 않는다(계획서 9.5).
                .general_autofill_enabled(false)
                .on_new_window(move |url, _features| {
                    open_external(&links, &url);
                    NewWindowResponse::Deny
                })
                .on_navigation(move |url| {
                    if is_app_url(url) {
                        return true;
                    }
                    open_external(&nav, url);
                    false
                })
                .build()?;
            let window_at = Instant::now();
            // 글자 크기(설정의 화면 확대). 측정·점검은 늘 100%.
            let zoom = if perf_test || ui_test || image_check {
                100
            } else {
                settings::load_settings(&root).0.zoom
            };
            if zoom != 100 {
                let _ = window.set_zoom(zoom as f64 / 100.0);
            }
            if saved.is_some() && !on_some_monitor(&window) {
                let _ = window.center();
            }
            let (state, opened_at) = opener
                .join()
                .map_err(|_| "저장소를 여는 중 오류가 났습니다")?;
            let state = state.inspect_err(|e| applog::error(format!("시작 실패: {e}")))?;
            for (name, at) in lock(&early_marks).drain(..) {
                state.mark(name, at);
            }
            state.mark("setup 시작", setup_at);
            state.mark("저장소·색인 열림(병렬)", opened_at);
            state.mark("창 만듦", window_at);
            files::clean_tmp(&root.join("tmp"));
            app.manage(state);

            // DB와 색인을 대조해 고친 뒤, 원문은 있고 색인되지 않은 메일을 로그인 없이 색인한다.
            let handle = app.handle().clone();
            tauri::async_runtime::spawn_blocking(move || {
                let st = handle.state::<AppState>();
                match eo_sync::reconcile_index(&st.store, &st.search) {
                    Ok((0, 0)) => {}
                    Ok((missing, stale)) => applog::warn(format!(
                        "색인 대조: 다시 색인할 메일 {missing}통, 지운 색인 문서 {stale}개"
                    )),
                    Err(e) => applog::error(format!("색인 대조 실패: {e}")),
                }
                // 한 번만: 바로잡기 전 버전으로 가져온 HCL Domino EML의 틀린 문자셋 표시를 바로잡는다(2026-10-19).
                const DOMINO_FIX: &str = "repair:domino-eml-charset-v1";
                if st.store.meta(DOMINO_FIX).ok().flatten().is_none() {
                    match eo_sync::repair_domino_eml(&st.store) {
                        Ok(changed) => {
                            let _ = st.store.set_meta(DOMINO_FIX, "1");
                            if !changed.is_empty() {
                                applog::info(format!(
                                    "HCL Domino EML 글자 바로잡음 {}통",
                                    changed.len()
                                ));
                                commands::bump_list_gen();
                                let mut folders: Vec<i64> = changed.iter().map(|c| c.1).collect();
                                folders.sort_unstable();
                                folders.dedup();
                                for folder_id in folders {
                                    let ids = changed
                                        .iter()
                                        .filter(|c| c.1 == folder_id)
                                        .map(|c| c.0)
                                        .collect();
                                    let _ = handle.emit(
                                        "messages-changed",
                                        commands::MessagesChanged { folder_id, ids },
                                    );
                                }
                            }
                        }
                        Err(e) => applog::error(format!("HCL Domino EML 글자 바로잡기 실패: {e}")),
                    }
                }
                match eo_sync::index_pending(&st.store, &st.search, None) {
                    Ok(r) if r.indexed > 0 => applog::info(format!("색인 {}통", r.indexed)),
                    Ok(_) => {}
                    Err(e) => applog::error(format!("색인 실패: {e}")),
                }
                applog::info(format!(
                    "저장소 열림: 색인된 메일 {}통, 서버 반영 대기 {}건",
                    st.search.num_docs(),
                    st.store.pending_op_count().unwrap_or(0)
                ));
            });

            // 화면 상태(창 위치·마지막 폴더)는 1초마다 보고 바뀌었으면 저장한다: 비정상 종료돼도 그 자리에서 다시 시작.
            // 측정·점검 중에는 창을 읽지 않는다(재는 값에 끼어들지 않게, 그때는 위치를 쓰지도 않는다).
            // 숨기거나 최소화했거나 다른 프로그램을 오래 쓰는 동안은 WebView2 메모리 목표를 낮춘다(1.0.3 메모리
            // 점검: 작업 집합 약 520→340MB). 창으로 돌아오면 1초 안에 되돌린다.
            const IDLE_LOW: Duration = Duration::from_secs(5 * 60);
            let saver = app.handle().clone();
            let watched = window.clone();
            std::thread::spawn(move || {
                let mut low = false;
                let mut blurred: Option<Instant> = None;
                loop {
                    std::thread::sleep(Duration::from_secs(1));
                    let st = saver.state::<AppState>();
                    if !st.perf_test && !st.ui_test && !image_check {
                        record_window(&st, &watched);
                        if watched.is_focused().unwrap_or(true) {
                            blurred = None;
                        } else if blurred.is_none() {
                            blurred = Some(Instant::now());
                        }
                        let want = !watched.is_visible().unwrap_or(true)
                            || watched.is_minimized().unwrap_or(false)
                            || blurred.is_some_and(|t| t.elapsed() >= IDLE_LOW);
                        if want != low {
                            low = want;
                            memory::set_low_memory(&watched, want);
                        }
                    }
                    save_ui(&st);
                }
            });

            // 알림 영역 상주(사용자 요청 2026-10-21): 창의 X는 숨기기, 끄기는 알림 영역 아이콘의 "종료".
            // 측정·점검은 예전처럼(창을 닫으면 끝).
            let tray = !perf_test && !ui_test && !image_check;
            if tray && let Err(e) = notify::install_tray(app.handle()) {
                applog::warn(format!("알림 영역 아이콘을 만들지 못함: {e}"));
            }
            let focus = app.handle().clone();
            let hide = window.clone();
            window.on_window_event(move |e| match e {
                WindowEvent::Focused(true) => {
                    tauri::async_runtime::spawn(commands::wake_on_focus(focus.clone()));
                }
                WindowEvent::CloseRequested { api, .. } if tray => {
                    api.prevent_close();
                    let _ = hide.hide();
                    // 숨긴 동안 WebView2 메모리를 줄인다(보일 때 notify::show_main이 되돌린다).
                    memory::set_hidden(&hide, true);
                }
                _ => {}
            });
            notify::refresh_badge(app.handle());
            // 예전 판의 작성 중 메일도 임시 보관함에 보이게(0.7.0).
            if !perf_test {
                compose::prepare_drafts(app.handle());
            }
            Ok(())
        })
        .build(context)
        .expect("Tauri 앱 실행 실패");
    lock(&marks).push(("앱 빌드", Instant::now()));
    app.run(move |app, event| {
        if let RunEvent::Exit = event {
            if let Some(st) = app.try_state::<AppState>() {
                save_ui(&st);
                settings::clear_running(&st.root);
            }
            applog::info("종료");
        }
    });
}
