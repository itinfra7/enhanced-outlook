//! 로그인·세션 만료·재로그인·비밀번호 재입력·전달 방식 대체를 가짜 서버로 확인한다.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use eo_owa::{
    Client, ClientConfig, CredentialPrompt, Credentials, Error, FolderRef, Password, PromptFuture,
    SessionEventKind,
};
use serde_json::json;
use wiremock::matchers::{body_string_contains, header, header_exists, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn creds(password: &str) -> Credentials {
    Credentials {
        username: "me".into(),
        password: Password::new(password.into()),
    }
}

fn folder_response() -> serde_json::Value {
    json!({ "Body": { "ResponseMessages": { "Items": [{
        "ResponseClass": "Success",
        "Folders": [{ "__type": "Folder:#Exchange",
                      "FolderId": { "__type": "FolderId:#Exchange", "Id": "F1" },
                      "DisplayName": "받은 편지함", "FolderClass": "IPF.Note" }]
    }] } } })
}

/// 로그인 화면·`/owa/`(캐너리 쿠키)·성공 로그인(지정한 비밀번호)을 띄운다.
async fn server_with_login(password: &str) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/owa/auth/logon.aspx"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/owa/"))
        .respond_with(
            ResponseTemplate::new(200).insert_header("Set-Cookie", "X-OWA-CANARY=c1; path=/"),
        )
        .mount(&server)
        .await;
    mount_login_success(&server, password, None, 5).await;
    server
}

