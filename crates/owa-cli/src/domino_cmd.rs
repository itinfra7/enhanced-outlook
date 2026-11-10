//! `owa-cli domino`: HCL Domino 웹메일 실계정 확인(0.5.0). 아이디·비밀번호는 터미널에서 받아 메모리에만 둔다.
//! - 기본: 로그인 → 웹메일 서버·메일 파일 → 메일함별 통수·'new' 수·사용량(읽기만, 메일을 열지 않는다).
//! - `--root`: 그 저장소에 가져온다(메일을 연다: 'new'였던 메일은 연 직후 웹메일의 안읽음 표시로 되돌린다).
//!   `--view`·`--max`로 범위를 줄인다. 제목·주소는 출력하지 않는다.

use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use eo_owa::domino::{Domino, DominoConfig, MAIL_VIEWS};
use eo_owa::{Credentials, Error};
use eo_store::Store;
use eo_sync::{DominoOptions, DominoScan, index_pending, sync_domino};

use crate::util::{fmt_bytes, ms};

/// 목록의 제목 열(HTML 꾸밈을 뺀 글자).
fn subject_text(e: &eo_owa::notes::ViewEntry) -> String {
    let raw = e.col("$VC_Subject").unwrap_or("");
    let mut out = String::new();
    let mut in_tag = false;
    for c in raw.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

fn find_in<'a>(
    entries: &'a [eo_owa::notes::ViewEntry],
    unid: &str,
) -> Option<&'a eo_owa::notes::ViewEntry> {
    entries.iter().find(|e| {
        e.unid
            .as_deref()
            .is_some_and(|u| u.eq_ignore_ascii_case(unid))
    })
}

fn has(entries: &[eo_owa::notes::ViewEntry], unid: &str) -> bool {
    find_in(entries, unid).is_some()
}

/// 받은 메일에서 제목에 표시가 든 메일이 딱 한 통이고 이틀 안에 받은 것일 때만 그 메일(본인 시험 메일).
async fn find_test_mail(client: &Domino, marker: &str) -> Result<eo_owa::notes::ViewEntry> {
    if marker.chars().count() < 6 {
        bail!("표시는 6자 이상이어야 합니다");
    }
    let inbox = client.view_all("XML_Inbox", None).await?;
    let mut found: Vec<_> = inbox
        .into_iter()
        .filter(|e| subject_text(e).contains(marker))
        .collect();
    if found.len() != 1 {
        bail!(
            "제목에 \"{marker}\"가 든 받은 메일이 {}통입니다(딱 1통이어야 함). 아무것도 바꾸지 않았습니다",
            found.len()
        );
    }
    let e = found.remove(0);
    let date = e.full_date().unwrap_or("").to_owned();
    let received = eo_mime::backup::parse_kst(&date).unwrap_or(0);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64);
    if now - received > 2 * 86_400_000 {
        bail!("시험 메일이 이틀보다 오래됐습니다({date}). 아무것도 바꾸지 않았습니다");
    }
    e.unid.as_deref().context("문서 번호 없음")?;
    println!(
        "시험 메일: 받은 시각 {date} · 크기 {} · 노트 {} · new {} · ☆ {}",
        e.size().map_or("?".to_owned(), |s| fmt_bytes(s as u64)),
        e.noteid,
        e.is_new(),
        e.star().map_or("?".to_owned(), |s| s.to_string())
    );
    Ok(e)
}

