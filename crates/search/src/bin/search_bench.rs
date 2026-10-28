//! 검색 벤치마크(계획서 8.4, 3단계 수용 기준).
//!
//! `search-bench [--docs 100000] [--dir target/search-bench] [--rebuild]`
//! 1. 합성 말뭉치 색인(없거나 --rebuild일 때)
//! 2. 정확성: 생성기로 전 메일을 다시 만들어 "보고서", 이메일 주소 일부, 숨은참조 필드 질의 결과를 전수 비교
//! 3. 성능: 이 프로세스에서 처음 연 색인으로 질의 200종(첫 질의 = 콜드 포함) p50/p95/최대
//!
//! 수용: p95 ≤ 100ms, 최대 ≤ 1000ms, 정확성 전부 통과. 아니면 종료 코드 1.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use eo_search::synth::{self, PLANTED_BCC};
use eo_search::{MailDoc, SearchIndex, SearchOptions};

struct Args {
    docs: u64,
    dir: PathBuf,
    rebuild: bool,
}

fn args() -> Args {
    let mut a = Args {
        docs: 100_000,
        dir: PathBuf::from("target/search-bench"),
        rebuild: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(x) = it.next() {
        match x.as_str() {
            "--docs" => a.docs = it.next().and_then(|v| v.parse().ok()).unwrap_or(a.docs),
            "--dir" => a.dir = it.next().map(PathBuf::from).unwrap_or(a.dir),
            "--rebuild" => a.rebuild = true,
            _ => {}
        }
    }
    a
}

fn dir_size(p: &std::path::Path) -> u64 {
    std::fs::read_dir(p)
        .map(|rd| {
            rd.flatten()
                .map(|e| {
                    let m = e.metadata().ok();
                    match m {
                        Some(m) if m.is_dir() => dir_size(&e.path()),
                        Some(m) => m.len(),
                        None => 0,
                    }
                })
                .sum()
        })
        .unwrap_or(0)
}

fn build(a: &Args) -> eo_search::Result<()> {
    if a.rebuild && a.dir.exists() {
        std::fs::remove_dir_all(&a.dir)?;
    }
    let ix = SearchIndex::open(&a.dir)?;
    if ix.num_docs() == a.docs {
        println!("색인 재사용: {}통 ({})", a.docs, a.dir.display());
        return Ok(());
    }
    if ix.num_docs() > 0 {
        drop(ix);
        std::fs::remove_dir_all(&a.dir)?;
        return build(a);
    }
    let started = Instant::now();
    let mut text_bytes = 0u64;
    let mut batch = Vec::with_capacity(1000);
    for i in 0..a.docs {
        let d = synth::doc(i, a.docs);
        text_bytes += d.body.len() as u64;
        batch.push(d);
        if batch.len() == 1000 {
            ix.add(&batch)?;
            batch.clear();
        }
        if (i + 1) % 20_000 == 0 {
            eprintln!("  색인 중 {}/{}", i + 1, a.docs);
        }
    }
    ix.add(&batch)?;
    ix.commit()?;
    println!(
        "색인 생성: {}통, 본문 평균 {}B, {:.1}초, 색인 크기 {:.0}MB",
        a.docs,
        text_bytes / a.docs.max(1),
        started.elapsed().as_secs_f64(),
        dir_size(&a.dir) as f64 / 1_048_576.0
    );
    Ok(())
}

/// 정답: CJK 질의는 부분 문자열, ASCII 단어는 영숫자 단어 경계로 일치.
fn ascii_words(s: &str) -> impl Iterator<Item = String> + '_ {
    s.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_ascii_lowercase)
}

fn all_text(d: &MailDoc) -> [&str; 7] {
    [
        &d.subject, &d.body, &d.from, &d.to, &d.cc, &d.bcc, &d.attach,
    ]
}

struct Check {
    name: &'static str,
    query: &'static str,
    truth: fn(&MailDoc) -> bool,
}

