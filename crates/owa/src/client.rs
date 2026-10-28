use std::collections::VecDeque;
use std::slice;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, RwLock};
use std::time::{Duration, Instant, SystemTime};

use http::StatusCode;
use http::header::LOCATION;
use serde_json::Value;
use url::Url;
use zeroize::Zeroizing;

use crate::creds::{CredentialPrompt, Credentials};
use crate::error::{Error, ItemError, Result};
use crate::transport::{Http, Request, Response, offers_ntlm};
use crate::types::{
    Address, AttachmentId, DeleteType, FileAttachment, FindItemPage, Folder, FolderRef,
    HierarchySync, ItemId, ItemSummary, ItemSync, UserConfig,
};
use crate::{DEFAULT_USER_AGENT, requests, responses};

/// 요청 JSON 전달 방식(계획서 5.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostMode {
    /// (a) 본문 JSON으로 보내고, 형식 오류면 (b)로 한 번 더 시도한다.
    Auto,
    /// (a) 본문 JSON만.
    Body,
    /// (b) 빈 본문 + `X-OWA-UrlPostData` 헤더만.
    Header,
}

/// 서버 인증 방식. 익명 `GET /owa/`가 401 + `WWW-Authenticate: NTLM`이면 NTLM, 아니면 폼 로그인.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthScheme {
    /// Windows 통합 인증(NTLM). 연결마다 인증한다.
    Ntlm,
    /// 폼 기반 인증(FBA, `POST /owa/auth.owa`). 세션 쿠키.
    Forms,
}

#[derive(Debug, Clone)]
pub struct ClientConfig {
    pub base_url: Url,
    pub user_agent: String,
    pub post_mode: PostMode,
    pub timeout: Duration,
    /// NTLM에 TLS 채널 바인딩(확장 보호)을 넣는다. 진단용으로만 끈다.
    pub channel_binding: bool,
}

impl ClientConfig {
    /// 웹메일 서버 주소(`mail.example.com`, `https://mail.example.com` 등, 사용자가 설정에서 넣는다)로.
    pub fn new(base_url: &str) -> Result<Self> {
        let mut url = crate::web_url(base_url)?;
        if !url.path().ends_with('/') {
            url.set_path(&format!("{}/", url.path()));
        }
        Ok(Self {
            base_url: url,
            user_agent: DEFAULT_USER_AGENT.to_owned(),
            post_mode: PostMode::Auto,
            timeout: Duration::from_secs(120),
            channel_binding: true,
        })
    }
}

/// 로그인 결과 진단 정보. 쿠키 값은 담지 않는다.
#[derive(Debug, Clone)]
pub struct LoginInfo {
    pub scheme: AuthScheme,
    pub cookie_names: Vec<String>,
    pub has_canary: bool,
    pub elapsed: Duration,
}

#[derive(Debug, Clone)]
pub struct SessionEvent {
    pub at: SystemTime,
    pub kind: SessionEventKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEventKind {
    LoggedIn,
    /// 세션 만료(인증 거부) 감지. `signal`은 서버 응답 형태(예: `HTTP 440`).
    SessionExpired {
        action: String,
        signal: String,
    },
    Relogin {
        ok: bool,
    },
    /// 재로그인이 비밀번호 오류로 거부돼 사용자에게 다시 물었다.
    CredentialsReprompted,
    CanaryRetry {
        action: String,
    },
    PostModeFallback {
        action: String,
    },
    /// 로그오프(폼 로그인) 또는 연결 끊기(NTLM).
    LoggedOff,
}

/// 헤더 방식으로 바꿔 보낼 수 있는 최대 크기. 첨부처럼 큰 요청은 본문 방식만 쓴다.
const MAX_HEADER_PAYLOAD: usize = 16 * 1024;
const MAX_EVENTS: usize = 200;

struct Session {
    http: Arc<Http>,
    generation: u64,
}

struct Inner {
    cfg: ClientConfig,
    prompt: Option<Arc<dyn CredentialPrompt>>,
    session: RwLock<Option<Arc<Session>>>,
    /// 실행 중 메모리에만 두는 아이디·비밀번호(계획서 5.2). 재로그인에 쓴다.
    creds: Mutex<Option<Credentials>>,
    scheme: Mutex<Option<AuthScheme>>,
    login_lock: tokio::sync::Mutex<()>,
    generation: AtomicU64,
    events: Mutex<VecDeque<SessionEvent>>,
}

/// 웹메일 세션. 복제해도 같은 세션을 공유한다.
#[derive(Clone)]
pub struct Client {
    inner: Arc<Inner>,
}

enum SendFailure {
    Expired(String),
    Canary,
    Error(Error),
}

impl From<Error> for SendFailure {
    fn from(e: Error) -> Self {
        Self::Error(e)
    }
}

impl Client {
    /// `prompt`: 재로그인이 비밀번호 오류로 거부될 때 다시 묻는 방법. 없으면 [`Error::LoginRequired`].
    pub fn new(cfg: ClientConfig, prompt: Option<Arc<dyn CredentialPrompt>>) -> Self {
        Self {
            inner: Arc::new(Inner {
                cfg,
                prompt,
                session: RwLock::new(None),
                creds: Mutex::new(None),
                scheme: Mutex::new(None),
                login_lock: tokio::sync::Mutex::new(()),
                generation: AtomicU64::new(0),
                events: Mutex::new(VecDeque::new()),
            }),
        }
    }