/// `--action-test <표시>`: 0.6.0 양방향 연동이 쓰는 웹메일 동작을 본인 시험 메일 한 통으로 확인한다
/// (사용자 허락을 받은 뒤에만 실행). 읽음 → 안읽음, ☆ 켜기 → 끄기, 받은 메일 → 지운 메일 → 되살리기.
/// 끝나면 메일은 받은 메일에 처음 상태(new·☆)로 남는다.
async fn action_test_run(client: &Domino, marker: &str) -> Result<()> {
    use eo_owa::domino::Mailbox;
    let e = find_test_mail(client, marker).await?;
    let unid = e.unid.clone().unwrap_or_default();
    let note = std::slice::from_ref(&e.noteid);
    let (was_new, was_star) = (e.is_new(), e.star().unwrap_or(false));
    let pause = || tokio::time::sleep(std::time::Duration::from_secs(2));
    let state = async |label: &str| -> Result<()> {
        let inbox = client.view_all("XML_Inbox", None).await?;
        let unread = client.view_all("XML_Unread", None).await?;
        let trash = client.view_all("XML_Trash", None).await?;
        let i = find_in(&inbox, &unid);
        println!(
            "{label} → 받은 메일 {} (new {} · ☆ {} · 노트 {}) · 미확인 목록 {} · 지운 메일 {}",
            i.is_some(),
            i.is_some_and(|x| x.is_new()),
            i.and_then(|x| x.star())
                .map_or("?".to_owned(), |s| s.to_string()),
            i.map_or("-", |x| x.noteid.as_str()),
            has(&unread, &unid),
            has(&trash, &unid)
        );
        Ok(())
    };
    client.set_read(note, true).await?;
    pause().await;
    state("1) 읽음(type=1)").await?;
    client.set_read(note, false).await?;
    pause().await;
    state("2) 안읽음(type=2)").await?;
    client.set_star(&e.noteid, true).await?;
    pause().await;
    state("3) ☆ 켜기(type=N)").await?;
    client.set_star(&e.noteid, false).await?;
    pause().await;
    state("4) ☆ 끄기(type=H)").await?;
    client.delete_docs(&Mailbox::Inbox, note).await?;
    pause().await;
    state("5) 지운 메일로(WebDelDoc2 Inbox)").await?;
    let trash = client.view_all("XML_Trash", None).await?;
    let Some(t) = find_in(&trash, &unid) else {
        bail!("지운 메일에 없어 되살리기를 건너뜀");
    };
    client.recover(std::slice::from_ref(&t.noteid)).await?;
    pause().await;
    state("6) 되살리기(RecoverDelDoc)").await?;
    // 처음 상태로.
    if was_star {
        client.set_star(&e.noteid, true).await?;
    }
    client.set_read(note, !was_new).await?;
    pause().await;
    state("처음 상태로").await?;
    Ok(())
}

/// `--delete-test <표시>`: 자동 정리의 서버 삭제가 실제로 어떻게 되는지 본인 시험 메일 한 통으로 확인한다
/// (사용자 허락을 받은 뒤에만 실행). 웹메일의 삭제(받은 메일 → 지운 메일) → 목록 확인 → 지운 메일에서 삭제 → 목록·용량 확인.
async fn delete_test_run(client: &Domino, marker: &str) -> Result<()> {
    use eo_owa::domino::Mailbox;
    let e = find_test_mail(client, marker).await?;
    let unid = e.unid.clone().unwrap_or_default();
    let before = client.quota().await.ok().and_then(|q| q.0);
    client
        .delete_docs(&Mailbox::Inbox, std::slice::from_ref(&e.noteid))
        .await?;
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    let inbox = client.view_all("XML_Inbox", None).await?;
    let trash = client.view_all("XML_Trash", None).await?;
    let trash_note = trash
        .iter()
        .find(|t| {
            t.unid
                .as_deref()
                .is_some_and(|u| u.eq_ignore_ascii_case(&unid))
        })
        .map(|t| t.noteid.clone());
    println!(
        "1) 받은 메일에서 삭제 → 받은 메일에 남음 {} · 지운 메일에 있음 {} (노트 {})",
        has(&inbox, &unid),
        trash_note.is_some(),
        trash_note.as_deref().unwrap_or("-")
    );
    let mid = client.quota().await.ok().and_then(|q| q.0);
    if let Some(n) = trash_note {
        client
            .delete_docs(&Mailbox::Trash, std::slice::from_ref(&n))
            .await?;
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        let trash = client.view_all("XML_Trash", None).await?;
        println!(
            "2) 지운 메일에서 삭제 → 지운 메일에 남음 {}",
            has(&trash, &unid)
        );
    } else {
        println!("2) 지운 메일에 없어 건너뜀");
    }
    let after = client.quota().await.ok().and_then(|q| q.0);
    let f = |v: Option<i64>| {
        v.map_or("모름".to_owned(), |b| {
            format!("{:.2}MB", b as f64 / 1_048_576.0)
        })
    };
    println!(
        "용량: 처음 {} → 지운 메일로 {} → 완전 삭제 뒤 {}",
        f(before),
        f(mid),
        f(after)
    );
    Ok(())
}

