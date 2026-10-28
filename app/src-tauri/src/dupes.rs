//! 도구 → 중복 메일(0.9.0 사용자 요청). 제목·본문·보낸 사람·받는 사람·참조가 같은 메일(`eo_sync::exact_duplicates`,
//! 받은 시각은 보지 않음)을 찾아 목록으로 보여 주고, 완전 삭제(로컬 + 서버)한다. 묶음마다 받은 시각이 가장 이른 한
//! 통을 남긴다(시각이 같으면 서버에 있는 것, 그다음 먼저 들어온 것). 화면에서 "제외"한 묶음은 지우지 않고, 제외는
//! 저장해 두어 "제외 취소"할 때까지 다시 찾아도 남는다(1.0.5). 한 묶음을 모두 지우지는 않는다.
//!
//! 0.7.0~0.8.4의 "HCL Domino 중복 전달 메일 자동 삭제"와 0.7.0~0.8.0의 도구 "중복 메일 찾기"는 0.9.0에서 뺐다. 중복
//! 메일 자동 삭제는 만들지 않는다(사용자 결정).

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use eo_store::DupStage;
use eo_sync::BodyCache;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

use crate::actions::ActionResult;
use crate::state::{AppState, lock};

type CmdResult<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// 본문 비교값 캐시(다시 찾을 때 같은 원문을 다시 읽지 않게, 사용자 데이터에서 처음 약 2분 → 다시 1초 안).
static CACHE: LazyLock<Mutex<BodyCache>> = LazyLock::new(|| Mutex::new(BodyCache::new()));
/// 찾는 중(하나만).
static BUSY: AtomicBool = AtomicBool::new(false);

/// 찾는 동안 `BUSY`를 쥔다.
struct Busy;

impl Busy {
    fn take() -> Option<Self> {
        (!BUSY.swap(true, Ordering::SeqCst)).then_some(Self)
    }
}

