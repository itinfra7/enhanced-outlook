//! HCL Domino 양방향 동기화(0.6.0): 상태를 가진 가짜 Domino 서버로
//! - 웹메일에서 바꾼 것(읽음·★·지운 메일로 이동·완전 삭제)이 다음 확인 때 로컬에 반영되고,
//! - 앱에서 쌓은 작업(안읽음·★·복구·지운 메일로)이 웹메일 화면 스크립트와 같은 주소로 가며,
//! - 보낸 뒤에는 "웹메일에서 바뀜"으로 다시 보지 않는다(서로 덮어쓰지 않음).

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use eo_owa::domino::{Domino, DominoConfig};
use eo_owa::{Credentials, Password};
use eo_store::{Store, domino_op, server_state};
use eo_sync::{DominoOptions, DominoScan, push_domino_ops, sync_domino};
use wiremock::matchers::any;
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const DB: &str = "/mail9/t.nsf";

#[derive(Debug, Clone)]
struct Doc {
    unid: String,
    noteid: String,
    subject: String,
    /// Inbox, Sent, POP3, Draft, Trash
    mailbox: String,
    new: bool,
    star: bool,
    deleted: bool,
}

#[derive(Default)]
struct State {
    docs: Vec<Doc>,
    /// 받은 바꾸기 요청(경로+질의).
    changes: Vec<String>,
    /// 허용하지 않은 요청.
    unexpected: Vec<String>,
}

struct Fake {
    state: Arc<Mutex<State>>,
    port: u16,
}

fn raw(body: impl Into<Vec<u8>>, mime: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(body.into(), mime)
}

fn view(docs: &[&Doc]) -> ResponseTemplate {
    let mut s = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<viewentries toplevelentries=\"{}\">\n",
        docs.len()
    );
    for (i, d) in docs.iter().enumerate() {
        s.push_str(&format!(
            "<viewentry position=\"{}\" unid=\"{}\" noteid=\"{}\">\
             <entrydata name=\"$11\"><text>{}</text></entrydata>\
             <entrydata name=\"$7\"><text>2026-10-2{} 10:00:00</text></entrydata>\
             <entrydata name=\"$8\"><number>1000</number></entrydata>\
             <entrydata name=\"NewDocument\"><text>{}</text></entrydata></viewentry>\n",
            i + 1,
            d.unid,
            d.noteid,
            if d.star { "★" } else { "☆" },
            i,
            u8::from(d.new)
        ));
    }
    s.push_str("</viewentries>\n");
    raw(s, "text/xml; charset=UTF-8")
}

fn ids_of(q: &str, key: &str) -> Vec<String> {
    q.split('&')
        .find_map(|p| p.strip_prefix(&format!("{key}=")))
        .map(|v| v.split(',').map(str::to_owned).collect())
        .unwrap_or_default()
}

