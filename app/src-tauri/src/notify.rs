//! 새 메일 알림·알림 영역(트레이)·작업 표시줄 안읽음 배지(사용자 요청 2026-10-21).
//!
//! - 알림은 앱 자체 창(`notify`)이다: Windows 정식 알림은 앱 등록(레지스트리·시작 메뉴 바로가기)이 필요해 포터블 원칙과
//!   맞지 않는다(사용자 결정). 화면 오른쪽 아래 작업 영역 안에, 다른 창의 입력 초점을 가져가지 않게 띄운다.
//! - 알리는 메일: Outlook 받은 편지함, HCL Domino 받은 메일·외부 메일에 새로 들어온 안 읽은 메일. 로그인·연동 뒤 처음
//!   확인에서 밀려 들어온 것은 알리지 않는다. 앱 창을 앞에 두고 보고 있을 때도 알리지 않는다.
//! - 내용(설정): "새로운 메일 도착" + 읽지 않은 메일 수(기본), 보낸 사람, 보낸 사람 + 제목. 여러 통이면 한 창에 모두.
//! - 누르면 주 창을 앞으로 가져와 그 메일함(통합 편지함을 켰으면 통합 편지함)에서 그 메일을 연다.
//! - 창의 X는 앱을 끄지 않고 알림 영역으로 숨긴다(알림이 계속 오게). 끄기는 알림 영역 아이콘의 "종료".

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::Serialize;
use tauri::image::Image;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{
    AppHandle, Emitter, LogicalSize, Manager, PhysicalPosition, WebviewUrl, WebviewWindowBuilder,
};

use crate::applog;
use crate::commands::unified_on;
use crate::settings::NotifyMode;
use crate::state::{AppState, lock};
use crate::view::{UNIFIED_ID, unified_members};

pub const NOTIFY_LABEL: &str = "notify";
const TRAY_ID: &str = "main";
const WIDTH: f64 = 340.0;
/// 한 창에 보이는 메일 수(넘치면 "외 n통").
const MAX_ITEMS: usize = 6;

/// 새 메일 출처.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Outlook,
    Domino,
}

/// 출처별로 마지막으로 본 메일 번호(없으면 아직 처음 확인 전).
#[derive(Default)]
struct Watch {
    outlook: Option<i64>,
    domino: Option<i64>,
}

static WATCH: Mutex<Watch> = Mutex::new(Watch {
    outlook: None,
    domino: None,
});

/// 알림 창이 그릴 내용(창이 뜨면 가져간다).
static PAYLOAD: Mutex<Option<NotifyPayload>> = Mutex::new(None);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NotifyItem {
    pub id: i64,
    pub folder_id: i64,
    pub sender: String,
    pub subject: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NotifyPayload {
    pub mode: NotifyMode,
    /// 읽지 않은 메일 수(받은 편지함, 통합 편지함을 켰으면 통합 편지함).
    pub unread: i64,
    /// 새로 들어온 메일(최근 것부터, 최대 [`MAX_ITEMS`]).
    pub items: Vec<NotifyItem>,
    /// 새 메일 전체 수.
    pub count: usize,
    /// 열 메일함(통합 편지함이면 -1).
    pub folder_id: i64,
}

/// 주 창이 열 메일(알림을 눌렀을 때).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenMail {
    pub folder_id: i64,
    pub message_id: Option<i64>,
}

fn folders_of(st: &AppState, source: Source) -> Vec<i64> {
    match source {
        Source::Outlook => st
            .store
            .folder_by_well_known("inbox")
            .ok()
            .flatten()
            .map(|f| vec![f.id])
            .unwrap_or_default(),
        Source::Domino => {
            let boxes = st.store.domino_folder_boxes().unwrap_or_default();
            boxes
                .into_iter()
                .filter(|(_, k)| k == "XML_Inbox" || k == "XML_POP3")
                .map(|(f, _)| f)
                .collect()
        }
    }
}

/// 로그인·연동을 새로 시작하면 처음 확인은 알리지 않게 기준을 지운다.
pub fn reset(source: Source) {
    let mut w = lock(&WATCH);
    match source {
        Source::Outlook => w.outlook = None,
        Source::Domino => w.domino = None,
    }
}

