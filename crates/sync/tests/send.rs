//! 가짜 서버로 발송과 임시 보관함 저장을 확인한다(6단계, 계획서 5.7·5.8·9.3, 0.7.0).
//! - 새 메일 + 첨부: 초안 생성 → 첨부 → 받는 사람·제목·본문 지정 + 보내기, 끝나면 작성 중 메일을 지운다.
//! - 지난 시도에서 보내기 요청까지 갔고 서버 초안이 없어졌으면 이미 보낸 것으로 본다(다시 보내지 않음).
//! - 서버가 거부하면 "발송 실패"로 남기고 서버 초안(임시 보관함)은 그대로 둔다.
//! - 로컬 보관본 회신: 새 메일 + In-Reply-To·References 확장 속성, 인용문 포함.
//! - 임시 보관함 저장: 처음엔 초안을 만들고, 다음부터는 바뀐 첨부만 올리고 뺀 첨부는 지운다. 보낼 때는 그 초안을 보낸다.

use std::sync::Arc;

use eo_owa::{Client, ClientConfig, Credentials, Password};
use eo_store::{DraftContent, MessageProps, Recipient, ServerFolder, Store, draft_state};
use eo_sync::{Engine, EngineConfig, Event};
use serde_json::{Value, json};
use wiremock::matchers::{body_string_contains, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn ok(items: Vec<Value>) -> Value {
    json!({ "Body": { "ResponseMessages": { "Items": items } } })
}

async fn mount(server: &MockServer, action: &str, body_has: &[&str], priority: u8, reply: Value) {
    let mut m = Mock::given(method("POST"))
        .and(path("/owa/service.svc"))
        .and(query_param("action", action));
    for s in body_has {
        m = m.and(body_string_contains(*s));
    }
    m.respond_with(ResponseTemplate::new(200).set_body_json(reply))
        .with_priority(priority)
        .mount(server)
        .await;
}

async fn server(send_ok: bool) -> MockServer {
    let s = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/owa/"))
        .respond_with(
            ResponseTemplate::new(200).insert_header("Set-Cookie", "X-OWA-CANARY=c; path=/"),
        )
        .mount(&s)
        .await;
    Mock::given(method("POST"))
        .and(path("/owa/auth.owa"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("Location", format!("{}/owa/", s.uri()))
                .insert_header("Set-Cookie", "cadata=x; path=/"),
        )
        .mount(&s)
        .await;
    mount(
        &s,
        "GetOwaUserConfiguration",
        &[],
        1,
        json!({ "SessionSettings": {
        "UserEmailAddress": "me@x.com", "UserDisplayName": "나" } }),
    )
    .await;
    mount(
        &s,
        "CreateItem",
        &[],
        1,
        ok(vec![json!({ "ResponseClass": "Success",
        "Items": [{ "ItemId": { "Id": "D1", "ChangeKey": "ck1" } }] })]),
    )
    .await;
    mount(&s, "CreateAttachmentFromLocalFile", &[], 1, ok(vec![json!({ "ResponseClass": "Success",
        "Attachments": [{ "AttachmentId": { "Id": "A1", "RootItemId": "D1", "RootItemChangeKey": "ck2" } }] })])).await;
    let send_reply = if send_ok {
        json!({ "ResponseClass": "Success", "Items": [] })
    } else {
        json!({ "ResponseClass": "Error", "ResponseCode": "ErrorInvalidRecipients", "MessageText": "받는 사람 오류" })
    };
    mount(
        &s,
        "UpdateItem",
        &["SendAndSaveCopy"],
        1,
        ok(vec![send_reply]),
    )
    .await;
    mount(
        &s,
        "DeleteItem",
        &[],
        1,
        ok(vec![json!({ "ResponseClass": "Success" })]),
    )
    .await;
    mount(
        &s,
        "GetItem",
        &[r#""Id":"OLD""#],
        1,
        ok(vec![json!({ "ResponseClass": "Error",
        "ResponseCode": "ErrorItemNotFound", "MessageText": "없음" })]),
    )
    .await;
    mount(&s, "GetItem", &[r#""Id":"D1""#], 1, ok(vec![json!({ "ResponseClass": "Success",
        "Items": [{ "__type": "Message:#Exchange", "ItemId": { "Id": "D1", "ChangeKey": "ck5" }, "Subject": "s" }] })])).await;
    mount(
        &s,
        "UpdateItem",
        &["SaveOnly"],
        1,
        ok(vec![json!({ "ResponseClass": "Success",
        "Items": [{ "ItemId": { "Id": "D1", "ChangeKey": "ck3" } }] })]),
    )
    .await;
    mount(
        &s,
        "DeleteAttachment",
        &[],
        1,
        ok(vec![json!({ "ResponseClass": "Success",
        "RootItemId": { "RootItemId": "D1", "RootItemChangeKey": "ck4" } })]),
    )
    .await;
    s
}

