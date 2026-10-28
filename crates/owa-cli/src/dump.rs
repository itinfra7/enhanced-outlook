//! 원문(EML) 받기: 묶음 × 동시 요청(계획서 2.2 기본값 동시 4, 묶음 4~8).

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use eo_owa::{ChangeKind, Client, ItemChange, ItemId};
use futures_util::StreamExt;
use futures_util::stream;
use serde::Serialize;

use crate::util::{date_stamp, fmt_bytes, mb_per_s, ms, short_hash};

#[derive(Clone)]
pub struct DumpItem {
    pub id: ItemId,
    pub received: Option<String>,
}

/// 전체 동기화 결과(생성 변경)에서 원문을 받을 항목을 고른다.
pub fn items_from_changes(changes: &[ItemChange]) -> Vec<DumpItem> {
    changes
        .iter()
        .filter(|c| matches!(c.kind, ChangeKind::Create | ChangeKind::Update))
        .filter_map(|c| {
            Some(DumpItem {
                id: c.item_id.clone()?,
                received: c.item.as_ref().and_then(|i| i.date_time_received.clone()),
            })
        })
        .collect()
}

#[derive(Debug, Default, Serialize)]
pub struct DumpStats {
    pub items: usize,
    pub files: usize,
    pub bytes: u64,
    pub elapsed_ms: u64,
    pub mb_per_s: f64,
    pub items_per_s: f64,
    /// 오류 코드별 건수.
    pub failures: BTreeMap<String, usize>,
    /// 머리글·본문 구분(빈 줄)이 없는 원문 수.
    pub no_header_end: usize,
    /// `Message-ID` 머리글이 없는 원문 수(서버 삭제 판정의 이동 짝짓기에 영향).
    pub no_message_id: usize,
}

impl DumpStats {
    fn finish(&mut self, elapsed: Duration) {
        self.elapsed_ms = ms(elapsed);
        self.mb_per_s = (mb_per_s(self.bytes, elapsed) * 10.0).round() / 10.0;
        let s = elapsed.as_secs_f64();
        self.items_per_s = if s > 0.0 {
            ((self.files as f64 / s) * 10.0).round() / 10.0
        } else {
            0.0
        };
    }

    pub fn summary(&self) -> String {
        let failed: usize = self.failures.values().sum();
        format!(
            "{}건 중 {}건, {} / {:.1}초 = {:.1}MB/s, {:.1}건/s, 실패 {}",
            self.items,
            self.files,
            fmt_bytes(self.bytes),
            self.elapsed_ms as f64 / 1000.0,
            self.mb_per_s,
            self.items_per_s,
            failed
        )
    }
}

fn check_eml(bytes: &[u8], stats: &mut DumpStats) {
    let header_end = find(bytes, b"\r\n\r\n").or_else(|| find(bytes, b"\n\n"));
    match header_end {
        None => stats.no_header_end += 1,
        Some(end) => {
            let head = String::from_utf8_lossy(&bytes[..end]).to_ascii_lowercase();
            if !head.starts_with("message-id:") && !head.contains("\nmessage-id:") {
                stats.no_message_id += 1;
            }
        }
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// 원문을 받아 `dir`에 `yyyymmdd_HHMMSS_해시.eml`로 쓴다. `dir`이 없으면 받기만 한다.
pub async fn fetch_items(
    client: &Client,
    items: &[DumpItem],
    dir: Option<&Path>,
    batch: usize,
    concurrency: usize,
    progress: bool,
) -> Result<DumpStats> {
    if let Some(d) = dir {
        std::fs::create_dir_all(d).with_context(|| format!("{} 만들기 실패", d.display()))?;
    }
    let started = Instant::now();
    let mut stats = DumpStats {
        items: items.len(),
        ..DumpStats::default()
    };
    let requests = items.chunks(batch.max(1)).map(|chunk| {
        let client = client.clone();
        let chunk = chunk.to_vec();
        async move {
            let ids: Vec<ItemId> = chunk.iter().map(|i| i.id.clone()).collect();
            let result = client.get_mime(&ids).await;
            (chunk, result)
        }
    });
    let mut results = stream::iter(requests).buffer_unordered(concurrency.max(1));
    let mut last_report = Instant::now();
    while let Some((chunk, result)) = results.next().await {
        match result {
            Ok(mimes) => {
                for (item, mime) in chunk.iter().zip(mimes) {
                    match mime {
                        Ok(bytes) => {
                            check_eml(&bytes, &mut stats);
                            if let Some(d) = dir {
                                write_unique(d, item, &bytes)?;
                            }
                            stats.files += 1;
                            stats.bytes += bytes.len() as u64;
                        }
                        Err(e) => *stats.failures.entry(e.code).or_default() += 1,
                    }
                }
            }
            Err(e) => *stats.failures.entry(format!("요청 실패: {e}")).or_default() += chunk.len(),
        }
        if progress && last_report.elapsed() >= Duration::from_secs(5) {
            last_report = Instant::now();
            eprintln!(
                "    … {}/{}건, {}",
                stats.files,
                stats.items,
                fmt_bytes(stats.bytes)
            );
        }
    }
    stats.finish(started.elapsed());
    Ok(stats)
}

fn write_unique(dir: &Path, item: &DumpItem, bytes: &[u8]) -> Result<()> {
    let base = format!(
        "{}_{}",
        date_stamp(item.received.as_deref()),
        short_hash(&item.id.id)
    );
    let mut path = dir.join(format!("{base}.eml"));
    let mut n = 1;
    while path.exists() {
        path = dir.join(format!("{base}_{n}.eml"));
        n += 1;
    }
    std::fs::write(&path, bytes).with_context(|| format!("{} 쓰기 실패", path.display()))
}
