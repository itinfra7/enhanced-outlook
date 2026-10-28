//! `owa-cli probe v070 --marker <표시>`: 0.7.0에서 메일함을 바꾸는 기능을 본인 메일함에서 확인한다
//! (사용자가 대화에서 허락한 뒤에만 실행, 터미널에서는 아이디·비밀번호만 묻는다). 앱과 같은 코드(동기화 엔진의
//! 작업 큐)를 시험 저장소(`--root`, 기본 `target\v070-try`)로 돌린다. 메일 제목·주소는 출력하지 않는다.
//!
//! 1. 임시 보관함 저장: 새 메일(받는 사람 본인, 제목에 표시) + 첨부 a.txt → 서버 초안 1통, 첨부 1개
//! 2. 다시 저장: a.txt를 빼고 b.txt를 붙이고 본문을 바꿈 → 같은 초안, 첨부는 b.txt 하나(`DeleteAttachment`)
//! 3. 보내기: 그 초안을 본인에게 보냄 → 초안이 임시 보관함에서 사라짐
//! 4. 폴더: 받은 편지함 아래 "{표시} 폴더" 만들기 → 이름 바꾸기 → "{표시} 대상" 아래로 이동 → 모두 읽음 →
//!    비우기 → 두 폴더 삭제(지운 편지함으로)

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use eo_owa::{Client, DeleteType, FolderRef, ItemId};
use eo_store::{DraftContent, Recipient, Store};
use eo_sync::{Engine, EngineConfig};

fn mark(ok: bool) -> &'static str {
    if ok { "✓" } else { "✗" }
}

/// 서버 초안의 첨부 이름들(없어졌으면 None).
async fn draft_files(client: &Client, id: &str) -> Result<Option<Vec<String>>> {
    let r = client
        .get_items(
            &[ItemId {
                id: id.to_owned(),
                change_key: None,
            }],
            &["Subject", "Attachments"],
        )
        .await?;
    Ok(match r.into_iter().next() {
        Some(Ok(item)) => Some(
            item.attachments
                .unwrap_or_default()
                .into_iter()
                .filter_map(|a| a.name)
                .collect(),
        ),
        _ => None,
    })
}

/// 지금 시각(ms). 작업은 쌓은 시각부터 돌므로 단계마다 실제 시각을 쓴다(2026-10-23 첫 실행에서 앞선 시각을
/// 넣어 2·3단계 작업이 그 단계에 돌지 않았다).
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// `--rename-check`: 지난 실행의 시험 폴더(`{표시} 폴더`)만 이름을 `{표시} 폴더2`로 바꾸고 서버에서 다시 읽어
/// 확인한다(2026-10-23 첫 실행 뒤 웹메일 화면에 예전 이름이 보여 만들었다. 서버에서는 이미 바뀌어 있었다).
/// `cleanup`이면 끝에 `{표시} 폴더*`·`{표시} 대상`을
/// 완전히 지운다(HardDelete, 되돌릴 수 없음, 허락받은 뒤에만).
pub async fn rename_check(client: &Client, marker: &str, cleanup: bool) -> Result<()> {
    if marker.trim().chars().count() < 6 {
        anyhow::bail!("표시는 6자 이상이어야 합니다");
    }
    let all = client.find_folders_deep().await?;
    let find = |name: &str| {
        all.iter()
            .filter(|f| f.display_name == name)
            .map(|f| f.folder_id.id.clone())
            .collect::<Vec<_>>()
    };
    let first = find(&format!("{marker} 폴더"));
    let target = find(&format!("{marker} 대상"));
    println!(
        "시험 폴더: '{marker} 폴더' {}개 · '{marker} 대상' {}개 · '{marker} 폴더2' {}개",
        first.len(),
        target.len(),
        find(&format!("{marker} 폴더2")).len()
    );
    if let [id] = first.as_slice() {
        let f = FolderRef::Id(id.clone());
        let r = client.rename_folder(&f, &format!("{marker} 폴더2")).await;
        let now = client.get_folder(&f).await.map(|x| x.display_name);
        println!(
            "이름 바꾸기 {} · 요청 {} · 서버에서 다시 읽은 이름 끝이 '폴더2' {}",
            mark(r.is_ok() && now.as_deref().is_ok_and(|n| n.ends_with("폴더2"))),
            if r.is_ok() { "성공" } else { "실패" },
            now.as_deref().is_ok_and(|n| n.ends_with("폴더2"))
        );
        if let Err(e) = r {
            println!("   {e}");
        }
    }
    if cleanup {
        let all = client.find_folders_deep().await?;
        for f in all.iter().filter(|f| {
            f.display_name == format!("{marker} 폴더")
                || f.display_name == format!("{marker} 폴더2")
                || f.display_name == format!("{marker} 대상")
        }) {
            let r = client
                .delete_folder_as(
                    &FolderRef::Id(f.folder_id.id.clone()),
                    DeleteType::HardDelete,
                )
                .await;
            println!("정리(완전 삭제) {}", mark(r.is_ok()));
        }
    }
    Ok(())
}