    pub fn config(&self) -> &ClientConfig {
        &self.inner.cfg
    }

    pub fn is_logged_in(&self) -> bool {
        read(&self.inner.session).is_some()
    }

    /// 판별된 인증 방식(로그인 전에는 없음).
    pub fn scheme(&self) -> Option<AuthScheme> {
        *lock(&self.inner.scheme)
    }

    /// 현재 세션의 NTLM 연결 인증 횟수(진단용).
    pub fn auth_handshakes(&self) -> u64 {
        read(&self.inner.session)
            .as_ref()
            .map_or(0, |s| s.http.handshakes())
    }

    /// 최근 세션 사건(최대 200개, 오래된 것부터).
    pub fn events(&self) -> Vec<SessionEvent> {
        lock(&self.inner.events).iter().cloned().collect()
    }

    /// 로그인(계획서 5.2). 인증 방식은 처음 한 번 판별한다. 성공하면 자격 증명을 메모리에 둔다.
    pub async fn login(&self, creds: Credentials) -> Result<LoginInfo> {
        let _guard = self.inner.login_lock.lock().await;
        let started = Instant::now();
        let http = self.login_once(&creds).await?;
        self.install(http.clone());
        *lock(&self.inner.creds) = Some(creds);
        self.event(SessionEventKind::LoggedIn);
        Ok(LoginInfo {
            scheme: if http.is_ntlm() {
                AuthScheme::Ntlm
            } else {
                AuthScheme::Forms
            },
            cookie_names: http.cookie_names(),
            has_canary: http.cookie("X-OWA-CANARY").is_some(),
            elapsed: started.elapsed(),
        })
    }

    /// 로그아웃: 메모리의 아이디·비밀번호와 세션 쿠키를 지운다(계획서 9.5).
    pub fn forget(&self) {
        *lock(&self.inner.creds) = None;
        *write(&self.inner.session) = None;
    }

    /// 세션을 끊는다. 폼 로그인은 `/owa/logoff.owa`, NTLM은 연결을 모두 닫는다.
    /// 자격 증명은 남아 있어 다음 호출이 다시 인증한다. 세션 만료·재로그인 확인용.
    pub async fn logoff(&self) -> Result<()> {
        let session = self.current_session()?;
        if session.http.is_ntlm() {
            session.http.drop_connections();
        } else {
            session.http.send(Request::get("owa/logoff.owa")).await?;
        }
        self.event(SessionEventKind::LoggedOff);
        Ok(())
    }

    /// `service.svc` 호출: 본문을 공통 머리로 감싸 보낸다.
    pub async fn call(&self, action: &str, body: Value) -> Result<Value> {
        let request = requests::envelope(&requests::json_request_type(action), body);
        self.call_raw(action, Some(request), self.inner.cfg.post_mode)
            .await
    }