async fn engine(server: &MockServer, store: Arc<Store>) -> Engine {
    let client = Client::new(ClientConfig::new(&server.uri()).unwrap(), None);
    client
        .login(Credentials {
            username: "me".into(),
            password: Password::new("pw".into()),
        })
        .await
        .unwrap();
    Engine::new(client, store, EngineConfig::default())
}

fn me() -> Recipient {
    Recipient {
        name: "나".into(),
        email: "me@x.com".into(),
    }
}

async fn bodies(server: &MockServer, action: &str) -> Vec<String> {
    server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| {
            r.url
                .query()
                .unwrap_or("")
                .contains(&format!("action={action}"))
        })
        .map(|r| String::from_utf8_lossy(&r.body).into_owned())
        .collect()
}

#[tokio::test]
async fn new_mail_with_attachment_is_sent_and_draft_removed() {
    let server = server(true).await;
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(dir.path()).unwrap());
    let engine = engine(&server, store.clone()).await;
    let mut events = engine.subscribe();
    let id = store
        .create_draft(
            "new",
            None,
            &DraftContent {
                to: vec![me()],
                subject: "Outlook-TEST 새 메일".into(),
                body_html: "<p>안녕하세요</p>".into(),
                ..DraftContent::default()
            },
            1,
        )
        .unwrap();
    let file = dir.path().join("보고서.pdf");
    std::fs::write(&file, b"%PDF-1.4").unwrap();
    store.add_draft_file(id, &file, "보고서.pdf").unwrap();
    store.queue_send(id, 1).unwrap();

    let r = engine.process_ops().await.unwrap();
    assert_eq!(r.done, 1);
    assert!(
        store.draft(id).unwrap().is_none(),
        "보낸 작성 중 메일은 지운다"
    );
    let create = bodies(&server, "CreateItem").await;
    assert!(
        create[0].contains("Outlook-TEST 새 메일")
            && create[0].contains("\"ComposeOperation\":\"newMail\"")
    );
    let att = bodies(&server, "CreateAttachmentFromLocalFile").await;
    assert!(att[0].contains("application/pdf") && att[0].contains(r#""ChangeKey":"ck1""#));
    let send = bodies(&server, "UpdateItem").await;
    assert!(
        send[0].contains(r#""ChangeKey":"ck2""#),
        "첨부 뒤 새 ChangeKey로 보낸다"
    );
    assert!(send[0].contains("안녕하세요") && send[0].contains("me@x.com"));
    let mut sent = false;
    while let Ok(e) = events.try_recv() {
        sent |= matches!(e, Event::SendResult { draft_id, error: None } if draft_id == id);
    }
    assert!(sent);
}

/// 작성 창에 붙여 넣은 그림(data:)은 본문 그림 인라인 첨부(IsInline + ContentId)로 올리고 본문은 cid:로 보낸다.
#[tokio::test]
async fn pasted_image_is_sent_as_inline_attachment() {
    let server = server(true).await;
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(dir.path()).unwrap());
    let engine = engine(&server, store.clone()).await;
    let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";
    let id = store
        .create_draft(
            "new",
            None,
            &DraftContent {
                to: vec![me()],
                subject: "Outlook-TEST 그림".into(),
                body_html: format!(
                    "<p>캡처<img src=\"data:image/png;base64,{png}\" alt=\"c.png\"></p>"
                ),
                ..DraftContent::default()
            },
            1,
        )
        .unwrap();
    store.queue_send(id, 1).unwrap();
    assert_eq!(engine.process_ops().await.unwrap().done, 1);
    let att = bodies(&server, "CreateAttachmentFromLocalFile").await;
    assert_eq!(att.len(), 1, "{att:?}");
    assert!(
        att[0].contains(r#""IsInline":true"#)
            && att[0].contains("image001.png@outlook")
            && att[0].contains("image/png"),
        "{}",
        att[0]
    );
    let send = bodies(&server, "UpdateItem").await;
    assert!(
        send[0].contains("cid:image001.png@outlook") && !send[0].contains("data:image"),
        "{}",
        send[0]
    );
}

#[tokio::test]
async fn earlier_attempt_already_sent_is_not_sent_again() {
    let server = server(true).await;
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(dir.path()).unwrap());
    let engine = engine(&server, store.clone()).await;
    let id = store
        .create_draft(
            "new",
            None,
            &DraftContent {
                to: vec![me()],
                subject: "s".into(),
                ..DraftContent::default()
            },
            1,
        )
        .unwrap();
    store.queue_send(id, 1).unwrap();
    store.set_draft_server_id(id, "OLD", "newMail").unwrap();
    store.set_draft_send_started(id, true).unwrap();
    engine.process_ops().await.unwrap();
    assert!(store.draft(id).unwrap().is_none());
    assert!(
        bodies(&server, "CreateItem").await.is_empty()
            && bodies(&server, "UpdateItem").await.is_empty(),
        "다시 보내지 않는다"
    );
}

#[tokio::test]
async fn rejected_send_leaves_failed_draft_and_keeps_server_draft() {
    let server = server(false).await;
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(dir.path()).unwrap());
    let engine = engine(&server, store.clone()).await;
    let id = store
        .create_draft(
            "new",
            None,
            &DraftContent {
                to: vec![me()],
                subject: "s".into(),
                ..DraftContent::default()
            },
            1,
        )
        .unwrap();
    store.queue_send(id, 1).unwrap();
    let r = engine.process_ops().await.unwrap();
    assert_eq!(r.abandoned.len(), 1);
    let d = store.draft(id).unwrap().unwrap();
    assert_eq!(d.state, draft_state::FAILED);
    assert!(
        d.last_error
            .unwrap_or_default()
            .contains("ErrorInvalidRecipients")
    );
    assert_eq!(
        d.server_draft_id.as_deref(),
        Some("D1"),
        "임시 보관함에 남는다"
    );
    assert!(!d.send_started);
    assert!(bodies(&server, "DeleteItem").await.is_empty());
    assert_eq!(store.pending_op_count().unwrap(), 0);
}

