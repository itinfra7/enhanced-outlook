//! HCL Domino 웹메일(자체 양식) 클라이언트: 메일을 가져온다.
//!
//! - 포털 주소는 사용자가 설정에서 넣는다([`DominoConfig::new`]). 소스에 기본 주소는 없다.
//! - 로그인: `POST {포털}/names.nsf?Login` → `LtpaToken` 쿠키(포털 상위 도메인 공용).
//!   웹메일 서버(바뀐다)와 메일 파일은 로그인할 때마다 `home.nsf/StartHome`에서 찾는다. 웹메일 서버는 포털과 같은
//!   상위 도메인이어야 한다(로그인 쿠키를 다른 곳에 보내지 않게).
//! - 세션이 끝나면(로그인 화면이 돌아옴) 메모리의 아이디·비밀번호로 한 번 다시 로그인한다. 실패하면 다시 시도하지
//!   않고 잊는다(연속으로 틀리면 계정이 잠긴다). 비밀번호는 저장하지 않는다.
//! - 모든 요청은 [`notes::allowed`]를 거친다. 웹메일의 동작은 모두 GET 주소라, 허용 목록 밖은 보내지 않는다.
//!   바꾸는 요청은 사용자가 정한 두 가지뿐이다(2026-10-21):
//!   - [`Domino::mark_unread`]: 메일을 열면(`OpenDocument`) 서버가 'new' 표시를 지운다. 'new'였던 메일을 가져온
//!     직후 웹메일의 안읽음 표시로 되돌린다.
//!   - [`Domino::delete_docs`]: 받은 메일 자동 정리(서버에서만 삭제).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, RwLock};
use std::time::Duration;

use http::StatusCode;
use http::header::{CONTENT_TYPE, LOCATION};
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use url::Url;
use zeroize::Zeroizing;

use crate::DEFAULT_USER_AGENT;
use crate::creds::Credentials;
use crate::error::{Error, Result};
use crate::notes::{self, Memo, ViewEntry, ViewPage};
use crate::transport::{Http, Request, Response};

/// 가져오는 메일함: (보기, 로컬 폴더 이름). 결재 메일(받은 메일의 일부)·모든 메일·미확인은 겹쳐서 뺀다.
pub const MAIL_VIEWS: [(&str, &str); 5] = [
    ("XML_Inbox", "받은 메일"),
    ("XML_Sent", "보낸 메일"),
    ("XML_POP3", "외부 메일"),
    ("XML_Draft", "미발송 메일"),
    ("XML_Trash", "지운 메일"),
];

/// 목록 한 번에 받는 줄 수.
const PAGE: usize = 500;
/// 바꾸는 요청 하나에 넣는 메일 수(주소 길이).
const IDS_PER_REQUEST: usize = 50;

#[derive(Debug, Clone)]
pub struct DominoConfig {
    pub portal: Url,
    pub user_agent: String,
    pub timeout: Duration,
    /// 웹메일 서버 이름이 이것으로 끝나야 한다(로그인 쿠키를 다른 곳에 보내지 않게).
    pub mail_host_suffix: String,
}

impl DominoConfig {
    /// 포털 주소(`portal.example.com`, `https://portal.example.com/` 등)로. 웹메일 서버는 포털의 상위 도메인
    /// (`portal.example.com` → `.example.com`) 아래여야 한다.
    pub fn new(portal: &str) -> Result<Self> {
        let portal = crate::web_url(portal)?;
        let host = portal.host_str().unwrap_or_default().to_ascii_lowercase();
        let labels: Vec<&str> = host.split('.').collect();
        let parent = if labels.len() > 2 {
            labels[1..].join(".")
        } else {
            host.clone()
        };
        Ok(Self {
            portal,
            user_agent: DEFAULT_USER_AGENT.to_owned(),
            timeout: Duration::from_secs(120),
            mail_host_suffix: format!(".{parent}"),
        })
    }
}