    /// `service.svc` 호출(감싼 요청을 그대로). `request`가 없으면 빈 본문으로 POST.
    /// 세션 만료면 재로그인 후 1회, 캐너리 오류(449)면 1회, `Auto`에서 형식 오류면 헤더 방식으로 1회 재시도한다.
    pub async fn call_raw(
        &self,
        action: &str,
        request: Option<Value>,
        mode: PostMode,
    ) -> Result<Value> {
        let mut transport = if mode == PostMode::Header {
            PostMode::Header
        } else {
            PostMode::Body
        };
        let (mut relogged, mut canary_retried, mut fell_back) = (false, false, false);
        loop {
            let session = self.current_session()?;
            match self
                .send_once(&session, action, request.as_ref(), transport)
                .await
            {
                Ok(v) => return Ok(v),
                Err(SendFailure::Expired(signal)) => {
                    self.event(SessionEventKind::SessionExpired {
                        action: action.to_owned(),
                        signal,
                    });
                    if relogged {
                        return Err(Error::SessionExpired);
                    }
                    relogged = true;
                    self.relogin(session.generation).await?;
                }
                Err(SendFailure::Canary) if !canary_retried => {
                    canary_retried = true;
                    self.event(SessionEventKind::CanaryRetry {
                        action: action.to_owned(),
                    });
                }
                Err(SendFailure::Canary) => {
                    return Err(Error::Status {
                        status: 449,
                        context: action.to_owned(),
                    });
                }
                Err(SendFailure::Error(e @ Error::RequestFormat { .. })) => {
                    let fits = request.as_ref().is_some_and(|r| {
                        requests::encode_uri_component(&r.to_string()).len() <= MAX_HEADER_PAYLOAD
                    });
                    if mode != PostMode::Auto || transport != PostMode::Body || fell_back || !fits {
                        return Err(e);
                    }
                    fell_back = true;
                    transport = PostMode::Header;
                    self.event(SessionEventKind::PostModeFallback {
                        action: action.to_owned(),
                    });
                }
                Err(SendFailure::Error(e)) => return Err(e),
            }
        }
    }

    pub async fn get_folders(&self, ids: &[FolderRef]) -> Result<Vec<Result<Folder, ItemError>>> {
        let v = self.call("GetFolder", requests::get_folder(ids)).await?;
        responses::parse_get_folder(&v)
    }

    pub async fn get_folder(&self, id: &FolderRef) -> Result<Folder> {
        self.get_folders(slice::from_ref(id))
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| Error::Decode {
                action: "GetFolder".into(),
                detail: "응답 메시지 0개".into(),
            })?
            .map_err(|e| Error::Response {
                action: "GetFolder".into(),
                code: e.code,
                message: e.message,
            })
    }

    pub async fn find_folders_deep(&self) -> Result<Vec<Folder>> {
        let v = self
            .call("FindFolder", requests::find_folder_deep())
            .await?;
        responses::parse_find_folder(&v)
    }

    pub async fn sync_folder_hierarchy(&self, sync_state: Option<&str>) -> Result<HierarchySync> {
        let v = self
            .call(
                "SyncFolderHierarchy",
                requests::sync_folder_hierarchy(sync_state),
            )
            .await?;
        responses::parse_sync_folder_hierarchy(&v)
    }

    /// 목록용 속성 포함 항목 동기화 한 번(최대 512건). `includes_last`가 false면 반복 호출한다.
    pub async fn sync_folder_items(
        &self,
        folder: &FolderRef,
        sync_state: Option<&str>,
    ) -> Result<ItemSync> {
        let body = requests::sync_folder_items(
            folder,
            sync_state,
            requests::MAX_CHANGES_RETURNED,
            requests::LIST_PROPERTIES,
        );
        let v = self.call("SyncFolderItems", body).await?;
        responses::parse_sync_folder_items(&v, requests::MAX_CHANGES_RETURNED)
    }

    pub async fn find_items(
        &self,
        folder: &FolderRef,
        offset: u32,
        max_entries: u32,
        props: &[&str],
    ) -> Result<FindItemPage> {
        let body = requests::find_item(folder, offset, max_entries, props);
        let v = self.call("FindItem", body).await?;
        responses::parse_find_item(&v)
    }

    /// 원문 EML. 요청한 순서대로 바이트 또는 항목 오류.
    pub async fn get_mime(&self, ids: &[ItemId]) -> Result<Vec<Result<Vec<u8>, ItemError>>> {
        let v = self.call("GetItem", requests::get_item_mime(ids)).await?;
        responses::parse_get_item_mime(&v)
    }

    /// 이동(메일함 상태 변경). 요청한 순서대로 새 항목 ID 또는 항목 오류.
    pub async fn move_items(
        &self,
        ids: &[ItemId],
        to: &FolderRef,
    ) -> Result<Vec<Result<Option<ItemId>, ItemError>>> {
        let v = self.call("MoveItem", requests::move_item(ids, to)).await?;
        responses::parse_transfer("MoveItem", &v)
    }

    /// 복사(메일함 상태 변경). 요청한 순서대로 새 항목 ID 또는 항목 오류.
    pub async fn copy_items(
        &self,
        ids: &[ItemId],
        to: &FolderRef,
    ) -> Result<Vec<Result<Option<ItemId>, ItemError>>> {
        let v = self.call("CopyItem", requests::copy_item(ids, to)).await?;
        responses::parse_transfer("CopyItem", &v)
    }