fn with_drafts_folder(store: &Store) -> i64 {
    store
        .transaction(|tx| {
            let f = tx.upsert_folder(&ServerFolder {
                server_id: "DRAFTS".into(),
                server_parent_id: Some("ROOT".into()),
                name: "임시 보관함".into(),
                folder_class: Some("IPF.Note".into()),
            })?;
            tx.set_well_known("DRAFTS", "drafts", 1)?;
            Ok(f)
        })
        .unwrap()
}

/// 임시 보관함 저장(0.7.0): 처음엔 초안을 만들고 저장만, 다음엔 바뀐 첨부만 맞추고, 보낼 때는 그 초안을 보낸다.
#[tokio::test]
async fn draft_is_kept_in_server_drafts_and_sent_from_there() {
    let server = server(true).await;
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(dir.path()).unwrap());
    let drafts = with_drafts_folder(&store);
    let engine = engine(&server, store.clone()).await;
    let id = store
        .create_draft(
            "new",
            None,
            &DraftContent {
                to: vec![me()],
                subject: "Outlook-TEST 임시".into(),
                body_html: "<p>쓰는 중</p>".into(),
                ..DraftContent::default()
            },
            1,
        )
        .unwrap();
    let a = dir.path().join("a.txt");
    std::fs::write(&a, b"aaa").unwrap();
    store.add_draft_file(id, &a, "a.txt").unwrap();
    let row = store.sync_draft_row(id, Some(&me())).unwrap().unwrap();
    let r = store.message(row).unwrap().unwrap();
    assert_eq!(
        (
            r.folder_id,
            r.server_item_id.as_deref(),
            r.props.subject.as_deref()
        ),
        (drafts, None, Some("Outlook-TEST 임시")),
        "올리기 전에도 임시 보관함에 보인다"
    );

    assert!(store.queue_draft_upload(id, 2).unwrap());
    assert!(
        !store.queue_draft_upload(id, 2).unwrap(),
        "대기 중이면 하나만"
    );
    assert_eq!(engine.process_ops().await.unwrap().done, 1);
    let d = store.draft(id).unwrap().unwrap();
    assert_eq!(
        (d.server_draft_id.as_deref(), d.server_dirty),
        (Some("D1"), false)
    );
    assert_eq!(
        store
            .message(row)
            .unwrap()
            .unwrap()
            .server_item_id
            .as_deref(),
        Some("D1"),
        "행에 서버 항목이 붙는다"
    );
    let save = bodies(&server, "UpdateItem").await;
    assert!(
        save[0].contains("\"MessageDisposition\":\"SaveOnly\"") && save[0].contains("쓰는 중"),
        "{}",
        save[0]
    );
    assert_eq!(
        bodies(&server, "CreateAttachmentFromLocalFile").await.len(),
        1
    );

    // 파일을 바꿔 다시 저장: 뺀 a.txt는 지우고 b.txt만 올린다.
    store.remove_draft_file(id, "a.txt").unwrap();
    let b = dir.path().join("b.txt");
    std::fs::write(&b, b"bbbb").unwrap();
    store.add_draft_file(id, &b, "b.txt").unwrap();
    store.touch_draft(id, 3).unwrap();
    store.queue_draft_upload(id, 3).unwrap();
    assert_eq!(engine.process_ops().await.unwrap().done, 1);
    let del = bodies(&server, "DeleteAttachment").await;
    assert!(del.len() == 1 && del[0].contains(r#""Id":"A1""#), "{del:?}");
    assert_eq!(
        bodies(&server, "CreateAttachmentFromLocalFile").await.len(),
        2
    );
    assert_eq!(bodies(&server, "CreateItem").await.len(), 1, "초안은 하나");

    // 보내기: 새 초안을 만들지 않고 그 초안을 보낸다.
    store.queue_send(id, 4).unwrap();
    assert_eq!(engine.process_ops().await.unwrap().done, 1);
    assert_eq!(bodies(&server, "CreateItem").await.len(), 1);
    assert_eq!(
        bodies(&server, "CreateAttachmentFromLocalFile").await.len(),
        2,
        "다시 올리지 않는다"
    );
    let send = bodies(&server, "UpdateItem").await;
    assert!(send.last().unwrap().contains("SendAndSaveCopy"));
    assert!(store.draft(id).unwrap().is_none());
    assert!(
        store.message(row).unwrap().is_none(),
        "보낸 초안 행은 바로 뺀다"
    );
}

/// 웹메일에서 지운 서버 초안: 다음에 올릴 때 새로 만든다.
#[tokio::test]
async fn draft_deleted_on_server_is_created_again() {
    let server = server(true).await;
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(dir.path()).unwrap());
    with_drafts_folder(&store);
    let engine = engine(&server, store.clone()).await;
    let id = store
        .create_draft(
            "new",
            None,
            &DraftContent {
                subject: "s".into(),
                ..DraftContent::default()
            },
            1,
        )
        .unwrap();
    store.sync_draft_row(id, Some(&me())).unwrap();
    store.set_draft_server_id(id, "OLD", "newMail").unwrap();
    store.queue_draft_upload(id, 2).unwrap();
    assert_eq!(engine.process_ops().await.unwrap().done, 1);
    assert_eq!(
        store.draft(id).unwrap().unwrap().server_draft_id.as_deref(),
        Some("D1")
    );
    assert_eq!(bodies(&server, "CreateItem").await.len(), 1);
}