/// 확인이 끝날 때 부른다: 새로 들어온 안 읽은 메일이 있으면 알린다(뒤에서).
pub fn check(app: &AppHandle, source: Source) {
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        if st.perf_test || st.ui_test {
            return;
        }
        let folders = folders_of(&st, source);
        let Ok(max) = st.store.max_message_id_in(&folders) else {
            return;
        };
        let max = max.unwrap_or(0);
        let since = {
            let mut w = lock(&WATCH);
            let slot = match source {
                Source::Outlook => &mut w.outlook,
                Source::Domino => &mut w.domino,
            };
            let prev = *slot;
            *slot = Some(max.max(prev.unwrap_or(0)));
            // 처음 확인(로그인·연동 직후 밀려 들어온 것까지): 기준만 잡는다.
            match prev {
                Some(p) if max > p => p,
                _ => return,
            }
        };
        let rows = match st.store.unread_after(&folders, since, 1000) {
            Ok(r) => r,
            Err(e) => {
                applog::warn(format!("새 메일 알림 확인 실패: {e}"));
                return;
            }
        };
        if rows.is_empty() {
            return;
        }
        let mode = lock(&st.settings).notify;
        if mode == NotifyMode::Off || main_in_front(&app) {
            return;
        }
        let unified = unified_on(&st);
        let payload = NotifyPayload {
            mode,
            unread: unread_count(&st),
            count: rows.len(),
            folder_id: if unified {
                UNIFIED_ID
            } else {
                rows[0].folder_id
            },
            items: rows
                .into_iter()
                .take(MAX_ITEMS)
                .map(|r| NotifyItem {
                    id: r.id,
                    folder_id: r.folder_id,
                    sender: r
                        .from_name
                        .filter(|n| !n.trim().is_empty())
                        .or(r.from_addr)
                        .unwrap_or_default(),
                    subject: r.subject.unwrap_or_default(),
                })
                .collect(),
        };
        show(&app, payload);
    });
}

/// 주 창을 앞에 두고 보고 있는가.
fn main_in_front(app: &AppHandle) -> bool {
    app.get_webview_window("main").is_some_and(|w| {
        w.is_visible().unwrap_or(false)
            && !w.is_minimized().unwrap_or(true)
            && w.is_focused().unwrap_or(false)
    })
}

/// 읽지 않은 메일 수: 받은 편지함(통합 편지함을 켰으면 통합 편지함).
pub fn unread_count(st: &AppState) -> i64 {
    let folders = if unified_on(st) {
        unified_members(&st.store).unwrap_or_default()
    } else {
        folders_of(st, Source::Outlook)
    };
    st.store
        .folder_counts()
        .map(|c| {
            c.iter()
                .filter(|f| folders.contains(&f.folder_id))
                .map(|f| f.unread)
                .sum()
        })
        .unwrap_or(0)
}

/// 알림 창을 띄운다. 떠 있던 것은 닫고 새로 만든다(다시 띄울 때도 입력 초점을 가져가지 않게, 이름은 매번 다르게).
/// 창 만들기는 주 스레드가 아닌 곳에서(주 스레드에서 만들면 Windows에서 멈출 수 있다).
fn show(app: &AppHandle, payload: NotifyPayload) {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    *lock(&PAYLOAD) = Some(payload);
    close_all(app);
    let label = format!("{NOTIFY_LABEL}-{}", SEQ.fetch_add(1, Ordering::Relaxed) + 1);
    let root = app.state::<AppState>().root.clone();
    let mut builder = WebviewWindowBuilder::new(
        app,
        &label,
        WebviewUrl::App(std::path::PathBuf::from("index.html")),
    )
    .title("새 메일")
    .inner_size(WIDTH, 96.0)
    .decorations(false)
    .resizable(false)
    .always_on_top(true)
    .skip_taskbar(true)
    .focusable(false)
    .focused(false)
    // 알림 창이 내용을 그리고 높이를 알려 주면 자리를 잡아 보인다.
    .visible(false)
    .data_directory(root.join("webview"));
    // 같은 프로필을 쓰는 창은 브라우저 인자가 같아야 열린다.
    if let Some(args) = crate::memory::test_browser_args() {
        builder = builder.additional_browser_args(&args);
    }
    if let Err(e) = builder.build() {
        applog::warn(format!("알림 창을 만들지 못함: {e}"));
    }
}

fn close_all(app: &AppHandle) {
    for (label, w) in app.webview_windows() {
        if label.starts_with(NOTIFY_LABEL) {
            let _ = w.destroy();
        }
    }
}

