//! 가져오기(계획서 10장 7단계): EML 폴더·ZIP, 유저스크립트 HTML 백업을 로컬 전용 메일로 넣는다.
//! 사용자 결정(2026-10-16): 같은 이름의 기존 폴더에 합친다. 서버에는 올리지 않는다.
//! 진행은 `import-progress` 이벤트, 끝나면 색인한 뒤 폴더·목록을 갱신한다.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::Instant;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_dialog::DialogExt;

use crate::commands::MessagesChanged;
use crate::state::AppState;

type CmdResult<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportProgress {
    pub done: usize,
    pub total: usize,
    pub imported: usize,
    pub duplicates: usize,
    pub skipped: usize,
    pub current: String,
    /// 가져오기가 끝나고 색인 중.
    pub indexing: bool,
    pub finished: bool,
    pub report: Option<ImportSummary>,
    pub error: Option<String>,
}

#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportSummary {
    pub total: usize,
    pub imported: usize,
    pub restored_html: usize,
    /// HCL Domino 백업에서 복원한 수.
    pub restored_domino: usize,
    pub duplicates: usize,
    pub skipped: usize,
    pub created_folders: Vec<String>,
    /// 폴더("상위/하위")별 가져온 통수.
    pub per_folder: Vec<(String, usize)>,
    /// 백업이 가리키는데 없던 파일 수, 아무도 안 가리켜 첨부로 붙인 파일 수.
    pub missing_files: usize,
    pub extra_files: usize,
    /// 첨부 칸에 이름만 있던 첨부(내려받지 않고 저장한 백업). 이름은 본문 끝에 남긴다.
    pub not_downloaded: usize,
    /// 로그인 전이라(서버 폴더 목록 없음) 넣지 않은 HCL Domino 아닌 메일 수.
    pub need_login: usize,
    pub errors: Vec<String>,
    pub cancelled: bool,
}

/// 가져올 것 고르기: 폴더 하나, 또는 ZIP·EML·HTML 파일 여러 개. 취소하면 빈 목록.
#[tauri::command]
pub async fn import_pick(app: AppHandle, folder: bool) -> CmdResult<Vec<String>> {
    tauri::async_runtime::spawn_blocking(move || {
        let d = app.dialog().file().set_title("가져올 백업 고르기");
        let picked: Vec<PathBuf> = if folder {
            d.blocking_pick_folder()
                .and_then(|p| p.into_path().ok())
                .into_iter()
                .collect()
        } else {
            d.add_filter(
                "메일 백업(ZIP, EML, HTML, PST, IMS)",
                &["zip", "eml", "html", "htm", "pst", "ims", "ost"],
            )
            .blocking_pick_files()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|p| p.into_path().ok())
            .collect()
        };
        picked.iter().map(|p| p.display().to_string()).collect()
    })
    .await
    .map_err(err)
}

