//! 앱 상태: 저장소·색인·웹메일 세션·동기화 엔진, 읽기 창 캐시(계획서 4.1-6 LRU 50통).

use std::collections::{HashMap, VecDeque};
use std::hash::Hash;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use eo_owa::domino::{Domino, DominoConfig};
use eo_owa::{Client, ClientConfig};
use eo_search::SearchIndex;
use eo_store::Store;
use eo_sync::{Engine, ListedMail};
use serde::Serialize;
use tokio::sync::watch;

use crate::settings::{Settings, UiState};
use crate::view::MessageView;

const MB: usize = 1024 * 1024;
/// 읽기 창 캐시 크기(계획서 4.1-6). 개수와 함께 바이트 합에도 상한을 둔다(1.0.3 메모리 점검: 개수만 세면 큰 HTML
/// 메일·큰 첨부 몇 통으로 수백 MB를 쥘 수 있었다).
const VIEW_CACHE: usize = 50;
const VIEW_BYTES: usize = 32 * MB;
/// `app-cid` 이미지용 원문 캐시(최근 연 메일 몇 통). 첨부까지 든 원문이라 작게.
const RAW_CACHE: usize = 4;
const RAW_BYTES: usize = 24 * MB;
/// 인라인 이미지 캐시(메일 단위). 읽기 창 캐시보다 작게: 이미지가 클 수 있다.
const CID_CACHE: usize = 8;
const CID_BYTES: usize = 32 * MB;

/// 메일 하나의 인라인 이미지: Content-ID(소문자) → (MIME 형식, 내용).
pub type CidMap = HashMap<String, (String, Vec<u8>)>;

pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// 주소를 아직 넣지 않았을 때 쓰는 자리(예약된 도메인이라 어디에도 연결되지 않는다, 로그인 전에 막는다).
const UNSET_SERVER: &str = "https://server.invalid";

/// 설정의 웹메일 주소로 연결을 만든다. 재로그인이 거부되면 묻지 않고 LoginRequired로 끝낸다(화면이 로그인 창을
/// 다시 띄운다).
fn make_client(url: &str) -> Client {
    let cfg = ClientConfig::new(url)
        .or_else(|_| ClientConfig::new(UNSET_SERVER))
        .expect("예약 주소는 올바른 URL");
    Client::new(cfg, None)
}

/// 캐시 값의 대략 크기(바이트).
pub trait Weigh {
    fn weigh(&self) -> usize;
}

impl Weigh for MessageView {
    fn weigh(&self) -> usize {
        let text = [
            &self.html,
            &self.subject,
            &self.from,
            &self.to,
            &self.cc,
            &self.bcc,
        ];
        text.iter().map(|s| s.len()).sum::<usize>() + 64 * self.attachments.len() + 256
    }
}

impl Weigh for Arc<Vec<u8>> {
    fn weigh(&self) -> usize {
        self.len()
    }
}

impl Weigh for Arc<CidMap> {
    fn weigh(&self) -> usize {
        self.iter()
            .map(|(cid, (mime, data))| cid.len() + mime.len() + data.len())
            .sum::<usize>()
            + 64
    }
}

/// `이름@도메인.xx` 모양(로그인 아이디가 주소인지).
fn email_shaped(a: &str) -> bool {
    a.split_once('@').is_some_and(|(user, domain)| {
        !user.is_empty() && domain.contains('.') && !domain.contains('@')
    }) && !a.contains(char::is_whitespace)
}

/// 단순 LRU(작은 크기 전용: 갱신 시 선형 탐색). 개수(`cap`)와 바이트 합(`max_bytes`)을 둘 다 넘지 않게 오래된
/// 것부터 버린다. 하나가 `max_bytes`보다 크면 담지 않는다(다음에 다시 읽는다).
pub struct Lru<K, V> {
    cap: usize,
    max_bytes: usize,
    bytes: usize,
    order: VecDeque<K>,
    map: HashMap<K, (V, usize)>,
}

impl<K: Eq + Hash + Clone, V: Clone + Weigh> Lru<K, V> {
    pub fn new(cap: usize, max_bytes: usize) -> Self {
        Self {
            cap,
            max_bytes,
            bytes: 0,
            order: VecDeque::new(),
            map: HashMap::new(),
        }
    }

    pub fn get(&mut self, k: &K) -> Option<V> {
        let v = self.map.get(k)?.0.clone();
        self.touch(k);
        Some(v)
    }

    pub fn contains(&self, k: &K) -> bool {
        self.map.contains_key(k)
    }