/// 웹메일의 메일 파일 위치(로그인 때마다 다시 찾는다).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailLocation {
    pub host: String,
    /// `mail9/u000001.nsf`
    pub db: String,
}

#[derive(Debug, Clone)]
pub struct DominoLogin {
    pub location: MailLocation,
    /// 메일함 한도(바이트, 시작 페이지의 `MailDbSize`). 모르면 없음.
    pub quota_limit: Option<i64>,
    /// 받은 쿠키 이름(진단용, 값은 담지 않는다).
    pub cookie_names: Vec<String>,
}

/// 메일 조각·첨부 파일 하나.
#[derive(Debug, Clone)]
pub struct Fetched {
    /// 응답의 `Content-Type`(없으면 빈 문자열).
    pub content_type: String,
    pub data: Vec<u8>,
}

impl Fetched {
    /// `Content-Type`의 `charset`.
    pub fn charset(&self) -> Option<&str> {
        self.content_type.split(';').skip(1).find_map(|p| {
            let (k, v) = p.split_once('=')?;
            k.trim()
                .eq_ignore_ascii_case("charset")
                .then(|| v.trim().trim_matches('"'))
        })
    }

    /// `charset`대로 글자로(모르면 UTF-8).
    pub fn text(&self) -> String {
        decode(&self.data, self.charset())
    }
}

/// 웹메일의 메일함(보기 이름과 삭제 주소의 `category`). 개인 폴더는 범주 이름.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Mailbox {
    Inbox,
    Sent,
    Pop3,
    Draft,
    Trash,
    /// 개인 폴더(`FolderTree`의 `folder_link('이름')`).
    Folder(String),
}

impl Mailbox {
    /// 목록 보기 이름(`XML_Inbox` 등). 개인 폴더는 `XML_Category`.
    pub fn view(&self) -> &'static str {
        match self {
            Self::Inbox => "XML_Inbox",
            Self::Sent => "XML_Sent",
            Self::Pop3 => "XML_POP3",
            Self::Draft => "XML_Draft",
            Self::Trash => "XML_Trash",
            Self::Folder(_) => "XML_Category",
        }
    }

    /// 저장해 두는 열쇠: 기본 메일함은 보기 이름, 개인 폴더는 `XML_Category:이름`.
    pub fn key(&self) -> String {
        match self {
            Self::Folder(f) => format!("XML_Category:{f}"),
            other => other.view().to_owned(),
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Some(match key {
            "XML_Inbox" => Self::Inbox,
            "XML_Sent" => Self::Sent,
            "XML_POP3" => Self::Pop3,
            "XML_Draft" => Self::Draft,
            "XML_Trash" => Self::Trash,
            k => Self::Folder(k.strip_prefix("XML_Category:")?.to_owned()),
        })
    }

    /// 삭제 주소의 `category` 값(브라우저처럼 `$`는 그대로, 폴더 이름만 인코딩).
    fn category(&self) -> String {
        match self {
            Self::Inbox => "Inbox".into(),
            Self::Sent => "Sent".into(),
            Self::Pop3 => "POP3".into(),
            Self::Draft => "Draft".into(),
            Self::Trash => "Trash".into(),
            Self::Folder(f) => format!("$$Str{}", utf8_percent_encode(f, NON_ALPHANUMERIC)),
        }
    }
}

struct Session {
    mail: Http,
    loc: MailLocation,
    quota_limit: Option<i64>,
    generation: u64,
}

/// HCL Domino 웹메일 세션. 하나를 여러 작업이 같이 쓴다.
pub struct Domino {
    /// 포털(설정에서 바꾸면 [`Domino::configure`]로). 없으면 로그인하지 않는다.
    cfg: RwLock<Option<DominoConfig>>,
    creds: Mutex<Option<Credentials>>,
    session: RwLock<Option<Arc<Session>>>,
    login_lock: tokio::sync::Mutex<()>,
    generation: AtomicU64,
}

