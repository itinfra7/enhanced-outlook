//! HTTP/1.1 연결 풀. NTLM은 연결 단위 인증이라 연결마다 한 번 인증하고 그 연결을 재사용한다.
//! 쿠키는 풀 전체가 공유하고, 리다이렉트는 따라가지 않는다.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Once};
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use bytes::Bytes;
use cookie_store::{CookieStore, RawCookie};
use http::header::{
    AUTHORIZATION, CONNECTION, CONTENT_LENGTH, COOKIE, HOST, SET_COOKIE, USER_AGENT,
    WWW_AUTHENTICATE,
};
use http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use http_body_util::{BodyExt, Full};
use hyper::client::conn::http1::SendRequest;
use hyper_util::rt::TokioIo;
use rustls_platform_verifier::ConfigVerifierExt;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio::sync::Semaphore;
use tokio_rustls::TlsConnector;
use url::Url;

use crate::cbt;
use crate::creds::Credentials;
use crate::error::{Error, Result};
use crate::ntlm::NtlmContext;

/// 동시 연결 상한(계획서 7장 동시 요청 최대 4, 검증용으로 8까지).
const MAX_CONNECTIONS: usize = 8;
/// 이보다 오래 쉰 연결은 버린다(IIS 기본 유휴 제한 120초보다 짧게).
const IDLE_REUSE: Duration = Duration::from_secs(60);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

pub(crate) struct Request {
    pub method: Method,
    /// 서버 기준 상대 경로(질의 포함), 예: `owa/service.svc?action=GetFolder`.
    pub path: String,
    pub headers: Vec<(HeaderName, HeaderValue)>,
    pub body: Bytes,
}

impl Request {
    pub fn get(path: impl Into<String>) -> Self {
        Self {
            method: Method::GET,
            path: path.into(),
            headers: Vec::new(),
            body: Bytes::new(),
        }
    }

    pub fn post(path: impl Into<String>, body: impl Into<Bytes>) -> Self {
        Self {
            method: Method::POST,
            path: path.into(),
            headers: Vec::new(),
            body: body.into(),
        }
    }

    pub fn header(mut self, name: &str, value: &str) -> Result<Self> {
        let name = HeaderName::try_from(name).map_err(|e| Error::Http(e.to_string()))?;
        let value = HeaderValue::try_from(value).map_err(|e| Error::Http(e.to_string()))?;
        self.headers.push((name, value));
        Ok(self)
    }

    fn without_body(&self) -> Self {
        Self {
            method: self.method.clone(),
            path: self.path.clone(),
            headers: self.headers.clone(),
            body: Bytes::new(),
        }
    }
}

pub(crate) struct Response {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Bytes,
}

impl Response {
    pub fn header(&self, name: HeaderName) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }
}

