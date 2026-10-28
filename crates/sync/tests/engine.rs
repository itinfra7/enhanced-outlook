//! 가짜 서버로 동기화 엔진 전체 흐름을 확인한다(2단계 수용 기준 3·4를 오프라인으로).
//! 1주기: 폴더 2개, 받은 편지함 메일 2통 → 원문 2통 저장.
//! 2주기: 웹메일에서 A를 다른 폴더로 이동, B를 영구 삭제 → A는 원문 재다운로드 없이 이동, B는 보관.

use std::sync::Arc;

use eo_owa::{Client, ClientConfig, Credentials, Password};
use eo_store::Store;
use eo_sync::{Engine, EngineConfig};
use serde_json::{Value, json};
use wiremock::matchers::{body_string_contains, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn ok(items: Vec<Value>) -> Value {
    json!({ "Body": { "ResponseMessages": { "Items": items } } })
}

fn folder(id: &str, name: &str) -> Value {
    json!({ "__type": "Folder:#Exchange",
            "FolderId": { "Id": id },
            "ParentFolderId": { "Id": "ROOT" },
            "DisplayName": name, "FolderClass": "IPF.Note" })
}

fn create(id: &str, imid: &str, received: &str) -> Value {
    json!({ "__type": "SyncFolderItemsCreateType:#Exchange",
            "Item": { "__type": "Message:#Exchange",
                      "ItemId": { "Id": id, "ChangeKey": "ck" },
                      "Subject": "제목", "InternetMessageId": imid,
                      "DateTimeReceived": received, "IsRead": false, "Size": 10 } })
}

fn delete(id: &str) -> Value {
    json!({ "__type": "SyncFolderItemsDeleteType:#Exchange", "ItemId": { "Id": id } })
}

fn sync_page(state: &str, changes: Vec<Value>) -> Value {
    ok(vec![
        json!({ "ResponseClass": "Success", "SyncState": state,
                    "Changes": { "Changes": changes } }),
    ])
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

async fn server() -> MockServer {
    let s = MockServer::start().await;
    // 폼 로그인(가짜 서버는 NTLM을 요구하지 않는다).
    Mock::given(method("GET"))
        .and(path("/owa/"))
        .respond_with(
            ResponseTemplate::new(200).insert_header("Set-Cookie", "X-OWA-CANARY=c; path=/"),
        )
        .mount(&s)
        .await;
    Mock::given(method("GET"))
        .and(path("/owa/auth/logon.aspx"))
        .respond_with(ResponseTemplate::new(200))
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

    let not_found = json!({ "ResponseClass": "Error", "ResponseCode": "ErrorFolderNotFound" });
    mount(
        &s,
        "GetFolder",
        &[r#""Id":"inbox""#],
        1,
        ok(vec![
            json!({ "ResponseClass": "Success", "Folders": [folder("INBOX", "받은 편지함")] }),
            not_found.clone(),
            not_found.clone(),
            not_found.clone(),
            not_found.clone(),
            not_found,
        ]),
    )
    .await;
    mount(&s, "GetFolder", &[r#""Id":"msgfolderroot""#], 2, ok(vec![
        json!({ "ResponseClass": "Success", "Folders": [{ "FolderId": { "Id": "ROOT" }, "DisplayName": "root" }] }),
    ])).await;

    mount(
        &s,
        "SyncFolderHierarchy",
        &[r#""SyncState":"H1""#],
        1,
        ok(vec![
            json!({ "ResponseClass": "Success", "SyncState": "H1", "Changes": [] }),
        ]),
    )
    .await;
    mount(&s, "SyncFolderHierarchy", &[], 2, ok(vec![json!({
        "ResponseClass": "Success", "SyncState": "H1",
        "Changes": [
            { "__type": "SyncFolderHierarchyCreateType:#Exchange", "Folder": folder("INBOX", "받은 편지함") },
            { "__type": "SyncFolderHierarchyCreateType:#Exchange", "Folder": folder("ARCH", "보관") }
        ] })])).await;

    let t_a = "2026-10-15T10:00:00+09:00";
    let t_b = "2026-10-15T11:00:00+09:00";
    mount(
        &s,
        "SyncFolderItems",
        &[r#""Id":"INBOX""#, r#""SyncState":"I1""#],
        1,
        sync_page("I2", vec![delete("A"), delete("B")]),
    )
    .await;
    mount(
        &s,
        "SyncFolderItems",
        &[r#""Id":"INBOX""#],
        2,
        sync_page(
            "I1",
            vec![create("A", "<a@x>", t_a), create("B", "<b@x>", t_b)],
        ),
    )
    .await;
    mount(
        &s,
        "SyncFolderItems",
        &[r#""Id":"ARCH""#, r#""SyncState":"R1""#],
        1,
        sync_page("R2", vec![create("A2", "<a@x>", t_a)]),
    )
    .await;
    mount(
        &s,
        "SyncFolderItems",
        &[r#""Id":"ARCH""#],
        2,
        sync_page("R1", vec![]),
    )
    .await;

    // 원문은 1주기에 한 번만 요청돼야 한다(이동한 메일을 다시 받지 않는다).
    let mime = |s: &str| json!({ "ResponseClass": "Success", "Items": [{ "MimeContent": { "CharacterSet": "UTF-8", "Value": s } }] });
    Mock::given(method("POST"))
        .and(path("/owa/service.svc"))
        .and(query_param("action", "GetItem"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(ok(vec![mime("Qg=="), mime("QQ==")])),
        )
        .expect(1)
        .mount(&s)
        .await;
    s
}

#[tokio::test]
async fn move_without_redownload_and_delete_archives() {
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

    let r1 = engine.cycle(None).await.unwrap();
    assert_eq!(r1.hierarchy.changes, 2);
    assert_eq!(r1.items.apply.created, 2);
    assert_eq!((r1.eml.queued, r1.eml.stored), (2, 2));
    let folders = store.folders().unwrap();
    let inbox = folders
        .iter()
        .find(|f| f.server_id.as_deref() == Some("INBOX"))
        .unwrap();
    let arch = folders
        .iter()
        .find(|f| f.server_id.as_deref() == Some("ARCH"))
        .unwrap();
    assert_eq!(inbox.well_known.as_deref(), Some("inbox"));
    assert!(arch.is_mail);

    let r2 = engine.cycle(None).await.unwrap();
    assert_eq!(r2.hierarchy.changes, 0);
    assert_eq!(r2.items.apply.created, 0);
    assert_eq!(r2.items.apply.moves.len(), 1);
    assert!(r2.items.apply.moves[0].had_eml);
    assert_eq!(r2.items.apply.archives.len(), 1);
    assert_eq!(r2.eml.queued, 0, "원문 재다운로드 없음");

    let in_arch = store.messages_in_folder(arch.id).unwrap();
    assert_eq!(in_arch.len(), 1);
    assert_eq!(in_arch[0].server_item_id.as_deref(), Some("A2"));
    assert!(in_arch[0].eml_hash.is_some());
    let in_inbox = store.messages_in_folder(inbox.id).unwrap();
    assert_eq!(in_inbox.len(), 1, "삭제된 B는 원래 폴더에 남는다");
    assert_eq!(
        (
            in_inbox[0].server_state,
            in_inbox[0].props.internet_msg_id.as_deref()
        ),
        (1, Some("<b@x>"))
    );
    assert!(in_inbox[0].eml_hash.is_some(), "보관본도 원문을 갖고 있다");

    let mut saw_changes = false;
    while let Ok(e) = events.try_recv() {
        saw_changes |= matches!(e, eo_sync::Event::MessagesChanged { .. });
    }
    assert!(saw_changes);
}

/// 앱은 10초마다 받은 편지함만 본다. 그때 다른 폴더로 옮겨진 메일의 삭제만 보이면 "보관"으로 잘못
/// 표시하고, 다음 전체 확인에서 같은 메일이 새로 생겼다(5단계에서 찾은 2단계 버그).
/// 삭제가 보이면 전 폴더로 넓혀 같은 주기에 짝을 짓는다.
#[tokio::test]
async fn inbox_only_cycle_widens_on_delete() {
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
    engine.cycle(None).await.unwrap();
    let inbox = store
        .folders()
        .unwrap()
        .into_iter()
        .find(|f| f.server_id.as_deref() == Some("INBOX"))
        .unwrap()
        .id;

    let r = engine
        .sync_items(Some(&std::collections::HashSet::from([inbox])))
        .await
        .unwrap();
    assert!(r.escalated);
    assert_eq!(r.apply.moves.len(), 1, "A는 이동으로 짝지어짐");
    assert_eq!(r.apply.archives.len(), 1, "B만 보관");
    assert_eq!(r.apply.created, 0);
}
