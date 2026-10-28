//! `owa-cli verify`: 1단계 읽기 전용 확인을 로그인 1회로 묶는다(계획서 10장 1단계, 11장).
//! 메일 제목·주소는 출력하지 않는다. 보고서는 JSON으로 덤프 폴더에 남긴다.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result};
use eo_owa::{
    Client, Error, FolderRef, LoginInfo, PostMode, SessionEvent, SessionEventKind, UserConfig,
    requests, responses,
};
use serde::Serialize;
use serde_json::Value;

use crate::dump::{self, DumpItem, DumpStats};
use crate::util::{
    dev_dir, fetch_hierarchy, fmt_bytes, item_sync, ms, now_unix, safe_name, short_hash,
};

#[derive(Default, Serialize)]
struct Report {
    started_at_unix: u64,
    base_url: String,
    login: Option<LoginReport>,
    user: Option<UserConfig>,
    hierarchy: Option<HierarchyReport>,
    folders: Vec<FolderReport>,
    post_modes: Vec<ModeReport>,
    sync_items_shape: Option<String>,
    dump: Option<DumpReport>,
    concurrency: Vec<ConcurrencyReport>,
    create_folder_probe: Option<String>,
    expiry: Option<ExpiryReport>,
    events: Vec<String>,
    errors: Vec<String>,
}

#[derive(Serialize)]
struct LoginReport {
    scheme: String,
    ms: u64,
    cookie_names: Vec<String>,
    has_canary: bool,
}

#[derive(Serialize)]
struct HierarchyReport {
    full_ms: u64,
    full_calls: usize,
    all_folders: usize,
    mail_folders: usize,
    change_types: BTreeMap<String, usize>,
    find_folder_ms: Option<u64>,
    find_folder_all: Option<usize>,
    find_folder_mail: Option<usize>,
    incremental_ms: Option<u64>,
    incremental_changes: Option<usize>,
}

#[derive(Serialize)]
struct FolderReport {
    path: String,
    total_count: Option<i64>,
    unread_count: Option<i64>,
    items: usize,
    full_ms: u64,
    full_calls: usize,
    change_types: BTreeMap<String, usize>,
    incremental_ms: Option<u64>,
    incremental_changes: Option<usize>,
    incremental_change_types: BTreeMap<String, usize>,
}

#[derive(Serialize)]
struct ModeResult {
    ok: bool,
    ms: u64,
    summary: String,
}

#[derive(Serialize)]
struct ModeReport {
    action: String,
    body: ModeResult,
    header: ModeResult,
    same_result: bool,
}

#[derive(Serialize)]
struct DumpReport {
    dir: String,
    total: DumpStats,
    per_folder: Vec<(String, DumpStats)>,
}

#[derive(Serialize)]
struct ConcurrencyReport {
    concurrency: usize,
    batch: usize,
    stats: DumpStats,
}

#[derive(Serialize)]
struct ExpiryReport {
    logoff: String,
    call_ok: bool,
    call_ms: u64,
    /// NTLM 연결 인증 횟수(끊기 전 → 호출 후).
    handshakes_before: u64,
    handshakes_after: u64,
    events: Vec<String>,
}

impl Report {
    fn error(&mut self, step: &str, e: impl std::fmt::Display) {
        let line = format!("{step}: {e}");
        println!("  ✗ {line}");
        self.errors.push(line);
    }
}

pub fn describe_event(e: &SessionEvent) -> String {
    match &e.kind {
        SessionEventKind::LoggedIn => "로그인".into(),
        SessionEventKind::SessionExpired { action, signal } => {
            format!("세션 만료 감지({action}): {signal}")
        }
        SessionEventKind::Relogin { ok } => {
            format!("재로그인 {}", if *ok { "성공" } else { "실패" })
        }
        SessionEventKind::CredentialsReprompted => "비밀번호 재입력 요청".into(),
        SessionEventKind::CanaryRetry { action } => format!("캐너리 오류(449) 재시도({action})"),
        SessionEventKind::PostModeFallback { action } => {
            format!("본문 방식 형식 오류 → 헤더 방식 재시도({action})")
        }
        SessionEventKind::LoggedOff => "로그오프".into(),
    }
}