const CHECKS: &[Check] = &[
    Check {
        name: "\"보고서\" (모든 필드, 한국어 바이그램 구문)",
        query: "보고서",
        truth: |d| all_text(d).iter().any(|t| t.contains("보고서")),
    },
    Check {
        name: "이메일 주소 일부: 도메인 \"example\"",
        query: "example",
        truth: |d| {
            all_text(d)
                .iter()
                .any(|t| ascii_words(t).any(|w| w == "example"))
        },
    },
    Check {
        name: "이메일 주소 일부: 로컬 \"minsu.kim\"",
        query: "minsu.kim",
        truth: |d| {
            all_text(d)
                .iter()
                .any(|t| t.to_ascii_lowercase().contains("minsu.kim@"))
        },
    },
    Check {
        name: "숨은참조 필드: \"숨은참조:audit.bcc\"",
        query: "숨은참조:audit.bcc",
        truth: |d| d.bcc.contains(PLANTED_BCC),
    },
    Check {
        name: "숨은참조 주소는 받는이에 없음: \"받는이:audit.bcc\"",
        query: "받는이:audit.bcc",
        truth: |_| false,
    },
    Check {
        name: "AND + 필드: \"제목:월간보고서 보안\"",
        query: "제목:월간보고서 보안",
        truth: |d| {
            d.subject.contains("월간보고서") && all_text(d).iter().any(|t| t.contains("보안"))
        },
    },
];

fn correctness(ix: &SearchIndex, n: u64) -> eo_search::Result<bool> {
    let docs: Vec<MailDoc> = (0..n).map(|i| synth::doc(i, n)).collect();
    let mut ok = true;
    for c in CHECKS {
        let truth: BTreeSet<u64> = docs.iter().filter(|d| (c.truth)(d)).map(|d| d.id).collect();
        let opts = SearchOptions {
            limit: n as usize + 1,
            ..SearchOptions::default()
        };
        let r = ix.search(c.query, &opts)?;
        let got: BTreeSet<u64> = r.ids.iter().copied().collect();
        let missing = truth.difference(&got).count();
        let extra = got.difference(&truth).count();
        let pass = missing == 0 && extra == 0 && r.total == truth.len();
        ok &= pass;
        println!(
            "  {} {}: 정답 {}통, 결과 {}통 (빠짐 {missing}, 섞임 {extra})",
            if pass { "✓" } else { "✗" },
            c.name,
            truth.len(),
            r.total
        );
    }
    Ok(ok)
}

fn percentile(sorted: &[Duration], p: f64) -> Duration {
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

fn main() -> ExitCode {
    let a = args();
    if let Err(e) = build(&a) {
        eprintln!("색인 생성 실패: {e}");
        return ExitCode::FAILURE;
    }

    // 이 프로세스에서 처음 여는 색인으로 잰다(첫 질의가 콜드).
    let open_started = Instant::now();
    let ix = match SearchIndex::open(&a.dir) {
        Ok(ix) => ix,
        Err(e) => {
            eprintln!("색인 열기 실패: {e}");
            return ExitCode::FAILURE;
        }
    };
    let open_ms = open_started.elapsed();

    let queries = synth::queries(200);
    let mut times = Vec::with_capacity(queries.len());
    let mut hits = 0usize;
    for q in &queries {
        let started = Instant::now();
        match ix.search(q, &SearchOptions::default()) {
            Ok(r) => hits += r.total,
            Err(e) => {
                eprintln!("질의 실패 {q}: {e}");
                return ExitCode::FAILURE;
            }
        }
        times.push(started.elapsed());
    }
    let cold = times[0];
    let mut sorted = times.clone();
    sorted.sort();
    let (p50, p95, max) = (
        percentile(&sorted, 0.5),
        percentile(&sorted, 0.95),
        sorted[sorted.len() - 1],
    );
    let slowest = times
        .iter()
        .zip(&queries)
        .max_by_key(|(t, _)| **t)
        .map(|(_, q)| q.as_str())
        .unwrap_or("");
    println!(
        "성능: 색인 {}통, 열기 {:.1}ms, 질의 {}종(최신순 200행 + 전체 건수)",
        ix.num_docs(),
        open_ms.as_secs_f64() * 1000.0,
        queries.len()
    );
    println!(
        "  첫 질의(콜드) {:.1}ms · p50 {:.1}ms · p95 {:.1}ms · 최대 {:.1}ms (가장 느린 질의: {slowest}) · 평균 일치 {}통",
        cold.as_secs_f64() * 1000.0,
        p50.as_secs_f64() * 1000.0,
        p95.as_secs_f64() * 1000.0,
        max.as_secs_f64() * 1000.0,
        hits / queries.len()
    );
    let perf_ok = p95 <= Duration::from_millis(100) && max <= Duration::from_millis(1000);

    println!("정확성(전수 비교):");
    let correct = match correctness(&ix, a.docs) {
        Ok(ok) => ok,
        Err(e) => {
            eprintln!("정확성 확인 실패: {e}");
            false
        }
    };
    println!(
        "수용 기준: 성능 {} (p95 ≤ 100ms, 최대 ≤ 1000ms), 정확성 {}",
        if perf_ok { "통과" } else { "실패" },
        if correct { "통과" } else { "실패" }
    );
    if perf_ok && correct {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