/// 서버가 NTLM을 제안하는가(`WWW-Authenticate: NTLM`).
pub(crate) fn offers_ntlm(headers: &HeaderMap) -> bool {
    headers
        .get_all(WWW_AUTHENTICATE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .any(|v| v.eq_ignore_ascii_case("NTLM") || v.starts_with("NTLM "))
}

fn ntlm_challenge(headers: &HeaderMap) -> Option<Vec<u8>> {
    headers
        .get_all(WWW_AUTHENTICATE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find_map(|v| v.strip_prefix("NTLM "))
        .and_then(|b64| STANDARD.decode(b64.trim()).ok())
}

trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

struct Conn {
    sender: SendRequest<Full<Bytes>>,
    /// TLS 서버 인증서로 만든 채널 바인딩(EPA). 평문 연결이거나 꺼 두면 없음.
    channel_bindings: Option<Vec<u8>>,
    authenticated: bool,
    last_used: Instant,
    reused: bool,
}

/// 요청 실패 종류: 재사용한 연결이 이미 닫혀 있었으면(`Stale`) 새 연결로 한 번 더 보낸다.
enum Failure {
    Stale,
    Fatal(Error),
}

impl From<Error> for Failure {
    fn from(e: Error) -> Self {
        Self::Fatal(e)
    }
}

impl Failure {
    fn into_error(self) -> Error {
        match self {
            Self::Stale => Error::Http("연결이 끊겼습니다".into()),
            Self::Fatal(e) => e,
        }
    }
}

pub(crate) struct Http {
    base: Url,
    host: String,
    tls: Option<TlsConnector>,
    user_agent: HeaderValue,
    timeout: Duration,
    channel_binding: bool,
    /// NTLM 자격 증명(실행 중 메모리에만). 없으면 쿠키만 쓴다(폼 로그인).
    ntlm: Option<Credentials>,
    cookies: Mutex<CookieStore>,
    idle: Mutex<Vec<Conn>>,
    permits: Semaphore,
    handshakes: AtomicU64,
}

impl Http {
    pub fn new(
        base: &Url,
        user_agent: &str,
        timeout: Duration,
        channel_binding: bool,
        ntlm: Option<Credentials>,
    ) -> Result<Self> {
        let host_name = base
            .host_str()
            .ok_or_else(|| Error::Http(format!("호스트 없는 주소: {base}")))?;
        let host = match base.port() {
            Some(p) => format!("{host_name}:{p}"),
            None => host_name.to_owned(),
        };
        let tls = match base.scheme() {
            "https" => Some(tls_connector()?),
            "http" => None,
            other => return Err(Error::Http(format!("지원하지 않는 스킴: {other}"))),
        };
        Ok(Self {
            base: base.clone(),
            host,
            tls,
            user_agent: HeaderValue::try_from(user_agent)
                .map_err(|e| Error::Http(e.to_string()))?,
            timeout,
            channel_binding,
            ntlm,
            cookies: Mutex::new(CookieStore::new()),
            idle: Mutex::new(Vec::new()),
            permits: Semaphore::new(MAX_CONNECTIONS),
            handshakes: AtomicU64::new(0),
        })
    }

    pub fn is_ntlm(&self) -> bool {
        self.ntlm.is_some()
    }

    /// NTLM 연결 인증 횟수(진단용).
    pub fn handshakes(&self) -> u64 {
        self.handshakes.load(Ordering::SeqCst)
    }

    /// 쉬고 있는 연결을 모두 끊는다. 다음 요청은 새 연결에서 다시 인증한다.
    pub fn drop_connections(&self) {
        lock(&self.idle).clear();
    }

    pub fn add_cookie(&self, cookie: &str) {
        let _ = lock(&self.cookies).parse(cookie, &self.base);
    }

    /// `/owa/` 요청에 실릴 쿠키 값.
    pub fn cookie(&self, name: &str) -> Option<String> {
        let url = self.base.join("owa/").ok()?;
        lock(&self.cookies)
            .get_request_values(&url)
            .find(|(n, _)| *n == name)
            .map(|(_, v)| v.to_owned())
    }

    /// `url`(다른 호스트도 됨) 요청에 실릴 쿠키들(이름, 값). 도메인 쿠키를 다른 호스트로 옮길 때.
    pub fn cookies_for(&self, url: &Url) -> Vec<(String, String)> {
        lock(&self.cookies)
            .get_request_values(url)
            .map(|(n, v)| (n.to_owned(), v.to_owned()))
            .collect()
    }

    pub fn cookie_names(&self) -> Vec<String> {
        let Ok(url) = self.base.join("owa/") else {
            return Vec::new();
        };
        lock(&self.cookies)
            .get_request_values(&url)
            .map(|(n, _)| n.to_owned())
            .collect()
    }

    pub async fn send(&self, req: Request) -> Result<Response> {
        let _permit = self
            .permits
            .acquire()
            .await
            .map_err(|_| Error::Http("연결 풀이 닫혔습니다".into()))?;
        tokio::time::timeout(self.timeout, self.send_inner(&req))
            .await
            .map_err(|_| Error::Http(format!("응답 시간 초과 ({}초)", self.timeout.as_secs())))?
    }

    async fn send_inner(&self, req: &Request) -> Result<Response> {
        let mut conn = match self.take_idle() {
            Some(c) => c,
            None => self.connect().await?,
        };
        let resp = match self.exchange(&mut conn, req).await {
            Ok(r) => r,
            Err(Failure::Stale) if conn.reused => {
                conn = self.connect().await?;
                self.exchange(&mut conn, req)
                    .await
                    .map_err(Failure::into_error)?
            }
            Err(f) => return Err(f.into_error()),
        };
        let closing = resp
            .header(CONNECTION)
            .is_some_and(|v| v.eq_ignore_ascii_case("close"));
        if !closing {
            conn.reused = true;
            lock(&self.idle).push(conn);
        }
        Ok(resp)
    }

    fn take_idle(&self) -> Option<Conn> {
        let mut idle = lock(&self.idle);
        while let Some(c) = idle.pop() {
            if !c.sender.is_closed() && c.last_used.elapsed() < IDLE_REUSE {
                return Some(c);
            }
        }
        None
    }

    async fn exchange(&self, conn: &mut Conn, req: &Request) -> Result<Response, Failure> {
        let Some(creds) = &self.ntlm else {
            return self.round_trip(conn, req, None).await;
        };
        if !conn.authenticated {
            return self.authenticate(conn, req, creds).await;
        }
        let resp = self.round_trip(conn, req, None).await?;
        if resp.status == StatusCode::UNAUTHORIZED && offers_ntlm(&resp.headers) {
            conn.authenticated = false;
            return self.authenticate(conn, req, creds).await;
        }
        Ok(resp)
    }

    /// NTLM: NEGOTIATE(빈 본문) → 401 CHALLENGE → AUTHENTICATE(실제 요청). 같은 연결에서 진행한다.
    async fn authenticate(
        &self,
        conn: &mut Conn,
        req: &Request,
        creds: &Credentials,
    ) -> Result<Response, Failure> {
        self.handshakes.fetch_add(1, Ordering::SeqCst);
        let spn = format!("HTTP/{}", self.base.host_str().unwrap_or_default());
        let mut ctx = NtlmContext::new(creds, &spn, conn.channel_bindings.as_deref())?;
        let negotiate = ctx.step(None)?;
        let first = self
            .round_trip(conn, &req.without_body(), Some(ntlm_header(&negotiate)))
            .await?;
        if first.status != StatusCode::UNAUTHORIZED {
            // 인증이 필요 없는 경로였다. 본문이 있었으면 원래 요청을 다시 보낸다.
            conn.authenticated = true;
            return if req.body.is_empty() {
                Ok(first)
            } else {
                self.round_trip(conn, req, None).await
            };
        }
        let challenge = ntlm_challenge(&first.headers)
            .ok_or_else(|| Error::Auth("서버가 NTLM 챌린지를 보내지 않았습니다".into()))?;
        let authenticate = ctx.step(Some(&challenge))?;
        let resp = self
            .round_trip(conn, req, Some(ntlm_header(&authenticate)))
            .await?;
        if resp.status == StatusCode::UNAUTHORIZED {
            return Err(Failure::Fatal(Error::BadCredentials {
                reason: "NTLM".into(),
            }));
        }
        conn.authenticated = true;
        Ok(resp)
    }

    async fn round_trip(
        &self,
        conn: &mut Conn,
        req: &Request,
        authorization: Option<String>,
    ) -> Result<Response, Failure> {
        let url = self.base.join(&req.path).map_err(Error::from)?;
        let target = match url.query() {
            Some(q) => format!("{}?{q}", url.path()),
            None => url.path().to_owned(),
        };
        let mut builder = http::Request::builder()
            .method(req.method.clone())
            .uri(target)
            .header(HOST, self.host.as_str())
            .header(USER_AGENT, self.user_agent.clone());
        for (name, value) in &req.headers {
            builder = builder.header(name, value);
        }
        if let Some(a) = authorization {
            builder = builder.header(AUTHORIZATION, a);
        }
        if let Some(c) = self.cookie_header(&url) {
            builder = builder.header(COOKIE, c);
        }
        if req.method == Method::POST {
            builder = builder.header(CONTENT_LENGTH, req.body.len());
        }
        let request = builder
            .body(Full::new(req.body.clone()))
            .map_err(|e| Error::Http(e.to_string()))?;

        let idle_for = conn.last_used.elapsed();
        if conn.sender.ready().await.is_err() {
            return Err(Failure::Stale);
        }
        let resp = match conn.sender.send_request(request).await {
            Ok(r) => r,
            Err(e)
                if e.is_canceled()
                    || (e.is_incomplete_message() && idle_for > Duration::from_secs(5)) =>
            {
                return Err(Failure::Stale);
            }
            Err(e) => return Err(Failure::Fatal(Error::Http(e.to_string()))),
        };
        let (parts, body) = resp.into_parts();
        let body = body
            .collect()
            .await
            .map_err(|e| Error::Http(format!("응답 본문 읽기 실패: {e}")))?
            .to_bytes();
        self.store_cookies(&url, &parts.headers);
        conn.last_used = Instant::now();
        Ok(Response {
            status: parts.status,
            headers: parts.headers,
            body,
        })
    }

    async fn connect(&self) -> Result<Conn> {
        let host = self.base.host_str().unwrap_or_default().to_owned();
        let port = self.base.port_or_known_default().unwrap_or(443);
        let tcp = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect((host.as_str(), port)))
            .await
            .map_err(|_| Error::Http(format!("{host}:{port} 연결 시간 초과")))?
            .map_err(|e| Error::Http(format!("{host}:{port} 연결 실패: {e}")))?;
        let _ = tcp.set_nodelay(true);
        let (io, channel_bindings): (Box<dyn Io>, Option<Vec<u8>>) = match &self.tls {
            Some(tls) => {
                let name = rustls::pki_types::ServerName::try_from(host.clone())
                    .map_err(|e| Error::Http(e.to_string()))?;
                let stream = tls
                    .connect(name, tcp)
                    .await
                    .map_err(|e| Error::Http(format!("TLS 연결 실패: {e}")))?;
                let bindings = if self.channel_binding {
                    stream
                        .get_ref()
                        .1
                        .peer_certificates()
                        .and_then(|certs| certs.first())
                        .map(|cert| cbt::channel_bindings(cert.as_ref()))
                } else {
                    None
                };
                (Box::new(stream), bindings)
            }
            None => (Box::new(tcp), None),
        };
        let (sender, connection) =
            hyper::client::conn::http1::handshake::<_, Full<Bytes>>(TokioIo::new(io))
                .await
                .map_err(|e| Error::Http(e.to_string()))?;
        tokio::spawn(async move {
            let _ = connection.await;
        });
        Ok(Conn {
            sender,
            channel_bindings,
            authenticated: false,
            last_used: Instant::now(),
            reused: false,
        })
    }

    fn cookie_header(&self, url: &Url) -> Option<String> {
        let store = lock(&self.cookies);
        let pairs: Vec<String> = store
            .get_request_values(url)
            .map(|(n, v)| format!("{n}={v}"))
            .collect();
        (!pairs.is_empty()).then(|| pairs.join("; "))
    }

    fn store_cookies(&self, url: &Url, headers: &HeaderMap) {
        let cookies: Vec<RawCookie<'static>> = headers
            .get_all(SET_COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .filter_map(|s| RawCookie::parse(s.to_owned()).ok())
            .collect();
        if !cookies.is_empty() {
            lock(&self.cookies).store_response_cookies(cookies.into_iter(), url);
        }
    }
}

fn ntlm_header(token: &[u8]) -> String {
    format!("NTLM {}", STANDARD.encode(token))
}

fn tls_connector() -> Result<TlsConnector> {
    install_crypto_provider();
    let mut cfg = rustls::ClientConfig::with_platform_verifier()
        .map_err(|e| Error::Http(format!("TLS 설정 실패: {e}")))?;
    // NTLM은 HTTP/2에서 동작하지 않는다(IIS가 HTTP/1.1을 요구).
    cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(TlsConnector::from(Arc::new(cfg)))
}

/// rustls 암호 모듈은 ring(빌드에 NASM·CMake 불필요). 프로세스당 한 번 설치한다.
fn install_crypto_provider() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}
