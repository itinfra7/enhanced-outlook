//! 가짜 서버로 작업 큐 실행을 확인한다(5단계 수용 기준을 오프라인으로).
//! - 연결이 끊긴 동안(503) 조작은 로컬에 그대로 있고 작업은 대기한다. 다시 연결되면 서버에 반영된다.
//! - 서버에 없는 항목(`ErrorItemNotFound`)은 로컬 보관으로 둔다.
//! - 서버가 계속 거부하면 5번째에 포기하고 로컬을 되돌린 뒤 알린다.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use eo_owa::{Client, ClientConfig, Credentials, Password};
use eo_store::Store;
use eo_sync::{Engine, EngineConfig, Event};
use serde_json::{Value, json};
use wiremock::matchers::{body_string_contains, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

fn ok(items: Vec<Value>) -> Value {
    json!({ "Body": { "ResponseMessages": { "Items": items } } })
}

fn success_id(id: &str) -> Value {
    json!({ "ResponseClass": "Success", "Items": [{ "ItemId": { "Id": id, "ChangeKey": "ck2" } }] })
}

fn item_error(code: &str) -> Value {
    json!({ "ResponseClass": "Error", "ResponseCode": code, "MessageText": "거부" })
}

fn folder(id: &str, name: &str) -> Value {
    json!({ "__type": "Folder:#Exchange", "FolderId": { "Id": id }, "ParentFolderId": { "Id": "ROOT" },
            "DisplayName": name, "FolderClass": "IPF.Note" })
}

fn create(id: &str, imid: &str) -> Value {
    json!({ "__type": "SyncFolderItemsCreateType:#Exchange",
            "Item": { "__type": "Message:#Exchange", "ItemId": { "Id": id, "ChangeKey": "ck" },
                      "Subject": "제목", "InternetMessageId": imid,
                      "DateTimeReceived": "2026-10-15T10:00:00+09:00", "IsRead": false, "Size": 10 } })
}

async fn mount(
    server: &MockServer,
    action: &str,
    body_has: &[&str],
    priority: u8,
    reply: ResponseTemplate,
) {
    let mut m = Mock::given(method("POST"))
        .and(path("/owa/service.svc"))
        .and(query_param("action", action));
    for s in body_has {
        m = m.and(body_string_contains(*s));
    }
    m.respond_with(reply)
        .with_priority(priority)
        .mount(server)
        .await;
}

fn json_reply(v: Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(v)
}

async fn server() -> MockServer {
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
    let nf = item_error("ErrorFolderNotFound");
    mount(
        &s,
        "GetFolder",
        &[r#""Id":"inbox""#],
        1,
        json_reply(ok(vec![
            json!({ "ResponseClass": "Success", "Folders": [folder("INBOX", "받은 편지함")] }),
            nf.clone(),
            nf.clone(),
            nf.clone(),
            nf.clone(),
            nf,
        ])),
    )
    .await;
    mount(&s, "GetFolder", &[r#""Id":"msgfolderroot""#], 2, json_reply(ok(vec![
        json!({ "ResponseClass": "Success", "Folders": [{ "FolderId": { "Id": "ROOT" }, "DisplayName": "root" }] }),
    ]))).await;
    mount(&s, "SyncFolderHierarchy", &[], 1, json_reply(ok(vec![json!({
        "ResponseClass": "Success", "SyncState": "H1",
        "Changes": [
            { "__type": "SyncFolderHierarchyCreateType:#Exchange", "Folder": folder("INBOX", "받은 편지함") },
            { "__type": "SyncFolderHierarchyCreateType:#Exchange", "Folder": folder("ARCH", "보관") }
        ] })]))).await;
    mount(&s, "SyncFolderItems", &[r#""Id":"INBOX""#], 1, json_reply(ok(vec![json!({
        "ResponseClass": "Success", "SyncState": "I1",
        "Changes": { "Changes": [create("A", "<a@x>"), create("B", "<b@x>"), create("C", "<c@x>"), create("D", "<d@x>")] } })]))).await;
    mount(
        &s,
        "SyncFolderItems",
        &[r#""Id":"ARCH""#],
        1,
        json_reply(ok(vec![json!({
        "ResponseClass": "Success", "SyncState": "R1", "Changes": { "Changes": [] } })])),
    )
    .await;

    // 첫 작업 요청 한 번은 연결 실패(503)처럼.
    Mock::given(method("POST"))
        .and(path("/owa/service.svc"))
        .and(query_param("action", "UpdateItem"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&s)
        .await;
    // 읽음(A)은 성공, 플래그(C)는 계속 거부.
    mount(
        &s,
        "UpdateItem",
        &[r#""FieldURI":"IsRead""#],
        2,
        json_reply(ok(vec![success_id("A")])),
    )
    .await;
    mount(
        &s,
        "UpdateItem",
        &[r#""FieldURI":"Flag""#],
        2,
        json_reply(ok(vec![item_error("ErrorAccessDenied")])),
    )
    .await;
    // B 이동 → 새 ID B2.
    mount(
        &s,
        "MoveItem",
        &[r#""Id":"B""#, r#""Id":"ARCH""#],
        2,
        json_reply(ok(vec![success_id("B2")])),
    )
    .await;
    // D 서버 삭제: 이미 서버에 없음.
    mount(
        &s,
        "DeleteItem",
        &[r#""Id":"D""#, "SoftDelete"],
        2,
        json_reply(ok(vec![item_error("ErrorItemNotFound")])),
    )
    .await;
    s
}

#[tokio::test]
async fn offline_ops_reach_server_after_reconnect_and_rejections_roll_back() {
    let server = server().await;
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(dir.path()).unwrap());
    let client = Client::new(ClientConfig::new(&server.uri()).unwrap(), None);
    client
        .login(Credentials {
            username: "me".into(),
            password: Password::new("pw".into()),
        })
        .await
        .unwrap();
    let engine = Engine::new(client, store.clone(), EngineConfig::default());
    let mut events = engine.subscribe();
    engine.sync_hierarchy().await.unwrap();
    engine.sync_items(None).await.unwrap();

    let folders = store.folders().unwrap();
    let id_of = |sid: &str| {
        folders
            .iter()
            .find(|f| f.server_id.as_deref() == Some(sid))
            .unwrap()
            .id
    };
    let (inbox, arch) = (id_of("INBOX"), id_of("ARCH"));
    let msg = |sid: &str| {
        store
            .messages_in_folder(inbox)
            .unwrap()
            .into_iter()
            .find(|m| m.server_item_id.as_deref() == Some(sid))
            .unwrap()
            .id
    };
    let (a, b, c, d) = (msg("A"), msg("B"), msg("C"), msg("D"));

    // 로컬 조작: 바로 반영된다.
    store.mark_read(&[a], true, 1).unwrap();
    store.move_messages(&[b], arch, 1, None).unwrap();
    store.mark_flag(&[c], 1, 1).unwrap();
    store.server_delete(&[d], 1, None).unwrap();
    assert_eq!(store.pending_op_count().unwrap(), 4);

    // 1회차: 연결 실패 → 멈춤, 로컬은 그대로.
    let r = engine.process_ops().await.unwrap();
    assert!(r.offline);
    assert_eq!(store.pending_op_count().unwrap(), 4);
    assert!(store.message(a).unwrap().unwrap().props.is_read);
    assert_eq!(store.message(b).unwrap().unwrap().folder_id, arch);

    // 다시 연결: 기다리던 작업을 바로 보낸다.
    store.retry_ops_now(now()).unwrap();
    let r = engine.process_ops().await.unwrap();
    assert!(!r.offline);
    assert_eq!(r.done, 3, "읽음·이동·삭제(서버에 없음) 완료");
    let moved = store.message(b).unwrap().unwrap();
    assert_eq!(
        (moved.folder_id, moved.server_item_id.as_deref()),
        (arch, Some("B2"))
    );
    let gone = store.message(d).unwrap().unwrap();
    assert_eq!(
        (gone.server_state, gone.server_item_id),
        (1, None),
        "서버에 없으면 로컬 보관"
    );
    assert_eq!(store.pending_op_count().unwrap(), 1, "거부된 플래그만 남음");

    // 플래그는 계속 거부 → 5번째에 포기하고 되돌린다.
    for _ in 0..4 {
        store.retry_ops_now(now()).unwrap();
        engine.process_ops().await.unwrap();
    }
    assert_eq!(store.pending_op_count().unwrap(), 0);
    assert_eq!(
        store.message(c).unwrap().unwrap().props.flag,
        0,
        "포기한 플래그는 되돌림"
    );
    let mut failed = None;
    while let Ok(e) = events.try_recv() {
        if let Event::OpFailed {
            message_id, kind, ..
        } = e
        {
            failed = Some((message_id, kind));
        }
    }
    assert_eq!(failed, Some((Some(c), "set_flag".to_owned())));
}