    /// 삭제(메일함 상태 변경). `HardDelete`는 되돌릴 수 없다.
    pub async fn delete_items(
        &self,
        ids: &[ItemId],
        delete_type: DeleteType,
    ) -> Result<Vec<Result<(), ItemError>>> {
        let v = self
            .call("DeleteItem", requests::delete_item(ids, delete_type))
            .await?;
        responses::parse_delete(&v)
    }

    /// 읽음·안읽음(메일함 상태 변경). 요청한 순서대로 결과(갱신된 ID 또는 항목 오류). 최대 10건씩 부른다.
    pub async fn set_read(
        &self,
        items: &[(ItemId, bool)],
    ) -> Result<Vec<Result<Option<ItemId>, ItemError>>> {
        let v = self
            .call("UpdateItem", requests::update_read(items))
            .await?;
        responses::parse_transfer("UpdateItem", &v)
    }

    /// 플래그(메일함 상태 변경). `flag`: 0 없음, 1 플래그, 2 완료. 최대 10건씩 부른다.
    pub async fn set_flag(
        &self,
        items: &[(ItemId, i64)],
    ) -> Result<Vec<Result<Option<ItemId>, ItemError>>> {
        let v = self
            .call("UpdateItem", requests::update_flag(items))
            .await?;
        responses::parse_transfer("UpdateItem", &v)
    }

    /// 항목의 지정한 속성(원문 없이).
    pub async fn get_items(
        &self,
        ids: &[ItemId],
        props: &[&str],
    ) -> Result<Vec<Result<ItemSummary, ItemError>>> {
        let v = self
            .call("GetItem", requests::get_item_properties(ids, props))
            .await?;
        responses::parse_get_items(&v)
    }

    /// 회신·전체 회신·전달 초안을 만든다(보내지 않음). 새 초안 ID(ChangeKey 포함).
    pub async fn create_reply_draft(&self, draft: &requests::ReplyDraft<'_>) -> Result<ItemId> {
        let v = self
            .call("CreateItem", requests::create_reply(draft))
            .await?;
        one_item_id("CreateItem", responses::parse_transfer("CreateItem", &v)?)?.ok_or_else(|| {
            Error::Decode {
                action: "CreateItem".into(),
                detail: "새 초안 ID 없음".into(),
            }
        })
    }

    /// 초안의 받는 사람을 `to`로 지정하고 보낸다(메일 발송).
    pub async fn send_draft(
        &self,
        draft: &ItemId,
        to: &[Address],
        compose_operation: &str,
    ) -> Result<()> {
        let v = self
            .call(
                "UpdateItem",
                requests::send_draft(draft, to, compose_operation),
            )
            .await?;
        one_item_id("UpdateItem", responses::parse_transfer("UpdateItem", &v)?).map(|_| ())
    }

    /// 새 메일 초안을 만든다(보내지 않음). 새 초안 ID(ChangeKey 포함).
    pub async fn create_new_mail_draft(&self, mail: &requests::NewMail<'_>) -> Result<ItemId> {
        let v = self
            .call("CreateItem", requests::create_new_mail(mail))
            .await?;
        one_item_id("CreateItem", responses::parse_transfer("CreateItem", &v)?)?.ok_or_else(|| {
            Error::Decode {
                action: "CreateItem".into(),
                detail: "새 초안 ID 없음".into(),
            }
        })
    }

    /// 원문(EML)으로 항목을 만들어 `folder`에 저장한다(보내지 않음).
    pub async fn create_item_from_mime(&self, mime: &[u8], folder: &FolderRef) -> Result<ItemId> {
        let v = self
            .call("CreateItem", requests::create_item_from_mime(mime, folder))
            .await?;
        one_item_id("CreateItem", responses::parse_transfer("CreateItem", &v)?)?.ok_or_else(|| {
            Error::Decode {
                action: "CreateItem".into(),
                detail: "새 항목 ID 없음".into(),
            }
        })
    }

    /// 초안에 파일을 첨부한다. 첨부 ID(초안의 새 ChangeKey 포함)와 서버가 돌려준 ContentId.
    pub async fn create_attachment(
        &self,
        parent: &ItemId,
        attachment: &FileAttachment,
    ) -> Result<(AttachmentId, Option<String>)> {
        let request = requests::envelope(
            requests::CREATE_ATTACHMENT_REQUEST_TYPE,
            requests::create_attachment(parent, attachment),
        );
        // 첨부는 커서 헤더 방식으로 보낼 수 없다(본문 방식만, 계획서 5.1).
        let v = self
            .call_raw(
                "CreateAttachmentFromLocalFile",
                Some(request),
                PostMode::Body,
            )
            .await?;
        responses::parse_create_attachment(&v)
    }