/// 원문이 서버에 있는 회신: 보내기 직전에 원본의 최신 ChangeKey를 읽어 ReferenceItemId에 넣는다
/// (없으면 ErrorChangeKeyRequiredForWriteOperations, 6단계 실서버 확인).
#[tokio::test]
async fn server_reply_uses_current_change_key_of_original() {
    let server = server(true).await;
    mount(&server, "GetItem", &[r#""Id":"ORIG""#], 1, ok(vec![json!({ "ResponseClass": "Success",
        "Items": [{ "__type": "Message:#Exchange", "ItemId": { "Id": "ORIG", "ChangeKey": "CKNOW" }, "Subject": "원본" }] })])).await;
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(dir.path()).unwrap());
    let engine = engine(&server, store.clone()).await;
    let eml = "From: me@x.com\r\nTo: me@x.com\r\nSubject: 원본\r\nMessage-ID: <orig@x.com>\r\n\r\n원본 본문";
    let msg = store
        .transaction(|tx| {
            let f = tx.upsert_folder(&ServerFolder {
                server_id: "INBOX".into(),
                server_parent_id: Some("ROOT".into()),
                name: "받은 편지함".into(),
                folder_class: Some("IPF.Note".into()),
            })?;
            tx.insert_message(
                f,
                "ORIG",
                Some("CKOLD"),
                &MessageProps {
                    date_received: 1,
                    ..MessageProps::default()
                },
            )
        })
        .unwrap();
    store.attach_eml(msg, "ORIG", eml.as_bytes()).unwrap();
    let id = store
        .create_draft(
            "reply_all",
            Some(msg),
            &DraftContent {
                to: vec![me()],
                subject: "RE: 원본".into(),
                include_quote: true,
                ..DraftContent::default()
            },
            1,
        )
        .unwrap();
    store.queue_send(id, 1).unwrap();
    assert_eq!(engine.process_ops().await.unwrap().done, 1);
    let create = bodies(&server, "CreateItem").await;
    assert!(create[0].contains("ReplyAllToItem"), "{}", create[0]);
    assert!(
        create[0].contains(
            r#""ReferenceItemId":{"__type":"ItemId:#Exchange","Id":"ORIG","ChangeKey":"CKNOW"}"#
        ),
        "{}",
        create[0]
    );
    let send = bodies(&server, "UpdateItem").await;
    assert!(send[0].contains("\"ComposeOperation\":\"replyAll\"") && send[0].contains("원본 본문"));
}