fn count_types<'a>(names: impl Iterator<Item = &'a str>) -> BTreeMap<String, usize> {
    let mut m = BTreeMap::new();
    for n in names {
        *m.entry(n.to_owned()).or_insert(0) += 1;
    }
    m
}

fn step(n: u32, title: &str) {
    println!("\n[{n}] {title}");
}

pub async fn run(
    client: &Client,
    login: &LoginInfo,
    dump_dir: Option<PathBuf>,
    skip_dump: bool,
) -> Result<PathBuf> {
    /// 전달 방식 비교용: 응답을 두 방식에서 같은지 비교할 짧은 요약으로 줄인다.
    type Summarize = fn(&Value) -> Result<String, Error>;

    let mut r = Report {
        started_at_unix: now_unix(),
        base_url: client.config().base_url.to_string(),
        ..Report::default()
    };

    step(1, "로그인 (#1)");
    println!(
        "  ✓ 방식 {:?}, {}ms, 쿠키: {}, 캐너리 {}",
        login.scheme,
        ms(login.elapsed),
        login.cookie_names.join(", "),
        if login.has_canary { "있음" } else { "없음" }
    );
    r.login = Some(LoginReport {
        scheme: format!("{:?}", login.scheme),
        ms: ms(login.elapsed),
        cookie_names: login.cookie_names.clone(),
        has_canary: login.has_canary,
    });

    step(2, "사용자 설정·용량");
    let started = Instant::now();
    match client.user_config().await {
        Ok(c) => {
            println!(
                "  ✓ {}ms, {} / 사용 {} · 경고 {} · 발송 제한 {} · 최대 메시지 {} · 최대 수신자 {}",
                ms(started.elapsed()),
                c.email_address.as_deref().unwrap_or("?"),
                opt_bytes(c.quota_used),
                opt_bytes(c.quota_warning),
                opt_bytes(c.quota_send),
                c.max_message_size_kb
                    .map_or("?".into(), |k| format!("{}MB", k / 1024)),
                c.max_recipients.map_or("?".into(), |n| n.to_string()),
            );
            r.user = Some(c);
        }
        Err(e) => r.error("사용자 설정", e),
    }

    step(3, "폴더 계층");
    let h = fetch_hierarchy(client).await.context("계층 동기화 실패")?;
    let tree = h.mail_tree();
    println!(
        "  ✓ SyncFolderHierarchy {}ms ({}회), 전체 {}개 중 메일 폴더 {}개",
        ms(h.elapsed),
        h.calls,
        h.folders.len(),
        tree.len()
    );
    let mut hr = HierarchyReport {
        full_ms: ms(h.elapsed),
        full_calls: h.calls,
        all_folders: h.folders.len(),
        mail_folders: tree.len(),
        change_types: count_types(h.change_types.iter().map(String::as_str)),
        find_folder_ms: None,
        find_folder_all: None,
        find_folder_mail: None,
        incremental_ms: None,
        incremental_changes: None,
    };
    let started = Instant::now();
    match client.find_folders_deep().await {
        Ok(ff) => {
            let mail = eo_owa::mail_folder_tree(&ff, &h.root_id).len();
            println!(
                "  ✓ FindFolder(Deep) {}ms, 전체 {}개 중 메일 폴더 {}개",
                ms(started.elapsed()),
                ff.len(),
                mail
            );
            hr.find_folder_ms = Some(ms(started.elapsed()));
            hr.find_folder_all = Some(ff.len());
            hr.find_folder_mail = Some(mail);
        }
        Err(e) => r.error("FindFolder", e),
    }
    let started = Instant::now();
    match client.sync_folder_hierarchy(Some(&h.sync_state)).await {
        Ok(inc) => {
            println!(
                "  ✓ 계층 증분(무변경) {}ms, 변경 {}건",
                ms(started.elapsed()),
                inc.changes.len()
            );
            hr.incremental_ms = Some(ms(started.elapsed()));
            hr.incremental_changes = Some(inc.changes.len());
        }
        Err(e) => r.error("계층 증분", e),
    }
    for n in &tree {
        println!(
            "    {}{} ({}통, 안읽음 {})",
            "  ".repeat(n.depth),
            n.folder.display_name,
            n.folder.total_count.unwrap_or(0),
            n.folder.unread_count.unwrap_or(0)
        );
    }
    r.hierarchy = Some(hr);

    step(4, "폴더별 항목 동기화: 최초 전체 → 즉시 증분(무변경)");
    let mut listings: Vec<(String, Vec<DumpItem>)> = Vec::new();
    for n in &tree {
        let folder = FolderRef::from(&n.folder.folder_id);
        let full = match item_sync(client, &folder, None).await {
            Ok(l) => l,
            Err(e) => {
                r.error(&format!("항목 동기화 {}", n.path), e);
                continue;
            }
        };
        let started = Instant::now();
        let inc = item_sync(client, &folder, Some(&full.sync_state)).await;
        let inc_ms = ms(started.elapsed());
        let mut fr = FolderReport {
            path: n.path.clone(),
            total_count: n.folder.total_count,
            unread_count: n.folder.unread_count,
            items: full.changes.len(),
            full_ms: ms(full.elapsed),
            full_calls: full.calls,
            change_types: count_types(full.changes.iter().map(|c| c.type_name.as_str())),
            incremental_ms: None,
            incremental_changes: None,
            incremental_change_types: BTreeMap::new(),
        };
        let inc_text = match inc {
            Ok(inc) => {
                fr.incremental_ms = Some(inc_ms);
                fr.incremental_changes = Some(inc.changes.len());
                fr.incremental_change_types =
                    count_types(inc.changes.iter().map(|c| c.type_name.as_str()));
                format!("증분 {inc_ms}ms·변경 {}건", inc.changes.len())
            }
            Err(e) => {
                r.error(&format!("증분 동기화 {}", n.path), &e);
                "증분 실패".into()
            }
        };
        let mismatch = match n.folder.total_count {
            Some(t) if t != full.changes.len() as i64 => format!(" ※ TotalCount {t}와 다름"),
            _ => String::new(),
        };
        println!(
            "  ✓ {}: {}통, 최초 {}ms({}회), {}{}",
            n.path,
            full.changes.len(),
            ms(full.elapsed),
            full.calls,
            inc_text,
            mismatch
        );
        listings.push((n.path.clone(), dump::items_from_changes(&full.changes)));
        r.folders.push(fr);
    }

    step(
        5,
        "요청 전달 방식 비교: (a) 본문 JSON vs (b) X-OWA-UrlPostData (#3)",
    );
    let inbox = client
        .get_folder(&FolderRef::distinguished("inbox"))
        .await
        .context("받은 편지함 조회 실패")?;
    let inbox_ref = FolderRef::from(&inbox.folder_id);
    let cases: Vec<(&str, Value, Summarize)> = vec![
        (
            "GetFolder",
            requests::get_folder(&[FolderRef::distinguished("inbox")]),
            |v| {
                let f = responses::parse_get_folder(v)?;
                Ok(match f.first() {
                    Some(Ok(f)) => format!("폴더 {}", short_hash(&f.folder_id.id)),
                    Some(Err(e)) => format!("항목 오류 {}", e.code),
                    None => "없음".into(),
                })
            },
        ),
        (
            "FindItem",
            requests::find_item(&inbox_ref, 0, 50, &["DateTimeReceived"]),
            |v| {
                let p = responses::parse_find_item(v)?;
                let ids: String = p
                    .items
                    .iter()
                    .filter_map(|i| i.item_id.as_ref().map(|id| id.id.as_str()))
                    .collect();
                Ok(format!("{}건 {}", p.items.len(), short_hash(&ids)))
            },
        ),
        (
            "SyncFolderHierarchy",
            requests::sync_folder_hierarchy(None),
            |v| {
                Ok(format!(
                    "변경 {}건",
                    responses::parse_sync_folder_hierarchy(v)?.changes.len()
                ))
            },
        ),
        (
            "SyncFolderItems",
            requests::sync_folder_items(&inbox_ref, None, 50, &["DateTimeReceived"]),
            |v| {
                let s = responses::parse_sync_folder_items(v, 50)?;
                Ok(format!(
                    "변경 {}건, 끝 {}",
                    s.changes.len(),
                    s.includes_last
                ))
            },
        ),
    ];
    for (action, body, summarize) in cases {
        let request = requests::envelope(&requests::json_request_type(action), body);
        let mut results = Vec::new();
        for mode in [PostMode::Body, PostMode::Header] {
            let started = Instant::now();
            let res = client.call_raw(action, Some(request.clone()), mode).await;
            let elapsed = ms(started.elapsed());
            results.push(match res.and_then(|v| summarize(&v)) {
                Ok(s) => ModeResult {
                    ok: true,
                    ms: elapsed,
                    summary: s,
                },
                Err(e) => ModeResult {
                    ok: false,
                    ms: elapsed,
                    summary: e.to_string(),
                },
            });
        }
        let header = results.pop().expect("두 방식");
        let body = results.pop().expect("두 방식");
        let same = body.ok && header.ok && body.summary == header.summary;
        println!(
            "  {} {action}: (a) {} {}ms {} / (b) {} {}ms {}",
            if same { "✓" } else { "✗" },
            if body.ok { "성공" } else { "실패" },
            body.ms,
            body.summary,
            if header.ok { "성공" } else { "실패" },
            header.ms,
            header.summary
        );
        r.post_modes.push(ModeReport {
            action: action.into(),
            body,
            header,
            same_result: same,
        });
    }
    // 응답 구조(값 없이): 이어받기 표시(IncludesLastItemInRange)가 어디 있는지 확인용.
    let probe = requests::envelope(
        &requests::json_request_type("SyncFolderItems"),
        requests::sync_folder_items(&inbox_ref, None, 5, &["DateTimeReceived"]),
    );
    match client
        .call_raw("SyncFolderItems", Some(probe), PostMode::Body)
        .await
    {
        Ok(v) => {
            let shape = v
                .pointer("/Body/ResponseMessages/Items/0")
                .map(|m| json_shape(m, 3))
                .unwrap_or_default();
            println!("  SyncFolderItems 응답 구조(MaxChangesReturned 5): {shape}");
            r.sync_items_shape = Some(shape);
        }
        Err(e) => r.error("SyncFolderItems 응답 구조", e),
    }

    let root_dir = dump_dir.unwrap_or_else(|| dev_dir().join(format!("dump-{}", now_unix())));
    if skip_dump {
        println!("\n[6] EML 덤프: 건너뜀(--skip-dump)");
    } else {
        step(6, "EML 덤프: 전 메일 폴더, 묶음 4 × 동시 4");
        println!("  위치: {}", root_dir.display());
        let started = Instant::now();
        let mut total = DumpStats::default();
        let mut per_folder = Vec::new();
        for (path, items) in &listings {
            let dir = folder_dir(&root_dir, path);
            match dump::fetch_items(client, items, Some(&dir), 4, 4, true).await {
                Ok(s) => {
                    println!("  ✓ {path}: {}", s.summary());
                    total.items += s.items;
                    total.files += s.files;
                    total.bytes += s.bytes;
                    total.no_header_end += s.no_header_end;
                    total.no_message_id += s.no_message_id;
                    for (k, v) in &s.failures {
                        *total.failures.entry(k.clone()).or_default() += v;
                    }
                    per_folder.push((path.clone(), s));
                }
                Err(e) => r.error(&format!("덤프 {path}"), e),
            }
        }
        let elapsed = started.elapsed();
        total.elapsed_ms = ms(elapsed);
        total.mb_per_s = (crate::util::mb_per_s(total.bytes, elapsed) * 10.0).round() / 10.0;
        total.items_per_s =
            ((total.files as f64 / elapsed.as_secs_f64().max(0.001)) * 10.0).round() / 10.0;
        println!(
            "  합계: {} (머리글 구분 없음 {}, Message-ID 없음 {})",
            total.summary(),
            total.no_header_end,
            total.no_message_id
        );
        r.dump = Some(DumpReport {
            dir: root_dir.display().to_string(),
            total,
            per_folder,
        });
    }

    step(
        7,
        "동시성 비교: 받은 편지함 최근 32통 원문 (#12, 가벼운 부하)",
    );
    let sample: Vec<DumpItem> = listings
        .iter()
        .find(|(p, _)| *p == inbox.display_name)
        .map(|(_, items)| {
            let mut v = items.clone();
            v.sort_by(|a, b| b.received.cmp(&a.received));
            v.truncate(32);
            v
        })
        .unwrap_or_default();
    for (concurrency, batch) in [(4, 4), (8, 2), (8, 4)] {
        match dump::fetch_items(client, &sample, None, batch, concurrency, false).await {
            Ok(s) => {
                println!("  ✓ 동시 {concurrency} × 묶음 {batch}: {}", s.summary());
                r.concurrency.push(ConcurrencyReport {
                    concurrency,
                    batch,
                    stats: s,
                });
            }
            Err(e) => r.error(&format!("동시 {concurrency} × 묶음 {batch}"), e),
        }
    }

    step(
        8,
        "CreateFolder 형식 확인: 잘못된 부모 ID (#11, 폴더는 만들어지지 않음)",
    );
    let probe = requests::create_folder(
        &FolderRef::Id("AAMkInvalidParentIdForFormatProbe".into()),
        "outlook-format-probe",
    );
    let outcome = match client.call("CreateFolder", probe).await {
        Ok(v) => match responses::single("CreateFolder", &v) {
            Err(Error::Response { code, .. }) => format!("형식 통과, 항목 오류 {code}"),
            Ok(_) => "주의: 성공 응답 — 폴더가 생겼는지 웹메일에서 확인 필요".into(),
            Err(e) => format!("응답 해석 실패: {e}"),
        },
        Err(Error::RequestFormat { code, message, .. }) => format!("형식 오류: {code} {message}"),
        Err(e) => format!("오류: {e}"),
    };
    println!("  {outcome}");
    r.create_folder_probe = Some(outcome);

    step(
        9,
        "세션 끊김 → 자동 재인증: 세션을 끊은 뒤 호출 (#2 일부, NTLM은 연결을 모두 닫음)",
    );
    let before = client.events().len();
    let handshakes_before = client.auth_handshakes();
    let logoff = match client.logoff().await {
        Ok(()) => "성공".to_owned(),
        Err(e) => format!("실패: {e}"),
    };
    let started = Instant::now();
    let call = client.get_folder(&FolderRef::distinguished("inbox")).await;
    let call_ms = ms(started.elapsed());
    let events: Vec<String> = client
        .events()
        .iter()
        .skip(before)
        .map(describe_event)
        .collect();
    let handshakes_after = client.auth_handshakes();
    println!(
        "  끊기 {logoff} → 호출 {} ({call_ms}ms), NTLM 연결 인증 {handshakes_before} → {handshakes_after}회",
        if call.is_ok() { "성공" } else { "실패" }
    );
    for e in &events {
        println!("    - {e}");
    }
    if let Err(e) = &call {
        r.error("끊긴 뒤 호출", e);
    }
    r.expiry = Some(ExpiryReport {
        logoff,
        call_ok: call.is_ok(),
        call_ms,
        handshakes_before,
        handshakes_after,
        events,
    });

    r.events = client.events().iter().map(describe_event).collect();
    std::fs::create_dir_all(&root_dir)?;
    let report_path = root_dir.join("verify-report.json");
    std::fs::write(&report_path, serde_json::to_vec_pretty(&r)?)?;
    println!(
        "\n완료: 오류 {}건. 보고서 {}",
        r.errors.len(),
        report_path.display()
    );
    Ok(report_path)
}