    pub fn put(&mut self, k: K, v: V) {
        self.remove(&k);
        let w = v.weigh();
        if w > self.max_bytes {
            return;
        }
        self.bytes += w;
        self.map.insert(k.clone(), (v, w));
        self.order.push_back(k);
        while self.order.len() > self.cap || self.bytes > self.max_bytes {
            let Some(old) = self.order.pop_front() else {
                break;
            };
            if let Some((_, w)) = self.map.remove(&old) {
                self.bytes -= w;
            }
        }
    }

    pub fn remove(&mut self, k: &K) {
        if let Some((_, w)) = self.map.remove(k) {
            self.bytes -= w;
            self.order.retain(|x| x != k);
        }
    }

    pub fn clear(&mut self) {
        self.map.clear();
        self.order.clear();
        self.bytes = 0;
    }

    fn touch(&mut self, k: &K) {
        if let Some(pos) = self.order.iter().position(|x| x == k)
            && let Some(x) = self.order.remove(pos)
        {
            self.order.push_back(x);
        }
    }
}

/// 상태 표시줄(계획서 9.1, 9.5). 바뀔 때마다 `status` 이벤트로 통째로 보낸다.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub logged_in: bool,
    pub connecting: bool,
    pub syncing: bool,
    /// 재로그인이 거부됨(비밀번호 변경 등).
    pub login_required: bool,
    pub last_sync: Option<i64>,
    pub last_error: Option<String>,
    pub eml_done: usize,
    pub eml_total: usize,
    pub quota_used: Option<i64>,
    pub quota_limit: Option<i64>,
    /// 서버 반영 대기 작업 수(로그인 전·오프라인이면 쌓여 있다).
    pub pending_ops: i64,
    pub domino: DominoStatus,
}

/// HCL Domino 계정 연동 상태(0.5.0).
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DominoStatus {
    pub logged_in: bool,
    pub connecting: bool,
    pub syncing: bool,
    /// 세션이 끝났는데 다시 로그인이 거부됨(비밀번호 변경 등).
    pub login_required: bool,
    /// 이번 실행 중 로그인한 아이디(메모리에만).
    pub username: Option<String>,
    pub last_sync: Option<i64>,
    pub last_error: Option<String>,
    /// 가져오는 중(끝낸 수, 가져올 수).
    pub done: usize,
    pub total: usize,
    pub quota_used: Option<i64>,
    pub quota_limit: Option<i64>,
    /// 웹메일에 보낼 작업 수(로그인 전이면 쌓여 있다).
    pub pending: i64,
}

/// HCL Domino 확인 작업(로그인한 동안).
pub struct DominoRunner {
    pub stop: watch::Sender<bool>,
    /// 모든 메일함을 바로 확인.
    pub wake: Arc<tokio::sync::Notify>,
    /// 쌓인 작업만 바로 보내기.
    pub kick: Arc<tokio::sync::Notify>,
    pub cancel: Arc<std::sync::atomic::AtomicBool>,
}

/// 로그인한 동안의 동기화 엔진.
pub struct Session {
    pub engine: Option<Arc<Engine>>,
    pub stop: Option<watch::Sender<bool>>,
    /// 실행 중에만 기억하는 아이디(계획서 9.5, 저장하지 않음).
    pub username: Option<String>,
    pub last_wake: Option<Instant>,
}