    /// 초안의 받는 사람·참조·숨은 참조·제목·본문·옵션을 지정한다(계획서 5.7 ③). `send`면 보내고(메일 발송),
    /// 아니면 임시 보관함에 저장만 한다(메일함 상태 변경). 저장이면 초안의 새 ID(ChangeKey 포함).
    pub async fn update_draft(
        &self,
        draft: &ItemId,
        fields: &requests::DraftFields<'_>,
        compose_operation: &str,
        send: bool,
    ) -> Result<Option<ItemId>> {
        let v = self
            .call(
                "UpdateItem",
                requests::update_draft(draft, fields, compose_operation, send),
            )
            .await?;
        one_item_id("UpdateItem", responses::parse_transfer("UpdateItem", &v)?)
    }

    /// 초안의 첨부를 지운다(메일함 상태 변경). 요청한 순서대로 초안의 새 ChangeKey 또는 항목 오류.
    pub async fn delete_attachments(
        &self,
        ids: &[String],
    ) -> Result<Vec<Result<Option<String>, ItemError>>> {
        let v = self
            .call("DeleteAttachment", requests::delete_attachment(ids))
            .await?;
        responses::parse_delete_attachment(&v)
    }

    /// 폴더 조작 한 번(0.7.0 폴더 메뉴, 메일함 상태 변경). 응답 항목 중 하나라도 실패면 그 오류.
    async fn folder_call(&self, action: &str, body: Value) -> Result<Value> {
        let v = self.call(action, body).await?;
        for m in responses::response_items(action, &v)? {
            if let Some(e) = responses::item_error(m) {
                return Err(Error::Response {
                    action: action.into(),
                    code: e.code,
                    message: e.message,
                });
            }
        }
        Ok(v)
    }