async fn mount_login_success(
    server: &MockServer,
    password: &str,
    times: Option<u64>,
    priority: u8,
) {
    let mock = Mock::given(method("POST"))
        .and(path("/owa/auth.owa"))
        .and(body_string_contains(format!("password={password}&")))
        .and(body_string_contains("flags=4"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("Location", format!("{}/owa/", server.uri()))
                .insert_header("Set-Cookie", "cadata=abc; path=/; HttpOnly"),
        )
        .with_priority(priority);
    match times {
        Some(n) => mock.up_to_n_times(n).mount(server).await,
        None => mock.mount(server).await,
    }
}

async fn mount_login_rejected(server: &MockServer, password: &str, priority: u8) {
    Mock::given(method("POST"))
        .and(path("/owa/auth.owa"))
        .and(body_string_contains(format!("password={password}&")))
        .respond_with(ResponseTemplate::new(302).insert_header(
            "Location",
            format!(
                "{}/owa/auth/logon.aspx?replaceCurrent=1&reason=2",
                server.uri()
            ),
        ))
        .with_priority(priority)
        .mount(server)
        .await;
}

fn client(server: &MockServer, prompt: Option<Arc<dyn CredentialPrompt>>) -> Client {
    Client::new(ClientConfig::new(&server.uri()).unwrap(), prompt)
}

fn kinds(c: &Client) -> Vec<SessionEventKind> {
    c.events().into_iter().map(|e| e.kind).collect()
}

#[tokio::test]
async fn login_then_call_sends_canary_and_action() {
    let server = server_with_login("pw1").await;
    Mock::given(method("POST"))
        .and(path("/owa/service.svc"))
        .and(query_param("action", "GetFolder"))
        .and(query_param("app", "Mail"))
        .and(header("Action", "GetFolder"))
        .and(header("X-OWA-CANARY", "c1"))
        .and(body_string_contains(
            r#""__type":"GetFolderJsonRequest:#Exchange""#,
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(folder_response()))
        .expect(1)
        .mount(&server)
        .await;

    let c = client(&server, None);
    let info = c.login(creds("pw1")).await.unwrap();
    assert!(info.has_canary);
    assert!(
        info.cookie_names.iter().any(|n| n == "cadata"),
        "{:?}",
        info.cookie_names
    );
    assert!(info.cookie_names.iter().any(|n| n == "PrivateComputer"));

    let f = c
        .get_folder(&FolderRef::distinguished("inbox"))
        .await
        .unwrap();
    assert_eq!(f.display_name, "받은 편지함");
}

#[tokio::test]
async fn bad_password_is_reported() {
    let server = server_with_login("right").await;
    mount_login_rejected(&server, "wrong", 1).await;
    let c = client(&server, None);
    let err = c.login(creds("wrong")).await.unwrap_err();
    assert!(
        matches!(err, Error::BadCredentials { ref reason } if reason == "reason=2"),
        "{err}"
    );
    assert!(!c.is_logged_in());
}

#[tokio::test]
async fn http_440_relogins_once_and_retries() {
    let server = server_with_login("pw1").await;
    Mock::given(method("POST"))
        .and(path("/owa/service.svc"))
        .respond_with(ResponseTemplate::new(440))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/owa/service.svc"))
        .respond_with(ResponseTemplate::new(200).set_body_json(folder_response()))
        .mount(&server)
        .await;

    let c = client(&server, None);
    c.login(creds("pw1")).await.unwrap();
    c.get_folder(&FolderRef::distinguished("inbox"))
        .await
        .unwrap();

    let k = kinds(&c);
    assert!(k.contains(&SessionEventKind::SessionExpired {
        action: "GetFolder".into(),
        signal: "HTTP 440".into()
    }));
    assert!(k.contains(&SessionEventKind::Relogin { ok: true }));
}

#[tokio::test]
async fn redirect_to_logon_is_treated_as_expiry() {
    let server = server_with_login("pw1").await;
    Mock::given(method("POST"))
        .and(path("/owa/service.svc"))
        .respond_with(ResponseTemplate::new(302).insert_header(
            "Location",
            format!(
                "{}/owa/auth/logon.aspx?replaceCurrent=1&reason=0",
                server.uri()
            ),
        ))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/owa/service.svc"))
        .respond_with(ResponseTemplate::new(200).set_body_json(folder_response()))
        .mount(&server)
        .await;

    let c = client(&server, None);
    c.login(creds("pw1")).await.unwrap();
    c.get_folder(&FolderRef::distinguished("inbox"))
        .await
        .unwrap();
    assert!(kinds(&c).iter().any(|k| matches!(k,
        SessionEventKind::SessionExpired { signal, .. }
            if signal == "HTTP 302 → /owa/auth/logon.aspx?replaceCurrent=1&reason=0")));
}

struct NewPassword {
    calls: AtomicUsize,
    answer: Option<&'static str>,
}

impl CredentialPrompt for NewPassword {
    fn credentials_rejected<'a>(&'a self, username: &'a str) -> PromptFuture<'a> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let answer = self.answer.map(|p| Credentials {
            username: username.to_owned(),
            password: Password::new(p.into()),
        });
        Box::pin(async move { answer })
    }
}

/// 비밀번호가 바뀌면: 메모리의 옛 비밀번호로 재로그인 실패 → 다시 입력받아 로그인 → 호출 재시도.
#[tokio::test]
async fn changed_password_is_asked_again() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/owa/auth/logon.aspx"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/owa/"))
        .respond_with(
            ResponseTemplate::new(200).insert_header("Set-Cookie", "X-OWA-CANARY=c1; path=/"),
        )
        .mount(&server)
        .await;
    mount_login_success(&server, "old", Some(1), 1).await; // 처음 로그인만 성공
    mount_login_rejected(&server, "old", 2).await; // 그 뒤로는 거부
    mount_login_success(&server, "new", None, 3).await;
    Mock::given(method("POST"))
        .and(path("/owa/service.svc"))
        .respond_with(ResponseTemplate::new(440))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/owa/service.svc"))
        .respond_with(ResponseTemplate::new(200).set_body_json(folder_response()))
        .mount(&server)
        .await;

    let prompt = Arc::new(NewPassword {
        calls: AtomicUsize::new(0),
        answer: Some("new"),
    });
    let c = client(&server, Some(prompt.clone()));
    c.login(creds("old")).await.unwrap();
    c.get_folder(&FolderRef::distinguished("inbox"))
        .await
        .unwrap();

    assert_eq!(prompt.calls.load(Ordering::SeqCst), 1);
    let k = kinds(&c);
    assert!(k.contains(&SessionEventKind::CredentialsReprompted));
    assert!(k.contains(&SessionEventKind::Relogin { ok: true }));
}

