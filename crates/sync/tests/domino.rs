//! 가짜 HCL Domino 서버로 계정 연동 전체 흐름을 확인한다(0.5.0).
//! - 로그인 → 시작 페이지에서 웹메일 서버·메일 파일 → 목록 → 메일 열기 → 조각·첨부 → EML.
//! - 'new'였던 메일만 연 직후 안읽음 표시로 되돌리고, 앱 안에서도 안읽음으로 둔다.
//! - 목록 중간에 세션이 끝나면(로그인 화면) 다시 로그인해 이어 간다.
//! - 허용 목록 밖의 요청은 하나도 가지 않는다. 다시 확인하면 이미 가져온 메일은 열지 않는다.

use std::sync::atomic::AtomicBool;

use eo_owa::domino::{Domino, DominoConfig};
use eo_owa::{Credentials, Password};
use eo_store::Store;
use eo_sync::{DominoOptions, DominoScan, sync_domino};
use wiremock::matchers::{any, body_string_contains, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DB: &str = "/mail9/t.nsf";
const A: &str = "D77632AF61A4F0A549258CD60024422D";
const B: &str = "B35A41FF8D6A1C0C49258E91002FD01D";
const PNG: &[u8] = b"\x89PNG\r\n\x1a\nfake";

fn view(entries: &[(&str, &str, &str, bool)]) -> String {
    let mut s = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<viewentries toplevelentries=\"{}\">\n",
        entries.len()
    );
    for (i, (unid, noteid, date, new)) in entries.iter().enumerate() {
        s.push_str(&format!(
            "<viewentry position=\"{}\" unid=\"{unid}\" noteid=\"{noteid}\" siblings=\"{}\">\n\
             <entrydata columnnumber=\"7\" name=\"$7\"><text>{date}</text></entrydata>\n\
             <entrydata columnnumber=\"8\" name=\"$8\"><number>1000</number></entrydata>\n\
             <entrydata columnnumber=\"9\" name=\"NewDocument\"><text>{}</text></entrydata>\n</viewentry>\n",
            i + 1,
            entries.len(),
            u8::from(*new)
        ));
    }
    s.push_str("</viewentries>\n");
    s
}

/// Domino처럼 형식을 정확히(`set_body_string`은 text/plain으로 바꾼다).
fn raw(body: impl Into<Vec<u8>>, mime: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(body.into(), mime)
}

fn xml(body: String) -> ResponseTemplate {
    raw(body, "text/xml; charset=UTF-8")
}

fn html(body: String) -> ResponseTemplate {
    raw(body, "text/html; charset=UTF-8")
}

const LOGIN_PAGE: &str = "<html><form method=\"post\" action=\"/names.nsf?Login\" name=\"_DominoForm\">\
                          <input type=\"text\" name=\"username\"><input type=\"password\" name=\"password\"></form></html>";

async fn get(s: &MockServer, p: &str, reply: ResponseTemplate) {
    Mock::given(method("GET"))
        .and(path(p))
        .respond_with(reply)
        .mount(s)
        .await;
}