impl Respond for Fake {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let path = req.url.path().to_owned();
        let q = req.url.query().unwrap_or("").to_owned();
        let mut s = self.state.lock().unwrap();
        if req.method.as_str() == "POST" && path == "/names.nsf" {
            return ResponseTemplate::new(302)
                .insert_header("Location", "/home.nsf/StartHome?openform")
                .append_header("Set-Cookie", "LtpaToken=tok; Path=/");
        }
        if path == "/home.nsf/StartHome" {
            return raw(
                format!(
                    "var mail_server=\"127.0.0.1:{}\";\nvar mailFile = \"mail9/t.nsf\";\nvar MailDbSize = \"300\";",
                    self.port
                ),
                "text/html; charset=euc-kr",
            );
        }
        let Some(rest) = path.strip_prefix(&format!("{DB}/")) else {
            s.unexpected.push(format!("{path}?{q}"));
            return ResponseTemplate::new(418);
        };
        let live = |d: &&Doc| !d.deleted;
        let pick = |mb: &str| -> Vec<Doc> {
            s.docs
                .iter()
                .filter(live)
                .filter(|d| d.mailbox == mb)
                .cloned()
                .collect()
        };
        match rest {
            "XML_Inbox" | "XML_Sent" | "XML_POP3" | "XML_Draft" | "XML_Trash" => {
                let docs = pick(&rest["XML_".len()..]);
                return view(&docs.iter().collect::<Vec<_>>());
            }
            "FolderTree" => return view(&[]),
            "XML_Unread" => {
                let docs: Vec<&Doc> = s.docs.iter().filter(live).filter(|d| d.new).collect();
                return view(&docs);
            }
            "XML_All" => {
                let docs: Vec<&Doc> = s
                    .docs
                    .iter()
                    .filter(live)
                    .filter(|d| d.mailbox != "Trash")
                    .collect();
                return view(&docs);
            }
            _ => {}
        }
        if let Some(unid) = rest.strip_prefix("all/") {
            let Some(d) = s.docs.iter().find(|d| d.unid == unid) else {
                return ResponseTemplate::new(404);
            };
            return raw(
                format!(
                    "<html><head><title>{}</title></head><body>\
                     <input type=\"hidden\" name=\"From\" value=\"a@x.com\">\
                     <input type=\"hidden\" name=\"BodyContentType\" value=\"M1^text/plain\"></body></html>",
                    d.subject
                ),
                "text/html; charset=UTF-8",
            );
        }
        if rest.starts_with("0/") && rest.ends_with("/Body/M1") {
            return raw("본문", "text/plain; charset=UTF-8");
        }
        // 바꾸기(웹메일 화면 스크립트와 같은 모양).
        s.changes.push(format!("{rest}?{q}"));
        let ok = raw("<html></html>", "text/html; charset=US-ASCII");
        let notes: Vec<String>;
        match rest {
            "wMarkRead" => {
                notes = ids_of(&q, "docid");
                let new = q.ends_with("type=2");
                for d in s.docs.iter_mut().filter(|d| notes.contains(&d.noteid)) {
                    d.new = new;
                }
            }
            "agt_impo" => {
                notes = ids_of(&q, "sid");
                let on = q.ends_with("type=N");
                for d in s.docs.iter_mut().filter(|d| notes.contains(&d.noteid)) {
                    d.star = on;
                }
            }
            "WebDelDoc2" => {
                notes = ids_of(&q, "docid");
                let trash = q.contains("category=Trash");
                for d in s.docs.iter_mut().filter(|d| notes.contains(&d.noteid)) {
                    if trash {
                        d.deleted = true;
                    } else {
                        d.mailbox = "Trash".into();
                    }
                }
            }
            "RecoverDelDoc" => {
                notes = ids_of(&q, "DocList");
                for d in s.docs.iter_mut().filter(|d| notes.contains(&d.noteid)) {
                    d.mailbox = "Inbox".into();
                }
            }
            _ => {
                s.changes.pop();
                s.unexpected.push(format!("{rest}?{q}"));
                return ResponseTemplate::new(418);
            }
        }
        ok
    }
}

fn doc(unid: &str, noteid: &str, subject: &str, new: bool) -> Doc {
    Doc {
        unid: unid.into(),
        noteid: noteid.into(),
        subject: subject.into(),
        mailbox: "Inbox".into(),
        new,
        star: false,
        deleted: false,
    }
}

const A: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
const B: &str = "BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB";
const C: &str = "CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC";

async fn full(client: &Domino, store: &Store) -> eo_sync::DominoReport {
    sync_domino(
        client,
        store,
        DominoScan::Full,
        &DominoOptions::default(),
        &AtomicBool::new(false),
        |_, _| {},
    )
    .await
    .unwrap()
}

fn local(store: &Store, subject: &str) -> eo_store::MessageRow {
    let id = store.message_ids_by_subject(subject, 5).unwrap()[0];
    store.message(id).unwrap().unwrap()
}

fn folder_name(store: &Store, id: i64) -> String {
    store
        .folders()
        .unwrap()
        .into_iter()
        .find(|f| f.id == id)
        .unwrap()
        .name
}