/// `--ui-test` 전용: 받은 편지함 첫 메일로 알림 창을 띄운다(창이 뜨고, 누르면 주 창에서 열리는지 본다).
#[tauri::command]
pub async fn ui_test_notify(app: AppHandle) -> Result<i64, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        if !st.ui_test {
            return Err("점검 모드가 아닙니다".to_owned());
        }
        let inbox = st
            .store
            .folder_by_well_known("inbox")
            .map_err(|e| e.to_string())?
            .ok_or("받은 편지함 없음")?;
        let row = st
            .store
            .list_rows(inbox.id, 0, 1)
            .map_err(|e| e.to_string())?
            .into_iter()
            .next()
            .ok_or("메일 없음")?;
        let id = row.id;
        show(
            &app,
            NotifyPayload {
                mode: NotifyMode::SenderSubject,
                unread: unread_count(&st),
                count: 1,
                folder_id: inbox.id,
                items: vec![NotifyItem {
                    id,
                    folder_id: inbox.id,
                    sender: row.from_name.unwrap_or_default(),
                    subject: row.subject.unwrap_or_default(),
                }],
            },
        );
        Ok(id)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// 알림 창: 그릴 내용.
#[tauri::command]
pub fn notify_payload() -> Option<NotifyPayload> {
    lock(&PAYLOAD).clone()
}

/// 알림 창: 내용을 그린 높이(논리 px). 오른쪽 아래 작업 영역 안에 두고 보인다.
#[tauri::command]
pub fn notify_ready(window: tauri::WebviewWindow, height: f64) {
    let w = window;
    let height = height.clamp(60.0, 420.0);
    let _ = w.set_size(LogicalSize::new(WIDTH, height));
    if let Ok(Some(m)) = w.primary_monitor() {
        let sf = m.scale_factor();
        let area = m.work_area();
        let (pw, ph) = ((WIDTH * sf) as i32, (height * sf) as i32);
        let margin = (12.0 * sf) as i32;
        let x = area.position.x + area.size.width as i32 - pw - margin;
        let y = area.position.y + area.size.height as i32 - ph - margin;
        let _ = w.set_position(PhysicalPosition::new(x, y));
    }
    let _ = w.show();
}

/// 알림 창: 닫기(시간이 지났거나 ✕).
#[tauri::command]
pub fn notify_close(window: tauri::WebviewWindow) {
    let _ = window.destroy();
}

/// 알림 창을 눌렀다: 주 창을 앞으로 가져와 그 메일을 연다.
#[tauri::command]
pub fn notify_open(app: AppHandle, folder_id: i64, message_id: Option<i64>) {
    close_all(&app);
    show_main(&app);
    let _ = app.emit_to(
        "main",
        "open-mail",
        OpenMail {
            folder_id,
            message_id,
        },
    );
}

/// 주 창을 보이고 앞으로(숨겼던 창이면 WebView2 그리기·메모리 목표도 되돌린다).
pub fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        crate::memory::set_hidden(&w, false);
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

