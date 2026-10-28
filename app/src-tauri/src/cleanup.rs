//! 받은 편지함 자동 정리(사용자 요청 2026-10-20): 설정의 기준에 맞는 받은 편지함 메일을 **서버에서만** 지운다.
//! 지우는 방식은 "서버에서 삭제"와 같다(SoftDelete, 로컬은 그 자리에 보관으로 남는다). 로컬 메일은 지우지 않는다.
//!
//! - 기준: n일 지난 메일, 또는 메일함 사용량이 n% 이상이면 오래된 메일부터 n% 아래가 될 만큼.
//! - 원문이 로컬에 없는 메일은 지우지 않는다(로컬 사본이 온전하지 않으므로).
//! - 로그인해 동기화 주기가 끝날 때, 30분에 한 번까지. 서버 반영 대기가 남아 있으면 다음으로 미룬다.
//! - 한 번에 1,000통까지.
//! - 용량 기준은 매번 사용량을 새로 받는다. 지난번에 지운 만큼(절반 이상) 사용량이 줄지 않았으면 멈춘다:
//!   서버에서 지운 메일이 용량에서 빠지지 않는 서버라면 계속 더 지우게 되므로.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use tauri::{AppHandle, Manager};

use crate::actions::after_local;
use crate::applog;
use crate::commands::{now_ms, refresh_user_config, update_status};
use crate::settings::{Cleanup, CleanupMode};
use crate::state::{AppState, lock};

const EVERY: Duration = Duration::from_secs(30 * 60);
const MAX_PER_RUN: usize = 1000;
const DAY_MS: i64 = 86_400_000;

/// 고른 결과.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Picked {
    pub ids: Vec<i64>,
    pub bytes: i64,
    /// 기준에 맞지만 원문이 없어 남긴 메일 수.
    pub without_eml: usize,
}

/// 받은 편지함 메일(`(ID, 크기, 원문 있음, 받은 시각 ms)`, 오래된 것부터)에서 지울 것을 고른다.
/// 용량 기준은 `quota`(사용량, 한도)를 모르면 아무것도 고르지 않는다.
pub fn pick(
    rows: &[(i64, i64, bool, i64)],
    c: &Cleanup,
    now_ms: i64,
    quota: Option<(i64, i64)>,
    max: usize,
) -> Picked {
    let mut p = Picked::default();
    match c.mode {
        CleanupMode::Off => {}
        CleanupMode::Age => {
            let cutoff = now_ms - i64::from(c.days) * DAY_MS;
            for &(id, size, has_eml, date) in rows {
                if date >= cutoff || p.ids.len() >= max {
                    break;
                }
                if has_eml {
                    p.ids.push(id);
                    p.bytes += size;
                } else {
                    p.without_eml += 1;
                }
            }
        }
        CleanupMode::Quota => {
            let Some((used, limit)) = quota.filter(|(_, l)| *l > 0) else {
                return p;
            };
            let goal = limit * i64::from(c.percent) / 100;
            if used < goal {
                return p;
            }
            // n% 아래가 될 만큼.
            let need = used - goal + 1;
            for &(id, size, has_eml, _) in rows {
                if p.bytes >= need || p.ids.len() >= max {
                    break;
                }
                if has_eml {
                    p.ids.push(id);
                    p.bytes += size;
                } else {
                    p.without_eml += 1;
                }
            }
        }
    }
    p
}

/// 지금 기준으로 고르기(설정 화면의 대상 수, 실행). 받은 편지함이 없으면(로그인 전) 빈 결과.
pub fn targets(st: &AppState, c: &Cleanup, max: usize) -> Result<Picked, String> {
    targets_at(st, c, max, now_ms())
}

/// [`targets`]를 `now_ms` 시각 기준으로(화면 자체 점검이 며칠 뒤를 가정할 때).
pub fn targets_at(st: &AppState, c: &Cleanup, max: usize, now_ms: i64) -> Result<Picked, String> {
    let Some(inbox) = st
        .store
        .folder_by_well_known("inbox")
        .map_err(|e| e.to_string())?
    else {
        return Ok(Picked::default());
    };
    let rows = st
        .store
        .oldest_on_server_in(inbox.id)
        .map_err(|e| e.to_string())?;
    let quota = {
        let s = lock(&st.status);
        s.quota_used.zip(s.quota_limit)
    };
    Ok(pick(&rows, c, now_ms, quota, max))
}

#[derive(Default)]
struct Runner {
    running: bool,
    last: Option<Instant>,
    /// 용량 기준으로 지난번에 지운 것: (지우기 전 사용량, 지운 크기).
    last_quota: Option<(i64, i64)>,
    /// 지운 만큼 사용량이 줄지 않아 멈춤(설정을 다시 저장하면 풀린다).
    stopped: bool,
}

static RUNNER: Mutex<Runner> = Mutex::new(Runner {
    running: false,
    last: None,
    last_quota: None,
    stopped: false,
});

/// 설정이 바뀌면 다음 주기에 바로 다시 본다(멈춤도 푼다).
pub fn reset() {
    let mut r = lock(&RUNNER);
    r.last = None;
    r.last_quota = None;
    r.stopped = false;
}

/// 동기화 주기가 끝날 때 부른다. 때가 되었으면 뒤에서 정리한다.
pub fn maybe_run(app: &AppHandle) {
    let st = app.state::<AppState>();
    let c = lock(&st.settings).cleanup;
    if c.mode == CleanupMode::Off {
        return;
    }
    {
        let s = lock(&st.status);
        if !s.logged_in || s.pending_ops > 0 {
            return;
        }
    }
    {
        let mut r = lock(&RUNNER);
        if r.running || r.stopped || r.last.is_some_and(|t| t.elapsed() < EVERY) {
            return;
        }
        r.running = true;
        r.last = Some(Instant::now());
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        run(&app, c).await;
        lock(&RUNNER).running = false;
    });
}