impl Domino {
    pub fn new(cfg: DominoConfig) -> Self {
        let d = Self::unconfigured();
        *write(&d.cfg) = Some(cfg);
        d
    }

    /// 포털 주소를 아직 모른다(설정에서 넣기 전).
    pub fn unconfigured() -> Self {
        Self {
            cfg: RwLock::new(None),
            creds: Mutex::new(None),
            session: RwLock::new(None),
            login_lock: tokio::sync::Mutex::new(()),
            generation: AtomicU64::new(0),
        }
    }

    /// 포털을 바꾼다(없애면 `None`). 바뀌면 세션을 지운다.
    pub fn configure(&self, cfg: Option<DominoConfig>) {
        let mut cur = write(&self.cfg);
        let same = match (cur.as_ref(), cfg.as_ref()) {
            (Some(a), Some(b)) => a.portal == b.portal,
            (None, None) => true,
            _ => false,
        };
        if !same {
            *cur = cfg;
            drop(cur);
            self.forget();
        }
    }

    /// 포털 주소(설정했을 때).
    pub fn portal(&self) -> Option<Url> {
        read(&self.cfg).as_ref().map(|c| c.portal.clone())
    }

    fn config(&self) -> Result<DominoConfig> {
        read(&self.cfg).clone().ok_or_else(|| {
            Error::LoginUnexpected("HCL Domino 포털 주소를 설정에서 넣어 주세요".into())
        })
    }

    pub fn is_logged_in(&self) -> bool {
        read(&self.session).is_some()
    }

    /// 로그인한 아이디(메모리에만).
    pub fn username(&self) -> Option<String> {
        lock(&self.creds).as_ref().map(|c| c.username.clone())
    }

    pub fn location(&self) -> Option<MailLocation> {
        read(&self.session).as_ref().map(|s| s.loc.clone())
    }

    pub fn quota_limit(&self) -> Option<i64> {
        read(&self.session).as_ref().and_then(|s| s.quota_limit)
    }

    /// 로그아웃: 메모리의 아이디·비밀번호와 세션을 지운다.
    pub fn forget(&self) {
        *lock(&self.creds) = None;
        *write(&self.session) = None;
    }

    /// 로그인. 성공하면 아이디·비밀번호를 메모리에 둔다(세션이 끝나면 다시 로그인하려고).
    pub async fn login(&self, creds: Credentials) -> Result<DominoLogin> {
        let _guard = self.login_lock.lock().await;
        let (session, cookie_names) = self.open_session(&creds).await?;
        let info = DominoLogin {
            location: session.loc.clone(),
            quota_limit: session.quota_limit,
            cookie_names,
        };
        *write(&self.session) = Some(Arc::new(session));
        *lock(&self.creds) = Some(creds);
        Ok(info)
    }

