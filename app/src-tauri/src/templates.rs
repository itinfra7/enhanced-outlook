//! 내 템플릿(0.7.0, 웹메일 작성 화면의 "내 템플릿"과 같은 기능): 자주 쓰는 문구·그림을 저장해 두고 작성 중에
//! 넣는다. 웹메일 추가 기능에 저장된 템플릿은 읽을 수 없어 앱 안에 따로 둔다(`EnhancedOutlook\templates.json`).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::state::AppState;

type CmdResult<T> = Result<T, String>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Template {
    pub name: String,
    /// 넣을 내용(작성 화면 HTML).
    pub html: String,
}

fn path(root: &Path) -> PathBuf {
    root.join("templates.json")
}

pub fn load(root: &Path) -> Vec<Template> {
    std::fs::read(path(root))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn save(root: &Path, list: &[Template]) -> std::io::Result<()> {
    let body = serde_json::to_vec_pretty(list).map_err(std::io::Error::other)?;
    // 쓰다 끊겨도 앞 내용이 남게 임시 파일에 쓰고 바꾼다.
    let tmp = root.join("templates.json.tmp");
    std::fs::write(&tmp, body)?;
    std::fs::rename(tmp, path(root))
}

#[tauri::command]
pub fn templates_list(app: AppHandle) -> Vec<Template> {
    load(&app.state::<AppState>().root)
}

/// 목록을 통째로 저장한다(넣기·지우기·이름 바꾸기 모두 화면이 고친 목록으로).
#[tauri::command]
pub fn templates_set(app: AppHandle, list: Vec<Template>) -> CmdResult<()> {
    let list: Vec<Template> = list
        .into_iter()
        .map(|t| Template {
            name: t.name.trim().to_owned(),
            html: t.html,
        })
        .filter(|t| !t.name.is_empty())
        .collect();
    save(&app.state::<AppState>().root, &list)
        .map_err(|e| format!("템플릿을 저장하지 못했습니다: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_and_load() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load(dir.path()).is_empty(), "없으면 빈 목록");
        let list = vec![Template {
            name: "인사".into(),
            html: "<p>안녕하세요</p>".into(),
        }];
        save(dir.path(), &list).unwrap();
        assert_eq!(load(dir.path()), list);
    }
}