/// `--resume <시험 저장소>`: 지난 실행에서 남은 작업(보내기 등)만 이어서 돌린다. 새 초안·폴더는 만들지 않는다.
pub async fn resume(client: &Client, root: &Path) -> Result<()> {
    let store = Arc::new(Store::open(root).context("시험 저장소")?);
    let engine = Engine::new(client.clone(), store.clone(), EngineConfig::default());
    let drafts: Vec<_> = store.list_drafts()?;
    for d in &drafts {
        let files = match &d.server_draft_id {
            Some(sid) => draft_files(client, sid).await?,
            None => None,
        };
        println!(
            "남은 초안 #{} · 상태 {} · 서버 초안 {} · 서버 첨부 {:?}",
            d.id,
            d.state,
            if d.server_draft_id.is_some() {
                "있음"
            } else {
                "없음"
            },
            files
        );
    }
    let r = engine.process_ops().await?;
    for (op, why) in &r.abandoned {
        println!("   포기한 작업 {}: {why}", op.kind);
    }
    for d in &drafts {
        let gone = match &d.server_draft_id {
            Some(sid) => draft_files(client, sid).await?.is_none(),
            None => false,
        };
        let removed = store.draft(d.id)?.is_none();
        println!(
            "이어서 실행 {} · 작업 완료 {} · 임시 보관함에서 사라짐 {} · 작성 중 메일 지움 {}",
            mark(gone && removed),
            r.done,
            gone,
            removed
        );
    }
    Ok(())
}