#[tokio::test]
async fn webmail_and_app_changes_meet_without_overwriting() {
    let s = MockServer::start().await;
    let state = Arc::new(Mutex::new(State {
        docs: vec![
            doc(A, "A1", "가 메일", true),
            doc(B, "B2", "나 메일", false),
            doc(C, "C3", "다 메일", false),
        ],
        ..State::default()
    }));
    Mock::given(any())
        .respond_with(Fake {
            state: state.clone(),
            port: s.address().port(),
        })
        .mount(&s)
        .await;
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let client = Domino::new(DominoConfig::new(&s.uri()).unwrap());
    client
        .login(Credentials {
            username: "bp1".into(),
            password: Password::new("pw".into()),
        })
        .await
        .unwrap();

    // 1) 처음 가져오기: 'new'였던 A는 앱에서도 안읽음, 웹메일의 'new'는 되돌려 둔다.
    let r = full(&client, &store).await;
    assert_eq!((r.imported, r.updated), (3, 0), "{:?}", r.errors);
    assert!(!local(&store, "가 메일").props.is_read);
    assert!(state.lock().unwrap().docs[0].new, "'new' 되돌림");

    // 2) 웹메일에서: A 읽음, B ★ 켜고 지운 메일로, C 완전 삭제.
    {
        let mut st = state.lock().unwrap();
        st.changes.clear();
        st.docs[0].new = false;
        st.docs[1].star = true;
        st.docs[1].mailbox = "Trash".into();
        st.docs[2].deleted = true;
    }
    let r = full(&client, &store).await;
    assert_eq!(r.updated, 3, "{:?}", r.errors);
    assert!(
        local(&store, "가 메일").props.is_read,
        "웹메일에서 읽음 → 앱도 읽음"
    );
    let b = local(&store, "나 메일");
    assert_eq!(b.props.flag, 1, "★ → 플래그");
    assert_eq!(folder_name(&store, b.folder_id), "지운 메일");
    let c = local(&store, "다 메일");
    assert_eq!(
        c.server_state,
        server_state::ARCHIVED,
        "웹메일에서 지움 → 로컬은 보관"
    );
    assert!(
        state.lock().unwrap().changes.is_empty(),
        "확인만 했으니 웹메일에 바꾸기 요청은 없음"
    );

    // 3) 앱에서: A 안읽음(로컬 먼저), A ★, B 지운 메일 → 받은 메일. 보낸 뒤 다시 확인해도 그대로.
    store
        .mark_read(&[local(&store, "가 메일").id], false, 1)
        .unwrap();
    store
        .domino_enqueue(A, None, domino_op::READ, "0", 1)
        .unwrap();
    store
        .domino_enqueue(A, None, domino_op::STAR, "1", 1)
        .unwrap();
    store
        .domino_enqueue(B, None, domino_op::RECOVER, "", 1)
        .unwrap();
    assert_eq!(store.domino_pending_count().unwrap(), 3);
    // 대기 중에는 웹메일 상태로 로컬을 덮어쓰지 않는다.
    let r = full(&client, &store).await;
    assert_eq!(r.updated, 0);
    assert!(!local(&store, "가 메일").props.is_read);
    let pushed = push_domino_ops(&client, &store).await.unwrap();
    assert_eq!((pushed.done, pushed.abandoned.len()), (3, 0));
    assert_eq!(store.domino_pending_count().unwrap(), 0);
    {
        let st = state.lock().unwrap();
        assert_eq!(
            st.changes,
            [
                "wMarkRead?OpenAgent&&docid=A1&type=2",
                "agt_impo?OpenAgent&sid=A1&type=N",
                "RecoverDelDoc?OpenAgent&RTView=($Inbox)&DocList=B2",
            ]
        );
        assert!(st.docs[0].new && st.docs[0].star);
        assert_eq!(st.docs[1].mailbox, "Inbox");
    }
    let r = full(&client, &store).await;
    // B는 웹메일에서 받은 메일로 돌아왔다(앱은 아직 지운 메일): 보낸 작업이 적은 서버 상태와 같아 다시 옮기지 않는다.
    // 화면에서는 이동과 함께 쌓으므로 로컬도 이미 받은 메일이다.
    assert_eq!(
        r.updated, 0,
        "보낸 것을 웹메일에서 바뀐 것으로 다시 보지 않음"
    );
    assert!(!local(&store, "가 메일").props.is_read);

    // 4) 지운 메일로 → 완전 삭제(서버에서만 삭제와 같은 순서).
    store
        .domino_enqueue(B, None, domino_op::DELETE, "XML_Inbox", 2)
        .unwrap();
    store
        .domino_enqueue(B, None, domino_op::DELETE, "XML_Trash", 2)
        .unwrap();
    push_domino_ops(&client, &store).await.unwrap();
    {
        let st = state.lock().unwrap();
        assert!(st.docs[1].deleted);
        assert!(st.unexpected.is_empty(), "{:?}", st.unexpected);
        assert_eq!(
            &st.changes[3..],
            [
                "WebDelDoc2?OpenAgent&category=Inbox&docid=B2",
                "WebDelDoc2?OpenAgent&category=Trash&docid=B2",
            ]
        );
    }
    drop(s);
}