impl Drop for Busy {
    fn drop(&mut self) {
        BUSY.store(false, Ordering::SeqCst);
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DupProgressView {
    /// "read": 메일 목록 읽기, "body": 본문 비교.
    stage: &'static str,
    done: usize,
    total: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DupMail {
    pub id: i64,
    pub folder_id: i64,
    pub subject: String,
    pub from: String,
    pub date: i64,
    pub size: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DupGroup {
    /// 묶음 열쇠(다시 찾아도 같다, `eo_sync::DupSet::key`).
    pub key: String,
    /// 첫 메일이 남길 메일, 나머지는 받은 시각 순.
    pub mails: Vec<DupMail>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DupFound {
    pub groups: Vec<DupGroup>,
    /// 저장해 둔 제외 묶음 중 이번에 찾은 것(열쇠).
    pub excluded: Vec<String>,
    pub scanned: usize,
    pub without_body: usize,
}

/// 제외한 묶음 열쇠(meta, 줄마다 하나). 사용자가 "제외 취소"할 때까지 남는다(1.0.5 사용자 요청).
const EXCLUDED_META: &str = "dupes:excluded";

fn load_excluded(st: &AppState) -> HashSet<String> {
    st.store
        .meta(EXCLUDED_META)
        .ok()
        .flatten()
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect()
}

/// 고른 묶음들(`keys`)을 제외하거나(`excluded`) 제외를 취소한다. 저장해 두어 앱을 다시 켜도 그대로 보인다.
#[tauri::command]
pub async fn duplicates_exclude(
    app: AppHandle,
    keys: Vec<String>,
    excluded: bool,
) -> CmdResult<()> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let mut saved = load_excluded(&st);
        let mut changed = false;
        for key in keys
            .into_iter()
            .filter(|k| !k.is_empty() && !k.contains('\n'))
        {
            changed |= if excluded {
                saved.insert(key)
            } else {
                saved.remove(&key)
            };
        }
        if changed {
            let mut list: Vec<String> = saved.into_iter().collect();
            list.sort();
            st.store
                .set_meta(EXCLUDED_META, &list.join("\n"))
                .map_err(err)?;
        }
        Ok(())
    })
    .await
    .map_err(err)?
}

/// 찾기. 진행은 `dupes-progress` 알림으로.
#[tauri::command]
pub async fn duplicates_find(app: AppHandle) -> CmdResult<DupFound> {
    tauri::async_runtime::spawn_blocking(move || {
        let _busy = Busy::take().ok_or("중복 메일을 찾는 중입니다.")?;
        let st = app.state::<AppState>();
        let mut last = Instant::now();
        let r = {
            let mut cache = lock(&CACHE);
            eo_sync::exact_duplicates(&st.store, &mut cache, &mut |stage, done, total| {
                if done == 0 || done == total || last.elapsed() >= Duration::from_millis(150) {
                    last = Instant::now();
                    let stage = match stage {
                        DupStage::Read => "read",
                        DupStage::Body => "body",
                    };
                    let _ = app.emit("dupes-progress", DupProgressView { stage, done, total });
                }
            })
            .map_err(err)?
        };
        let mut groups = Vec::with_capacity(r.sets.len());
        for set in &r.sets {
            let mut mails = Vec::with_capacity(set.drop.len() + 1);
            for &id in std::iter::once(&set.keep).chain(&set.drop) {
                let Some(m) = st.store.message(id).map_err(err)? else {
                    continue;
                };
                mails.push(DupMail {
                    id,
                    folder_id: m.folder_id,
                    subject: m.props.subject.unwrap_or_default(),
                    from: m
                        .props
                        .from_name
                        .filter(|n| !n.trim().is_empty())
                        .or(m.props.from_addr)
                        .unwrap_or_default(),
                    date: m.props.date_received,
                    size: m.props.size.unwrap_or(0),
                });
            }
            if mails.len() > 1 {
                groups.push(DupGroup {
                    key: set.key.clone(),
                    mails,
                });
            }
        }
        let saved = load_excluded(&st);
        let excluded: Vec<String> = groups
            .iter()
            .filter(|g| saved.contains(&g.key))
            .map(|g| g.key.clone())
            .collect();
        crate::applog::info(format!(
            "중복 메일 찾기: {}통 중 {}묶음(제외 {}), 지울 후보 {}통, 원문 없어 비교 못함 {}통",
            r.scanned,
            groups.len(),
            excluded.len(),
            groups.iter().map(|g| g.mails.len() - 1).sum::<usize>(),
            r.without_body
        ));
        Ok(DupFound {
            groups,
            excluded,
            scanned: r.scanned,
            without_body: r.without_body,
        })
    })
    .await
    .map_err(err)?
}

/// 완전 삭제할 묶음: 묶음의 모든 메일과 그중 지울 것.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DupChoice {
    pub ids: Vec<i64>,
    pub drop: Vec<i64>,
}

/// 묶음마다 한 통은 남는지 보고 지울 메일을 모은다.
fn chosen(groups: &[DupChoice]) -> CmdResult<Vec<i64>> {
    let mut out = Vec::new();
    for g in groups {
        let all: HashSet<i64> = g.ids.iter().copied().collect();
        let drop: HashSet<i64> = g.drop.iter().copied().collect();
        if !drop.is_subset(&all) {
            return Err("목록에 없는 메일은 지울 수 없습니다.".into());
        }
        if drop.len() >= all.len() {
            return Err("중복 묶음마다 한 통은 남겨야 합니다.".into());
        }
        out.extend(g.drop.iter().copied());
    }
    Ok(out)
}

/// 완전 삭제(로컬 + 서버). 화면에서 이중 확인을 받은 뒤에만 부른다.
#[tauri::command]
pub async fn duplicates_purge(app: AppHandle, groups: Vec<DupChoice>) -> CmdResult<ActionResult> {
    let ids = chosen(&groups)?;
    crate::applog::info(format!("중복 메일 완전 삭제: {}통", ids.len()));
    crate::actions::purge_messages(app, ids).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_group_keeps_one() {
        let g = |ids: &[i64], drop: &[i64]| DupChoice {
            ids: ids.to_vec(),
            drop: drop.to_vec(),
        };
        assert_eq!(
            chosen(&[g(&[1, 2, 3], &[2, 3]), g(&[4, 5], &[5])]).unwrap(),
            vec![2, 3, 5]
        );
        assert!(
            chosen(&[g(&[1, 2], &[1, 2])]).is_err(),
            "모두 지울 수는 없다"
        );
        assert!(chosen(&[g(&[1, 2], &[9])]).is_err());
    }
}