async fn run(app: &AppHandle, c: Cleanup) {
    if c.mode == CleanupMode::Quota {
        if !refresh_user_config(app.clone()).await {
            return;
        }
        let st = app.state::<AppState>();
        let used = lock(&st.status).quota_used;
        let last = lock(&RUNNER).last_quota.take();
        if let (Some(used), Some((before, deleted))) = (used, last)
            && before - used < deleted / 2
        {
            lock(&RUNNER).stopped = true;
            applog::warn(format!(
                "자동 정리 멈춤: 서버에서 {} 지웠는데 메일함 사용량이 {} 줄었음",
                mb(deleted),
                mb(before - used)
            ));
            update_status(app, |s| {
                s.last_error = Some("자동 정리 멈춤: 지운 만큼 메일함 용량이 줄지 않음".into())
            });
            return;
        }
    }
    let app2 = app.clone();
    let done = tauri::async_runtime::spawn_blocking(move || {
        let st = app2.state::<AppState>();
        let p = targets(&st, &c, MAX_PER_RUN)?;
        if p.ids.is_empty() {
            return Ok::<_, String>(None);
        }
        let used = lock(&st.status).quota_used;
        let batch = st.store.new_batch().map_err(|e| e.to_string())?;
        let t = st
            .store
            .server_delete(&p.ids, now_ms(), Some(batch))
            .map_err(|e| e.to_string())?;
        Ok(Some((p, t, used)))
    })
    .await;
    match done {
        Ok(Ok(Some((p, t, used)))) => {
            if c.mode == CleanupMode::Quota
                && let Some(u) = used
            {
                lock(&RUNNER).last_quota = Some((u, p.bytes));
            }
            applog::info(format!(
                "자동 정리: 받은 편지함 {}통({}) 서버에서 삭제 · 기준 {}{}",
                p.ids.len(),
                mb(p.bytes),
                match c.mode {
                    CleanupMode::Age => format!("{}일 지남", c.days),
                    _ => format!("용량 {}% 이상", c.percent),
                },
                if p.without_eml > 0 {
                    format!(" · 원문 없어 남김 {}통", p.without_eml)
                } else {
                    String::new()
                }
            ));
            after_local(app, &t);
        }
        Ok(Ok(None)) => {}
        Ok(Err(e)) => applog::warn(format!("자동 정리 실패: {e}")),
        Err(e) => applog::warn(format!("자동 정리 실패: {e}")),
    }
}

fn mb(bytes: i64) -> String {
    format!("{:.1}MB", bytes as f64 / 1_048_576.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000_000;

    /// 오래된 것부터: 40일·35일(원문 없음)·31일·29일·1일 전, 크기 10·20·30·40·50.
    fn rows() -> Vec<(i64, i64, bool, i64)> {
        [
            (1, 10, true, 40),
            (2, 20, false, 35),
            (3, 30, true, 31),
            (4, 40, true, 29),
            (5, 50, true, 1),
        ]
        .into_iter()
        .map(|(id, size, eml, days)| (id, size, eml, NOW - days * DAY_MS))
        .collect()
    }

    fn c(mode: CleanupMode, days: u32, percent: u32) -> Cleanup {
        Cleanup {
            mode,
            days,
            percent,
        }
    }

    #[test]
    fn age_picks_older_than_days_with_eml_only() {
        let p = pick(&rows(), &c(CleanupMode::Age, 30, 90), NOW, None, 1000);
        assert_eq!((p.ids, p.bytes, p.without_eml), (vec![1, 3], 40, 1));
        let p = pick(&rows(), &c(CleanupMode::Age, 60, 90), NOW, None, 1000);
        assert!(p.ids.is_empty());
        let p = pick(&rows(), &c(CleanupMode::Age, 30, 90), NOW, None, 1);
        assert_eq!(p.ids, [1], "한 번에 정한 수까지");
        assert!(
            pick(
                &rows(),
                &c(CleanupMode::Off, 1, 1),
                NOW,
                Some((99, 100)),
                1000
            )
            .ids
            .is_empty()
        );
    }

    #[test]
    fn quota_picks_oldest_until_below_percent() {
        // 한도 1000, 90% = 900. 사용량 950 → 51 이상 줄여야: 10 + 30 + 40 = 80(원문 없는 20은 건너뜀).
        let p = pick(
            &rows(),
            &c(CleanupMode::Quota, 30, 90),
            NOW,
            Some((950, 1000)),
            1000,
        );
        assert_eq!((p.ids, p.bytes, p.without_eml), (vec![1, 3, 4], 80, 1));
        // 정확히 90%도 "이상"이라 지운다(1바이트라도 아래로).
        let p = pick(
            &rows(),
            &c(CleanupMode::Quota, 30, 90),
            NOW,
            Some((900, 1000)),
            1000,
        );
        assert_eq!(p.ids, [1]);
        // 아래면 그대로, 용량을 모르면 아무것도 안 한다.
        assert!(
            pick(
                &rows(),
                &c(CleanupMode::Quota, 30, 90),
                NOW,
                Some((899, 1000)),
                1000
            )
            .ids
            .is_empty()
        );
        assert!(
            pick(&rows(), &c(CleanupMode::Quota, 30, 90), NOW, None, 1000)
                .ids
                .is_empty()
        );
        assert!(
            pick(
                &rows(),
                &c(CleanupMode::Quota, 30, 90),
                NOW,
                Some((5, 0)),
                1000
            )
            .ids
            .is_empty()
        );
    }
}