    async fn open_session(&self, creds: &Credentials) -> Result<(Session, Vec<String>)> {
        let cfg = self.config()?;
        let portal = Http::new(&cfg.portal, &cfg.user_agent, cfg.timeout, false, None)?;
        let form = Zeroizing::new(
            url::form_urlencoded::Serializer::new(String::new())
                .append_pair("username", &creds.username)
                .append_pair("password", creds.password.expose())
                .append_pair("RedirectTo", "/home.nsf/StartHome?openform")
                .finish(),
        );
        let resp = send(&portal, true, "names.nsf?Login", form.as_bytes().to_vec()).await?;
        let portal_root = cfg.portal.clone();
        let has_token = portal
            .cookies_for(&portal_root)
            .iter()
            .any(|(n, _)| n.eq_ignore_ascii_case("LtpaToken"));
        if !has_token {
            let body = decode(&resp.body, charset_of(&resp));
            return Err(if notes::is_login_page(&body) || resp.status.is_success() {
                Error::BadCredentials {
                    reason: "로그인 화면으로 돌아옴".into(),
                }
            } else {
                Error::LoginUnexpected(format!("HTTP {}", resp.status.as_u16()))
            });
        }
        let home = send(&portal, false, "home.nsf/StartHome?openform", Vec::new()).await?;
        if !home.status.is_success() {
            return Err(Error::Status {
                status: home.status.as_u16(),
                context: "HCL Domino 시작 페이지".into(),
            });
        }
        let html = decode(&home.body, charset_of(&home).or(Some("euc-kr")));
        let (host, db) = notes::parse_start_home(&html)
            .ok_or_else(|| Error::LoginUnexpected("웹메일 서버를 찾지 못했습니다".into()))?;
        let host_name = host.split(':').next().unwrap_or_default();
        let portal_host = cfg.portal.host_str().unwrap_or_default();
        if !(host_name.ends_with(&cfg.mail_host_suffix) || host_name == portal_host) {
            return Err(Error::LoginUnexpected(format!(
                "포털과 다른 도메인의 웹메일 서버: {host}"
            )));
        }
        let mail_url = Url::parse(&format!("{}://{host}/", cfg.portal.scheme()))?;
        let mail = Http::new(&mail_url, &cfg.user_agent, cfg.timeout, false, None)?;
        let cookies = portal.cookies_for(&mail_url);
        for (n, v) in &cookies {
            mail.add_cookie(&format!("{n}={v}; Path=/"));
        }
        // 한도: 시작 페이지 변수(실계정은 이것), 없으면 쿠키.
        let quota_limit = notes::quota_limit_from_start_home(&html).or_else(|| {
            cookies
                .iter()
                .find(|(n, _)| n == "ckUserInfo")
                .and_then(|(_, v)| notes::quota_limit_from_cookie(v))
        });
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        Ok((
            Session {
                mail,
                loc: MailLocation { host, db },
                quota_limit,
                generation,
            },
            cookies.into_iter().map(|(n, _)| n).collect(),
        ))
    }

    fn current(&self) -> Result<Arc<Session>> {
        read(&self.session).clone().ok_or(Error::LoginRequired)
    }

    /// 세션이 끝났으면 메모리의 아이디·비밀번호로 한 번 다시 로그인. 실패하면(비밀번호를 바꾼 경우 등) 다시
    /// 시도하지 않는다: 잊고 [`Error::LoginRequired`](화면이 로그인 창을 띄운다). 로그인을 연속으로 여러 번 틀리면
    /// 계정이 잠기는 서버라서(사용자 알림 2026-10-26) 자동으로 틀리는 것은 한 번까지다. 서버에 닿지 못한 연결 실패·
    /// 서버 오류만 아이디·비밀번호를 두고 다음 확인 때 다시 본다.
    async fn relogin(&self, seen: u64) -> Result<()> {
        let _guard = self.login_lock.lock().await;
        if read(&self.session).as_ref().map(|s| s.generation) != Some(seen) {
            return Ok(());
        }
        let creds = lock(&self.creds).clone().ok_or(Error::LoginRequired)?;
        match self.open_session(&creds).await {
            Ok((s, _)) => {
                *write(&self.session) = Some(Arc::new(s));
                Ok(())
            }
            Err(e) if crate::transient_login_error(&e) => Err(e),
            Err(_) => {
                self.forget();
                Err(Error::LoginRequired)
            }
        }
    }

    /// 메일 파일 안의 경로로 GET(허용 목록 확인, 세션이 끝났으면 다시 로그인해 한 번 더).
    /// 연결이 끊기면(큰 첨부를 받다가 등, 실계정 2026-10-21) 한 번 더 보낸다. 보내는 요청은 모두 여러 번 보내도 같다.
    async fn get(&self, rel: impl Fn(&MailLocation) -> String) -> Result<Response> {
        let mut relogged = false;
        let mut resent = false;
        loop {
            let s = self.current()?;
            let path = rel(&s.loc);
            let resp = match send(&s.mail, false, &path, Vec::new()).await {
                Ok(r) => r,
                Err(Error::Http(_)) if !resent => {
                    resent = true;
                    continue;
                }
                Err(e) => return Err(e),
            };
            if !is_expired(&resp) {
                return Ok(resp);
            }
            if relogged {
                return Err(Error::SessionExpired);
            }
            relogged = true;
            self.relogin(s.generation).await?;
        }
    }