#[tokio::test]
async fn giving_up_on_new_password_requires_login() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/owa/auth/logon.aspx"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/owa/"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    mount_login_success(&server, "old", Some(1), 1).await;
    mount_login_rejected(&server, "old", 2).await;
    Mock::given(method("POST"))
        .and(path("/owa/service.svc"))
        .respond_with(ResponseTemplate::new(440))
        .mount(&server)
        .await;

    let prompt = Arc::new(NewPassword {
        calls: AtomicUsize::new(0),
        answer: None,
    });
    let c = client(&server, Some(prompt));
    c.login(creds("old")).await.unwrap();
    let err = c
        .get_folder(&FolderRef::distinguished("inbox"))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::LoginRequired), "{err}");
}

/// 비밀번호를 바꾼 뒤 세션이 끝났다(사용자 요청 2026-10-26): 자동 재로그인은 한 번만, 거부가 비밀번호 오류 화면이
/// 아니어도(403 등) 잊고 [`Error::LoginRequired`]. 그 뒤로는 로그인하지 않는다(화면이 로그인 창을 띄운다).
#[tokio::test]
async fn failed_relogin_is_not_retried() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/owa/auth/logon.aspx"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/owa/"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/owa/auth.owa"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("Location", format!("{}/owa/", server.uri()))
                .insert_header("Set-Cookie", "cadata=abc; path=/; HttpOnly"),
        )
        .up_to_n_times(1)
        .with_priority(1)
        .expect(1)
        .named("처음 로그인")
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/owa/auth.owa"))
        .respond_with(ResponseTemplate::new(403))
        .with_priority(2)
        .expect(1)
        .named("자동 재로그인(한 번만)")
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/owa/service.svc"))
        .respond_with(ResponseTemplate::new(440))
        .mount(&server)
        .await;

    let c = client(&server, None);
    c.login(creds("old")).await.unwrap();
    for _ in 0..3 {
        let err = c
            .get_folder(&FolderRef::distinguished("inbox"))
            .await
            .unwrap_err();
        assert!(matches!(err, Error::LoginRequired), "{err}");
    }
}

#[tokio::test]
async fn format_error_in_body_mode_falls_back_to_header_mode() {
    let server = server_with_login("pw1").await;
    Mock::given(method("POST"))
        .and(path("/owa/service.svc"))
        .and(header_exists("X-OWA-UrlPostData"))
        .respond_with(ResponseTemplate::new(200).set_body_json(folder_response()))
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/owa/service.svc"))
        .respond_with(ResponseTemplate::new(500).set_body_json(json!({
            "Body": { "ErrorCode": 0, "ExceptionName": "SerializationException", "FaultMessage": "x" }
        })))
        .mount(&server)
        .await;

    let c = client(&server, None);
    c.login(creds("pw1")).await.unwrap();
    c.get_folder(&FolderRef::distinguished("inbox"))
        .await
        .unwrap();
    assert!(kinds(&c).contains(&SessionEventKind::PostModeFallback {
        action: "GetFolder".into()
    }));
}

#[tokio::test]
async fn format_error_without_fallback_is_not_retried() {
    let server = server_with_login("pw1").await;
    Mock::given(method("POST"))
        .and(path("/owa/service.svc"))
        .respond_with(ResponseTemplate::new(500).set_body_json(json!({
            "Body": { "ErrorCode": 0, "ExceptionName": "SerializationException", "FaultMessage": "x" }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let mut cfg = ClientConfig::new(&server.uri()).unwrap();
    cfg.post_mode = eo_owa::PostMode::Body;
    let c = Client::new(cfg, None);
    c.login(creds("pw1")).await.unwrap();
    let err = c
        .get_folder(&FolderRef::distinguished("inbox"))
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::RequestFormat { ref code, .. } if code == "SerializationException"),
        "{err}"
    );
}