pub struct AppState {
    pub root: PathBuf,
    pub store: Arc<Store>,
    pub search: Arc<SearchIndex>,
    /// Outlook 웹메일 연결(설정의 주소가 바뀌면 새로 만든다, [`AppState::client`]).
    client: Mutex<Client>,
    pub session: tokio::sync::Mutex<Session>,
    pub views: Mutex<Lru<i64, MessageView>>,
    pub raws: Mutex<Lru<i64, Arc<Vec<u8>>>>,
    pub cids: Mutex<Lru<i64, Arc<CidMap>>>,
    pub status: Mutex<Status>,
    /// 웹메일 설정의 차단 첨부 형식(로그인 후 받음).
    pub blocked_types: Mutex<Vec<String>>,
    /// 내 이름·주소와 서버의 최대 메일 크기(로그인 후 받음, 메모리에만).
    pub me: Mutex<Option<(String, String)>>,
    /// 대표 주소 말고 나를 가리키는 주소(메모리에만): 주소 모양인 로그인 아이디(Outlook·HCL Domino), 웹메일 설정의
    /// 로그온 주소·UPN. 전체 회신에서 나를 뺄 때(1.0.5 사용자 지적).
    pub my_aliases: Mutex<Vec<String>>,
    /// 웹메일 설정의 서명(설정했을 때만, 0.7.0).
    pub signature: Mutex<Option<eo_owa::Signature>>,
    pub max_message_bytes: Mutex<Option<i64>>,
    pub perf_test: bool,
    /// `--ui-test`(합성 저장소에서만 실제로 돈다).
    pub ui_test: bool,
    pub started: Instant,
    /// 시작 단계별 시각(프로세스 시작 후 ms, `--perf-test` 기록용).
    pub marks: Mutex<Vec<(String, f64)>>,
    /// 진행 중인 내보내기 중지 요청.
    pub export_cancel: Arc<std::sync::atomic::AtomicBool>,
    /// 진행 중인 가져오기 중지 요청, 진행 중 여부(한 번에 하나).
    pub import_cancel: Arc<std::sync::atomic::AtomicBool>,
    pub import_running: std::sync::atomic::AtomicBool,
    /// 설정(`EnhancedOutlook\settings.json`).
    pub settings: Mutex<Settings>,
    /// 창 위치·마지막 폴더(`EnhancedOutlook\state.json`). 바뀌면 `ui_dirty`를 세우고 뒤에서 저장한다.
    pub ui: Mutex<UiState>,
    pub ui_dirty: std::sync::atomic::AtomicBool,
    /// 지난 실행이 비정상 종료됐다(시작 때 한 번 알린다).
    pub crashed_before: bool,
    /// HCL Domino 웹메일 세션(아이디·비밀번호는 메모리에만).
    pub domino: Arc<Domino>,
    pub domino_runner: Mutex<Option<DominoRunner>>,
    /// 마지막으로 모든 메일함을 확인했을 때의 받은 메일 목록(자동 정리 대상 수).
    pub domino_inbox: Mutex<Option<Vec<ListedMail>>>,
}

impl AppState {
    pub fn open(
        root: PathBuf,
        perf_test: bool,
        ui_test: bool,
        started: Instant,
        crashed_before: bool,
    ) -> Result<Self, String> {
        let (settings, warn) = crate::settings::load_settings(&root);
        if let Some(w) = warn {
            crate::applog::warn(w);
        }
        let ui = crate::settings::load_ui_state(&root);
        let store = Store::open(&root).map_err(|e| format!("저장소를 열 수 없습니다: {e}"))?;
        // 색인 형식이 바뀌었으면(0.7.0: 단어 중간 검색) 비우고 모든 메일을 뒤에서 다시 색인한다.
        let (search, rebuilt) = SearchIndex::open_checked(&root.join("index"))
            .map_err(|e| format!("검색 색인을 열 수 없습니다: {e}"))?;
        if rebuilt {
            let n = store.mark_all_unindexed().map_err(|e| e.to_string())?;
            crate::applog::info(format!("검색 색인 형식이 바뀌어 다시 만든다: {n}통"));
        }
        let client = make_client(&settings.outlook_url);
        let domino = Domino::unconfigured();
        if !settings.domino.portal.is_empty() {
            domino.configure(DominoConfig::new(&settings.domino.portal).ok());
        }
        let pending_ops = store.pending_op_count().unwrap_or(0);
        Ok(Self {
            root,
            store: Arc::new(store),
            search: Arc::new(search),
            client: Mutex::new(client),
            session: tokio::sync::Mutex::new(Session {
                engine: None,
                stop: None,
                username: None,
                last_wake: None,
            }),
            views: Mutex::new(Lru::new(VIEW_CACHE, VIEW_BYTES)),
            raws: Mutex::new(Lru::new(RAW_CACHE, RAW_BYTES)),
            cids: Mutex::new(Lru::new(CID_CACHE, CID_BYTES)),
            status: Mutex::new(Status {
                pending_ops,
                ..Status::default()
            }),
            blocked_types: Mutex::new(Vec::new()),
            me: Mutex::new(None),
            my_aliases: Mutex::new(Vec::new()),
            signature: Mutex::new(None),
            max_message_bytes: Mutex::new(None),
            perf_test,
            ui_test,
            started,
            marks: Mutex::new(Vec::new()),
            export_cancel: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            import_cancel: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            import_running: std::sync::atomic::AtomicBool::new(false),
            settings: Mutex::new(settings),
            ui: Mutex::new(ui),
            ui_dirty: std::sync::atomic::AtomicBool::new(false),
            crashed_before,
            domino: Arc::new(domino),
            domino_runner: Mutex::new(None),
            domino_inbox: Mutex::new(None),
        })
    }

    /// Outlook 웹메일 연결.
    pub fn client(&self) -> Client {
        lock(&self.client).clone()
    }