/// 값 없이 구조만: 키와 타입, 문자열은 길이, 배열은 개수와 첫 원소 구조. `__type`·진리값·숫자는 그대로.
fn json_shape(v: &Value, depth: usize) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => format!("str({})", s.chars().count()),
        Value::Array(a) => match a.first() {
            Some(first) if depth > 0 => format!("[{}×{}]", a.len(), json_shape(first, depth - 1)),
            _ => format!("[{}]", a.len()),
        },
        Value::Object(m) if depth == 0 => format!("{{키 {}개}}", m.len()),
        Value::Object(m) => {
            let fields: Vec<String> = m
                .iter()
                .map(|(k, v)| match (k.as_str(), v) {
                    ("__type", Value::String(t)) => format!("__type:{t}"),
                    _ => format!("{k}:{}", json_shape(v, depth - 1)),
                })
                .collect();
            format!("{{{}}}", fields.join(", "))
        }
    }
}

fn opt_bytes(n: Option<i64>) -> String {
    n.and_then(|n| u64::try_from(n).ok())
        .map_or("?".into(), fmt_bytes)
}

fn folder_dir(root: &Path, path: &str) -> PathBuf {
    path.split(" / ")
        .fold(root.to_path_buf(), |dir, part| dir.join(safe_name(part)))
}