    /// 새 폴더를 만든다(메일함 상태 변경). 새 폴더 ID.
    pub async fn create_folder(&self, parent: &FolderRef, name: &str) -> Result<String> {
        let v = self
            .folder_call("CreateFolder", requests::create_folder(parent, name))
            .await?;
        v.pointer("/Body/ResponseMessages/Items/0/Folders/0/FolderId/Id")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| Error::Decode {
                action: "CreateFolder".into(),
                detail: "새 폴더 ID 없음".into(),
            })
    }

    /// 폴더 이름 바꾸기(메일함 상태 변경).
    pub async fn rename_folder(&self, id: &FolderRef, name: &str) -> Result<()> {
        self.folder_call("UpdateFolder", requests::rename_folder(id, name))
            .await
            .map(|_| ())
    }

    /// 폴더 삭제(지운 편지함으로, 메일함 상태 변경).
    pub async fn delete_folder(&self, id: &FolderRef) -> Result<()> {
        self.delete_folder_as(id, DeleteType::MoveToDeletedItems)
            .await
    }

    /// 폴더 삭제 방식을 골라(메일함 상태 변경). `HardDelete`는 되돌릴 수 없다.
    pub async fn delete_folder_as(&self, id: &FolderRef, delete_type: DeleteType) -> Result<()> {
        self.folder_call(
            "DeleteFolder",
            requests::delete_folder(slice::from_ref(id), delete_type),
        )
        .await
        .map(|_| ())
    }

    /// 폴더 이동(메일함 상태 변경).
    pub async fn move_folder(&self, id: &FolderRef, to: &FolderRef) -> Result<()> {
        self.folder_call("MoveFolder", requests::move_folder(slice::from_ref(id), to))
            .await
            .map(|_| ())
    }

    /// 폴더 비우기(메일함 상태 변경). 하위 폴더는 그대로.
    pub async fn empty_folder(&self, id: &FolderRef, delete_type: DeleteType) -> Result<()> {
        self.folder_call(
            "EmptyFolder",
            requests::empty_folder(slice::from_ref(id), delete_type),
        )
        .await
        .map(|_| ())
    }

    /// 모두 읽은 상태로 표시(메일함 상태 변경).
    pub async fn mark_all_read(&self, ids: &[FolderRef]) -> Result<()> {
        self.folder_call("MarkAllItemsAsRead", requests::mark_all_read(ids, true))
            .await
            .map(|_| ())
    }

    /// 주소 자동완성(계획서 5.9). 이름·주소 일부로 찾는다.
    pub async fn find_people(&self, query: &str, max: u32) -> Result<Vec<Address>> {
        let v = self
            .call("FindPeople", requests::find_people(query, max))
            .await?;
        responses::parse_find_people(&v)
    }

    /// 사용자 설정·용량(계획서 5.10). 본문 없이 POST.
    pub async fn user_config(&self) -> Result<UserConfig> {
        let v = self
            .call_raw("GetOwaUserConfiguration", None, PostMode::Body)
            .await?;
        responses::parse_user_config(&v)
    }

    fn event(&self, kind: SessionEventKind) {
        let mut events = lock(&self.inner.events);
        if events.len() == MAX_EVENTS {
            events.pop_front();
        }
        events.push_back(SessionEvent {
            at: SystemTime::now(),
            kind,
        });
    }

    fn current_session(&self) -> Result<Arc<Session>> {
        read(&self.inner.session)
            .clone()
            .ok_or(Error::LoginRequired)
    }

    fn install(&self, http: Arc<Http>) {
        let generation = self.inner.generation.fetch_add(1, Ordering::SeqCst) + 1;
        *write(&self.inner.session) = Some(Arc::new(Session { http, generation }));
    }

    fn new_http(&self, ntlm: Option<Credentials>) -> Result<Http> {
        let cfg = &self.inner.cfg;
        Http::new(
            &cfg.base_url,
            &cfg.user_agent,
            cfg.timeout,
            cfg.channel_binding,
            ntlm,
        )
    }

    /// 익명 `GET /owa/`로 인증 방식을 판별한다.
    async fn detect_scheme(&self) -> Result<AuthScheme> {
        let probe = self.new_http(None)?;
        let resp = probe.send(Request::get("owa/")).await?;
        Ok(
            if resp.status == StatusCode::UNAUTHORIZED && offers_ntlm(&resp.headers) {
                AuthScheme::Ntlm
            } else {
                AuthScheme::Forms
            },
        )
    }

    async fn login_once(&self, creds: &Credentials) -> Result<Arc<Http>> {
        let known = *lock(&self.inner.scheme);
        let scheme = match known {
            Some(s) => s,
            None => {
                let s = self.detect_scheme().await?;
                *lock(&self.inner.scheme) = Some(s);
                s
            }
        };
        match scheme {
            AuthScheme::Ntlm => self.login_ntlm(creds).await,
            AuthScheme::Forms => self.login_forms(creds).await,
        }
    }

    /// NTLM: 인증된 연결에서 `/owa/`를 열어 캐너리 쿠키를 받는다.
    async fn login_ntlm(&self, creds: &Credentials) -> Result<Arc<Http>> {
        let http = Arc::new(self.new_http(Some(creds.clone()))?);
        let resp = http.send(Request::get("owa/")).await?;
        check_owa_landing(&resp)?;
        Ok(http)
    }

    /// 폼 로그인: 로그인 화면 → 폼 POST → `/owa/`(캐너리 쿠키).
    async fn login_forms(&self, creds: &Credentials) -> Result<Arc<Http>> {
        let http = Arc::new(self.new_http(None)?);
        // 로그인 화면 스크립트가 제출 시 설정하는 쿠키(계획서 5.2).
        http.add_cookie("PrivateComputer=true; Path=/");
        http.add_cookie("PBack=0; Path=/");
        http.send(Request::get("owa/auth/logon.aspx?replaceCurrent=1"))
            .await?;

        let destination = self.inner.cfg.base_url.join("owa/")?;
        let form = Zeroizing::new(
            url::form_urlencoded::Serializer::new(String::new())
                .append_pair("destination", destination.as_str())
                .append_pair("flags", "4")
                .append_pair("forcedownlevel", "0")
                .append_pair("username", &creds.username)
                .append_pair("password", creds.password.expose())
                .append_pair("passwordText", "")
                .append_pair("isUtf8", "1")
                .finish(),
        );
        let resp = http
            .send(
                Request::post("owa/auth.owa", form.as_bytes().to_vec())
                    .header("Content-Type", "application/x-www-form-urlencoded")?,
            )
            .await?;
        match classify_login_response(resp.status, resp.header(LOCATION)) {
            LoginOutcome::Ok => {}
            LoginOutcome::Rejected(reason) if reason == "2" => {
                return Err(Error::BadCredentials {
                    reason: format!("reason={reason}"),
                });
            }
            LoginOutcome::Rejected(reason) => {
                return Err(Error::LoginUnexpected(format!(
                    "로그인 화면으로 돌아옴 (reason={reason})"
                )));
            }
            LoginOutcome::Unexpected(detail) => return Err(Error::LoginUnexpected(detail)),
        }

        let resp = http.send(Request::get("owa/")).await?;
        check_owa_landing(&resp)?;
        Ok(http)
    }

    /// 세션 만료 시 메모리의 자격 증명으로 재로그인. 비밀번호 오류면 사용자에게 다시 묻는다.
    async fn relogin(&self, seen_generation: u64) -> Result<()> {
        let _guard = self.inner.login_lock.lock().await;
        if self.inner.generation.load(Ordering::SeqCst) != seen_generation {
            return Ok(()); // 다른 호출이 이미 재로그인했다.
        }
        let mut creds = lock(&self.inner.creds)
            .clone()
            .ok_or(Error::LoginRequired)?;
        loop {
            match self.login_once(&creds).await {
                Ok(http) => {
                    self.install(http);
                    *lock(&self.inner.creds) = Some(creds);
                    self.event(SessionEventKind::Relogin { ok: true });
                    return Ok(());
                }
                Err(Error::BadCredentials { .. }) => {
                    let new = match &self.inner.prompt {
                        Some(prompt) => {
                            self.event(SessionEventKind::CredentialsReprompted);
                            prompt.credentials_rejected(&creds.username).await
                        }
                        None => None,
                    };
                    match new {
                        Some(new) => creds = new,
                        None => {
                            *lock(&self.inner.creds) = None;
                            self.event(SessionEventKind::Relogin { ok: false });
                            return Err(Error::LoginRequired);
                        }
                    }
                }
                // 연결 실패·서버 오류는 아이디·비밀번호를 두고 다음에 다시. 그 밖의 실패는 다시 시도하지 않는다
                // (사용자 요청 2026-10-26: 비밀번호를 바꾸면 화면이 로그인 창을 띄운다).
                Err(e) if crate::transient_login_error(&e) => {
                    self.event(SessionEventKind::Relogin { ok: false });
                    return Err(e);
                }
                Err(_) => {
                    *lock(&self.inner.creds) = None;
                    self.event(SessionEventKind::Relogin { ok: false });
                    return Err(Error::LoginRequired);
                }
            }
        }
    }

    async fn send_once(
        &self,
        session: &Session,
        action: &str,
        request: Option<&Value>,
        transport: PostMode,
    ) -> std::result::Result<Value, SendFailure> {
        let path = format!("owa/service.svc?action={action}&UA=0&app=Mail");
        let body = match (request, transport) {
            (Some(r), PostMode::Body | PostMode::Auto) => r.to_string().into_bytes(),
            _ => Vec::new(),
        };
        let mut req = Request::post(path, body)
            .header("Action", action)?
            .header("Content-Type", "application/json; charset=utf-8")?;
        if let Some(canary) = session.http.cookie("X-OWA-CANARY") {
            req = req.header("X-OWA-CANARY", &canary)?;
        }
        if let (Some(r), PostMode::Header) = (request, transport) {
            req = req.header(
                "X-OWA-UrlPostData",
                &requests::encode_uri_component(&r.to_string()),
            )?;
        }

        let resp = match session.http.send(req).await {
            Ok(r) => r,
            // NTLM 연결 재인증이 거부됐다(비밀번호 변경 등): 재로그인 절차로 넘긴다.
            Err(Error::BadCredentials { reason }) => {
                return Err(SendFailure::Expired(format!("재인증 거부 ({reason})")));
            }
            Err(e) => return Err(e.into()),
        };
        let status = resp.status;
        if status.as_u16() == 440 || status == StatusCode::UNAUTHORIZED {
            return Err(SendFailure::Expired(format!("HTTP {}", status.as_u16())));
        }
        if status.is_redirection() {
            let location = resp.header(LOCATION).unwrap_or_default();
            if is_logon_url(location) {
                return Err(SendFailure::Expired(format!(
                    "HTTP {} → {}",
                    status.as_u16(),
                    strip_origin(location)
                )));
            }
            return Err(SendFailure::Error(Error::Status {
                status: status.as_u16(),
                context: format!("{action} → {}", strip_origin(location)),
            }));
        }
        if status.as_u16() == 449 {
            return Err(SendFailure::Canary);
        }
        let parsed: Option<Value> = serde_json::from_slice(&resp.body).ok();
        if let Some((code, message)) = parsed.as_ref().and_then(responses::format_error) {
            return Err(SendFailure::Error(Error::RequestFormat {
                action: action.to_owned(),
                code,
                message,
            }));
        }
        if !status.is_success() {
            return Err(SendFailure::Error(Error::Status {
                status: status.as_u16(),
                context: action.to_owned(),
            }));
        }
        parsed.ok_or_else(|| {
            SendFailure::Error(Error::Decode {
                action: action.to_owned(),
                detail: format!("JSON이 아닌 응답 ({} bytes)", resp.body.len()),
            })
        })
    }
}