    /// 웹메일 주소가 바뀌었다: 새 연결로(앞 연결의 로그인은 버린다).
    pub fn set_outlook_url(&self, url: &str) {
        let old = std::mem::replace(&mut *lock(&self.client), make_client(url));
        old.forget();
    }

    /// 나를 가리키는 주소를 더한다(주소 모양일 때만: `DOMAIN\아이디`나 사번은 뺀다).
    pub fn add_alias(&self, address: &str) {
        let a = address.trim();
        let mut aliases = lock(&self.my_aliases);
        if email_shaped(a) && !aliases.iter().any(|b| b.eq_ignore_ascii_case(a)) {
            aliases.push(a.to_owned());
        }
    }

    pub fn mark(&self, name: &str, at: Instant) {
        let ms = at.duration_since(self.started).as_secs_f64() * 1000.0;
        lock(&self.marks).push((name.to_owned(), ms));
    }

    /// 메일 원문(최근 연 몇 통은 메모리에서). `hash`를 모르면 DB에서 찾는다.
    pub fn raw(&self, id: i64, hash: Option<&str>) -> Option<Arc<Vec<u8>>> {
        if let Some(r) = lock(&self.raws).get(&id) {
            return Some(r);
        }
        let hash = match hash {
            Some(h) => h.to_owned(),
            None => self.store.message(id).ok()??.eml_hash?,
        };
        let raw = Arc::new(self.store.eml().get(&hash).ok()?);
        lock(&self.raws).put(id, raw.clone());
        Some(raw)
    }

    /// 인라인 이미지 하나. 메일을 열 때 꺼내 둔 것을 쓰고, 없으면(캐시에서 밀려남) 원문에서 찾는다.
    pub fn cid_image(&self, id: i64, cid: &str) -> Option<(String, Vec<u8>)> {
        let key = cid.trim().trim_matches(['<', '>']).to_ascii_lowercase();
        if let Some(map) = lock(&self.cids).get(&id) {
            return map.get(&key).cloned();
        }
        // 한 번 해석해 이 메일의 인라인 이미지를 모두 다시 꺼내 둔다(나머지 이미지 요청용).
        let raw = self.raw(id, None)?;
        let opts = eo_mime::RenderOptions {
            cid_base: format!("http://app-cid.localhost/{id}/"),
            allow_remote: false,
        };
        let map: CidMap = eo_mime::render(&raw, &opts)?
            .inline_images
            .into_iter()
            .map(|i| (i.cid, (i.mime, i.data)))
            .collect();
        let hit = map.get(&key).cloned();
        lock(&self.cids).put(id, Arc::new(map));
        hit
    }
}

#[cfg(test)]
mod tests {
    use super::{Lru, Weigh, email_shaped};

    #[test]
    fn login_ids_that_are_addresses() {
        assert!(email_shaped("me@x.example.com"));
        assert!(!email_shaped("DOMAIN\\me"));
        assert!(!email_shaped("me"));
        assert!(!email_shaped("me@localhost"));
        assert!(!email_shaped("@x.com"));
        assert!(!email_shaped("a b@x.com"));
    }

    impl Weigh for &str {
        fn weigh(&self) -> usize {
            self.len()
        }
    }

    #[test]
    fn lru_evicts_least_recent() {
        let mut l = Lru::new(2, usize::MAX);
        l.put(1, "a");
        l.put(2, "b");
        assert_eq!(l.get(&1), Some("a"));
        l.put(3, "c");
        assert!(l.contains(&1) && l.contains(&3) && !l.contains(&2));
        l.remove(&1);
        assert!(!l.contains(&1));
    }

    #[test]
    fn lru_keeps_byte_total_under_limit() {
        let mut l = Lru::new(10, 10);
        l.put(1, "aaaa");
        l.put(2, "bbbb");
        assert!(l.contains(&1) && l.contains(&2));
        // 4+4+4 > 10: 가장 오래된 1을 버린다.
        l.put(3, "cccc");
        assert!(!l.contains(&1) && l.contains(&2) && l.contains(&3));
        // 같은 키를 다시 넣으면 앞 값의 크기를 빼고 센다.
        l.put(3, "cc");
        l.put(4, "dddd");
        assert!(l.contains(&2) && l.contains(&3) && l.contains(&4));
        // 혼자서 한도를 넘는 값은 담지 않고 다른 것도 버리지 않는다.
        l.put(5, "eeeeeeeeeeee");
        assert!(!l.contains(&5) && l.contains(&2) && l.contains(&4));
        l.clear();
        l.put(6, "ffffffffff");
        assert!(l.contains(&6));
    }
}