async fn server() -> MockServer {
    let s = MockServer::start().await;
    let port = s.address().port();
    // 허용 목록 밖으로 나간 요청이 있으면 실패.
    Mock::given(any())
        .respond_with(ResponseTemplate::new(418))
        .with_priority(10)
        .expect(0)
        .named("허용 목록 밖")
        .mount(&s)
        .await;
    Mock::given(method("POST"))
        .and(path("/names.nsf"))
        .and(query_param("Login", ""))
        .and(body_string_contains("username=bp1"))
        .and(body_string_contains("password=pw"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("Location", "/home.nsf/StartHome?openform")
                .append_header("Set-Cookie", "LtpaToken=tok; Path=/")
                .append_header(
                    "Set-Cookie",
                    "ckUserInfo=MailFile%3Dmail9/t.nsf%26mq1%3D300%26mq2%3D240; Path=/",
                ),
        )
        .expect(2)
        .named("로그인(처음 + 세션 만료 뒤 한 번)")
        .mount(&s)
        .await;
    get(
        &s,
        "/home.nsf/StartHome",
        raw(
            format!(
                "<script>var  mail_server=\"127.0.0.1:{port}\";\nvar  mailFile = \"mail9/t.nsf\";</script>"
            ),
            "text/html; charset=euc-kr",
        ),
    )
    .await;
    get(
        &s,
        &format!("{DB}/XML_Inbox"),
        xml(view(&[
            (A, "AAA1", "2025-07-29 15:36:26", true),
            (B, "BBB2", "2026-10-15 17:42:14", false),
        ])),
    )
    .await;
    // 보낸 메일 목록을 처음 볼 때 세션이 끝나 있다.
    Mock::given(method("GET"))
        .and(path(format!("{DB}/XML_Sent")))
        .respond_with(html(LOGIN_PAGE.to_owned()))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&s)
        .await;
    for v in [
        "XML_Sent",
        "XML_POP3",
        "XML_Draft",
        "XML_Trash",
        "FolderTree",
    ] {
        get(&s, &format!("{DB}/{v}"), xml(view(&[]))).await;
    }
    get(
        &s,
        &format!("{DB}/XML_Unread"),
        xml(view(&[(A, "AAA1", "2025-07-29 15:36:26", true)])),
    )
    .await;
    get(
        &s,
        &format!("{DB}/XML_All"),
        xml(view(&[
            (A, "AAA1", "2025-07-29 15:36:26", true),
            (B, "BBB2", "2026-10-15 17:42:14", false),
        ])),
    )
    .await;
    let a = A.to_ascii_lowercase();
    Mock::given(method("GET"))
        .and(path(format!("{DB}/all/{A}")))
        .and(query_param("OpenDocument", ""))
        .and(query_param("Charset", "UTF-8"))
        .respond_with(html(format!(
            "<html><head><title>Fwd: 협업NAS 조치 방법</title></head><body>\
             <input type=\"hidden\" name=\"From\" value=\"&quot;=?EUC-KR?B?sejDtrz2?=&quot; &lt;U000002@inbound.example.com&gt;\">\
             <input type=\"hidden\" name=\"SendTo\" value=\"user1@example.com\">\
             <input type=\"hidden\" name=\"BodyContentType\" value=\"; M1^multipart/related; M1.1^multipart/alternative; M1.1.1^text/html; M1.2^image/png; M1.3^image/png\">\
             <input type=\"hidden\" name=\"tmpAttUrl\" value=\"{DB}/all/{A}/$FILE/%EB%B3%B4%EA%B3%A0.pdf\">\
             </body></html>"
        )))
        .expect(1)
        .named("A 열기(한 번만)")
        .mount(&s)
        .await;
    get(
        &s,
        &format!("{DB}/0/{a}/Body/M1.1.1"),
        html(format!(
            "<p>본문 <img src=\"{DB}/0/{a}/Body/M1.2?OpenElement\"></p><img src=\"/icons/x.gif\">"
        )),
    )
    .await;
    get(&s, &format!("{DB}/0/{a}/Body/M1.2"), raw(PNG, "image/png")).await;
    get(
        &s,
        &format!("{DB}/all/{A}/$FILE/%EB%B3%B4%EA%B3%A0.pdf"),
        raw(b"%PDF-1.4".as_slice(), "application/octet-stream"),
    )
    .await;
    // 서버가 빈 응답을 주는 조각(실계정의 전달 메일): 건너뛰고 메일은 넣는다.
    get(
        &s,
        &format!("{DB}/0/{a}/Body/M1.3"),
        raw(Vec::new(), "image/png"),
    )
    .await;
    Mock::given(method("GET"))
        .and(path(format!("{DB}/wMarkRead")))
        .and(query_param("docid", "AAA1"))
        .and(query_param("type", "2"))
        .respond_with(html("<html></html>".into()))
        .expect(1)
        .named("A의 'new' 되돌리기")
        .mount(&s)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{DB}/all/{B}")))
        .respond_with(html(
            "<html><head><title>기계실 장비 주황점등</title></head><body>\
             <input type=\"hidden\" name=\"From\" value=\"kim@inbound.example.com\">\
             <input type=\"hidden\" name=\"BodyContentType\" value=\"M1^text/plain\">\
             <input type=\"hidden\" name=\"BodyCharset\" value=\"M1^ks_c_5601-1987\"></body></html>"
                .into(),
        ))
        .expect(1)
        .mount(&s)
        .await;
    // 웹메일 "환경 설정 → 메일용량확인" 창.
    get(
        &s,
        &format!("{DB}/mail_size"),
        raw(
            encoding_rs::EUC_KR
                .encode(
                    "<title>메일 용량 확인</title><script>var max = \"300\";\nvar warning = \"240\";\n\
                     var inbox = \"211.71\";\nvar trash = \"0\";\nvar totvalue = \"212.28\";</script>",
                )
                .0
                .into_owned(),
            "text/html; charset=EUC-KR",
        ),
    )
    .await;
    let (euckr, _, _) = encoding_rs::EUC_KR.encode("점검 <결과>\n이상 없음");
    get(
        &s,
        &format!("{DB}/0/{}/Body/M1", B.to_ascii_lowercase()),
        raw(euckr.into_owned(), "text/plain; charset=ks_c_5601-1987"),
    )
    .await;
    s
}