/// 알림 영역 아이콘(열기·종료). 왼쪽 클릭은 열기.
pub fn install_tray(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "열기", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "종료", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &quit])?;
    let mut tray = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip("Enhanced Outlook")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, e| match e.id.as_ref() {
            "open" => show_main(app),
            "quit" => {
                applog::info("알림 영역에서 종료");
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, e| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = e
            {
                show_main(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
        let _ = TRAY_BASE.set((icon.rgba().to_vec(), icon.width(), icon.height()));
    }
    tray.build(app)?;
    Ok(())
}

/// 알림 영역 아이콘의 원래 그림(RGBA, 폭, 높이). 읽지 않은 메일 수를 그 위에 그린다.
static TRAY_BASE: std::sync::OnceLock<(Vec<u8>, u32, u32)> = std::sync::OnceLock::new();

/// 알림 영역 아이콘 + 오른쪽 아래 빨간 원과 흰 숫자(0.7.0 사용자 지적: 창을 숨기면 작업 표시줄 단추가 없어
/// 숫자가 아무 데도 안 보였다). 32×32(RGBA, 한 변 px). 100 이상은 "99".
pub fn tray_image(base: &[u8], bw: u32, bh: u32, n: i64) -> (Vec<u8>, u32) {
    const S: u32 = 32;
    let mut px = vec![0u8; (S * S * 4) as usize];
    // 원래 아이콘을 32×32로(칸 평균).
    if bw > 0 && bh > 0 && base.len() >= (bw * bh * 4) as usize {
        for y in 0..S {
            for x in 0..S {
                let (x0, x1) = (x * bw / S, ((x + 1) * bw / S).max(x * bw / S + 1));
                let (y0, y1) = (y * bh / S, ((y + 1) * bh / S).max(y * bh / S + 1));
                let mut acc = [0u32; 4];
                let mut count = 0u32;
                for sy in y0..y1.min(bh) {
                    for sx in x0..x1.min(bw) {
                        let i = ((sy * bw + sx) * 4) as usize;
                        for (k, a) in acc.iter_mut().enumerate() {
                            *a += u32::from(base[i + k]);
                        }
                        count += 1;
                    }
                }
                let i = ((y * S + x) * 4) as usize;
                for k in 0..4 {
                    if let Some(v) = acc[k].checked_div(count) {
                        px[i + k] = v as u8;
                    }
                }
            }
        }
    }
    if n <= 0 {
        return (px, S);
    }
    // 오른쪽 아래 원(지름 22, 흰 테두리 1px).
    let (cx, cy, r) = (20.5_f64, 20.5_f64, 11.0_f64);
    for y in 0..S {
        for x in 0..S {
            let d = ((x as f64 - cx).powi(2) + (y as f64 - cy).powi(2)).sqrt();
            let i = ((y * S + x) * 4) as usize;
            if d <= r - 1.0 {
                px[i..i + 4].copy_from_slice(&[0xc5, 0x0f, 0x1f, 0xff]);
            } else if d <= r {
                px[i..i + 4].copy_from_slice(&[0xff, 0xff, 0xff, 0xff]);
            }
        }
    }
    let text: Vec<usize> = n
        .clamp(0, 99)
        .to_string()
        .bytes()
        .map(|b| (b - b'0') as usize)
        .collect();
    let scale: u32 = if text.len() == 1 { 3 } else { 2 };
    let gap = if text.len() == 1 { 0 } else { 2 };
    let w = text.len() as u32 * 3 * scale + (text.len() as u32 - 1) * gap;
    let h = 5 * scale;
    let (x0, y0) = (21 - w.div_ceil(2), 21 - h.div_ceil(2));
    for (k, d) in text.iter().enumerate() {
        let dx = x0 + k as u32 * (3 * scale + gap);
        for (row, bits) in DIGITS[*d].iter().enumerate() {
            for col in 0..3u32 {
                if bits & (0b100 >> col) == 0 {
                    continue;
                }
                for yy in 0..scale {
                    for xx in 0..scale {
                        let (x, y) = (dx + col * scale + xx, y0 + row as u32 * scale + yy);
                        if x < S && y < S {
                            let i = ((y * S + x) * 4) as usize;
                            px[i..i + 4].copy_from_slice(&[0xff, 0xff, 0xff, 0xff]);
                        }
                    }
                }
            }
        }
    }
    (px, S)
}

/// 안읽음 수가 바뀌었을 수 있다: 작업 표시줄 배지·알림 영역 설명을 고친다(짧게 모아서).
pub fn refresh_badge(app: &AppHandle) {
    static SCHEDULED: AtomicBool = AtomicBool::new(false);
    if SCHEDULED.swap(true, Ordering::SeqCst) {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_millis(400)).await;
        SCHEDULED.store(false, Ordering::SeqCst);
        let app2 = app.clone();
        let Ok(n) =
            tauri::async_runtime::spawn_blocking(move || unread_count(&app2.state::<AppState>()))
                .await
        else {
            return;
        };
        set_badge(&app, n);
    });
}

fn set_badge(app: &AppHandle, unread: i64) {
    static LAST: Mutex<Option<i64>> = Mutex::new(None);
    if lock(&LAST).replace(unread) == Some(unread) {
        return;
    }
    if let Some(w) = app.get_webview_window("main") {
        let icon = (unread > 0).then(|| {
            let (rgba, size) = badge_image(unread);
            Image::new_owned(rgba, size, size)
        });
        let _ = w.set_overlay_icon(icon);
    }
    if let Some(t) = app.tray_by_id(TRAY_ID) {
        let tip = if unread > 0 {
            format!("Enhanced Outlook · 읽지 않은 메일 {unread}통")
        } else {
            "Enhanced Outlook".to_owned()
        };
        let _ = t.set_tooltip(Some(tip));
        if let Some((rgba, w, h)) = TRAY_BASE.get() {
            let (img, s) = tray_image(rgba, *w, *h, unread);
            let _ = t.set_icon(Some(Image::new_owned(img, s, s)));
        }
    }
}

/// 3×5 숫자 글꼴(행마다 3비트).
const DIGITS: [[u8; 5]; 10] = [
    [0b111, 0b101, 0b101, 0b101, 0b111],
    [0b010, 0b110, 0b010, 0b010, 0b111],
    [0b111, 0b001, 0b111, 0b100, 0b111],
    [0b111, 0b001, 0b111, 0b001, 0b111],
    [0b101, 0b101, 0b111, 0b001, 0b001],
    [0b111, 0b100, 0b111, 0b001, 0b111],
    [0b111, 0b100, 0b111, 0b101, 0b111],
    [0b111, 0b001, 0b010, 0b010, 0b010],
    [0b111, 0b101, 0b111, 0b101, 0b111],
    [0b111, 0b101, 0b111, 0b001, 0b111],
];

