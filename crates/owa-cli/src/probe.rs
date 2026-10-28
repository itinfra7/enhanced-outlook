//! `owa-cli probe`: 메일함 상태를 바꾸는 확인(계획서 11장 #5~#10). 사용자가 허락한 항목만,
//! 본인이 본인에게 보낸 테스트 메일(제목에 표시 문자열)로만 실행하고, 실행 전에 터미널에서 y/N을 묻는다.
//! 메일 제목·주소는 출력하지 않고 항목 ID·Message-ID는 짧은 해시로만 보여 준다.

use std::io::{BufRead, Write};

use anyhow::{Context, Result, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use eo_owa::requests::{NewMail, PROP_IN_REPLY_TO, PROP_REFERENCES, ReplyDraft};
use eo_owa::{
    Address, ChangeKind, Client, DeleteType, FileAttachment, FolderRef, ItemChange, ItemError,
    ItemId, ItemSummary, ReplyKind,
};

use crate::util::{date_stamp, fmt_bytes, item_sync, resolve_folder, short_hash};

const PAGE: u32 = 200;

fn confirm(question: &str) -> Result<()> {
    eprint!("{question} [y/N]: ");
    std::io::stderr().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    if line.trim().eq_ignore_ascii_case("y") {
        Ok(())
    } else {
        bail!("취소했습니다. 메일함은 바뀌지 않았습니다.")
    }
}

fn hash_opt(s: Option<&str>) -> String {
    s.map_or("?".into(), short_hash)
}

/// 회신·전달 메일(앞선 확인에서 생긴 것)은 원본 찾기에서 뺀다.
fn is_reply_subject(subject: &str) -> bool {
    let s = subject.trim_start().to_lowercase();
    ["re:", "fw:", "fwd:", "회신:", "전달:"]
        .iter()
        .any(|p| s.starts_with(p))
}

/// EML 머리글에서 이름이 `name`인 값(접힌 줄 이어 붙임).
fn header_value(eml: &[u8], name: &str) -> Option<String> {
    let text = String::from_utf8_lossy(eml);
    let head = text
        .split("\r\n\r\n")
        .next()
        .unwrap_or("")
        .split("\n\n")
        .next()
        .unwrap_or("");
    let prefix = format!("{}:", name.to_ascii_lowercase());
    let mut value: Option<String> = None;
    for line in head.lines() {
        let line = line.trim_end_matches('\r');
        match &mut value {
            Some(v) if line.starts_with([' ', '\t']) => v.push_str(line.trim()),
            Some(_) => break,
            None if line.to_ascii_lowercase().starts_with(&prefix) => {
                value = Some(line[prefix.len()..].trim().to_owned());
            }
            None => {}
        }
    }
    value
}

/// 받은 편지함에서 제목에 `marker`가 들어 있고 본인이 보낸 메일을 정확히 1통 찾는다.
async fn find_test_mail(client: &Client, inbox: &FolderRef, marker: &str) -> Result<ItemSummary> {
    let me = client
        .user_config()
        .await?
        .email_address
        .context("내 주소를 알 수 없습니다")?;
    let props = [
        "Subject",
        "DateTimeReceived",
        "Size",
        "From",
        "InternetMessageId",
    ];
    let mut found = Vec::new();
    let mut offset = 0;
    loop {
        let page = client.find_items(inbox, offset, PAGE, &props).await?;
        let n = page.items.len();
        found.extend(page.items.into_iter().filter(|i| {
            i.subject
                .as_deref()
                .is_some_and(|s| s.contains(marker) && !is_reply_subject(s))
        }));
        offset += u32::try_from(n).unwrap_or(PAGE);
        if n < PAGE as usize {
            break;
        }
    }
    let mail = match found.len() {
        1 => found.remove(0),
        0 => bail!(
            "받은 편지함에 제목에 '{marker}'가 들어간 메일이 없습니다. 본인에게 테스트 메일을 먼저 보내 주세요."
        ),
        n => bail!("제목에 '{marker}'가 들어간 메일이 {n}통입니다. 정확히 1통일 때만 진행합니다."),
    };
    let from = mail
        .from
        .as_ref()
        .and_then(|r| r.mailbox.as_ref())
        .and_then(|m| m.email_address.as_deref())
        .unwrap_or("");
    if !from.eq_ignore_ascii_case(&me) {
        bail!(
            "찾은 메일의 보낸 사람이 본인 주소가 아닙니다. 본인이 보낸 테스트 메일로만 진행합니다."
        );
    }
    Ok(mail)
}

/// 관련 폴더의 증분 동기화 상태를 들고 있다가, 작업마다 무엇이 바뀌었는지 보여 준다.
struct Tracker {
    folders: Vec<(String, FolderRef, String)>,
}

impl Tracker {
    async fn new(client: &Client, folders: Vec<(String, FolderRef)>) -> Result<Self> {
        let mut out = Vec::new();
        for (name, folder) in folders {
            let state = item_sync(client, &folder, None).await?.sync_state;
            out.push((name, folder, state));
        }
        Ok(Self { folders: out })
    }

    async fn changes(&mut self, client: &Client) -> Result<Vec<(String, ItemChange)>> {
        let mut out = Vec::new();
        for (name, folder, state) in &mut self.folders {
            let l = item_sync(client, folder, Some(state)).await?;
            *state = l.sync_state;
            out.extend(l.changes.into_iter().map(|c| (name.clone(), c)));
        }
        for (folder, c) in &out {
            println!(
                "      변경: {folder}: {} 항목 {} · Message-ID {}",
                c.type_name,
                hash_opt(c.item_id.as_ref().map(|i| i.id.as_str())),
                hash_opt(
                    c.item
                        .as_ref()
                        .and_then(|i| i.internet_message_id.as_deref())
                ),
            );
        }
        if out.is_empty() {
            println!("      변경: 없음");
        }
        Ok(out)
    }
}

/// 항목 하나짜리 이동·복사 결과에서 새 ID를 꺼낸다.
fn one_new_id(action: &str, r: Vec<Result<Option<ItemId>, ItemError>>) -> Result<Option<ItemId>> {
    match r.into_iter().next() {
        Some(Ok(id)) => {
            println!(
                "    ✓ {action} 성공, 새 항목 ID {}",
                hash_opt(id.as_ref().map(|i| i.id.as_str()))
            );
            Ok(id)
        }
        Some(Err(e)) => bail!("{action} 실패: {e}"),
        None => bail!("{action} 응답 없음"),
    }
}

fn one_ok(action: &str, r: Vec<Result<(), ItemError>>) -> Result<()> {
    match r.into_iter().next() {
        Some(Ok(())) => {
            println!("    ✓ {action} 성공");
            Ok(())
        }
        Some(Err(e)) => bail!("{action} 실패: {e}"),
        None => bail!("{action} 응답 없음"),
    }
}

/// 변경 목록에서 `folder`에 새로 생긴 항목 ID(응답에 새 ID가 없을 때 대신 쓴다).
fn created_in(changes: &[(String, ItemChange)], folder: &str) -> Option<ItemId> {
    changes
        .iter()
        .find(|(f, c)| f == folder && c.kind == ChangeKind::Create)
        .and_then(|(_, c)| c.item_id.clone())
}

/// #5: 테스트 메일 복사본으로 CopyItem → MoveItem → DeleteItem(지운 편지함) → DeleteItem(SoftDelete),
/// `hard_delete`면 복사본을 하나 더 만들어 DeleteItem(HardDelete). 원본은 건드리지 않는다.
pub async fn items(
    client: &Client,
    marker: &str,
    copy_to: &str,
    move_to: &str,
    hard_delete: bool,
) -> Result<()> {
    let inbox = client
        .get_folder(&FolderRef::distinguished("inbox"))
        .await?;
    let inbox_ref = FolderRef::from(&inbox.folder_id);
    let mail = find_test_mail(client, &inbox_ref, marker).await?;
    let original = mail
        .item_id
        .clone()
        .context("테스트 메일에 항목 ID가 없습니다")?;
    let original = ItemId {
        id: original.id,
        change_key: None,
    };
    let (copy_ref, copy_name) = resolve_folder(client, copy_to).await?;
    let (move_ref, move_name) = resolve_folder(client, move_to).await?;
    let deleted = client
        .get_folder(&FolderRef::distinguished("deleteditems"))
        .await?;
    let deleted_name = deleted.display_name.clone();
    let deleted_ref = FolderRef::from(&deleted.folder_id);

    println!(
        "테스트 메일: 받은 시각 {}, {}, 항목 {} · Message-ID {} (보낸 사람 = 본인 확인됨)",
        date_stamp(mail.date_time_received.as_deref()),
        fmt_bytes(mail.size.unwrap_or(0).max(0) as u64),
        short_hash(&original.id),
        hash_opt(mail.internet_message_id.as_deref())
    );
    println!("실행할 단계 (원본은 그대로 둠):");
    println!("  1. CopyItem: 원본 → {copy_name} (복사본 A)");
    println!("  2. MoveItem: 복사본 A {copy_name} → {move_name}");
    println!("  3. DeleteItem(MoveToDeletedItems): 복사본 A → {deleted_name}");
    println!("  4. DeleteItem(SoftDelete): {deleted_name}의 복사본 A → 복구 가능한 항목");
    if hard_delete {
        println!("  5. CopyItem: 원본 → {copy_name} (복사본 B)");
        println!("  6. DeleteItem(HardDelete): 복사본 B 영구 삭제");
    }
    confirm("위 단계를 실행할까요?")?;

    let mut tracker = Tracker::new(
        client,
        vec![
            (inbox.display_name.clone(), inbox_ref.clone()),
            (copy_name.clone(), copy_ref.clone()),
            (move_name.clone(), move_ref.clone()),
            (deleted_name.clone(), deleted_ref),
        ],
    )
    .await?;

    println!("\n[1] CopyItem → {copy_name}");
    let id = one_new_id(
        "CopyItem",
        client.copy_items(slice(&original), &copy_ref).await?,
    )?;
    let ch = tracker.changes(client).await?;
    let a = id
        .or_else(|| created_in(&ch, &copy_name))
        .context("복사본 A의 ID를 알 수 없습니다")?;

    println!("\n[2] MoveItem {copy_name} → {move_name}");
    let id = one_new_id("MoveItem", client.move_items(slice(&a), &move_ref).await?)?;
    let ch = tracker.changes(client).await?;
    let a = id
        .or_else(|| created_in(&ch, &move_name))
        .context("이동한 복사본 A의 ID를 알 수 없습니다")?;

    println!("\n[3] DeleteItem(MoveToDeletedItems) → {deleted_name}");
    one_ok(
        "DeleteItem(MoveToDeletedItems)",
        client
            .delete_items(slice(&a), DeleteType::MoveToDeletedItems)
            .await?,
    )?;
    let ch = tracker.changes(client).await?;
    let a = created_in(&ch, &deleted_name)
        .context("지운 편지함으로 간 복사본 A의 ID를 알 수 없습니다")?;

    println!("\n[4] DeleteItem(SoftDelete) {deleted_name}");
    one_ok(
        "DeleteItem(SoftDelete)",
        client
            .delete_items(slice(&a), DeleteType::SoftDelete)
            .await?,
    )?;
    tracker.changes(client).await?;

    if hard_delete {
        println!("\n[5] CopyItem → {copy_name} (복사본 B)");
        let id = one_new_id(
            "CopyItem",
            client.copy_items(slice(&original), &copy_ref).await?,
        )?;
        let ch = tracker.changes(client).await?;
        let b = id
            .or_else(|| created_in(&ch, &copy_name))
            .context("복사본 B의 ID를 알 수 없습니다")?;

        println!("\n[6] DeleteItem(HardDelete) 복사본 B");
        one_ok(
            "DeleteItem(HardDelete)",
            client
                .delete_items(slice(&b), DeleteType::HardDelete)
                .await?,
        )?;
        tracker.changes(client).await?;
    }

    // 원본이 그대로인지 확인한다.
    let still = client.get_mime(slice(&original)).await?;
    match still.first() {
        Some(Ok(bytes)) => println!(
            "\n원본 확인: 받은 편지함에 그대로 있음 ({})",
            fmt_bytes(bytes.len() as u64)
        ),
        Some(Err(e)) => println!("\n주의: 원본을 읽을 수 없습니다: {e}"),
        None => println!("\n주의: 원본 확인 응답 없음"),
    }
    println!("#5 완료");
    Ok(())
}

fn slice(id: &ItemId) -> &[ItemId] {
    std::slice::from_ref(id)
}

/// 초안을 정리한다(이 도구가 만든 미발송 초안만).
async fn discard_draft(client: &Client, draft: &ItemId) {
    let bare = ItemId {
        id: draft.id.clone(),
        change_key: None,
    };
    match client
        .delete_items(slice(&bare), DeleteType::HardDelete)
        .await
    {
        Ok(r) if r.iter().all(Result::is_ok) => println!("    초안 정리(HardDelete) 완료"),
        Ok(r) => println!("    주의: 초안 정리 실패 {r:?} — 임시 보관함에서 직접 지워 주세요"),
        Err(e) => println!("    주의: 초안 정리 실패 {e} — 임시 보관함에서 직접 지워 주세요"),
    }
}

/// #7 회신 초안(ReferenceItemDocumentId 없이, 발송 안 함) → 정리,
/// #6 전체 회신 초안 → 받는 사람이 본인뿐인지 확인 → 본인에게 발송 → 스레드 머리글 확인.
pub async fn replies(client: &Client, marker: &str) -> Result<()> {
    let cfg = client.user_config().await?;
    let me = Address {
        name: cfg.display_name.clone().unwrap_or_default(),
        email: cfg
            .email_address
            .clone()
            .context("내 주소를 알 수 없습니다")?,
    };
    let inbox = client
        .get_folder(&FolderRef::distinguished("inbox"))
        .await?;
    let inbox_ref = FolderRef::from(&inbox.folder_id);
    let mail = find_test_mail(client, &inbox_ref, marker).await?;
    let original = mail
        .item_id
        .clone()
        .context("테스트 메일에 항목 ID가 없습니다")?;
    let original_mid = mail.internet_message_id.clone().unwrap_or_default();
    let subject = format!("RE: {}", mail.subject.as_deref().unwrap_or(""));
    let drafts = client
        .get_folder(&FolderRef::distinguished("drafts"))
        .await?;
    let sent = client
        .get_folder(&FolderRef::distinguished("sentitems"))
        .await?;
    println!(
        "테스트 메일: 받은 시각 {}, 항목 {} · Message-ID {} (보낸 사람 = 본인 확인됨)",
        date_stamp(mail.date_time_received.as_deref()),
        short_hash(&original.id),
        hash_opt(Some(&original_mid))
    );
    let mut tracker = Tracker::new(
        client,
        vec![
            (inbox.display_name.clone(), inbox_ref.clone()),
            (
                drafts.display_name.clone(),
                FolderRef::from(&drafts.folder_id),
            ),
            (sent.display_name.clone(), FolderRef::from(&sent.folder_id)),
        ],
    )
    .await?;

    // #7
    println!(
        "\n[#7] CreateItem(ReplyToItem, SaveOnly), ReferenceItemDocumentId 없음 → 초안 확인 → 초안 HardDelete (발송 없음)"
    );
    confirm("#7을 실행할까요?")?;
    let draft = client
        .create_reply_draft(&ReplyDraft {
            kind: ReplyKind::Reply,
            reference: &original,
            from: &me,
            to: slice_addr(&me),
            cc: &[],
            bcc: &[],
            subject: &subject,
            body_html: "<p>Outlook 클라이언트 #7 회신 초안 테스트 (보내지 않음)</p>",
        })
        .await;
    match draft {
        Ok(id) => {
            println!(
                "    ✓ ReferenceItemDocumentId 없이 회신 초안 생성 성공, 초안 {}",
                short_hash(&id.id)
            );
            tracker.changes(client).await?;
            discard_draft(client, &id).await;
            tracker.changes(client).await?;
        }
        Err(e) => println!("    ✗ 회신 초안 생성 실패: {e}"),
    }

    // #6
    println!(
        "\n[#6] CreateItem(ReplyAllToItem, SaveOnly) → 받는 사람 확인 → UpdateItem(SendAndSaveCopy)로 본인에게 1통 발송"
    );
    let draft = client
        .create_reply_draft(&ReplyDraft {
            kind: ReplyKind::ReplyAll,
            reference: &original,
            from: &me,
            to: slice_addr(&me),
            cc: &[],
            bcc: &[],
            subject: &subject,
            body_html: "<p>Outlook 클라이언트 #6 전체 회신 테스트</p>",
        })
        .await
        .context("전체 회신 초안 생성 실패")?;
    println!("    ✓ 전체 회신 초안 생성, 초안 {}", short_hash(&draft.id));
    tracker.changes(client).await?;

    // 발송 전 안전 확인: 서버에 저장된 초안의 받는 사람·참조·숨은 참조가 모두 본인뿐이어야 한다.
    let read = client
        .get_items(
            slice(&draft),
            &["ToRecipients", "CcRecipients", "BccRecipients"],
        )
        .await?;
    let Some(Ok(item)) = read.into_iter().next() else {
        discard_draft(client, &draft).await;
        bail!("초안을 다시 읽지 못해 보내지 않았습니다");
    };
    let lists = [
        ("받는 사람", item.to_recipients.unwrap_or_default()),
        ("참조", item.cc_recipients.unwrap_or_default()),
        ("숨은 참조", item.bcc_recipients.unwrap_or_default()),
    ];
    let mut all_me = true;
    let mut total = 0;
    for (label, list) in &lists {
        let mine = list
            .iter()
            .filter(|r| r.email().is_some_and(|e| e.eq_ignore_ascii_case(&me.email)))
            .count();
        total += list.len();
        all_me &= mine == list.len();
        println!("    초안 {label}: {}명 (본인 {mine}명)", list.len());
    }
    if total == 0 || !all_me {
        discard_draft(client, &draft).await;
        bail!("초안에 본인 외 주소가 있거나 받는 사람이 없어 보내지 않았습니다");
    }
    let latest = item.item_id.unwrap_or(draft.clone());
    if let Err(e) = confirm("받는 사람이 본인뿐입니다. 이 전체 회신을 본인 주소로 1통 보낼까요?")
    {
        discard_draft(client, &latest).await;
        return Err(e);
    }
    client
        .send_draft(
            &latest,
            slice_addr(&me),
            ReplyKind::ReplyAll.compose_operation(),
        )
        .await
        .context("발송 실패")?;
    println!("    ✓ 발송 요청 성공. 도착을 기다립니다(최대 60초)…");

    let mut received = None;
    for _ in 0..12 {
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        let ch = tracker.changes(client).await?;
        received = ch.into_iter().find_map(|(folder, c)| {
            let mid = c.item.as_ref()?.internet_message_id.clone()?;
            (folder == inbox.display_name && c.kind == ChangeKind::Create && mid != original_mid)
                .then_some(c.item_id?)
        });
        if received.is_some() {
            break;
        }
    }
    let Some(received) = received else {
        println!(
            "    주의: 60초 안에 받은 편지함에 도착하지 않았습니다. 웹메일에서 확인해 주세요."
        );
        return Ok(());
    };
    let eml = match client.get_mime(slice(&received)).await?.into_iter().next() {
        Some(Ok(b)) => b,
        other => bail!("받은 회신 원문을 읽지 못했습니다: {other:?}"),
    };
    let in_reply_to = header_value(&eml, "In-Reply-To").unwrap_or_default();
    let references = header_value(&eml, "References").unwrap_or_default();
    println!(
        "    받은 회신 {}: In-Reply-To = 원본 Message-ID {}, References에 원본 포함 {}",
        short_hash(&received.id),
        if in_reply_to.contains(&original_mid) {
            "예"
        } else {
            "아니오"
        },
        if references.contains(&original_mid) {
            "예"
        } else {
            "아니오"
        }
    );
    println!("#7·#6 완료");
    Ok(())
}

fn slice_addr(a: &Address) -> &[Address] {
    std::slice::from_ref(a)
}

/// 1×1 PNG(#8 인라인 이미지용).
const TINY_PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";
const PROBE_CID: &str = "outlook-probe-8";

/// y면 true, 그 밖은 false(건너뛰기).
fn ask(question: &str) -> Result<bool> {
    eprint!("{question} [y/N]: ");
    std::io::stderr().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(line.trim().eq_ignore_ascii_case("y"))
}

/// 발송 전 안전 확인: 서버에 저장된 초안의 받는 사람·참조·숨은 참조가 모두 본인뿐인지 다시 읽는다.
/// 맞으면 (최신 ChangeKey가 담긴 초안 ID, 초안 Message-ID).
async fn only_me(
    client: &Client,
    draft: &ItemId,
    me: &Address,
) -> Result<Option<(ItemId, String)>> {
    let read = client
        .get_items(
            slice(draft),
            &[
                "ToRecipients",
                "CcRecipients",
                "BccRecipients",
                "InternetMessageId",
            ],
        )
        .await?;
    let Some(Ok(item)) = read.into_iter().next() else {
        return Ok(None);
    };
    let lists = [
        ("받는 사람", item.to_recipients.clone().unwrap_or_default()),
        ("참조", item.cc_recipients.clone().unwrap_or_default()),
        ("숨은 참조", item.bcc_recipients.clone().unwrap_or_default()),
    ];
    let mut all_me = true;
    let mut total = 0;
    for (label, list) in &lists {
        let mine = list
            .iter()
            .filter(|r| r.email().is_some_and(|e| e.eq_ignore_ascii_case(&me.email)))
            .count();
        total += list.len();
        all_me &= mine == list.len();
        println!("    초안 {label}: {}명 (본인 {mine}명)", list.len());
    }
    if total == 0 || !all_me {
        return Ok(None);
    }
    let mid = item.internet_message_id.clone().unwrap_or_default();
    Ok(Some((item.item_id.unwrap_or_else(|| draft.clone()), mid)))
}

/// 보낸 메일이 받은 편지함에 도착할 때까지(Message-ID 일치) 최대 60초 기다린다.
async fn wait_for_arrival(
    client: &Client,
    tracker: &mut Tracker,
    inbox: &str,
    message_id: &str,
) -> Result<Option<ItemId>> {
    for _ in 0..12 {
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        let ch = tracker.changes(client).await?;
        let found = ch.into_iter().find_map(|(folder, c)| {
            let mid = c.item.as_ref()?.internet_message_id.clone()?;
            (folder == inbox && c.kind == ChangeKind::Create && mid == message_id)
                .then_some(c.item_id?)
        });
        if found.is_some() {
            return Ok(found);
        }
    }
    Ok(None)
}

/// #9용 원문: 본문 + 원본 메일을 `message/rfc822`로 첨부(계획서 5.8 대안: 전달 시 원본 첨부).
fn probe_mime(me: &Address, subject: &str, attached: &[u8]) -> Vec<u8> {
    let b = "outlook-probe-9-boundary";
    let mut m = format!(
        "From: <{e}>\r\nTo: <{e}>\r\nSubject: =?UTF-8?B?{subject}?=\r\nMIME-Version: 1.0\r\n\
         Content-Type: multipart/mixed; boundary=\"{b}\"\r\n\r\n\
         --{b}\r\nContent-Type: text/plain; charset=UTF-8\r\nContent-Transfer-Encoding: base64\r\n\r\n{body}\r\n\
         --{b}\r\nContent-Type: message/rfc822\r\nContent-Disposition: attachment; filename=\"original.eml\"\r\n\r\n",
        e = me.email,
        subject = STANDARD.encode(subject),
        body = STANDARD.encode("Outlook 클라이언트 #9 MIME 초안 테스트 (보내지 않음)"),
    )
    .into_bytes();
    m.extend_from_slice(attached);
    m.extend_from_slice(format!("\r\n--{b}--\r\n").as_bytes());
    m
}

fn yes_no(b: bool) -> &'static str {
    if b { "예" } else { "아니오" }
}

