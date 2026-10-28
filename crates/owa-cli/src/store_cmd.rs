//! `owa-cli store`: 로컬 저장소·동기화 엔진 확인(계획서 10장 2단계). 로그인 외에는 입력이 없다.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use eo_owa::Client;
use eo_search::{SearchIndex, SearchOptions, Sort};
use eo_store::{FolderRow, Store};
use eo_sync::{CycleReport, Engine, EngineConfig};

use crate::util::{fmt_bytes, mb_per_s, ms, now_unix};

fn clock() -> String {
    let s = now_unix() + 9 * 3600;
    format!("{:02}:{:02}:{:02}", (s / 3600) % 24, (s / 60) % 60, s % 60)
}

fn folder_names(store: &Store) -> Result<HashMap<i64, String>> {
    Ok(store
        .folders()?
        .into_iter()
        .map(|f| (f.id, f.name))
        .collect())
}

/// 메일 폴더를 트리 순서(깊이 우선, 같은 단계는 표시 순서·이름순)로.
fn tree_order(folders: &[FolderRow]) -> Vec<(&FolderRow, usize)> {
    fn walk<'a>(
        all: &'a [FolderRow],
        parent: Option<i64>,
        depth: usize,
        out: &mut Vec<(&'a FolderRow, usize)>,
    ) {
        for f in all.iter().filter(|f| f.is_mail && f.parent_id == parent) {
            out.push((f, depth));
            if depth < 16 {
                walk(all, Some(f.id), depth + 1, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(folders, None, 0, &mut out);
    out
}

/// 로그인 없이 로컬 현황만.
pub fn status(root: &Path) -> Result<()> {
    let store = Store::open(root)?;
    println!("저장 위치: {}", root.display());
    let folders = store.folders()?;
    let counts: HashMap<i64, _> = store
        .folder_counts()?
        .into_iter()
        .map(|c| (c.folder_id, c))
        .collect();
    println!("폴더 (메일 / 안읽음 / 보관 / 원문 없음):");
    let (mut total, mut archived, mut missing) = (0, 0, 0);
    for (f, depth) in tree_order(&folders) {
        let c = counts.get(&f.id).cloned().unwrap_or_default();
        total += c.total;
        archived += c.archived;
        missing += c.missing_eml;
        println!(
            "  {}{}{}: {} / {} / {} / {}",
            "  ".repeat(depth),
            f.name,
            if f.archived_at.is_some() {
                " (보관 폴더)"
            } else {
                ""
            },
            c.total,
            c.unread,
            c.archived,
            c.missing_eml
        );
    }
    let (files, bytes) = store.eml().usage()?;
    let db = std::fs::metadata(root.join("db.sqlite")).map_or(0, |m| m.len());
    println!("합계: {total}통, 보관 {archived}, 원문 없음 {missing}");
    println!("원문 저장소: 파일 {files}개, {} (zstd)", fmt_bytes(bytes));
    println!(
        "DB: {}, 대기 작업 {}",
        fmt_bytes(db),
        store.pending_op_count()?
    );
    let ops = store.op_summary()?;
    if !ops.is_empty() {
        println!("작업 큐 (종류: 상태별 수):");
        for o in ops {
            let (kind, n) = (&o.kind, o.count);
            let state = match o.state {
                0 => "대기",
                1 => "완료",
                _ => "포기·중지",
            };
            match &o.last_error {
                Some(e) if state != "완료" => {
                    println!("  {kind}: {state} {n} (마지막 오류: {e})")
                }
                _ => println!("  {kind}: {state} {n}"),
            }
        }
    }
    Ok(())
}

/// 스레드 확인(6단계): 메일 원문의 In-Reply-To·References가 로컬의 어느 메일을 가리키는지, 첨부 수.
/// 제목·주소·Message-ID는 출력하지 않는다(가리키는 메일 번호·폴더만).
pub fn thread(root: &Path, id: i64) -> Result<()> {
    let store = Store::open(root)?;
    let names = folder_names(&store)?;
    let Some(m) = store.message(id)? else {
        println!("#{id}: 없음");
        return Ok(());
    };
    let Some(hash) = m.eml_hash.as_deref() else {
        println!("#{id}: 원문을 아직 받지 않았습니다");
        return Ok(());
    };
    let raw = store.eml().get(hash)?;
    let head = String::from_utf8_lossy(&raw);
    let head = head.split("\r\n\r\n").next().unwrap_or("");
    let header = |name: &str| -> String {
        let prefix = format!("{}:", name.to_ascii_lowercase());
        let mut value: Option<String> = None;
        for line in head.lines() {
            match &mut value {
                // 접힌 줄은 공백 하나로 잇는다(References의 ID들이 붙지 않게).
                Some(v) if line.starts_with([' ', '\t']) => {
                    v.push(' ');
                    v.push_str(line.trim());
                }
                Some(_) => break,
                None if line.to_ascii_lowercase().starts_with(&prefix) => {
                    value = Some(line[prefix.len()..].trim().to_owned());
                }
                None => {}
            }
        }
        value.unwrap_or_default()
    };
    let locate = |mid: &str| -> String {
        let found = store.message_ids_by_imid(mid).unwrap_or_default();
        if found.is_empty() {
            "로컬에 없음".to_owned()
        } else {
            found
                .iter()
                .map(|(i, f)| format!("#{i}({})", names.get(f).map_or("?", String::as_str)))
                .collect::<Vec<_>>()
                .join(", ")
        }
    };
    let irt = header("In-Reply-To");
    let refs: Vec<String> = header("References")
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    let attachments = eo_mime::extract(&raw).map_or(0, |x| x.attachment_names.len());
    println!(
        "#{id}: {} · 폴더 {} · 첨부 {attachments}개",
        kst(m.props.date_received),
        names.get(&m.folder_id).map_or("?", String::as_str)
    );
    if irt.is_empty() {
        println!("  In-Reply-To: 없음");
    } else {
        println!("  In-Reply-To → {}", locate(&irt));
    }
    println!("  References {}개", refs.len());
    for r in &refs {
        println!("    → {}", locate(r));
    }
    Ok(())
}

/// 메일 한 통의 상태만(제목·주소는 출력하지 않는다).
pub fn show(root: &Path, id: i64) -> Result<()> {
    let store = Store::open(root)?;
    let names = folder_names(&store)?;
    let Some(m) = store.message(id)? else {
        println!("#{id}: 없음(완전 삭제됐거나 없는 번호)");
        return Ok(());
    };
    let state = match m.server_state {
        eo_store::server_state::SYNCED => "서버에 있음",
        eo_store::server_state::ARCHIVED => "보관(서버에서 삭제됨)",
        eo_store::server_state::PENDING => "서버 반영 대기 사본",
        _ => "로컬 전용",
    };
    let flag = match m.props.flag {
        1 => "플래그",
        2 => "완료",
        _ => "없음",
    };
    println!(
        "#{id}: {} · {} · 폴더 {} · 읽음 {} · 플래그 {} · 서버 항목 ID {} · 원문 {}",
        kst(m.props.date_received),
        state,
        names.get(&m.folder_id).map_or("?", String::as_str),
        if m.props.is_read { "예" } else { "아니오" },
        flag,
        if m.server_item_id.is_some() {
            "있음"
        } else {
            "없음"
        },
        if m.eml_hash.is_some() {
            "있음"
        } else {
            "없음"
        },
    );
    Ok(())
}

fn print_cycle(r: &CycleReport) {
    println!(
        "[1] 계층: 변경 {}건, {}회, {}ms",
        r.hierarchy.changes,
        r.hierarchy.calls,
        ms(r.hierarchy.elapsed)
    );
    let a = &r.items.apply;
    println!(
        "[2] 항목: 폴더 {}개, 호출 {}회, {:.1}초 → 새 {} · 갱신 {} · 읽음 {} · 이동 {} · 보관 {} · 무시 {}",
        r.items.folders,
        r.items.calls,
        r.items.elapsed.as_secs_f64(),
        a.created,
        a.updated,
        a.read_flags,
        a.moves.len(),
        a.archives.len(),
        a.ignored
    );
    for e in &r.items.errors {
        println!("    ✗ {e}");
    }
    let e = &r.eml;
    println!(
        "[3] 원문: 대상 {}통 → 저장 {}통, {} / {:.1}초 = {:.1}MB/s, 실패 {}{}",
        e.queued,
        e.stored,
        fmt_bytes(e.bytes),
        e.elapsed.as_secs_f64(),
        mb_per_s(e.bytes, e.elapsed),
        e.failures.values().sum::<usize>(),
        if e.failures.is_empty() {
            String::new()
        } else {
            format!(" {:?}", e.failures)
        }
    );
    if let Some(i) = &r.index {
        println!(
            "[4] 색인: {}통, 원문 해석 실패 {}, {:.1}초",
            i.indexed,
            i.parse_failed,
            i.elapsed.as_secs_f64()
        );
    }
}

fn open_search(root: &Path) -> Result<Arc<SearchIndex>> {
    let (search, rebuilt) = SearchIndex::open_checked(&root.join("index"))?;
    if rebuilt {
        let n = Store::open(root)?.mark_all_unindexed()?;
        println!("색인 형식이 바뀌어 다시 만듭니다: {n}통");
    }
    Ok(Arc::new(search))
}

/// 로그인 없이: 원문은 있고 색인되지 않은 로컬 메일을 색인한다.
pub fn index(root: &Path, check: bool) -> Result<()> {
    let store = Store::open(root)?;
    let search = open_search(root)?;
    if check {
        // DB와 색인 대조: 완료로 표시됐는데 색인에 없는 메일은 다시 색인, DB에 없는 색인 문서는 지운다.
        let in_index: std::collections::HashSet<u64> = search.all_ids()?.into_iter().collect();
        let marked = store.indexed_ids()?;
        let missing: Vec<i64> = marked
            .iter()
            .copied()
            .filter(|id| !in_index.contains(&(*id as u64)))
            .collect();
        let existing: std::collections::HashSet<u64> =
            store.message_ids()?.into_iter().map(|i| i as u64).collect();
        let stale: Vec<u64> = in_index
            .iter()
            .copied()
            .filter(|id| !existing.contains(id))
            .collect();
        println!(
            "대조: 색인 문서 {}개, 완료 표시 {}통 → 색인에 없음 {}통, DB에 없는 색인 문서 {}개",
            in_index.len(),
            marked.len(),
            missing.len(),
            stale.len()
        );
        if !missing.is_empty() {
            let names = folder_names(&store)?;
            for id in &missing {
                if let Some(m) = store.message(*id)? {
                    println!(
                        "  색인에 없음 #{id} · {} · {}",
                        kst(m.props.date_received),
                        names.get(&m.folder_id).map_or("?", String::as_str)
                    );
                }
            }
            store.mark_unindexed(&missing)?;
        }
        for id in &stale {
            search.delete(*id)?;
        }
        if !stale.is_empty() {
            search.commit()?;
        }
    }
    let before = store.unindexed_count()?;
    let r = eo_sync::index_pending(&store, &search, None)?;
    let size = dir_size(&root.join("index"));
    println!(
        "색인: 대기 {before}통 → {}통 색인(원문 해석 실패 {}), {:.1}초. 색인된 메일 {}통, 색인 크기 {}",
        r.indexed,
        r.parse_failed,
        r.elapsed.as_secs_f64(),
        search.num_docs(),
        fmt_bytes(size)
    );
    Ok(())
}

/// 로그인 없이: 본문 그림 참조를 종류·서버별로 센다(제목·주소·경로는 출력하지 않는다). 저장소는 읽기만 한다.
pub fn images(root: &Path, id: Option<i64>, subject: Option<&str>) -> Result<()> {
    let store = Store::open(root)?;
    let ids = match (id, subject) {
        (Some(id), _) => vec![id],
        (None, Some(s)) => store.message_ids_by_subject(s, 5)?,
        (None, None) => anyhow::bail!("--id 또는 --subject를 주세요"),
    };
    if ids.is_empty() {
        println!("맞는 메일이 없습니다");
    }
    let names = folder_names(&store)?;
    for id in ids {
        let Some(m) = store.message(id)? else {
            println!("#{id}: 없음");
            continue;
        };
        let sender = m.props.from_addr.clone().unwrap_or_default();
        let domain = sender.rsplit_once('@').map_or("", |x| x.1);
        println!(
            "#{id} · {} · {} · 보낸 사람 도메인 @{domain} · 외부 이미지 허용 {}",
            kst(m.props.date_received),
            names.get(&m.folder_id).map_or("?", String::as_str),
            if store.remote_images_allowed(&sender)? {
                "예"
            } else {
                "아니오"
            }
        );
        let Some(hash) = m.eml_hash.as_deref() else {
            println!("  원문 없음");
            continue;
        };
        let raw = store.eml().get(hash)?;
        let Some(r) = eo_mime::image_report(&raw) else {
            println!("  원문을 해석할 수 없음");
            continue;
        };
        println!("  HTML 본문 {}", if r.html { "있음" } else { "없음" });
        for (kind, host, n) in &r.refs {
            println!(
                "  {kind}{} × {n}",
                if host.is_empty() {
                    String::new()
                } else {
                    format!(" {host}")
                }
            );
        }
        for (mime, size, cid, att) in &r.parts {
            println!(
                "  부분 {mime} {} · Content-ID {} · {}",
                fmt_bytes(*size as u64),
                if *cid { "있음" } else { "없음" },
                if *att { "첨부" } else { "본문용" }
            );
        }
        for c in &r.missing_cids {
            println!("  빠진 cid 형태: {}", cid_shape(c));
        }
        println!(
            "  복원 표시 {} · cid 나오는 곳: {}",
            if r.restored { "있음" } else { "없음" },
            r.cid_contexts.join(" ")
        );
        let links = eo_mime::cid_link_parts(&raw);
        for (k, p, loose) in &links {
            println!(
                "  연결: {} ({}자) → 부분 {p}{}",
                cid_shape(k),
                k.chars().count(),
                if *loose {
                    " (이름·순서로 이음)"
                } else {
                    ""
                }
            );
        }
        for c in &r.missing_cids {
            let near = links
                .iter()
                .map(|(k, _, _)| {
                    let common = k.chars().zip(c.chars()).take_while(|(a, b)| a == b).count();
                    format!("{common}자 같음")
                })
                .collect::<Vec<_>>()
                .join(", ");
            println!(
                "  빠진 cid {}자 · 연결된 것과 앞부분: {near}",
                c.chars().count()
            );
        }
        println!("  구조:");
        for (d, t) in &r.tree {
            println!("    {}{t}", "  ".repeat(*d));
        }
        for (k, v) in &r.headers {
            println!("  {k}: {v}");
        }
        // 앱과 같은 처리로 그려 본 결과(허용 여부는 앱과 같게).
        let opts = eo_mime::RenderOptions {
            cid_base: format!("http://app-cid.localhost/{id}/"),
            allow_remote: store.remote_images_allowed(&sender)?,
        };
        if let Some(v) = eo_mime::render(&raw, &opts) {
            let mut n: std::collections::BTreeMap<&str, usize> = Default::default();
            let l = v.html.to_ascii_lowercase();
            for tag in l.split("<img").skip(1) {
                let tag = tag.split('>').next().unwrap_or("");
                let k = match tag.find("src=\"").map(|i| &tag[i + 5..]) {
                    Some(s) if s.starts_with("http://app-cid.localhost") => "본문 그림 연결",
                    Some(s) if s.starts_with("https:") || s.starts_with("http:") => {
                        "외부 그림 유지"
                    }
                    Some(s) if s.starts_with("data:") => "data 그림",
                    Some(_) => "그 밖의 주소",
                    None => "주소 지워짐",
                };
                *n.entry(k).or_default() += 1;
            }
            let csp = v
                .html
                .split("img-src ")
                .nth(1)
                .and_then(|s| s.split(';').next())
                .unwrap_or("?");
            println!(
                "  앱 표시: {} · 차단 표시 {} · img-src {csp}",
                n.iter()
                    .map(|(k, c)| format!("{k} {c}"))
                    .collect::<Vec<_>>()
                    .join(" · "),
                if v.has_remote_images {
                    "있음"
                } else {
                    "없음"
                }
            );
        }
    }
    Ok(())
}

/// `cid:` 이름의 형태: 숫자·16진 덩어리는 #으로 가린다(예: jira-generated-image-avatar-#).
fn cid_shape(c: &str) -> String {
    c.split(['-', '_', '.', '@'])
        .map(|w| {
            if w.chars().any(|c| c.is_ascii_digit()) || w.len() > 12 {
                "#"
            } else {
                w
            }
        })
        .collect::<Vec<_>>()
        .join("-")
}

/// 진단용: 앱이 그리는 본문(허용대로)에서 각 그림 앞 구조와 `<style>` 규칙 중 숨김·크기에 관한 것.
/// 글자는 T, 속성 값은 style·class·width·height·align만 보이고 나머지(주소 등)는 …로 가린다.
pub fn image_tags(root: &Path, id: i64) -> Result<()> {
    let store = Store::open(root)?;
    let m = store.message(id)?.context("메일 없음")?;
    let raw = store
        .eml()
        .get(m.eml_hash.as_deref().context("원문 없음")?)?;
    let sender = m.props.from_addr.clone().unwrap_or_default();
    let opts = eo_mime::RenderOptions {
        cid_base: format!("http://app-cid.localhost/{id}/"),
        allow_remote: store.remote_images_allowed(&sender)?,
    };
    let v = eo_mime::render(&raw, &opts).context("해석할 수 없음")?;
    let html = &v.html;
    let body = html.find("<body>").map_or(0, |i| i + 6);
    // <style> 안의 숨김·크기·미디어 규칙.
    let mut at = 0;
    while let Some(i) = html[at..].find("<style>") {
        let s = at + i + 7;
        let e = html[s..].find("</style>").map_or(html.len(), |x| s + x);
        let css = &html[s..e];
        for rule in css.split('}') {
            let r = rule.trim();
            let l = r.to_ascii_lowercase();
            if [
                "display",
                "@media",
                "visibility",
                "max-height",
                "mso-hide",
                "height:0",
                "width:0",
                "overflow",
            ]
            .iter()
            .any(|k| l.contains(k))
            {
                println!(
                    "CSS: {}",
                    r.chars().take(200).collect::<String>().replace('\n', " ")
                );
            }
        }
        at = e;
    }
    let lower = html.to_ascii_lowercase();
    let mut at = body;
    let mut n = 0;
    while let Some(i) = lower[at..].find("<img") {
        let s = at + i;
        let e = lower[s..].find('>').map_or(lower.len(), |x| s + x + 1);
        n += 1;
        let mut from = s.saturating_sub(1500).max(body);
        while !html.is_char_boundary(from) {
            from += 1;
        }
        println!("--- 그림 {n} 앞 구조 ---");
        println!("{}", mask_html(&html[from..e]));
        at = e;
    }
    Ok(())
}

/// HTML 조각에서 글자는 T로, 속성 값은 style·class·width·height·align·valign·border만 남기고 …로 바꾼다.
fn mask_html(h: &str) -> String {
    const KEEP: [&str; 7] = [
        "style", "class", "width", "height", "align", "valign", "border",
    ];
    let mut out = String::new();
    let mut rest = h;
    while !rest.is_empty() {
        match rest.find('<') {
            Some(0) => {
                let e = rest.find('>').map_or(rest.len(), |x| x + 1);
                let tag = &rest[..e];
                // 태그 이름과 속성: name="value" 단위로.
                let mut t = String::new();
                let mut name = String::new();
                let mut in_val: Option<char> = None;
                let mut keep = false;
                for c in tag.chars() {
                    match in_val {
                        Some(q) => {
                            if c == q {
                                in_val = None;
                                if !keep {
                                    t.push('…');
                                }
                                t.push(c);
                            } else if keep {
                                t.push(c);
                            }
                        }
                        None => {
                            if (c == '"' || c == '\'') && t.ends_with('=') {
                                in_val = Some(c);
                                keep = KEEP.contains(&name.trim().to_ascii_lowercase().as_str());
                                t.push(c);
                            } else {
                                if c == ' ' || c == '<' {
                                    name.clear();
                                } else if c != '=' {
                                    name.push(c);
                                }
                                t.push(c);
                            }
                        }
                    }
                }
                out.push_str(&t);
                rest = &rest[e..];
            }
            Some(i) => {
                if !rest[..i].trim().is_empty() {
                    out.push('T');
                }
                rest = &rest[i..];
            }
            None => {
                if !rest.trim().is_empty() {
                    out.push('T');
                }
                break;
            }
        }
    }
    out
}

/// 로그인 없이: OST 4K(HCL Traveler `.ims`) 파일을 읽어 폴더별 통수, EML로 만든 수·실패, 보낸 사람 도메인(많은
/// 순)만 보인다. 제목·주소는 출력하지 않는다. 저장소에는 넣지 않는다.
pub fn ims(path: &Path, limit: Option<usize>) -> Result<()> {
    let t = Instant::now();
    let ost = eo_sync::OstFile::open(path).map_err(anyhow::Error::msg)?;
    let opened = t.elapsed();
    let items = ost.items().map_err(anyhow::Error::msg)?;
    println!(
        "열기 {}ms · 목록 {}ms · 메일 항목 {}통",
        ms(opened),
        ms(t.elapsed() - opened),
        items.len()
    );
    let mut by_folder: std::collections::BTreeMap<String, usize> = Default::default();
    for it in &items {
        *by_folder.entry(it.folder.join("/")).or_default() += 1;
    }
    for (f, n) in &by_folder {
        println!("  {f}: {n}");
    }
    let t = Instant::now();
    let (mut ok, mut skipped, mut failed, mut bytes) = (0, 0, 0, 0usize);
    let (mut html, mut atts, mut no_date, mut no_from) = (0, 0, 0, 0);
    let mut domains: HashMap<String, usize> = HashMap::new();
    let mut errors: HashMap<String, usize> = HashMap::new();
    for it in items.iter().take(limit.unwrap_or(usize::MAX)) {
        match ost.eml(it.node) {
            Ok(Some(raw)) => {
                ok += 1;
                bytes += raw.len();
                if let Some(s) = eo_mime::backup::summarize(&raw) {
                    no_date += usize::from(s.date.is_none());
                    match s.from.as_ref().map(|f| f.email.clone()) {
                        Some(e) if e.contains('@') => {
                            let d = e.rsplit_once('@').map_or("", |x| x.1).to_lowercase();
                            *domains.entry(d).or_default() += 1;
                        }
                        _ => no_from += 1,
                    }
                }
                if let Some(x) = eo_mime::extract(&raw) {
                    atts += usize::from(!x.attachment_names.is_empty());
                }
                html +=
                    usize::from(!String::from_utf8_lossy(&raw).contains("white-space:pre-wrap"));
            }
            Ok(None) => skipped += 1,
            Err(e) => {
                failed += 1;
                // 오류 문장의 숫자(블록 번호 등)는 묶어 센다.
                let k: String = e
                    .chars()
                    .map(|c| if c.is_ascii_hexdigit() { '#' } else { c })
                    .collect();
                *errors.entry(k).or_default() += 1;
            }
        }
    }
    println!(
        "EML: {ok}통({}) · 메일 아님 {skipped} · 실패 {failed} · {:.1}초 · HTML 본문 {html} · 첨부 있음 {atts} · 날짜 없음 {no_date} · 보낸 사람 없음 {no_from}",
        fmt_bytes(bytes as u64),
        t.elapsed().as_secs_f64()
    );
    let mut d: Vec<_> = domains.into_iter().collect();
    d.sort_by_key(|a| std::cmp::Reverse(a.1));
    println!(
        "보낸 사람 도메인(많은 순 8): {}",
        d.iter()
            .take(8)
            .map(|(k, n)| format!("{k} {n}"))
            .collect::<Vec<_>>()
            .join(" · ")
    );
    for (e, n) in errors {
        println!("  실패: {e} × {n}");
    }
    // 첨부 방식·데이터 형식별 수(빠뜨리는 첨부가 없는지).
    let mut kinds: std::collections::BTreeMap<(i32, u16), (usize, usize)> = Default::default();
    for it in items.iter().take(limit.unwrap_or(usize::MAX)) {
        for (m, t, n) in ost.attachment_kinds(it.node).unwrap_or_default() {
            let e = kinds.entry((m, t)).or_default();
            e.0 += 1;
            e.1 += n;
        }
    }
    println!(
        "첨부(방식/형식: 개수, 크기): {}",
        kinds
            .iter()
            .map(|((m, t), (c, n))| format!("{m}/{t:04X}: {c}, {}", fmt_bytes(*n as u64)))
            .collect::<Vec<_>>()
            .join(" · ")
    );
    if std::env::var_os("EO_IMS_PROPS").is_some() {
        // 속성 번호·형식·길이만(값은 주소 종류와 주소 모양만).
        let fmt = |v: Vec<(u16, u16, usize)>| {
            v.iter()
                .map(|(k, t, n)| {
                    if *k == 0xFFFF {
                        "| 받는 사람:".to_owned()
                    } else {
                        format!("{k:04X}/{t:04X}:{n}")
                    }
                })
                .collect::<Vec<_>>()
                .join(" ")
        };
        println!(
            "저장소: {}",
            fmt(ost.prop_list(None).map_err(anyhow::Error::msg)?)
        );
        if let Some(it) = items.first() {
            println!(
                "첫 메일: {}",
                fmt(ost.prop_list(Some(it.node)).map_err(anyhow::Error::msg)?)
            );
        }
        let shape = |s: &str| -> String {
            s.chars()
                .map(|c| {
                    if c.is_alphanumeric() && !c.is_ascii_uppercase() {
                        'x'
                    } else {
                        c
                    }
                })
                .collect::<String>()
                .chars()
                .take(40)
                .collect()
        };
        let mut kinds: HashMap<String, usize> = HashMap::new();
        for it in items.iter().take(300) {
            for id in [0x0064u16, 0x0C1E] {
                let t = ost.prop_text(it.node, id).unwrap_or_else(|| "-".into());
                *kinds.entry(format!("{id:04X}={t}")).or_default() += 1;
            }
            let e = ost.prop_text(it.node, 0x0065).unwrap_or_default();
            if !e.contains('@') {
                *kinds.entry(format!("0065 모양 {}", shape(&e))).or_default() += 1;
            }
        }
        let mut k: Vec<_> = kinds.into_iter().collect();
        k.sort_by_key(|a| std::cmp::Reverse(a.1));
        for (s, n) in k.iter().take(14) {
            println!("  {s} × {n}");
        }
        // 글자는 x로 가리고 기호(@ < > =? ?= 등)만 남긴 모양.
        let mask = |s: &str| -> String {
            s.chars()
                .map(|c| if c.is_alphanumeric() { 'x' } else { c })
                .collect::<String>()
                .replace("xx", "x")
                .replace("xx", "x")
        };
        for it in items.iter().take(3) {
            let e = ost.prop_text(it.node, 0x0065).unwrap_or_default();
            println!("  0065: {}", mask(&e));
            let h = ost.prop_text(it.node, 0x007D).unwrap_or_default();
            for line in h.lines().filter(|l| {
                let l = l.to_ascii_lowercase();
                l.starts_with("from:") || l.starts_with("to:") || l.starts_with("cc:")
            }) {
                println!(
                    "  머리글: {}",
                    mask(&line.chars().take(160).collect::<String>())
                );
            }
        }
    }
    Ok(())
}

/// 로그인 없이: 중복 메일(제목·본문·보낸 사람·받는 사람·참조가 같은 메일, 0.9.0) 찾기의 시간과 묶음 수. 지우지
/// 않는다. 묶음마다 남길 메일·지울 메일의 폴더·받은 시각·제목을 `show`개까지 보인다(내용은 찍지 않는다).
pub fn dupes(root: &Path, show: usize, diff: Option<(i64, i64)>) -> Result<()> {
    let store = Store::open(root)?;
    if let Some((a, b)) = diff {
        // 두 메일 원문의 다른 점: 글이 같은지, Date·Message-ID 머리글, 링크 주소(앞 60글자), HTML 길이.
        for id in [a, b] {
            let m = store
                .message(id)?
                .ok_or_else(|| anyhow::anyhow!("#{id} 없음"))?;
            let raw = store.eml().get(m.eml_hash.as_deref().unwrap_or(""))?;
            let x = eo_mime::extract(&raw).unwrap_or_default();
            let text: String = x.body_text.chars().filter(|c| !c.is_whitespace()).collect();
            let s = String::from_utf8_lossy(&raw);
            let head = |name: &str| {
                s.lines()
                    .find(|l| l.to_ascii_lowercase().starts_with(&format!("{name}:")))
                    .unwrap_or("")
                    .to_owned()
            };
            let hrefs: Vec<String> = s
                .match_indices("href=")
                .map(|(i, _)| s[i + 5..].chars().take(60).collect())
                .collect();
            println!(
                "#{id} 글 {}자(해시 {:x}) · 원문 {}바이트 · {} · {} · 링크 {}개",
                text.chars().count(),
                {
                    use std::hash::{Hash, Hasher};
                    let mut h = std::hash::DefaultHasher::new();
                    text.hash(&mut h);
                    h.finish()
                },
                raw.len(),
                head("date"),
                head("message-id"),
                hrefs.len()
            );
            for h in hrefs.iter().take(8) {
                println!("    {h}");
            }
        }
        return Ok(());
    }
    let names = folder_names(&store)?;
    let t = Instant::now();
    let mut cache = eo_sync::BodyCache::new();
    let mut last = (eo_store::DupStage::Read, 0usize);
    let scan = eo_sync::exact_duplicates(&store, &mut cache, &mut |stage, done, total| {
        if stage != last.0 || done >= last.1 + 5000 || done == total {
            last = (stage, done);
            eprintln!(
                "  {stage:?} {done}/{total} ({:.1}초)",
                t.elapsed().as_secs_f64()
            );
        }
    })?;
    let first = t.elapsed();
    let t2 = Instant::now();
    let again = eo_sync::exact_duplicates(&store, &mut cache, &mut |_, _, _| {})?;
    let drops: usize = scan.sets.iter().map(|g| g.drop.len()).sum();
    println!(
        "메일 {}통 → 중복 {}묶음 · 지울 메일 {}통 · 원문 없어 비교 못함 {}통 · {:.1}초(캐시로 다시 {:.1}초, 같은 결과 {})",
        scan.scanned,
        scan.sets.len(),
        drops,
        scan.without_body,
        first.as_secs_f64(),
        t2.elapsed().as_secs_f64(),
        again == scan
    );
    let line = |id: i64| -> Result<String> {
        let Some(m) = store.message(id)? else {
            return Ok(format!("#{id} 없음"));
        };
        Ok(format!(
            "#{id} {} · {} · 상태 {} · {}",
            kst(m.props.date_received),
            names.get(&m.folder_id).map_or("?", String::as_str),
            m.server_state,
            m.props.subject.as_deref().unwrap_or("")
        ))
    };
    // 폴더 짝별 수(남길 메일 폴더 → 지울 메일 폴더).
    let mut pairs: std::collections::BTreeMap<String, usize> = Default::default();
    let mut same_time = 0;
    for g in &scan.sets {
        let keep = store.message(g.keep)?;
        for d in &g.drop {
            let m = store.message(*d)?;
            let f = |m: &Option<eo_store::MessageRow>| {
                m.as_ref()
                    .and_then(|m| names.get(&m.folder_id).cloned())
                    .unwrap_or_default()
            };
            *pairs
                .entry(format!("{} → {}", f(&keep), f(&m)))
                .or_default() += 1;
            if keep.as_ref().map(|k| k.props.date_received)
                == m.as_ref().map(|m| m.props.date_received)
            {
                same_time += 1;
            }
        }
    }
    println!("받은 시각까지 같은 지울 메일 {same_time}통(먼저 들어온 것을 남김)");
    // 지울 메일과 남길 메일의 받은 시각 차·Message-ID 관계.
    let synthetic = |id: &str| {
        let l = id.to_ascii_lowercase();
        l.ends_with("@eo.local>") || l.ends_with("@domino.local>") || l.starts_with("<restored-")
    };
    let mut gaps: std::collections::BTreeMap<(&str, &str), usize> = Default::default();
    for g in &scan.sets {
        let Some(k) = store.message(g.keep)? else {
            continue;
        };
        for d in &g.drop {
            let Some(m) = store.message(*d)? else {
                continue;
            };
            let gap = (m.props.date_received - k.props.date_received).abs();
            let bucket = match gap {
                0..60_000 => "1분 안",
                60_000..3_600_000 => "1시간 안",
                3_600_000..86_400_000 => "하루 안",
                _ => "하루 넘게",
            };
            let ids = match (
                k.props.internet_msg_id.as_deref(),
                m.props.internet_msg_id.as_deref(),
            ) {
                (Some(a), Some(b)) if !synthetic(a) && !synthetic(b) => {
                    if a.eq_ignore_ascii_case(b) {
                        "같은 Message-ID"
                    } else {
                        "다른 Message-ID"
                    }
                }
                _ => "만든 ID·없음 섞임",
            };
            *gaps.entry((bucket, ids)).or_default() += 1;
        }
    }
    for ((bucket, ids), n) in gaps {
        println!("  시각 차 {bucket} · {ids}: {n}통");
    }
    for (k, n) in pairs {
        println!("  남김 폴더 → 지울 폴더 {k}: {n}통");
    }
    for g in scan.sets.iter().rev().take(show) {
        println!("남김 {}", line(g.keep)?);
        for d in &g.drop {
            println!("  지움 {}", line(*d)?);
        }
    }
    Ok(())
}

/// 로그인 없이: 가져온 메일(출처별)의 변환 품질을 센다. 내용은 찍지 않고 통수만.
pub fn quality(root: &Path, origin: &str) -> Result<()> {
    let store = Store::open(root)?;
    let ids = store.message_ids_by_origin(origin)?;
    let opts = eo_mime::RenderOptions {
        cid_base: "http://app-cid.localhost/0/".into(),
        allow_remote: false,
    };
    let (mut n, mut no_subject, mut no_from, mut no_rcpt, mut no_body, mut bad_text) =
        (0, 0, 0, 0, 0, 0);
    let (mut with_att, mut atts, mut with_inline, mut missing_cid, mut parse_fail) =
        (0, 0, 0, 0, 0);
    let (mut oldest, mut newest) = (i64::MAX, i64::MIN);
    for id in ids {
        let Some(m) = store.message(id)? else {
            continue;
        };
        let Some(hash) = m.eml_hash.as_deref() else {
            continue;
        };
        let Ok(raw) = store.eml().get(hash) else {
            continue;
        };
        n += 1;
        oldest = oldest.min(m.props.date_received);
        newest = newest.max(m.props.date_received);
        let Some(x) = eo_mime::extract(&raw) else {
            parse_fail += 1;
            continue;
        };
        no_subject += usize::from(x.subject.trim().is_empty());
        no_from += usize::from(x.from.trim().is_empty());
        no_rcpt += usize::from(
            x.to.trim().is_empty() && x.cc.trim().is_empty() && x.bcc.trim().is_empty(),
        );
        let bad = |s: &str| s.contains('\u{FFFD}');
        bad_text += usize::from(bad(&x.subject) || bad(&x.from) || bad(&x.body_text));
        if !x.attachment_names.is_empty() {
            with_att += 1;
            atts += x.attachment_names.len();
        }
        if let Some(v) = eo_mime::render(&raw, &opts) {
            if !v.inline_images.is_empty() {
                with_inline += 1;
            }
            no_body += usize::from(
                x.body_text.trim().is_empty()
                    && v.inline_images.is_empty()
                    && x.attachment_names.is_empty(),
            );
        }
        if let Some(r) = eo_mime::image_report(&raw) {
            missing_cid += usize::from(!r.missing_cids.is_empty());
        }
    }
    println!("출처 {origin}: {n}통 (원문 해석 실패 {parse_fail})");
    if n > 0 {
        println!("  기간 {} ~ {}", kst(oldest), kst(newest));
    }
    println!(
        "  제목 없음 {no_subject} · 보낸 사람 없음 {no_from} · 받는 사람 없음 {no_rcpt} · 본문·그림·첨부 모두 없음 {no_body}"
    );
    println!("  깨진 글자(U+FFFD) 있는 메일 {bad_text}");
    println!(
        "  첨부 있는 메일 {with_att}통(첨부 {atts}개) · 본문 그림 있는 메일 {with_inline}통 · 본문 그림이 빠진 메일 {missing_cid}통"
    );
    Ok(())
}

/// 주소 형태: 숫자 없는 영문 낱말(30자 이하) 경로 조각과 질의 이름만 남기고 나머지는 *로.
fn url_shape(u: &str) -> String {
    let rest = u.split_once("//").map_or(u, |x| x.1);
    let path = rest.split_once('/').map_or("", |x| x.1);
    let (path, query) = path.split_once('?').unwrap_or((path, ""));
    let word = |s: &str| {
        !s.is_empty()
            && s.len() <= 30
            && s.chars()
                .all(|c| c.is_ascii_alphabetic() || matches!(c, '.' | '_' | '-'))
    };
    let p: Vec<&str> = path
        .split('/')
        .map(|s| if word(s) { s } else { "*" })
        .collect();
    let q: Vec<String> = query
        .split('&')
        .filter(|kv| !kv.is_empty())
        .map(|kv| {
            let k = kv.split('=').next().unwrap_or("");
            format!("{}=…", if word(k) { k } else { "*" })
        })
        .collect();
    if q.is_empty() {
        format!("/{}", p.join("/"))
    } else {
        format!("/{}?{}", p.join("/"), q.join("&"))
    }
}

/// 로그인 없이: 이 서버를 가리키는 그림 주소의 형태별 수, 출처·폴더별 메일 수, 예(번호·날짜·폴더).
pub fn image_host(root: &Path, host: &str) -> Result<()> {
    let store = Store::open(root)?;
    let names = folder_names(&store)?;
    let want = host.to_ascii_lowercase();
    let mut shapes: HashMap<String, usize> = HashMap::new();
    let mut by_folder: HashMap<String, usize> = HashMap::new();
    let mut examples: Vec<String> = Vec::new();
    let mut mails = 0;
    for id in store.list_all_ids()? {
        let Some(m) = store.message(id)? else {
            continue;
        };
        let Some(hash) = m.eml_hash.as_deref() else {
            continue;
        };
        let Ok(raw) = store.eml().get(hash) else {
            continue;
        };
        let Some(r) = eo_mime::image_report(&raw) else {
            continue;
        };
        let hits: Vec<&String> = r
            .remote
            .iter()
            .filter(|u| {
                let h = u.split_once("//").map_or("", |x| x.1);
                let h = h.split(['/', '?', '#', ':']).next().unwrap_or("");
                h.eq_ignore_ascii_case(&want)
            })
            .collect();
        if hits.is_empty() {
            continue;
        }
        mails += 1;
        for u in hits {
            *shapes.entry(url_shape(u)).or_default() += 1;
        }
        let folder = names.get(&m.folder_id).map_or("?", String::as_str);
        let key = format!("{folder}({})", m.origin.as_deref().unwrap_or("서버"));
        *by_folder.entry(key).or_default() += 1;
        if examples.len() < 6 {
            examples.push(format!("#{id} {} {folder}", kst(m.props.date_received)));
        }
    }
    let top = |m: &HashMap<String, usize>| {
        let mut v: Vec<_> = m.iter().collect();
        v.sort_by(|a, b| b.1.cmp(a.1));
        v.into_iter()
            .take(12)
            .map(|(k, n)| format!("    {k} {n}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    println!("{host}: 메일 {mails}통");
    println!("  주소 형태:\n{}", top(&shapes));
    println!("  폴더(출처):\n{}", top(&by_folder));
    println!("  예: {}", examples.join(" · "));
    Ok(())
}

/// 로그인 없이: 본문 그림(cid로 쓰는 것)의 실제 형식을 내용으로 가려 센다. 화면(WebView2)이 못 그리는 형식의 예도.
pub fn image_formats(root: &Path) -> Result<()> {
    fn sniff(d: &[u8]) -> &'static str {
        match d {
            [0x89, b'P', b'N', b'G', ..] => "PNG",
            [0xFF, 0xD8, 0xFF, ..] => "JPEG",
            [b'G', b'I', b'F', b'8', ..] => "GIF",
            [b'B', b'M', ..] => "BMP",
            [
                b'R',
                b'I',
                b'F',
                b'F',
                _,
                _,
                _,
                _,
                b'W',
                b'E',
                b'B',
                b'P',
                ..,
            ] => "WEBP",
            [b'I', b'I', 0x2A, 0, ..] | [b'M', b'M', 0, 0x2A, ..] => "TIFF(못 그림)",
            [0xD7, 0xCD, 0xC6, 0x9A, ..] | [1, 0, 9, 0, ..] | [2, 0, 9, 0, ..] => "WMF(못 그림)",
            [0, 0, 1, 0, ..] => "ICO",
            _ if d.len() > 44 && &d[40..44] == b" EMF" => "EMF(못 그림)",
            _ if d.starts_with(b"<svg") || d.starts_with(b"<?xml") => "SVG",
            [] => "빈 내용(못 그림)",
            _ => "알 수 없음(못 그림)",
        }
    }
    let store = Store::open(root)?;
    let names = folder_names(&store)?;
    let opts = eo_mime::RenderOptions {
        cid_base: "http://app-cid.localhost/0/".into(),
        allow_remote: false,
    };
    let mut kinds: HashMap<String, usize> = HashMap::new();
    let mut bad: HashMap<String, Vec<String>> = HashMap::new();
    for id in store.list_all_ids()? {
        let Some(m) = store.message(id)? else {
            continue;
        };
        let Some(hash) = m.eml_hash.as_deref() else {
            continue;
        };
        let Ok(raw) = store.eml().get(hash) else {
            continue;
        };
        let Some(v) = eo_mime::render(&raw, &opts) else {
            continue;
        };
        for i in &v.inline_images {
            let k = sniff(&i.data);
            let key = format!("{k} (표시 형식 {})", i.mime);
            *kinds.entry(key.clone()).or_default() += 1;
            if k.contains("못 그림") {
                let ex = bad.entry(key).or_default();
                if ex.len() < 5 {
                    let folder = names.get(&m.folder_id).map_or("?", String::as_str);
                    ex.push(format!(
                        "#{id} {} {folder}({}) {}",
                        kst(m.props.date_received),
                        m.origin.as_deref().unwrap_or("서버"),
                        fmt_bytes(i.data.len() as u64)
                    ));
                }
            }
        }
    }
    let mut v: Vec<_> = kinds.into_iter().collect();
    v.sort_by_key(|x| std::cmp::Reverse(x.1));
    println!("본문 그림 형식(그림 수):");
    for (k, n) in v {
        println!("  {k} {n}");
        if let Some(ex) = bad.get(&k) {
            println!("    예: {}", ex.join(" · "));
        }
    }
    Ok(())
}

/// 진단용: 이 서버를 가리키는 그림 주소를 서로 다른 메일에서 `n`개 내보낸다(변수로 받아 접속 확인에만 쓴다).
pub fn host_urls(root: &Path, host: &str, n: usize) -> Result<()> {
    let store = Store::open(root)?;
    let mut out = 0;
    for id in store.list_all_ids()?.into_iter().rev() {
        let Some(m) = store.message(id)? else {
            continue;
        };
        let Some(hash) = m.eml_hash.as_deref() else {
            continue;
        };
        let Ok(raw) = store.eml().get(hash) else {
            continue;
        };
        let Some(r) = eo_mime::image_report(&raw) else {
            continue;
        };
        let hit = r.remote.iter().find(|u| {
            let h = u.split_once("//").map_or("", |x| x.1);
            h.split(['/', '?', '#', ':'])
                .next()
                .unwrap_or("")
                .eq_ignore_ascii_case(host)
        });
        if let Some(u) = hit {
            println!("{u}");
            out += 1;
            if out >= n {
                break;
            }
        }
    }
    Ok(())
}

/// 진단용: 메일 하나의 외부 그림 주소를 한 줄씩 내보낸다(PowerShell 변수로 받아 접속 확인에만 쓴다).
pub fn image_urls(root: &Path, id: i64) -> Result<()> {
    let store = Store::open(root)?;
    let m = store.message(id)?.context("메일 없음")?;
    let raw = store
        .eml()
        .get(m.eml_hash.as_deref().context("원문 없음")?)?;
    for u in eo_mime::image_report(&raw)
        .map(|r| r.remote)
        .unwrap_or_default()
    {
        println!("{u}");
    }
    Ok(())
}

/// 로그인 없이: 원문 있는 메일 전체에서 출처별로 cid 그림 상태·상대 경로 그림·외부 그림 서버를 센다.
pub fn images_scan(root: &Path) -> Result<()> {
    #[derive(Default)]
    struct Origin {
        mails: usize,
        cid_mails: usize,
        cid_missing: usize,
        other_mails: usize,
        miss_shapes: HashMap<String, usize>,
        other_shapes: HashMap<String, usize>,
        miss_domains: HashMap<String, usize>,
        miss_match: HashMap<String, usize>,
        loose_pairs: HashMap<String, usize>,
        loose_names: HashMap<String, usize>,
        examples: HashMap<String, Vec<i64>>,
        fixed: usize,
        fixed_examples: Vec<String>,
    }
    let store = Store::open(root)?;
    let names = folder_names(&store)?;
    let mut by: std::collections::BTreeMap<String, Origin> = Default::default();
    let mut hosts: HashMap<String, usize> = HashMap::new();
    for id in store.list_all_ids()? {
        let Some(m) = store.message(id)? else {
            continue;
        };
        let Some(hash) = m.eml_hash.as_deref() else {
            continue;
        };
        let Ok(raw) = store.eml().get(hash) else {
            continue;
        };
        let Some(r) = eo_mime::image_report(&raw) else {
            continue;
        };
        let o = by
            .entry(m.origin.clone().unwrap_or_else(|| "서버".into()))
            .or_default();
        o.mails += 1;
        let has = r.refs.iter().any(|(k, _, _)| k.ends_with("cid(있음)"));
        if has || !r.missing_cids.is_empty() {
            o.cid_mails += 1;
        }
        if let Some(c) = r.missing_cids.first() {
            o.cid_missing += 1;
            *o.miss_shapes.entry(cid_shape(c)).or_default() += 1;
            // 빠진 cid의 @ 앞 이름(image001.png 등)과 같은 이름의 첨부가 있는가.
            let matched = r.missing_cids.iter().filter(|c| {
                let local = c.split('@').next().unwrap_or(c).to_lowercase();
                r.names
                    .iter()
                    .any(|n| *n == local || *n == c.to_lowercase())
            });
            let k = matched.count();
            let loose = r
                .parts
                .iter()
                .filter(|(m, _, cid, _)| m.starts_with("image/") && !cid)
                .count();
            if k == 0 && loose > 0 {
                let mut uniq = r.missing_cids.clone();
                uniq.sort();
                uniq.dedup();
                *o.loose_pairs
                    .entry(format!("빠진 {} / 그림 {loose}", uniq.len()))
                    .or_default() += 1;
                for n in r.names.iter().take(3) {
                    *o.loose_names.entry(name_shape(n)).or_default() += 1;
                }
            }
            let key = if k == 0 && loose > 0 {
                "이름 맞는 첨부 없음(Content-ID 없는 그림 첨부는 있음)"
            } else if k == 0 {
                "이름 맞는 첨부 없음"
            } else if k == r.missing_cids.len() {
                "모두 이름 맞는 첨부 있음"
            } else {
                "일부 이름 맞는 첨부 있음"
            };
            *o.miss_match.entry(key.into()).or_default() += 1;
            let ex = o.examples.entry(key.into()).or_default();
            if ex.len() < 3 {
                ex.push(id);
            }
            let from = m.props.from_addr.clone().unwrap_or_default();
            let d = from
                .rsplit_once('@')
                .map_or("(주소 없음)", |x| x.1)
                .to_owned();
            *o.miss_domains.entry(d).or_default() += 1;
        }
        // 이름·순서로 이어 빠진 그림이 없어진 메일(화면 확인용 예: 번호·날짜·폴더만).
        if r.missing_cids.is_empty() && eo_mime::cid_link_parts(&raw).iter().any(|l| l.2) {
            o.fixed += 1;
            if o.fixed_examples.len() < 3 {
                o.fixed_examples.push(format!(
                    "#{id} {} {}",
                    kst(m.props.date_received),
                    names.get(&m.folder_id).map_or("?", String::as_str)
                ));
            }
        }
        if let Some(v) = r.others.first() {
            o.other_mails += 1;
            *o.other_shapes.entry(path_shape(v)).or_default() += 1;
        }
        let mut seen = std::collections::HashSet::new();
        for (k, h, _) in &r.refs {
            if (k.contains("https") || k.contains("http"))
                && !h.is_empty()
                && seen.insert(h.clone())
            {
                *hosts.entry(h.clone()).or_default() += 1;
            }
        }
    }
    let top = |m: &HashMap<String, usize>, n: usize| {
        let mut v: Vec<_> = m.iter().collect();
        v.sort_by(|a, b| b.1.cmp(a.1));
        v.into_iter()
            .take(n)
            .map(|(k, c)| format!("{k} {c}"))
            .collect::<Vec<_>>()
            .join(" · ")
    };
    for (origin, o) in &by {
        println!(
            "[{origin}] {}통 · cid 그림 쓰는 메일 {} · cid 빠진 메일 {} · 상대 경로 그림 메일 {}",
            o.mails, o.cid_mails, o.cid_missing, o.other_mails
        );
        if o.fixed > 0 {
            println!(
                "  이름·순서로 이어 그림이 모두 보이게 된 메일 {}통 · 예: {}",
                o.fixed,
                o.fixed_examples.join(" · ")
            );
        }
        if o.cid_missing > 0 {
            println!("  빠진 cid 형태: {}", top(&o.miss_shapes, 6));
            println!("  빠진 메일 보낸 도메인: {}", top(&o.miss_domains, 6));
            println!("  같은 이름 첨부: {}", top(&o.miss_match, 3));
            let mut ex: Vec<_> = o.examples.iter().collect();
            ex.sort();
            for (k, ids) in ex {
                println!("    예({k}): {ids:?}");
            }
            if !o.loose_pairs.is_empty() {
                println!(
                    "  Content-ID 없는 그림이 있는 메일: {}",
                    top(&o.loose_pairs, 8)
                );
                println!("  그 첨부 이름 형태: {}", top(&o.loose_names, 10));
            }
        }
        if o.other_mails > 0 {
            println!("  상대 경로 형태: {}", top(&o.other_shapes, 6));
        }
    }
    println!("외부 그림 서버(메일 수, 많은 순 10): {}", top(&hosts, 10));
    Ok(())
}

/// 첨부 이름의 형태: 숫자는 #, 한글 등은 '가', 영문은 그대로(image#.png 같은 이름을 보려는 것).
fn name_shape(n: &str) -> String {
    let mut out = String::new();
    for c in n.chars().take(40) {
        let m = match c {
            c if c.is_ascii_digit() => '#',
            c if c.is_ascii() => c,
            _ => '가',
        };
        if !(matches!(m, '#' | '가') && out.ends_with(m)) {
            out.push(m);
        }
    }
    out
}

/// 상대 경로 그림 주소의 형태: 알려진 폴더 이름과 확장자만 남기고 나머지 이름은 *로 가린다.
fn path_shape(v: &str) -> String {
    const KEEP: [&str; 6] = ["$$_Image_$$", "$$_Attach_$$", "..", ".", "images", "files"];
    let head = v.split(['?', '#']).next().unwrap_or("");
    // "#이름" 같은 짧은 영문 표식은 내용이 아니므로 그대로 보인다.
    if v.len() <= 24
        && v.starts_with('#')
        && v[1..]
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '#'))
    {
        return v.to_owned();
    }
    if head.is_empty() {
        // "#…"·"?…"처럼 시작하는 값: 글자 종류만.
        let cls: String = v
            .chars()
            .take(16)
            .map(|c| match c {
                c if c.is_ascii_alphabetic() => 'a',
                c if c.is_ascii_digit() => '9',
                c if c.is_ascii() => c,
                _ => '가',
            })
            .collect();
        return format!("'{cls}…'({}자)", v.chars().count());
    }
    let segs: Vec<&str> = head.split(['/', '\\']).collect();
    let last = segs.len().saturating_sub(1);
    segs.iter()
        .enumerate()
        .map(|(i, s)| {
            if KEEP.contains(s) || s.is_empty() {
                (*s).to_owned()
            } else if i == last {
                match s.rsplit_once('.') {
                    Some((_, ext))
                        if ext.len() <= 4 && ext.chars().all(|c| c.is_ascii_alphanumeric()) =>
                    {
                        format!("*.{}", ext.to_ascii_lowercase())
                    }
                    _ => "*".into(),
                }
            } else if s.ends_with(':') && s.len() == 2 {
                "C:".into()
            } else {
                "*".into()
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// 오류 줄의 "폴더/파일: 이유"에서 파일 이름(메일 제목이 들어 있다)을 가린다: 폴더와 번호·확장자만 남긴다.
fn mask_file(line: &str) -> String {
    let (path, why) = line.split_once(": ").unwrap_or((line, ""));
    let (dir, file) = path.rsplit_once('/').unwrap_or(("", path));
    let tail = file
        .rfind("_$")
        .map(|i| &file[i..])
        .or_else(|| file.rfind('.').map(|i| &file[i..]))
        .unwrap_or("");
    let dir = if dir.is_empty() {
        String::new()
    } else {
        format!("{dir}/")
    };
    format!("{dir}…{tail}: {why}")
}

/// 로그인 없이: EML 폴더·ZIP, 유저스크립트 HTML 백업, HCL Domino 백업을 로컬 전용 메일로 가져오고 색인한다.
/// `dry_run`이면 저장하지 않고 해석·복원만 한다.
pub fn import(root: &Path, paths: &[std::path::PathBuf], dry_run: bool) -> Result<()> {
    let store = Store::open(root)?;
    let search = open_search(root)?;
    if !dry_run && store.folder_by_well_known("inbox")?.is_none() {
        println!(
            "참고: 서버 폴더 목록이 없는 저장소입니다. 백업의 폴더는 모두 로컬 폴더로 만들어집니다(앱은 이 경우 가져오기를 막습니다)."
        );
    }
    let started = Instant::now();
    let mut last = Instant::now();
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let r = eo_sync::import_paths_with(
        &store,
        paths,
        eo_sync::ImportOptions { dry_run },
        &cancel,
        |p| {
            if last.elapsed() >= Duration::from_secs(5) {
                last = Instant::now();
                println!(
                    "  {}/{} (가져옴 {}, 중복 {}, 건너뜀 {})",
                    p.done, p.total, p.imported, p.duplicates, p.skipped
                );
            }
        },
    )?;
    println!(
        "가져오기{}: 후보 {}통 → {} {}(HTML 백업에서 복원 {}, HCL Domino {}), 이미 있음 {}, 건너뜀 {}, {:.1}초",
        if dry_run {
            "(검사만, 저장 안 함)"
        } else {
            ""
        },
        r.total,
        if dry_run {
            "가져올 수 있음"
        } else {
            "가져옴"
        },
        r.imported,
        r.restored_html,
        r.restored_domino,
        r.duplicates,
        r.skipped,
        started.elapsed().as_secs_f64()
    );
    println!(
        "복원한 EML {} · 가리키는데 없던 파일 {}개 · 아무도 안 가리켜 첨부로 붙인 파일 {}개 · 내려받지 않은 첨부 {}개",
        fmt_bytes(r.eml_bytes),
        r.missing_files,
        r.extra_files,
        r.not_downloaded
    );
    if r.need_login > 0 {
        println!(
            "로그인 전이라(서버 폴더 목록 없음) 넣지 않은 메일 {}통: 한 번 동기화한 뒤 다시 가져오세요",
            r.need_login
        );
    }
    for (folder, n) in &r.per_folder {
        println!("  {folder}: {n}통");
    }
    if !r.created_folders.is_empty() {
        println!("새로 만든 로컬 폴더: {}", r.created_folders.join(", "));
    }
    for e in &r.errors {
        println!("  건너뜀: {}", mask_file(e));
    }
    if r.skipped > r.errors.len() {
        println!("  (건너뜀은 앞 {}개만 보임)", r.errors.len());
    }
    if !dry_run {
        let ix = eo_sync::index_pending(&store, &search, None)?;
        println!("색인: {}통, {:.1}초", ix.indexed, ix.elapsed.as_secs_f64());
    }
    Ok(())
}

fn dir_size(p: &Path) -> u64 {
    std::fs::read_dir(p)
        .map(|rd| {
            rd.flatten()
                .map(|e| match e.metadata() {
                    Ok(m) if m.is_dir() => dir_size(&e.path()),
                    Ok(m) => m.len(),
                    Err(_) => 0,
                })
                .sum()
        })
        .unwrap_or(0)
}

/// unix ms → 한국 시간 `yyyy-mm-dd HH:MM`.
fn kst(ms: i64) -> String {
    let secs = ms.div_euclid(1000) + 9 * 3600;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Howard Hinnant의 civil_from_days
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}",
        rem / 3600,
        (rem / 60) % 60
    )
}

/// 로그인 없이 로컬 검색. 기본은 날짜·폴더·번호만 보여 주고 `show`면 제목·보낸 사람도.
pub fn search(
    root: &Path,
    query: &str,
    folder: Option<&str>,
    relevance: bool,
    show: bool,
) -> Result<()> {
    let store = Store::open(root)?;
    let names = folder_names(&store)?;
    let folder_id = match folder {
        Some(n) => Some(
            names
                .iter()
                .find(|(_, name)| name.as_str() == n)
                .map(|(id, _)| *id as u64)
                .ok_or_else(|| anyhow::anyhow!("'{n}' 폴더가 없습니다"))?,
        ),
        None => None,
    };
    let opened = Instant::now();
    let search = open_search(root)?;
    let open_ms = opened.elapsed();
    let opts = SearchOptions {
        folder: folder_id,
        sort: if relevance {
            Sort::Relevance
        } else {
            Sort::Newest
        },
        ..SearchOptions::default()
    };
    let first = search.search(query, &opts)?;
    let again = search.search(query, &opts)?;
    println!(
        "\"{query}\": {}통 (색인 {}통 중) · 색인 열기 {:.1}ms · 첫 질의 {:.1}ms · 다시 {:.1}ms",
        first.total,
        search.num_docs(),
        open_ms.as_secs_f64() * 1000.0,
        first.elapsed.as_secs_f64() * 1000.0,
        again.elapsed.as_secs_f64() * 1000.0
    );
    for id in first.ids.iter().take(20) {
        let Some(m) = store.message(*id as i64)? else {
            continue;
        };
        let folder = names.get(&m.folder_id).map_or("?", String::as_str);
        let state = if m.server_state == eo_store::server_state::ARCHIVED {
            " (보관)"
        } else {
            ""
        };
        if show {
            println!(
                "  {} · {folder}{state} · #{} · {} · {}",
                kst(m.props.date_received),
                m.id,
                m.props.from_name.as_deref().unwrap_or("?"),
                m.props.subject.as_deref().unwrap_or("")
            );
        } else {
            println!(
                "  {} · {folder}{state} · #{}",
                kst(m.props.date_received),
                m.id
            );
        }
    }
    if first.total > 20 {
        println!("  … 외 {}통", first.total - 20);
    }
    Ok(())
}

fn print_details(store: &Store, r: &CycleReport) -> Result<()> {
    let names = folder_names(store)?;
    let name = |id: &i64| names.get(id).cloned().unwrap_or_else(|| id.to_string());
    for m in &r.items.apply.moves {
        println!(
            "    이동: 메일 #{} {} → {} (원문 {})",
            m.message_id,
            name(&m.from_folder),
            name(&m.to_folder),
            if m.had_eml {
                "유지, 다시 받지 않음"
            } else {
                "아직 없음"
            }
        );
    }
    for a in &r.items.apply.archives {
        println!(
            "    보관: 메일 #{} {}에 그대로 남김 (서버에서 삭제됨, 원문 {})",
            a.message_id,
            name(&a.folder_id),
            if a.had_eml { "있음" } else { "없음" }
        );
    }
    Ok(())
}

/// 한 주기 동기화. `watch`가 있으면 그 간격으로 `minutes`분 동안 반복하며 바뀐 것만 출력한다.
pub async fn sync(
    client: Client,
    root: &Path,
    eml_limit: Option<usize>,
    watch: Option<u64>,
    minutes: u64,
) -> Result<()> {
    let store = Arc::new(Store::open(root)?);
    let engine =
        Engine::new(client, store.clone(), EngineConfig::default()).with_search(open_search(root)?);
    println!("저장 위치: {}", root.display());
    let started = Instant::now();
    let r = engine.cycle(eml_limit).await?;
    print_cycle(&r);
    print_details(&store, &r)?;
    println!("총 {:.1}초", started.elapsed().as_secs_f64());

    let Some(every) = watch else {
        println!();
        return status(root);
    };
    let until = Instant::now() + Duration::from_secs(minutes * 60);
    println!(
        "\n[{}] 감시 시작: {every}초마다, {minutes}분 뒤 자동 종료. 지금 웹메일에서 이동·삭제를 해 보세요.",
        clock()
    );
    while Instant::now() < until {
        tokio::time::sleep(Duration::from_secs(every.max(1))).await;
        match engine.cycle(None).await {
            Ok(r) => {
                let a = &r.items.apply;
                let changed = a.created
                    + a.updated
                    + a.read_flags
                    + a.moves.len()
                    + a.archives.len()
                    + r.eml.stored
                    + r.hierarchy.changes;
                if changed > 0 {
                    println!(
                        "[{}] 새 {} · 갱신 {} · 읽음 {} · 이동 {} · 보관 {} · 원문 받음 {}",
                        clock(),
                        a.created,
                        a.updated,
                        a.read_flags,
                        a.moves.len(),
                        a.archives.len(),
                        r.eml.stored
                    );
                    print_details(&store, &r)?;
                }
            }
            Err(e) => println!("[{}] 오류: {e}", clock()),
        }
    }
    println!("[{}] 감시 끝\n", clock());
    status(root)
}
