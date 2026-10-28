//! `owa-cli watch`: 유휴 시간을 늘려 가며 세션 만료 시점과 응답 형태를 잰다(계획서 11장 #2).
//! 결과는 화면과 실행 파일 폴더의 `EnhancedOutlook\dev\watch.log`에 남긴다(비밀 값 없음).

use std::io::Write;
use std::time::{Duration, Instant};

use anyhow::Result;
use eo_owa::{Client, FolderRef};

use crate::util::{dev_dir, ms, now_unix};
use crate::verify::describe_event;

fn log(line: &str) {
    println!("{line}");
    let dir = dev_dir();
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("watch.log"))
    {
        let _ = writeln!(f, "[{}] {line}", now_unix());
    }
}

pub async fn run(client: &Client, gaps_min: &[u64]) -> Result<()> {
    let total: u64 = gaps_min.iter().sum();
    log(&format!(
        "watch 시작: 유휴 간격 {gaps_min:?}분, 총 약 {}시간 {}분. PC가 절전 모드로 들어가지 않게 해 주세요.",
        total / 60,
        total % 60
    ));
    let inbox = FolderRef::distinguished("inbox");
    for &gap in gaps_min {
        log(&format!("{gap}분 유휴 대기…"));
        tokio::time::sleep(Duration::from_secs(gap * 60)).await;
        let before = client.events().len();
        let started = Instant::now();
        let result = client.get_folder(&inbox).await;
        let events: Vec<String> = client
            .events()
            .iter()
            .skip(before)
            .map(describe_event)
            .collect();
        let state = if events.is_empty() {
            "세션 유지".to_owned()
        } else {
            events.join(" → ")
        };
        match result {
            Ok(_) => log(&format!(
                "유휴 {gap}분 후 호출 성공 ({}ms): {state}",
                ms(started.elapsed())
            )),
            Err(e) => log(&format!("유휴 {gap}분 후 호출 실패: {e} ({state})")),
        }
    }
    log("watch 끝");
    Ok(())
}