/// 작업 표시줄 덧씌움 아이콘: 빨간 원 + 흰 숫자(100 이상은 "99"). (RGBA, 한 변 px)
pub fn badge_image(n: i64) -> (Vec<u8>, u32) {
    const S: u32 = 32;
    let mut px = vec![0u8; (S * S * 4) as usize];
    let c = (S as f64 - 1.0) / 2.0;
    for y in 0..S {
        for x in 0..S {
            let d = ((x as f64 - c).powi(2) + (y as f64 - c).powi(2)).sqrt();
            // 가장자리 부드럽게.
            let a = (c + 0.5 - d).clamp(0.0, 1.0);
            if a > 0.0 {
                let i = ((y * S + x) * 4) as usize;
                px[i..i + 4].copy_from_slice(&[0xc5, 0x0f, 0x1f, (a * 255.0) as u8]);
            }
        }
    }
    let text: Vec<usize> = n
        .clamp(0, 99)
        .to_string()
        .bytes()
        .map(|b| (b - b'0') as usize)
        .collect();
    // 한 자리면 크게(4배), 두 자리면 3배.
    let scale: u32 = if text.len() == 1 { 4 } else { 3 };
    let gap = scale;
    let w = text.len() as u32 * 3 * scale + (text.len() as u32 - 1) * gap;
    let h = 5 * scale;
    let (x0, y0) = ((S - w) / 2, (S - h) / 2);
    for (k, d) in text.iter().enumerate() {
        let dx = x0 + k as u32 * (3 * scale + gap);
        for (row, bits) in DIGITS[*d].iter().enumerate() {
            for col in 0..3u32 {
                if bits & (0b100 >> col) == 0 {
                    continue;
                }
                for yy in 0..scale {
                    for xx in 0..scale {
                        let x = dx + col * scale + xx;
                        let y = y0 + row as u32 * scale + yy;
                        let i = ((y * S + x) * 4) as usize;
                        px[i..i + 4].copy_from_slice(&[0xff, 0xff, 0xff, 0xff]);
                    }
                }
            }
        }
    }
    (px, S)
}

#[cfg(test)]
mod tests {
    use super::{badge_image, tray_image};

    #[test]
    fn tray_icon_keeps_app_icon_and_adds_count() {
        // 64×64 파란 아이콘.
        let base: Vec<u8> = std::iter::repeat_n([0x00, 0x60, 0xc0, 0xff], 64 * 64)
            .flatten()
            .collect();
        let (px, s) = tray_image(&base, 64, 64, 0);
        let at = |px: &[u8], x: u32, y: u32| {
            let i = ((y * s + x) * 4) as usize;
            [px[i], px[i + 1], px[i + 2], px[i + 3]]
        };
        assert_eq!(s, 32);
        assert_eq!(
            at(&px, 5, 5),
            [0x00, 0x60, 0xc0, 0xff],
            "0통이면 원래 아이콘"
        );
        assert_eq!(at(&px, 30, 30), [0x00, 0x60, 0xc0, 0xff]);
        for n in [3, 42, 500] {
            let (px, _) = tray_image(&base, 64, 64, n);
            assert_eq!(
                at(&px, 5, 5),
                [0x00, 0x60, 0xc0, 0xff],
                "{n}: 왼쪽 위는 아이콘 그대로"
            );
            assert_eq!(
                at(&px, 20, 11),
                [0xc5, 0x0f, 0x1f, 0xff],
                "{n}: 오른쪽 아래 빨간 원"
            );
            let white = (0..s * s)
                .filter(|i| px[(*i * 4) as usize] == 0xff && px[(*i * 4 + 1) as usize] == 0xff)
                .count();
            assert!(white > 30, "{n}: 흰 숫자와 테두리");
        }
    }

    #[test]
    fn badge_has_red_disc_and_white_digits() {
        for n in [1, 7, 42, 99, 1234] {
            let (px, s) = badge_image(n);
            assert_eq!(px.len(), (s * s * 4) as usize);
            let at = |x: u32, y: u32| {
                let i = ((y * s + x) * 4) as usize;
                [px[i], px[i + 1], px[i + 2], px[i + 3]]
            };
            assert_eq!(at(0, 0)[3], 0, "모서리는 투명");
            let white = (0..s * s).filter(|i| px[(*i * 4) as usize] == 0xff).count();
            assert!(white > 20, "{n}: 숫자가 그려짐");
            assert_eq!(at(s / 2, 2), [0xc5, 0x0f, 0x1f, 0xff], "{n}: 빨간 원");
        }
    }
}