    /// 목록 한 쪽(`start`는 1부터). `category`: 개인 폴더(`XML_Category`).
    pub async fn view_page(
        &self,
        view: &str,
        category: Option<&str>,
        start: usize,
        count: usize,
    ) -> Result<ViewPage> {
        let restrict = category
            .map(|c| {
                format!(
                    "&RestrictToCategory={}",
                    utf8_percent_encode(&format!("$$Str{c}"), NON_ALPHANUMERIC)
                )
            })
            .unwrap_or_default();
        let resp = self
            .get(|l| {
                format!(
                    "{}/{view}?ReadViewEntries&start={start}&count={count}{restrict}",
                    l.db
                )
            })
            .await?;
        ok_status(&resp, view)?;
        let xml = decode(&resp.body, charset_of(&resp));
        notes::parse_view(&xml).ok_or_else(|| Error::Decode {
            action: view.to_owned(),
            detail: "목록 XML이 아님".into(),
        })
    }

    /// 목록 전부(쪽을 넘기며, 같은 메일은 한 번).
    pub async fn view_all(&self, view: &str, category: Option<&str>) -> Result<Vec<ViewEntry>> {
        let mut out: Vec<ViewEntry> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut start = 1;
        // 계층 보기(개인 폴더 트리)는 위치가 `1.2`라 쪽 번호로 넘길 수 없다: 한 번에 넉넉히.
        let count = if view == "FolderTree" { 2000 } else { PAGE };
        for _ in 0..400 {
            let page = self.view_page(view, category, start, count).await?;
            let n = page.entries.len();
            for e in page.entries {
                let key = e.unid.clone().unwrap_or_else(|| e.position.clone());
                if seen.insert(key) {
                    out.push(e);
                }
            }
            if n < count || view == "FolderTree" {
                break;
            }
            start += n;
        }
        Ok(out)
    }

    /// 메일 열기(Memo 양식). 서버가 'new' 표시를 지운다: 'new'였으면 [`Self::mark_unread`]로 되돌린다.
    pub async fn memo(&self, unid: &str) -> Result<Memo> {
        let unid = unid.to_ascii_uppercase();
        let resp = self
            .get(|l| format!("{}/all/{unid}?OpenDocument&Charset=UTF-8", l.db))
            .await?;
        ok_status(&resp, "메일 열기")?;
        let html = decode(&resp.body, charset_of(&resp));
        Ok(notes::parse_memo(&html, &unid))
    }

    /// 본문 조각(`M1.2`) 또는 서식 있는 본문의 요소(`0.1A2`). `extra`: 덧붙일 질의
    /// (글 조각은 `&Charset=UTF-8`, 서식 있는 본문의 그림은 `&FieldElemFormat=gif`).
    pub async fn body_element(&self, unid: &str, element: &str, extra: &str) -> Result<Fetched> {
        let unid = unid.to_ascii_lowercase();
        let resp = self
            .get(|l| format!("{}/0/{unid}/Body/{element}?OpenElement{extra}", l.db))
            .await?;
        ok_status(&resp, "본문 조각")?;
        Ok(fetched(resp))
    }

    /// 메일 파일 안 경로(`all/{UNID}/$FILE/이름`처럼 메일에 있던 링크)의 파일.
    /// `link`: 메일의 링크 그대로(이 서버의 절대 주소, `/메일 파일/…`, 메일 파일 기준 상대 경로).
    pub async fn file(&self, link: &str) -> Result<Fetched> {
        let resp = self
            .get(|l| {
                let path = link
                    .strip_prefix("https://")
                    .or_else(|| link.strip_prefix("http://"))
                    .and_then(|r| r.strip_prefix(l.host.as_str()))
                    .unwrap_or(link)
                    .trim_start_matches('/');
                if path.starts_with(&format!("{}/", l.db)) {
                    path.to_owned()
                } else {
                    format!("{}/{path}", l.db)
                }
            })
            .await?;
        ok_status(&resp, "첨부 파일")?;
        Ok(fetched(resp))
    }