#[tokio::test]
async fn reply_to_local_archive_carries_thread_headers_and_quote() {
    let server = server(true).await;
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(dir.path()).unwrap());
    let engine = engine(&server, store.clone()).await;
    let eml = "From: 김민수 <minsu@x.com>\r\nTo: me@x.com\r\nSubject: 원본\r\nMessage-ID: <orig@x.com>\r\n\
               Date: Thu, 15 Oct 2026 16:34:00 +0900\r\nContent-Type: text/html; charset=UTF-8\r\n\r\n<p>원본 본문</p>";
    let msg = store
        .transaction(|tx| {
            let f = tx.upsert_folder(&ServerFolder {
                server_id: "INBOX".into(),
                server_parent_id: Some("ROOT".into()),
                name: "받은 편지함".into(),
                folder_class: Some("IPF.Note".into()),
            })?;
            let id = tx.insert_message(
                f,
                "S1",
                None,
                &MessageProps {
                    date_received: 1,
                    ..MessageProps::default()
                },
            )?;
            Ok(id)
        })
        .unwrap();
    store.attach_eml(msg, "S1", eml.as_bytes()).unwrap();
    store.transaction(|tx| tx.archive_message(msg, 2)).unwrap(); // 서버에서 삭제됨
    let id = store
        .create_draft(
            "reply",
            Some(msg),
            &DraftContent {
                to: vec![Recipient {
                    name: "김민수".into(),
                    email: "minsu@x.com".into(),
                }],
                subject: "RE: 원본".into(),
                body_html: "<p>회신합니다</p>".into(),
                include_quote: true,
                ..DraftContent::default()
            },
            3,
        )
        .unwrap();
    store.queue_send(id, 3).unwrap();
    assert_eq!(engine.process_ops().await.unwrap().done, 1);
    let create = bodies(&server, "CreateItem").await;
    assert!(
        create[0].contains("\"PropertyTag\":\"0x1042\"") && create[0].contains("<orig@x.com>"),
        "{}",
        create[0]
    );
    assert!(create[0].contains("0x1039"));
    let send = bodies(&server, "UpdateItem").await;
    assert!(
        send[0].contains("회신합니다")
            && send[0].contains("원본 본문")
            && send[0].contains("보낸 사람:")
    );
}