pub async fn run(
    portal: &str,
    root: Option<&Path>,
    view: Option<String>,
    max: Option<usize>,
    delete_test: Option<String>,
    action_test: Option<String>,
    fields: &[String],
) -> Result<()> {
    let client = Domino::new(DominoConfig::new(portal)?);
    let username = crate::prompt::read_username_as("HCL Domino 아이디")?;
    let mut login = None;
    for attempt in 1..=3 {
        let password = crate::prompt::read_password().await?;
        if password.is_empty() {
            bail!("비밀번호가 비었습니다");
        }
        let t = Instant::now();
        match client
            .login(Credentials {
                username: username.clone(),
                password,
            })
            .await
        {
            Ok(info) => {
                login = Some((info, t.elapsed()));
                break;
            }
            Err(Error::BadCredentials { .. }) if attempt < 3 => {
                eprintln!("아이디 또는 비밀번호가 맞지 않습니다. 다시 입력하세요.");
            }
            Err(e) => return Err(e).context("HCL Domino 로그인 실패"),
        }
    }
    let (info, took) = login.context("HCL Domino 로그인 실패")?;
    println!(
        "로그인 성공 {}ms · 웹메일 {} · 메일 파일 {} · 한도 {} · 쿠키 {}",
        ms(took),
        info.location.host,
        info.location.db,
        info.quota_limit
            .map_or("모름".to_owned(), |b| fmt_bytes(b as u64)),
        info.cookie_names.join(", ")
    );
    match client.quota().await {
        Ok((used, limit)) => println!(
            "메일용량확인: 사용량 {} / 한도 {}",
            used.map_or("모름".to_owned(), |b| fmt_bytes(b as u64)),
            limit.map_or("모름".to_owned(), |b| fmt_bytes(b as u64))
        ),
        Err(e) => println!("용량 확인 실패: {e}"),
    }
    if !fields.is_empty() {
        for unid in fields {
            let memo = client.memo(unid).await.context("메일 열기")?;
            println!("==== {unid} · 필드 {}개", memo.fields.len());
            for (name, value) in &memo.fields {
                let v: String = value.chars().take(400).collect();
                println!("{name} = {v}");
            }
        }
        return Ok(());
    }
    let mut views: Vec<&str> = MAIL_VIEWS.iter().map(|(v, _)| *v).collect();
    views.push("XML_Unread");
    for v in views {
        let t = Instant::now();
        match client.view_all(v, None).await {
            Ok(entries) => {
                let new = entries.iter().filter(|e| e.is_new()).count();
                let dated = entries.iter().filter(|e| e.full_date().is_some()).count();
                println!(
                    "{v}: {}통 (new {new}, 날짜 있음 {dated}) {}ms",
                    entries.len(),
                    ms(t.elapsed())
                );
            }
            Err(e) => println!("{v}: 실패 {e}"),
        }
    }
    match client.view_all("FolderTree", None).await {
        Ok(f) => println!("개인 폴더 {}개", f.len()),
        Err(e) => println!("개인 폴더 목록 실패: {e}"),
    }
    if let Some(marker) = delete_test {
        return delete_test_run(&client, &marker).await;
    }
    if let Some(marker) = action_test {
        return action_test_run(&client, &marker).await;
    }
    let Some(root) = root else {
        return Ok(());
    };
    let store = Store::open(root).context("저장소 열기")?;
    let t = Instant::now();
    let cancel = AtomicBool::new(false);
    let mut last = Instant::now();
    let r = sync_domino(
        &client,
        &store,
        DominoScan::Full,
        &DominoOptions {
            max_fetch: max,
            only_view: view,
        },
        &cancel,
        |done, total| {
            if last.elapsed().as_secs() >= 2 {
                last = Instant::now();
                eprintln!("  {done}/{total}");
            }
        },
    )
    .await?;
    println!(
        "가져오기 {}초: 목록 {} · 새 메일 {} · 넣음 {} · 백업에 있던 같은 메일 {} · 실패 {} · 'new' 되돌림 {} (실패 {})",
        t.elapsed().as_secs(),
        r.listed,
        r.new,
        r.imported,
        r.duplicates,
        r.failed,
        r.unread_restored,
        r.unread_restore_failed
    );
    if r.with_missing > 0 {
        println!(
            "그림·첨부 일부를 받지 못하고 넣은 메일 {}통(못 받은 것 {}개)",
            r.with_missing, r.missing_parts
        );
    }
    for e in &r.errors {
        println!("  오류: {e}");
    }
    // 'new' 되돌리기 확인: 가져온 뒤의 미확인 수(그 사이 새 메일이 오면 늘 수 있다).
    match client.view_all("XML_Unread", None).await {
        Ok(e) => println!("가져온 뒤 XML_Unread: {}통", e.len()),
        Err(e) => println!("가져온 뒤 XML_Unread 확인 실패: {e}"),
    }
    if r.imported > 0 {
        let (search, rebuilt) =
            eo_search::SearchIndex::open_checked(&root.join("index")).context("색인 열기")?;
        if rebuilt {
            store.mark_all_unindexed()?;
        }
        let rep = index_pending(&store, &search, None)?;
        println!("색인 {}통", rep.indexed);
    }
    Ok(())
}
