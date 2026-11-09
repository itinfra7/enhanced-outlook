//! 메모리(1.0.3 사용자 요청: 누수 점검·사용량 줄이기).
//!
//! - 재기(`--mem-test`): 이 프로세스와 그 아래 WebView2 프로세스(브라우저·렌더러·GPU 등)의 전용 바이트(커밋)와
//!   작업 집합. 작업 관리자의 "메모리" 열(전용 작업 집합)은 둘 사이 값이다.
//! - 숨긴 창: 알림 영역으로 숨기면 WebView2에 그리기를 멈추고 메모리를 줄이라고 알린다. 최소화했거나 다른
//!   프로그램을 5분 넘게 쓰면 메모리 목표만 낮춘다(측정: 전용 −27MB, 작업 집합 −180MB).
#![allow(unsafe_code)]

use std::collections::HashMap;

use serde::Serialize;
use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::ProcessStatus::{
    K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcessId, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
};

#[derive(Debug, Clone, Copy, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcMem {
    pub private: u64,
    pub working_set: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryReport {
    pub main: ProcMem,
    /// 아래 WebView2 프로세스 합.
    pub webview: ProcMem,
    pub webview_count: usize,
}

fn proc_mem(pid: u32) -> Option<ProcMem> {
    // SAFETY: 핸들은 여기서 열고 닫는다. 구조체 크기를 cb로 알려 준다.
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return None;
        }
        let mut c: PROCESS_MEMORY_COUNTERS_EX = std::mem::zeroed();
        c.cb = size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
        let ok = K32GetProcessMemoryInfo(h, (&raw mut c).cast::<PROCESS_MEMORY_COUNTERS>(), c.cb);
        CloseHandle(h);
        (ok != 0).then_some(ProcMem {
            private: c.PrivateUsage as u64,
            working_set: c.WorkingSetSize as u64,
        })
    }
}

/// `root` 아래(손자까지) msedgewebview2.exe 프로세스들.
fn webview_descendants(root: u32) -> Vec<u32> {
    let mut children: HashMap<u32, Vec<(u32, bool)>> = HashMap::new();
    // SAFETY: 스냅숏 핸들은 여기서 열고 닫는다. dwSize를 채워 넘긴다.
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return Vec::new();
        }
        let mut e: PROCESSENTRY32W = std::mem::zeroed();
        e.dwSize = size_of::<PROCESSENTRY32W>() as u32;
        let mut ok = Process32FirstW(snap, &mut e);
        while ok != 0 {
            let len = e
                .szExeFile
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(e.szExeFile.len());
            let name = String::from_utf16_lossy(&e.szExeFile[..len]);
            let webview = name.eq_ignore_ascii_case("msedgewebview2.exe");
            children
                .entry(e.th32ParentProcessID)
                .or_default()
                .push((e.th32ProcessID, webview));
            ok = Process32NextW(snap, &mut e);
        }
        CloseHandle(snap);
    }
    // 부모 번호가 재사용된 남의 프로세스를 세지 않게 WebView2만 따라 내려간다.
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(p) = stack.pop() {
        for &(c, webview) in children.get(&p).into_iter().flatten() {
            if webview && c != root && !out.contains(&c) {
                out.push(c);
                stack.push(c);
            }
        }
    }
    out
}

pub fn report() -> MemoryReport {
    // SAFETY: 인자 없는 조회.
    let me = unsafe { GetCurrentProcessId() };
    let mut r = MemoryReport {
        main: proc_mem(me).unwrap_or_default(),
        ..MemoryReport::default()
    };
    for pid in webview_descendants(me) {
        if let Some(m) = proc_mem(pid) {
            r.webview.private += m.private;
            r.webview.working_set += m.working_set;
            r.webview_count += 1;
        }
    }
    r
}

/// 앱 메모리(점검 화면이 회차마다 부른다).
#[tauri::command]
pub fn perf_memory() -> MemoryReport {
    report()
}

/// WebView2 그리기(`visible`, 주지 않으면 그대로)와 메모리 목표(`low`: MemoryUsageTargetLevel Low, 캐시를 비우고
/// 힙을 줄인다). 스크립트는 계속 돌아 새 메일 알림·목록 갱신은 그대로다.
fn apply(w: &tauri::WebviewWindow, visible: Option<bool>, low: bool) {
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_LOW, COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_NORMAL,
        ICoreWebView2_19,
    };
    use windows_core::Interface;
    let _ = w.with_webview(move |pw| {
        let c = pw.controller();
        let level = if low {
            COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_LOW
        } else {
            COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_NORMAL
        };
        // SAFETY: WebView2 COM 호출. with_webview가 창의 스레드에서 부른다.
        unsafe {
            if let Some(v) = visible {
                let _ = c.SetIsVisible(v);
            }
            if let Ok(v19) = c.CoreWebView2().and_then(|v| v.cast::<ICoreWebView2_19>()) {
                let _ = v19.SetMemoryUsageTargetLevel(level);
            }
        }
    });
}

/// 알림 영역으로 숨기면 그리기를 멈추고 메모리 목표를 낮춘다. 보이면(notify::show_main) 바로 되돌린다.
pub fn set_hidden(w: &tauri::WebviewWindow, hidden: bool) {
    apply(w, Some(!hidden), hidden);
}

/// 최소화했거나 오래 다른 프로그램을 쓰면 메모리 목표만 낮춘다(되돌리기가 1초 늦어도 화면이 비지 않게).
pub fn set_low_memory(w: &tauri::WebviewWindow, low: bool) {
    apply(w, None, low);
}

/// `--mem-test`에서만 WebView2에 넘기는 인자: JS GC를 부를 수 있게(`window.gc`, 없으면 JS 힙이 GC 때마다 크게
/// 오르내려 누수를 가리기 어렵다). 같은 프로필을 쓰는 창은 인자가 같아야 열리므로 앱 창을 만드는 곳마다 쓴다.
/// 앞부분은 wry 기본값 그대로.
/// 개발용: 환경 변수 `EO_WEBVIEW_DEBUG_PORT`가 있으면 WebView2 원격 디버깅(127.0.0.1)을 연다.
pub fn test_browser_args() -> Option<String> {
    let mut extra = String::new();
    if crate::commands::MEM_TEST.load(std::sync::atomic::Ordering::Relaxed) {
        extra.push_str(" --js-flags=--expose-gc");
    }
    if let Some(port) = std::env::var("EO_WEBVIEW_DEBUG_PORT")
        .ok()
        .and_then(|p| p.trim().parse::<u16>().ok())
    {
        extra.push_str(&format!(" --remote-debugging-port={port}"));
    }
    (!extra.is_empty())
        .then(|| format!("--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection{extra}"))
}

/// `--mem-test` 전용: 주 창을 숨기거나(`hidden`) 최소화한 것처럼(`low`), 아니면 되돌린다. 창은 보이게 둔다.
#[tauri::command]
pub fn perf_background(
    window: tauri::WebviewWindow,
    state: tauri::State<'_, crate::state::AppState>,
    mode: String,
) {
    if !state.perf_test {
        return;
    }
    match mode.as_str() {
        "hidden" => set_hidden(&window, true),
        "low" => set_low_memory(&window, true),
        _ => set_hidden(&window, false),
    }
}