pub async fn run(client: &Client, marker: &str, root: &Path) -> Result<()> {
    if marker.trim().chars().count() < 6 {
        anyhow::bail!("표시는 6자 이상이어야 합니다");
    }
    let root = root.join(format!("{}", crate::util::now_unix()));
    std::fs::create_dir_all(&root)?;
    let store = Arc::new(Store::open(&root).context("시험 저장소")?);
    let cfg = client.user_config().await?;
    let me = Recipient {
        name: cfg.display_name.unwrap_or_default(),
        email: cfg.email_address.context("내 주소 없음")?,
    };
    let engine = Engine::new(client.clone(), store.clone(), EngineConfig::default());
    engine.sync_hierarchy().await?;
    println!("시험 저장소: {}", root.display());

    // 1. 임시 보관함 저장
    let now = now_ms();
    let id = store.create_draft(
        "new",
        None,
        &DraftContent {
            to: vec![me.clone()],
            subject: format!("{marker} 임시 보관함 시험"),
            body_html: "<p>1차 본문</p>".into(),
            ..DraftContent::default()
        },
        now,
    )?;
    let a = root.join("a.txt");
    std::fs::write(&a, b"first attachment")?;
    store.add_draft_file(id, &a, "a.txt")?;
    store.sync_draft_row(id, Some(&me))?;
    store.queue_draft_upload(id, now)?;
    let r = engine.process_ops().await?;
    let sid = store
        .draft(id)?
        .and_then(|d| d.server_draft_id)
        .unwrap_or_default();
    let files = draft_files(client, &sid).await?;
    println!(
        "1) 임시 보관함 저장 {} · 작업 완료 {} · 서버 초안 {} · 첨부 {:?}",
        mark(!sid.is_empty() && files.as_deref() == Some(&["a.txt".to_owned()][..])),
        r.done,
        if sid.is_empty() { "없음" } else { "있음" },
        files
    );
    for (op, why) in &r.abandoned {
        println!("   포기한 작업 {}: {why}", op.kind);
    }

    // 2. 다시 저장(첨부 바꾸기)
    store.remove_draft_file(id, "a.txt")?;
    let b = root.join("b.txt");
    std::fs::write(&b, b"second attachment")?;
    store.add_draft_file(id, &b, "b.txt")?;
    let mut d = store.draft(id)?.context("작성 중 메일 없음")?;
    d.body_html = "<p>2차 본문</p>".into();
    store.save_draft(
        id,
        &DraftContent {
            to: d.to.clone(),
            subject: d.subject.clone(),
            body_html: d.body_html.clone(),
            ..DraftContent::default()
        },
        now_ms(),
    )?;
    store.queue_draft_upload(id, now_ms())?;
    let r = engine.process_ops().await?;
    let sid2 = store
        .draft(id)?
        .and_then(|d| d.server_draft_id)
        .unwrap_or_default();
    let files = draft_files(client, &sid2).await?;
    println!(
        "2) 다시 저장 {} · 작업 완료 {} · 같은 초안 {} · 첨부 {:?}",
        mark(sid2 == sid && files.as_deref() == Some(&["b.txt".to_owned()][..])),
        r.done,
        sid2 == sid,
        files
    );
    for (op, why) in &r.abandoned {
        println!("   포기한 작업 {}: {why}", op.kind);
    }

    // 3. 보내기(본인에게)
    store.queue_send(id, now_ms())?;
    let r = engine.process_ops().await?;
    let gone = draft_files(client, &sid2).await?.is_none();
    println!(
        "3) 보내기 {} · 작업 완료 {} · 임시 보관함에서 사라짐 {} · 작성 중 메일 지움 {}",
        mark(gone && store.draft(id)?.is_none()),
        r.done,
        gone,
        store.draft(id)?.is_none()
    );
    for (op, why) in &r.abandoned {
        println!("   포기한 작업 {}: {why}", op.kind);
    }

    // 4. 폴더 메뉴
    let inbox = FolderRef::distinguished("inbox");
    let step = |n: &str, r: &Result<(), eo_owa::Error>| {
        println!(
            "4) {n} {}{}",
            mark(r.is_ok()),
            r.as_ref()
                .err()
                .map_or(String::new(), |e| format!(" · {e}"))
        );
    };
    let f1 = client
        .create_folder(&inbox, &format!("{marker} 폴더"))
        .await;
    step(
        "새 하위 폴더 만들기",
        &f1.as_ref().map(|_| ()).map_err(clone_err),
    );
    let f2 = client
        .create_folder(&inbox, &format!("{marker} 대상"))
        .await;
    step(
        "두 번째 폴더 만들기",
        &f2.as_ref().map(|_| ()).map_err(clone_err),
    );
    if let (Ok(f1), Ok(f2)) = (f1, f2) {
        let (f1, f2) = (FolderRef::Id(f1), FolderRef::Id(f2));
        step(
            "이름 바꾸기",
            &client.rename_folder(&f1, &format!("{marker} 폴더2")).await,
        );
        step("이동", &client.move_folder(&f1, &f2).await);
        step(
            "모두 읽은 상태로 표시",
            &client.mark_all_read(std::slice::from_ref(&f1)).await,
        );
        step(
            "폴더 비우기",
            &client
                .empty_folder(&f1, DeleteType::MoveToDeletedItems)
                .await,
        );
        step("삭제(첫 폴더)", &client.delete_folder(&f1).await);
        step("삭제(두 번째 폴더)", &client.delete_folder(&f2).await);
    }
    Ok(())
}

fn clone_err(e: &eo_owa::Error) -> eo_owa::Error {
    eo_owa::Error::Decode {
        action: "probe".into(),
        detail: e.to_string(),
    }
}