/// 가져오기 시작(뒤에서). 이미 진행 중이면 거절.
#[tauri::command]
pub async fn import_start(app: AppHandle, paths: Vec<String>) -> CmdResult<()> {
    if paths.is_empty() {
        return Err("가져올 것을 고르세요".into());
    }
    let st = app.state::<AppState>();
    // 로그인 전(서버 폴더 목록 없음)에도 시작한다: HCL Domino 메일만 넣고 나머지는 need_login으로 센다.
    if st.import_running.swap(true, Ordering::SeqCst) {
        return Err("가져오기가 이미 진행 중입니다".into());
    }
    st.import_cancel.store(false, Ordering::Relaxed);
    let handle = app.clone();
    std::thread::spawn(move || {
        let st = handle.state::<AppState>();
        let paths: Vec<PathBuf> = paths.into_iter().map(PathBuf::from).collect();
        let mut last = Instant::now();
        let r = eo_sync::import_paths(&st.store, &paths, &st.import_cancel, |p| {
            if last.elapsed().as_millis() >= 200 {
                last = Instant::now();
                let _ = handle.emit(
                    "import-progress",
                    ImportProgress {
                        done: p.done,
                        total: p.total,
                        imported: p.imported,
                        duplicates: p.duplicates,
                        skipped: p.skipped,
                        current: p.current.clone(),
                        ..ImportProgress::default()
                    },
                );
            }
        });
        let fin = match r {
            Ok(r) => {
                crate::applog::info(format!(
                    "가져오기{}: 후보 {}통, 가져옴 {}(HTML 백업 {}, HCL Domino {}), 이미 있음 {}, 읽지 못함 {}, 새 폴더 {}개, 없던 파일 {}개, 첨부로 붙인 남은 파일 {}개, 내려받지 않은 첨부 {}개",
                    if r.cancelled { "(중지)" } else { "" },
                    r.total,
                    r.imported,
                    r.restored_html,
                    r.restored_domino,
                    r.duplicates,
                    r.skipped,
                    r.created_folders.len(),
                    r.missing_files,
                    r.extra_files,
                    r.not_downloaded
                ));
                let _ = handle.emit(
                    "import-progress",
                    ImportProgress {
                        done: r.total,
                        total: r.total,
                        imported: r.imported,
                        duplicates: r.duplicates,
                        skipped: r.skipped,
                        indexing: true,
                        ..ImportProgress::default()
                    },
                );
                let index_error = eo_sync::index_pending(&st.store, &st.search, None)
                    .err()
                    .map(|e| format!("가져왔지만 색인하지 못했습니다: {e}"));
                crate::commands::bump_list_gen();
                let _ = handle.emit("folders-changed", ());
                for folder_id in &r.folders {
                    let _ = handle.emit(
                        "messages-changed",
                        MessagesChanged {
                            folder_id: *folder_id,
                            ids: Vec::new(),
                        },
                    );
                }
                ImportProgress {
                    done: r.total,
                    total: r.total,
                    imported: r.imported,
                    duplicates: r.duplicates,
                    skipped: r.skipped,
                    finished: true,
                    error: index_error,
                    report: Some(ImportSummary {
                        total: r.total,
                        imported: r.imported,
                        restored_html: r.restored_html,
                        restored_domino: r.restored_domino,
                        duplicates: r.duplicates,
                        skipped: r.skipped,
                        created_folders: r.created_folders,
                        per_folder: r.per_folder,
                        missing_files: r.missing_files,
                        extra_files: r.extra_files,
                        not_downloaded: r.not_downloaded,
                        need_login: r.need_login,
                        errors: r.errors,
                        cancelled: r.cancelled,
                    }),
                    ..ImportProgress::default()
                }
            }
            Err(e) => {
                crate::applog::warn(format!("가져오기 실패: {e}"));
                ImportProgress {
                    finished: true,
                    error: Some(e.to_string()),
                    ..ImportProgress::default()
                }
            }
        };
        st.import_running.store(false, Ordering::SeqCst);
        let _ = handle.emit("import-progress", fin);
    });
    Ok(())
}

#[tauri::command]
pub fn import_cancel(app: AppHandle) {
    app.state::<AppState>()
        .import_cancel
        .store(true, Ordering::Relaxed);
}

/// `--ui-test` 전용: 유저스크립트 형식 HTML 백업 견본을 `EnhancedOutlook\dev\`에 만들고 경로를 돌려준다.
#[tauri::command]
pub async fn ui_test_sample_backup(app: AppHandle) -> CmdResult<String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        if !st.ui_test {
            return Err("점검 모드가 아닙니다".to_owned());
        }
        let dev = st.root.join("dev");
        std::fs::create_dir_all(&dev).map_err(err)?;
        let dest = dev.join("uitest-html-backup_01.zip");
        eo_sync::write_sample_html_backup(&dest).map_err(err)?;
        Ok(dest.display().to_string())
    })
    .await
    .map_err(err)?
}

/// `--ui-test` 전용: HCL Domino 백업 형식 견본 폴더를 `EnhancedOutlook\dev\uitest-domino\Domino`에 만들고 경로를 돌려준다.
/// 내용이 늘 같아서 다시 만들어도 된다.
#[tauri::command]
pub async fn ui_test_sample_domino(app: AppHandle) -> CmdResult<String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        if !st.ui_test {
            return Err("점검 모드가 아닙니다".to_owned());
        }
        let dest = st.root.join("dev").join("uitest-domino").join("Domino");
        eo_sync::write_sample_domino_backup(&dest).map_err(err)?;
        Ok(dest.display().to_string())
    })
    .await
    .map_err(err)?
}