/// 쓰는 도중에 웹에서 비밀번호를 바꿨다(사용자 알림 2026-10-26): 세션이 끝나 다시 로그인하다 거부되면 더는
/// 로그인하지 않는다(연속으로 틀리면 계정이 잠긴다). 거부가 비밀번호 오류 화면이 아니어도(403 등) 같다.
#[tokio::test]
async fn changed_password_is_tried_once_and_then_never_again() {
    let s = MockServer::start().await;
    let port = s.address().port();
    Mock::given(method("POST"))
        .and(path("/names.nsf"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("Location", "/home.nsf/StartHome?openform")
                .append_header("Set-Cookie", "LtpaToken=tok; Path=/"),
        )
        .up_to_n_times(1)
        .with_priority(1)
        .expect(1)
        .named("처음 로그인")
        .mount(&s)
        .await;
    Mock::given(method("POST"))
        .and(path("/names.nsf"))
        .respond_with(ResponseTemplate::new(403))
        .with_priority(2)
        .expect(1)
        .named("비밀번호를 바꾼 뒤 자동 재로그인(한 번만)")
        .mount(&s)
        .await;
    get(
        &s,
        "/home.nsf/StartHome",
        raw(
            format!(
                "<script>var  mail_server=\"127.0.0.1:{port}\";\nvar  mailFile = \"mail9/t.nsf\";</script>"
            ),
            "text/html; charset=euc-kr",
        ),
    )
    .await;
    // 세션이 끝났다(목록 대신 로그인 화면).
    get(&s, &format!("{DB}/XML_Inbox"), html(LOGIN_PAGE.to_owned())).await;
    let client = Domino::new(DominoConfig::new(&s.uri()).unwrap());
    client
        .login(Credentials {
            username: "bp1".into(),
            password: Password::new("pw".into()),
        })
        .await
        .unwrap();
    for _ in 0..3 {
        let r = client.view_page("XML_Inbox", None, 1, 10).await;
        assert!(
            matches!(r, Err(eo_owa::Error::LoginRequired)),
            "다시 로그인 필요: {r:?}"
        );
    }
    assert!(
        !client.is_logged_in() && client.username().is_none(),
        "아이디·비밀번호를 잊는다"
    );
}

#[tokio::test]
async fn syncs_mail_restores_new_and_skips_known() {
    let s = server().await;
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let client = Domino::new(DominoConfig::new(&s.uri()).unwrap());
    let login = client
        .login(Credentials {
            username: "bp1".into(),
            password: Password::new("pw".into()),
        })
        .await
        .unwrap();
    assert_eq!(login.location.db, "mail9/t.nsf");
    assert_eq!(login.quota_limit, Some(300 * 1024 * 1024));
    assert!(!login.cookie_names.is_empty());

    let cancel = AtomicBool::new(false);
    let r = sync_domino(
        &client,
        &store,
        DominoScan::Full,
        &DominoOptions::default(),
        &cancel,
        |_, _| {},
    )
    .await
    .unwrap();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(
        (r.listed, r.new, r.imported, r.unread_restored, r.failed),
        (2, 2, 2, 1, 0)
    );
    assert_eq!(r.inbox.as_ref().map(Vec::len), Some(2));
    assert_eq!(
        (r.with_missing, r.missing_parts),
        (1, 1),
        "빈 조각은 건너뜀"
    );

    let folders = store.folders().unwrap();
    let top = folders
        .iter()
        .find(|f| f.name == "HCL Domino 메일")
        .unwrap();
    assert_eq!(
        folders
            .iter()
            .filter(|f| f.parent_id == Some(top.id))
            .map(|f| f.name.as_str())
            .collect::<Vec<_>>(),
        [
            "받은 메일",
            "보낸 메일",
            "외부 메일",
            "미발송 메일",
            "지운 메일"
        ],
        "빈 메일함도, 웹메일 순서대로"
    );
    assert!(r.folders_changed);
    let inbox = folders
        .iter()
        .find(|f| f.name == "받은 메일" && f.parent_id == Some(top.id))
        .unwrap();
    let msgs = store.messages_in_folder(inbox.id).unwrap();
    assert_eq!(msgs.len(), 2);
    let a = msgs
        .iter()
        .find(|m| m.props.subject.as_deref() == Some("Fwd: 협업NAS 조치 방법"))
        .unwrap();
    assert!(!a.props.is_read, "'new'였던 메일은 앱에서도 안읽음");
    assert_eq!(a.origin.as_deref(), Some("domino-sync"));
    assert_eq!(a.props.from_name.as_deref(), Some("김철수"));
    let raw = store.eml().get(a.eml_hash.as_deref().unwrap()).unwrap();
    let opts = eo_mime::RenderOptions {
        cid_base: "http://app-cid.localhost/1/".into(),
        allow_remote: false,
    };
    let view = eo_mime::render(&raw, &opts).unwrap();
    assert_eq!(view.inline_images.len(), 1, "본문 그림");
    assert_eq!(view.inline_images[0].data, PNG);
    assert!(
        view.attachments
            .iter()
            .any(|x| x.name == "보고.pdf" && !x.inline),
        "{:?}",
        view.attachments
    );
    assert!(view.html.contains("본문"));
    let b = msgs.iter().find(|m| m.id != a.id).unwrap();
    assert!(b.props.is_read);
    let raw = store.eml().get(b.eml_hash.as_deref().unwrap()).unwrap();
    let view = eo_mime::render(&raw, &opts).unwrap();
    assert!(
        view.html.contains("점검 &lt;결과&gt;"),
        "EUC-KR 일반 글: {}",
        view.html
    );

    // 다시 확인: 이미 가져온 메일은 열지 않는다(열기 mock의 expect(1)).
    let r = sync_domino(
        &client,
        &store,
        DominoScan::Quick,
        &DominoOptions::default(),
        &cancel,
        |_, _| {},
    )
    .await
    .unwrap();
    assert_eq!((r.listed, r.new, r.imported), (2, 0, 0));
    assert_eq!(store.domino_copies().unwrap().len(), 2);

    let (used, limit) = client.quota().await.unwrap();
    assert_eq!(used, Some((212.28_f64 * 1_048_576.0).round() as i64));
    assert_eq!(limit, Some(300 * 1024 * 1024));
    drop(s);
}