/// 항목 하나짜리 응답에서 ID(또는 없음)를 꺼내고, 항목 오류는 [`Error::Response`]로 바꾼다.
fn one_item_id(
    action: &str,
    results: Vec<std::result::Result<Option<ItemId>, ItemError>>,
) -> Result<Option<ItemId>> {
    match results.into_iter().next() {
        Some(Ok(id)) => Ok(id),
        Some(Err(e)) => Err(Error::Response {
            action: action.into(),
            code: e.code,
            message: e.message,
        }),
        None => Err(Error::Decode {
            action: action.into(),
            detail: "응답 메시지 0개".into(),
        }),
    }
}

/// 로그인 뒤 `/owa/`가 200이어야 한다. 리다이렉트(언어 선택 화면 등)는 해석할 수 없는 상태.
fn check_owa_landing(resp: &Response) -> Result<()> {
    if resp.status.is_redirection() {
        return Err(Error::LoginUnexpected(format!(
            "/owa/ → HTTP {} {}",
            resp.status.as_u16(),
            resp.header(LOCATION).map(strip_origin).unwrap_or_default()
        )));
    }
    if !resp.status.is_success() {
        return Err(Error::Status {
            status: resp.status.as_u16(),
            context: "GET /owa/".into(),
        });
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
enum LoginOutcome {
    Ok,
    /// 로그인 화면으로 돌아옴. `reason` 값(2 = 아이디·비밀번호 오류).
    Rejected(String),
    Unexpected(String),
}

fn classify_login_response(status: StatusCode, location: Option<&str>) -> LoginOutcome {
    if !status.is_redirection() {
        return LoginOutcome::Unexpected(format!(
            "로그인 POST 응답이 리다이렉트가 아님 (HTTP {})",
            status.as_u16()
        ));
    }
    let Some(location) = location else {
        return LoginOutcome::Unexpected("Location 헤더 없음".into());
    };
    if is_logon_url(location) {
        let reason = query_param(location, "reason").unwrap_or_else(|| "?".into());
        return LoginOutcome::Rejected(reason);
    }
    LoginOutcome::Ok
}

fn is_logon_url(location: &str) -> bool {
    location.to_ascii_lowercase().contains("/auth/logon.aspx")
}

fn query_param(location: &str, name: &str) -> Option<String> {
    let query = location.split_once('?')?.1;
    url::form_urlencoded::parse(query.as_bytes())
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.into_owned())
}

/// 진단 기록용: 절대 URL이면 경로와 질의만 남긴다.
fn strip_origin(location: &str) -> String {
    match Url::parse(location) {
        Ok(u) => match u.query() {
            Some(q) => format!("{}?{q}", u.path()),
            None => u.path().to_owned(),
        },
        Err(_) => location.to_owned(),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_redirect_to_owa_is_success() {
        let r = classify_login_response(StatusCode::FOUND, Some("https://mail.example.com/owa/"));
        assert_eq!(r, LoginOutcome::Ok);
    }

    #[test]
    fn login_redirect_to_logon_with_reason_2_is_rejected() {
        let loc = "https://mail.example.com/owa/auth/logon.aspx?replaceCurrent=1&reason=2&url=https%3a%2f%2fmail.example.com%2fowa%2f";
        let r = classify_login_response(StatusCode::FOUND, Some(loc));
        assert_eq!(r, LoginOutcome::Rejected("2".into()));
    }

    #[test]
    fn login_without_redirect_is_unexpected() {
        let r = classify_login_response(StatusCode::OK, None);
        assert!(matches!(r, LoginOutcome::Unexpected(_)));
    }

    #[test]
    fn strip_origin_keeps_path_and_query() {
        assert_eq!(
            strip_origin("https://mail.example.com/owa/auth/logon.aspx?reason=0"),
            "/owa/auth/logon.aspx?reason=0"
        );
        assert_eq!(strip_origin("/owa/"), "/owa/");
    }

    #[test]
    fn config_adds_trailing_slash() {
        let c = ClientConfig::new("https://mail.example.com").unwrap();
        assert_eq!(c.base_url.as_str(), "https://mail.example.com/");
        assert_eq!(
            c.base_url.join("owa/auth.owa").unwrap().as_str(),
            "https://mail.example.com/owa/auth.owa"
        );
    }
}