    /// 웹메일의 안읽음 표시('new' 되돌리기). `noteids`: 목록의 `noteid`.
    pub async fn mark_unread(&self, noteids: &[String]) -> Result<()> {
        self.set_read(noteids, false).await
    }

    /// 웹메일의 읽음(`type=1`)·안읽음(`type=2`) 표시.
    pub async fn set_read(&self, noteids: &[String], read: bool) -> Result<()> {
        let t = if read { 1 } else { 2 };
        self.each_chunk(noteids, "읽음 표시", |l, ids| {
            format!("{}/wMarkRead?OpenAgent&&docid={ids}&type={t}", l.db)
        })
        .await
    }

    /// 웹메일의 ★(중요 메일) 켜기(`type=N`)·끄기(`type=H`). 화면 스크립트처럼 한 통씩.
    pub async fn set_star(&self, noteid: &str, on: bool) -> Result<()> {
        let t = if on { "N" } else { "H" };
        let resp = self
            .get(|l| format!("{}/agt_impo?OpenAgent&sid={noteid}&type={t}", l.db))
            .await?;
        ok_status(&resp, "중요 표시")
    }

    /// 웹메일의 삭제: 지운 메일이 아니면 지운 메일로, 지운 메일이면 완전 삭제.
    pub async fn delete_docs(&self, from: &Mailbox, noteids: &[String]) -> Result<()> {
        let category = from.category();
        self.each_chunk(noteids, "서버에서 삭제", |l, ids| {
            format!(
                "{}/WebDelDoc2?OpenAgent&category={category}&docid={ids}",
                l.db
            )
        })
        .await
    }

    /// 지운 메일 → 받은 메일(웹메일의 "복구").
    pub async fn recover(&self, noteids: &[String]) -> Result<()> {
        self.each_chunk(noteids, "복구", |l, ids| {
            format!(
                "{}/RecoverDelDoc?OpenAgent&RTView=($Inbox)&DocList={ids}",
                l.db
            )
        })
        .await
    }

    /// 개인 폴더로 이동.
    pub async fn move_to_folder(&self, noteids: &[String], folder: &str) -> Result<()> {
        let fname = utf8_percent_encode(folder, NON_ALPHANUMERIC).to_string();
        self.each_chunk(noteids, "폴더 이동", |l, ids| {
            format!("{}/folder_move?OpenAgent&docid={ids}&fn={fname}", l.db)
        })
        .await
    }

    /// 개인 폴더 → 원래 메일함(웹메일의 "원래의 메일함으로 이동").
    pub async fn move_back(&self, noteids: &[String]) -> Result<()> {
        self.each_chunk(noteids, "원래 메일함으로 이동", |l, ids| {
            format!("{}/wFolderMoveCancel?OpenAgent&&docid={ids}", l.db)
        })
        .await
    }

    /// 노트 번호를 나눠 여러 번 보낸다.
    async fn each_chunk(
        &self,
        noteids: &[String],
        context: &str,
        path: impl Fn(&MailLocation, &str) -> String,
    ) -> Result<()> {
        for chunk in noteids.chunks(IDS_PER_REQUEST) {
            let ids = chunk.join(",");
            let resp = self.get(|l| path(l, &ids)).await?;
            ok_status(&resp, context)?;
        }
        Ok(())
    }