/// #8 인라인 이미지 초안, #9 MIME 초안(둘 다 발송 없이 정리), #10 확장 속성 스레드 머리글을 넣은
/// 새 메일을 본인에게 발송. 항목마다 y/N을 묻고 n이면 그 항목만 건너뛴다.
pub async fn drafts(client: &Client, marker: &str) -> Result<()> {
    let cfg = client.user_config().await?;
    let me = Address {
        name: cfg.display_name.clone().unwrap_or_default(),
        email: cfg
            .email_address
            .clone()
            .context("내 주소를 알 수 없습니다")?,
    };
    let inbox = client
        .get_folder(&FolderRef::distinguished("inbox"))
        .await?;
    let inbox_ref = FolderRef::from(&inbox.folder_id);
    let mail = find_test_mail(client, &inbox_ref, marker).await?;
    let original = mail
        .item_id
        .clone()
        .context("테스트 메일에 항목 ID가 없습니다")?;
    let original_mid = mail.internet_message_id.clone().unwrap_or_default();
    let original_eml = match client.get_mime(slice(&original)).await?.into_iter().next() {
        Some(Ok(b)) => b,
        other => bail!("원본 원문을 읽지 못했습니다: {other:?}"),
    };
    let drafts = client
        .get_folder(&FolderRef::distinguished("drafts"))
        .await?;
    let drafts_ref = FolderRef::from(&drafts.folder_id);
    let sent = client
        .get_folder(&FolderRef::distinguished("sentitems"))
        .await?;
    println!(
        "테스트 메일: 받은 시각 {}, 항목 {} · Message-ID {} (보낸 사람 = 본인 확인됨)",
        date_stamp(mail.date_time_received.as_deref()),
        short_hash(&original.id),
        hash_opt(Some(&original_mid))
    );
    let mut tracker = Tracker::new(
        client,
        vec![
            (inbox.display_name.clone(), inbox_ref.clone()),
            (drafts.display_name.clone(), drafts_ref.clone()),
            (sent.display_name.clone(), FolderRef::from(&sent.folder_id)),
        ],
    )
    .await?;

    // #8
    println!(
        "\n[#8] 새 메일 초안(본인, 발송 안 함) → 1×1 PNG를 IsInline + ContentId로 첨부 → 초안 MIME 확인 → 초안 HardDelete"
    );
    if ask("#8을 실행할까요?")? {
        let draft = client
            .create_new_mail_draft(&NewMail {
                from: &me,
                to: slice_addr(&me),
                cc: &[],
                bcc: &[],
                subject: "Outlook-TEST #8 인라인 이미지 초안",
                body_html: &format!(
                    "<p>Outlook 클라이언트 #8 인라인 이미지 테스트 (보내지 않음)</p><p><img src=\"cid:{PROBE_CID}\"></p>"
                ),
                extended: &[],
            })
            .await
            .context("#8 초안 생성 실패")?;
        println!("    ✓ 새 메일 초안 생성, 초안 {}", short_hash(&draft.id));
        tracker.changes(client).await?;
        let attachment = FileAttachment {
            name: format!("{PROBE_CID}.png"),
            content_type: "image/png".into(),
            content: STANDARD.decode(TINY_PNG)?,
            is_inline: true,
            content_id: Some(PROBE_CID.into()),
        };
        match client.create_attachment(&draft, &attachment).await {
            Ok((att, cid)) => {
                println!(
                    "    ✓ 인라인 첨부 성공, 첨부 {}, 초안 새 ChangeKey {}, 응답 ContentId {}",
                    short_hash(&att.id),
                    if att.root_item_change_key.is_some() {
                        "받음"
                    } else {
                        "없음"
                    },
                    cid.as_deref().unwrap_or("-")
                );
                let bare = ItemId {
                    id: draft.id.clone(),
                    change_key: None,
                };
                match client.get_mime(slice(&bare)).await?.into_iter().next() {
                    Some(Ok(eml)) => {
                        let text = String::from_utf8_lossy(&eml).to_ascii_lowercase();
                        let has_cid = text
                            .lines()
                            .any(|l| l.starts_with("content-id:") && l.contains(PROBE_CID));
                        let inline = text.contains("content-disposition: inline");
                        let body_ref = text.contains(&format!("cid:{PROBE_CID}"));
                        println!(
                            "    초안 MIME: Content-ID 파트 {}, inline 표시 {}, 본문 cid 참조 {}",
                            yes_no(has_cid),
                            yes_no(inline),
                            yes_no(body_ref)
                        );
                    }
                    other => println!("    주의: 초안 원문을 읽지 못했습니다: {other:?}"),
                }
            }
            Err(e) => println!("    ✗ 인라인 첨부 실패: {e}"),
        }
        discard_draft(client, &draft).await;
        tracker.changes(client).await?;
    } else {
        println!("    #8 건너뜀");
    }

    // #9
    let subject9 = "Outlook-TEST #9 MIME 초안";
    println!(
        "\n[#9] CreateItem(MimeContent, SaveOnly) → 임시 보관함: 본인→본인, 원본을 message/rfc822로 첨부 → 확인 → HardDelete (발송 없음)"
    );
    if ask("#9를 실행할까요?")? {
        let mime = probe_mime(&me, subject9, &original_eml);
        match client.create_item_from_mime(&mime, &drafts_ref).await {
            Ok(id) => {
                println!("    ✓ MIME으로 항목 생성 성공, 항목 {}", short_hash(&id.id));
                tracker.changes(client).await?;
                let props = client
                    .get_items(slice(&id), &["Subject", "HasAttachments", "IsDraft"])
                    .await?;
                if let Some(Ok(item)) = props.first() {
                    println!(
                        "    제목 일치 {}, 첨부 있음 {}, 초안 표시 {}",
                        yes_no(item.subject.as_deref() == Some(subject9)),
                        yes_no(item.has_attachments == Some(true)),
                        item.is_draft.map_or("?", yes_no)
                    );
                }
                let bare = ItemId {
                    id: id.id.clone(),
                    change_key: None,
                };
                if let Some(Ok(eml)) = client.get_mime(slice(&bare)).await?.into_iter().next() {
                    let text = String::from_utf8_lossy(&eml).to_ascii_lowercase();
                    println!(
                        "    저장된 원문에 message/rfc822 첨부 {}",
                        yes_no(text.contains("message/rfc822"))
                    );
                }
                discard_draft(client, &id).await;
                tracker.changes(client).await?;
            }
            Err(e) => println!("    ✗ MIME으로 항목 생성 실패: {e}"),
        }
    } else {
        println!("    #9 건너뜀");
    }

    // #10
    println!(
        "\n[#10] 새 메일(본인) + 확장 속성 0x1042(In-Reply-To)·0x1039(References) → 받는 사람 확인 → 본인에게 1통 발송 → 받은 원문 머리글 확인"
    );
    if !ask("#10 초안을 만들까요? (발송 전에 한 번 더 묻습니다)")? {
        println!("    #10 건너뜀");
        return Ok(());
    }
    let references = header_value(&original_eml, "References")
        .filter(|r| !r.is_empty())
        .map_or_else(|| original_mid.clone(), |r| format!("{r} {original_mid}"));
    let extended = [
        (PROP_IN_REPLY_TO, original_mid.clone()),
        (PROP_REFERENCES, references),
    ];
    let draft = client
        .create_new_mail_draft(&NewMail {
            from: &me,
            to: slice_addr(&me),
            cc: &[],
            bcc: &[],
            subject: "RE: Outlook-TEST #10 스레드 머리글",
            body_html: "<p>Outlook 클라이언트 #10 확장 속성 스레드 머리글 테스트</p>",
            extended: &extended,
        })
        .await
        .context("#10 초안 생성 실패")?;
    println!(
        "    ✓ 확장 속성을 넣은 초안 생성, 초안 {}",
        short_hash(&draft.id)
    );
    tracker.changes(client).await?;
    let Some((latest, draft_mid)) = only_me(client, &draft, &me).await? else {
        discard_draft(client, &draft).await;
        bail!("초안에 본인 외 주소가 있거나 받는 사람이 없어 보내지 않았습니다");
    };
    if !ask("받는 사람이 본인뿐입니다. #10 메일을 본인 주소로 1통 보낼까요?")?
    {
        discard_draft(client, &latest).await;
        println!("    #10 발송 취소(초안 정리)");
        return Ok(());
    }
    client
        .send_draft(&latest, slice_addr(&me), "newMail")
        .await
        .context("#10 발송 실패")?;
    println!("    ✓ 발송 요청 성공. 도착을 기다립니다(최대 60초)…");
    let Some(received) =
        wait_for_arrival(client, &mut tracker, &inbox.display_name, &draft_mid).await?
    else {
        println!("    주의: 60초 안에 도착하지 않았습니다. 웹메일에서 확인해 주세요.");
        return Ok(());
    };
    let eml = match client.get_mime(slice(&received)).await?.into_iter().next() {
        Some(Ok(b)) => b,
        other => bail!("받은 메일 원문을 읽지 못했습니다: {other:?}"),
    };
    let in_reply_to = header_value(&eml, "In-Reply-To").unwrap_or_default();
    let refs = header_value(&eml, "References").unwrap_or_default();
    println!(
        "    받은 메일 {}: In-Reply-To = 원본 Message-ID {}, References에 원본 포함 {}",
        short_hash(&received.id),
        yes_no(in_reply_to.contains(&original_mid)),
        yes_no(refs.contains(&original_mid))
    );
    println!("#8·#9·#10 완료");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_folded_headers_only_from_head() {
        let eml = b"From: a\r\nIn-Reply-To: <x@y>\r\nReferences: <p@q>\r\n <x@y>\r\nSubject: s\r\n\r\nReferences: body";
        assert_eq!(header_value(eml, "in-reply-to").as_deref(), Some("<x@y>"));
        assert_eq!(
            header_value(eml, "References").as_deref(),
            Some("<p@q><x@y>")
        );
        assert_eq!(header_value(eml, "X-None"), None);
    }

    #[test]
    fn reply_subjects_are_excluded() {
        assert!(is_reply_subject("RE: Outlook-TEST"));
        assert!(is_reply_subject("  Fw: Outlook-TEST"));
        assert!(is_reply_subject("회신: Outlook-TEST"));
        assert!(!is_reply_subject("Outlook-TEST 원본"));
    }
}
