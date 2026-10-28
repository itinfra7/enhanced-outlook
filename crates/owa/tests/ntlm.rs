//! NTLM 흐름을 가짜 서버로 확인한다: 방식 판별 → NEGOTIATE → CHALLENGE → AUTHENTICATE,
//! 인증된 연결 재사용, 비밀번호 거부. SSPI는 이 PC에서 실제로 돈다(네트워크 인증은 없음).
#![cfg(windows)]

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use eo_owa::{AuthScheme, Client, ClientConfig, Credentials, Error, FolderRef, Password};
use serde_json::json;
use wiremock::matchers::{header_regex, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// `NTLMSSP\0` + 메시지 종류 1(NEGOTIATE) / 3(AUTHENTICATE)의 base64 앞부분.
const NEGOTIATE: &str = "^NTLM TlRMTVNTUAAB";
const AUTHENTICATE: &str = "^NTLM TlRMTVNTUAAD";

/// SSPI가 받아들이는 최소 CHALLENGE 메시지(NTLMv2, 대상 정보 포함).
fn challenge() -> String {
    fn utf16(s: &str) -> Vec<u8> {
        s.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }
    let target = utf16("TEST");
    let mut info = Vec::new();
    for (kind, value) in [
        (2u16, utf16("TEST")),
        (1, utf16("SRV")),
        (4, utf16("test.local")),
        (3, utf16("srv.test.local")),
    ] {
        info.extend(kind.to_le_bytes());
        info.extend((value.len() as u16).to_le_bytes());
        info.extend(value);
    }
    info.extend([0, 0, 0, 0]); // MsvAvEOL
    let flags: u32 = 0xE289_8215;
    let header_len = 56u32;
    let mut m = Vec::new();
    m.extend(b"NTLMSSP\0");
    m.extend(2u32.to_le_bytes());
    m.extend((target.len() as u16).to_le_bytes());
    m.extend((target.len() as u16).to_le_bytes());
    m.extend(header_len.to_le_bytes());
    m.extend(flags.to_le_bytes());
    m.extend([1, 2, 3, 4, 5, 6, 7, 8]); // server challenge
    m.extend([0u8; 8]);
    m.extend((info.len() as u16).to_le_bytes());
    m.extend((info.len() as u16).to_le_bytes());
    m.extend((header_len + target.len() as u32).to_le_bytes());
    m.extend([10, 0, 0x61, 0x4A, 0, 0, 0, 15]); // version 10.0.19041, NTLM revision 15
    m.extend(target);
    m.extend(info);
    format!("NTLM {}", STANDARD.encode(m))
}

fn folder_response() -> serde_json::Value {
    json!({ "Body": { "ResponseMessages": { "Items": [{
        "ResponseClass": "Success",
        "Folders": [{ "__type": "Folder:#Exchange",
                      "FolderId": { "__type": "FolderId:#Exchange", "Id": "F1" },
                      "DisplayName": "받은 편지함", "FolderClass": "IPF.Note" }]
    }] } } })
}

async fn ntlm_server(accept: bool) -> MockServer {
    let server = MockServer::start().await;
    for p in ["/owa/", "/owa/service.svc"] {
        Mock::given(path(p))
            .and(header_regex("Authorization", NEGOTIATE))
            .respond_with(ResponseTemplate::new(401).insert_header("WWW-Authenticate", challenge()))
            .with_priority(1)
            .mount(&server)
            .await;
    }
    let after_auth = if accept {
        ResponseTemplate::new(200).insert_header("Set-Cookie", "X-OWA-CANARY=c1; path=/")
    } else {
        ResponseTemplate::new(401).insert_header("WWW-Authenticate", "NTLM")
    };
    Mock::given(path("/owa/"))
        .and(header_regex("Authorization", AUTHENTICATE))
        .respond_with(after_auth)
        .with_priority(1)
        .mount(&server)
        .await;
    // 인증 헤더 없는 첫 요청(방식 판별)은 401 + NTLM 제안.
    Mock::given(method("GET"))
        .and(path("/owa/"))
        .respond_with(
            ResponseTemplate::new(401)
                .append_header("WWW-Authenticate", "Negotiate")
                .append_header("WWW-Authenticate", "NTLM"),
        )
        .with_priority(5)
        .mount(&server)
        .await;
    // 인증된 연결에서는 Authorization 없이도 통과한다(가짜 서버는 연결 상태를 모르므로 그대로 200).
    Mock::given(method("POST"))
        .and(path("/owa/service.svc"))
        .respond_with(ResponseTemplate::new(200).set_body_json(folder_response()))
        .with_priority(5)
        .mount(&server)
        .await;
    server
}

fn creds() -> Credentials {
    Credentials {
        username: r"TEST\me".into(),
        password: Password::new("pw".into()),
    }
}

#[tokio::test]
async fn ntlm_login_then_reuses_authenticated_connection() {
    let server = ntlm_server(true).await;
    let c = Client::new(ClientConfig::new(&server.uri()).unwrap(), None);
    let info = c.login(creds()).await.unwrap();
    assert_eq!(info.scheme, AuthScheme::Ntlm);
    assert!(info.has_canary);
    assert_eq!(c.auth_handshakes(), 1);

    c.get_folder(&FolderRef::distinguished("inbox"))
        .await
        .unwrap();
    c.get_folder(&FolderRef::distinguished("inbox"))
        .await
        .unwrap();
    assert_eq!(c.auth_handshakes(), 1, "인증된 연결을 재사용해야 한다");

    // 연결을 끊으면 다음 호출에서 다시 인증한다.
    c.logoff().await.unwrap();
    c.get_folder(&FolderRef::distinguished("inbox"))
        .await
        .unwrap();
    assert_eq!(c.auth_handshakes(), 2);
}

#[tokio::test]
async fn ntlm_rejected_password_is_bad_credentials() {
    let server = ntlm_server(false).await;
    let c = Client::new(ClientConfig::new(&server.uri()).unwrap(), None);
    let err = c.login(creds()).await.unwrap_err();
    assert!(matches!(err, Error::BadCredentials { .. }), "{err}");
}