#[tokio::test]
async fn edited_quote_is_sent_instead_of_the_original() {
    let server = server(true).await;
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(dir.path()).unwrap());
    let engine = engine(&server, store.clone()).await;
    let eml = "From: kim@x.com\r\nTo: me@x.com\r\nSubject: 원본\r\nMessage-ID: <orig@x.com>\r\n\
               Content-Type: text/html; charset=UTF-8\r\n\r\n<p>원본 본문</p>";
    let msg = store
        .transaction(|tx| {
            let f = tx.upsert_folder(&ServerFolder {
                server_id: "INBOX".into(),
                server_parent_id: Some("ROOT".into()),
                name: "받은 편지함".into(),
                folder_class: Some("IPF.Note".into()),
            })?;
            tx.insert_message(
                f,
                "S1",
                None,
                &MessageProps {
                    date_received: 1,
                    ..MessageProps::default()
                },
            )
        })
        .unwrap();
    store.attach_eml(msg, "S1", eml.as_bytes()).unwrap();
    store.transaction(|tx| tx.archive_message(msg, 2)).unwrap();
    let content = DraftContent {
        to: vec![me()],
        subject: "RE: 원본".into(),
        body_html: "<p>회신합니다</p>".into(),
        include_quote: true,
        ..DraftContent::default()
    };
    let id = store.create_draft("reply", Some(msg), &content, 3).unwrap();
    // 작성 창에서 인용문을 고쳤다(스크립트는 보낼 때 빠진다).
    let edited = DraftContent {
        quote_html: Some(
            "<b>보낸 사람:</b> kim@x.com<br><p>고친 원문</p><script>x</script>".into(),
        ),
        ..content
    };
    assert!(store.save_draft(id, &edited, 4).unwrap());
    store.queue_send(id, 5).unwrap();
    assert_eq!(engine.process_ops().await.unwrap().done, 1);
    let send = bodies(&server, "UpdateItem").await;
    assert!(
        send[0].contains("회신합니다") && send[0].contains("고친 원문"),
        "{}",
        send[0]
    );
    assert!(!send[0].contains("원본 본문") && !send[0].contains("<script"));
}