    /// 메일함 (사용량, 한도)(바이트). 사용량은 웹메일의 "메일용량확인" 창(`mail_size?openform`, 누를 때마다 새 값),
    /// 읽지 못하면 `Size_Sum` 보기 합계. 한도는 로그인 때 시작 페이지에서 읽은 값을 먼저 쓴다: 그 창의 `max`는
    /// 화면 스크립트가 만드는 쿠키가 없으면 기본값(70)을 보여 준다(실계정 2026-10-21, 브라우저에서는 300).
    pub async fn quota(&self) -> Result<(Option<i64>, Option<i64>)> {
        let resp = self.get(|l| format!("{}/mail_size?openform", l.db)).await?;
        if resp.status.is_success() {
            let html = decode(&resp.body, charset_of(&resp).or(Some("euc-kr")));
            if let Some(s) = notes::parse_mail_size(&html) {
                return Ok((Some(s.total), self.quota_limit().or(s.limit)));
            }
        }
        let page = self.view_page("Size_Sum", None, 1, 1).await?;
        let used = page
            .entries
            .first()
            .and_then(|e| e.first_value())
            .and_then(|v| v.trim().parse::<f64>().ok())
            .map(|v| v as i64);
        Ok((used, self.quota_limit()))
    }
}

/// 허용 목록을 거쳐 보낸다.
async fn send(http: &Http, post: bool, path: &str, body: Vec<u8>) -> Result<Response> {
    if !notes::allowed(post, path) {
        return Err(Error::NotAllowed(path.to_owned()));
    }
    let req = if post {
        Request::post(path, body).header("Content-Type", "application/x-www-form-urlencoded")?
    } else {
        Request::get(path)
    };
    http.send(req).await
}

/// 세션이 끝났다는 응답: 로그인 화면으로 보내거나, 로그인 화면 HTML을 준다.
fn is_expired(resp: &Response) -> bool {
    if resp.status.is_redirection() {
        return resp.header(LOCATION).is_some_and(|l| {
            let l = l.to_ascii_lowercase();
            l.contains("names.nsf") || l.contains("?login") || l.contains("returnauthorization")
        });
    }
    // 형식 머리글은 보지 않는다(서버마다 다르다). 큰 파일은 로그인 화면일 리 없다.
    let markup = resp.body.len() < 256 * 1024
        && resp
            .body
            .iter()
            .find(|b| !b.is_ascii_whitespace())
            .is_some_and(|b| *b == b'<');
    (resp.status == StatusCode::OK || resp.status == StatusCode::UNAUTHORIZED)
        && markup
        && notes::is_login_page(&String::from_utf8_lossy(&resp.body))
}

fn ok_status(resp: &Response, context: &str) -> Result<()> {
    if resp.status.is_success() {
        Ok(())
    } else {
        Err(Error::Status {
            status: resp.status.as_u16(),
            context: context.to_owned(),
        })
    }
}

fn fetched(resp: Response) -> Fetched {
    Fetched {
        content_type: resp.header(CONTENT_TYPE).unwrap_or_default().to_owned(),
        data: resp.body.to_vec(),
    }
}

fn charset_of(resp: &Response) -> Option<&str> {
    resp.header(CONTENT_TYPE)?.split(';').skip(1).find_map(|p| {
        let (k, v) = p.split_once('=')?;
        k.trim()
            .eq_ignore_ascii_case("charset")
            .then(|| v.trim().trim_matches('"'))
    })
}

/// 문자셋 이름대로 글자로(모르면 UTF-8, 틀린 바이트는 대체 문자).
pub fn decode(bytes: &[u8], charset: Option<&str>) -> String {
    let enc = charset
        .and_then(|c| encoding_rs::Encoding::for_label(c.trim().as_bytes()))
        .unwrap_or(encoding_rs::UTF_8);
    enc.decode(bytes).0.into_owned()
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn read<T>(l: &RwLock<T>) -> std::sync::RwLockReadGuard<'_, T> {
    l.read().unwrap_or_else(|e| e.into_inner())
}

fn write<T>(l: &RwLock<T>) -> std::sync::RwLockWriteGuard<'_, T> {
    l.write().unwrap_or_else(|e| e.into_inner())
}
