//! 동기화 엔진(계획서 7장): 계층 동기화 → 폴더별 항목 동기화(목록 속성 포함) → 로컬 반영·이벤트
//! → 원문 큐(최근 메일부터, 묶음 4 × 동시 4). 서버와 이야기하는 유일한 계층이다.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use eo_owa::{ChangeKind, Client, FolderRef, ItemId};
use eo_search::SearchIndex;
use eo_store::{EmlTodo, ServerFolder, Store};
use futures_util::StreamExt;
use futures_util::stream;
use tokio::sync::{Notify, broadcast, watch};

use crate::apply::{self, ApplyStats, FolderChanges};
use crate::index::{self, IndexReport};
use crate::{Error, Result};

const META_HIERARCHY_STATE: &str = "hierarchy_sync_state";
const META_ROOT: &str = "root_server_id";

/// 잘 알려진 폴더와 표시 순서.
const WELL_KNOWN: &[(&str, i64)] = &[
    ("inbox", 0),
    ("drafts", 1),
    ("sentitems", 2),
    ("deleteditems", 3),
    ("junkemail", 4),
    ("outbox", 5),
];

pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// 원문 동시 요청 수(계획서 2.2 기본 4).
    pub eml_concurrency: usize,
    /// 원문 묶음 크기(계획서 2.2 기본 4~8).
    pub eml_batch: usize,
    /// 받은 편지함 확인 주기(계획서 7장 10초).
    pub inbox_every: Duration,
    /// 나머지 폴더·계층 확인 주기(60초).
    pub others_every: Duration,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            eml_concurrency: 4,
            eml_batch: 4,
            inbox_every: Duration::from_secs(10),
            others_every: Duration::from_secs(60),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    FoldersChanged,
    /// 해당 행만 갱신하면 된다(계획서 4.2 `messages-changed`).
    MessagesChanged {
        folder_id: i64,
        message_ids: Vec<i64>,
    },
    EmlProgress {
        done: usize,
        total: usize,
    },
    /// 주기 실행([`Engine::run`])의 한 주기 시작(true)·끝(false). 상태 표시줄용.
    Syncing(bool),
    /// 서버 반영 대기 작업 수(작업 회차가 끝날 때마다).
    OpsPending(i64),
    /// 발송 결과(작성 중 메일 번호, 실패 사유). 실패하면 작성 중 메일은 "발송 실패"로 남는다.
    SendResult {
        draft_id: i64,
        error: Option<String>,
    },
    /// 작성 중 메일이 서버 쪽 사정으로 바뀌었다(임시 보관함에 올림, 웹메일에서 지움 등, 0.7.0).
    DraftsChanged,
    /// 서버가 끝내 거부해 포기하고 로컬을 되돌린 작업(계획서 4.1-2 "실패하면 되돌리고 알린다").
    OpFailed {
        message_id: Option<i64>,
        kind: String,
        error: String,
    },
    /// 재로그인이 거부돼 사용자 입력이 필요하다(로컬 메일은 계속 쓸 수 있음).
    LoginRequired,
    Error(String),
}

#[derive(Debug, Clone, Default)]
pub struct HierarchyReport {
    pub calls: usize,
    pub changes: usize,
    pub elapsed: Duration,
}

#[derive(Debug, Clone, Default)]
pub struct ItemsReport {
    pub folders: usize,
    /// 받은 편지함만 보다가 삭제가 보여 전 폴더로 넓혔다.
    pub escalated: bool,
    pub calls: usize,
    pub fetched_changes: usize,
    pub apply: ApplyStats,
    /// 폴더 단위 오류(그 폴더만 건너뜀).
    pub errors: Vec<String>,
    pub elapsed: Duration,
}

#[derive(Debug, Clone, Default)]
pub struct EmlReport {
    pub queued: usize,
    pub stored: usize,
    pub bytes: u64,
    pub failures: BTreeMap<String, usize>,
    pub final_concurrency: usize,
    pub elapsed: Duration,
}

#[derive(Debug, Clone, Default)]
pub struct CycleReport {
    pub hierarchy: HierarchyReport,
    pub items: ItemsReport,
    pub eml: EmlReport,
    /// 검색 색인을 붙였을 때만.
    pub index: Option<IndexReport>,
}

pub struct Engine {
    client: Client,
    store: Arc<Store>,
    search: Option<Arc<SearchIndex>>,
    cfg: EngineConfig,
    events: broadcast::Sender<Event>,
    wake: Notify,
    ops_wake: Notify,
    /// 내 이름·주소(발송용, 처음 보낼 때 받는다).
    me: std::sync::Mutex<Option<eo_owa::Address>>,
    /// (받은 편지함, 나머지 폴더) 확인 주기. 설정 화면에서 실행 중에 바꾼다(계획서 10장 8단계).
    intervals: std::sync::Mutex<(Duration, Duration)>,
}

impl Engine {
    pub fn new(client: Client, store: Arc<Store>, cfg: EngineConfig) -> Self {
        let intervals = (cfg.inbox_every, cfg.others_every);
        Self {
            client,
            store,
            search: None,
            cfg,
            events: broadcast::channel(256).0,
            wake: Notify::new(),
            ops_wake: Notify::new(),
            me: std::sync::Mutex::new(None),
            intervals: std::sync::Mutex::new(intervals),
        }
    }

    /// 확인 주기를 바꾼다. 다음 확인부터 적용된다.
    pub fn set_intervals(&self, inbox_every: Duration, others_every: Duration) {
        *self.intervals.lock().unwrap_or_else(|e| e.into_inner()) = (inbox_every, others_every);
    }

    fn intervals(&self) -> (Duration, Duration) {
        *self.intervals.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn me_cache(&self) -> Option<eo_owa::Address> {
        self.me.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub(crate) fn set_me_cache(&self, me: eo_owa::Address) {
        *self.me.lock().unwrap_or_else(|e| e.into_inner()) = Some(me);
    }

    pub(crate) fn client(&self) -> &Client {
        &self.client
    }

    pub(crate) fn search(&self) -> Option<&Arc<SearchIndex>> {
        self.search.as_ref()
    }

    /// 작업을 쌓았으니 기다리지 말고 서버에 보내라(로컬 조작 직후).
    pub fn wake_ops(&self) {
        self.ops_wake.notify_one();
    }

    /// 주기마다 원문 받기 뒤에 색인 큐도 처리한다(계획서 7장 순서의 마지막).
    pub fn with_search(mut self, search: Arc<SearchIndex>) -> Self {
        self.search = Some(search);
        self
    }

    /// 색인 대기 메일을 색인한다(검색 색인을 붙이지 않았으면 `None`).
    pub async fn index_pending(&self) -> Result<Option<IndexReport>> {
        let Some(search) = self.search.clone() else {
            return Ok(None);
        };
        let store = self.store.clone();
        let report =
            tokio::task::spawn_blocking(move || index::index_pending(&store, &search, None))
                .await
                .map_err(|e| Error::Task(e.to_string()))??;
        Ok(Some(report))
    }

    pub fn store(&self) -> &Arc<Store> {
        &self.store
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    /// 다음 주기를 기다리지 않고 바로 전체 확인(앱 포커스 시, 계획서 7장).
    pub fn wake(&self) {
        self.wake.notify_one();
    }

    pub(crate) fn emit(&self, e: Event) {
        let _ = self.events.send(e);
    }

    async fn root_server_id(&self) -> Result<String> {
        if let Some(root) = self.store.meta(META_ROOT)? {
            return Ok(root);
        }
        let root = self
            .client
            .get_folder(&FolderRef::distinguished("msgfolderroot"))
            .await?
            .folder_id
            .id;
        self.store.set_meta(META_ROOT, &root)?;
        Ok(root)
    }

    /// 계층 동기화(계획서 5.3): 폴더 추가·변경·삭제를 반영하고 메일 폴더 트리를 다시 계산한다.
    pub async fn sync_hierarchy(&self) -> Result<HierarchyReport> {
        let started = Instant::now();
        let root = self.root_server_id().await?;
        let mut state = self.store.meta(META_HIERARCHY_STATE)?;
        let mut changes = Vec::new();
        let mut calls = 0;
        loop {
            let h = self.client.sync_folder_hierarchy(state.as_deref()).await?;
            calls += 1;
            changes.extend(h.changes);
            state = Some(h.sync_state);
            if h.includes_last {
                break;
            }
        }
        let well_known = if changes.is_empty() {
            Vec::new()
        } else {
            let refs: Vec<FolderRef> = WELL_KNOWN
                .iter()
                .map(|(n, _)| FolderRef::distinguished(n))
                .collect();
            self.client
                .get_folders(&refs)
                .await?
                .into_iter()
                .zip(WELL_KNOWN)
                .filter_map(|(r, (name, order))| r.ok().map(|f| (f.folder_id.id, *name, *order)))
                .collect()
        };
        let now = now_ms();
        let n = changes.len();
        self.store.transaction(|tx| {
            for c in &changes {
                match (c.kind, &c.folder, &c.folder_id) {
                    (ChangeKind::Delete, _, Some(id)) => {
                        tx.archive_folder(&id.id, now)?;
                    }
                    (_, Some(f), _) => {
                        tx.upsert_folder(&ServerFolder {
                            server_id: f.folder_id.id.clone(),
                            server_parent_id: f.parent_folder_id.as_ref().map(|p| p.id.clone()),
                            name: f.display_name.clone(),
                            folder_class: f.folder_class.clone(),
                        })?;
                    }
                    _ => {}
                }
            }
            for (server_id, name, order) in &well_known {
                tx.set_well_known(server_id, name, *order)?;
            }
            if n > 0 {
                tx.rebuild_tree(&root)?;
            }
            if let Some(s) = &state {
                tx.set_meta(META_HIERARCHY_STATE, s)?;
            }
            Ok(())
        })?;
        if n > 0 {
            self.emit(Event::FoldersChanged);
        }
        Ok(HierarchyReport {
            calls,
            changes: n,
            elapsed: started.elapsed(),
        })
    }

    /// 폴더별 항목 동기화. 모든 대상 폴더의 변경을 먼저 모은 뒤 한 트랜잭션으로 반영한다(계획서 6.3).
    /// `only`가 있으면 그 폴더만. 그런데 삭제가 보이면 다른 폴더로 옮겨진 것일 수 있어(짝은 다른 폴더의
    /// "생김") 계층·전 폴더로 넓혀 다시 모은다. 아직 아무것도 반영·저장하지 않았으니 같은 변경을 다시 받는다.
    /// 반영할 때는 서버 반영을 기다리는 로컬 조작을 덮어쓰지 않는다([`eo_store::PendingGuard`]).
    pub async fn sync_items(&self, only: Option<&HashSet<i64>>) -> Result<ItemsReport> {
        let started = Instant::now();
        let mut report = ItemsReport::default();
        let mut batch = self.collect_items(only, &mut report).await?;
        let has_delete = batch
            .iter()
            .any(|fc| fc.changes.iter().any(|c| c.kind == ChangeKind::Delete));
        if only.is_some() && has_delete {
            self.sync_hierarchy().await?;
            report = ItemsReport {
                escalated: true,
                ..ItemsReport::default()
            };
            batch = self.collect_items(None, &mut report).await?;
        }
        let now = now_ms();
        let guard = self.store.pending_guard()?;
        report.apply = self
            .store
            .transaction(|tx| apply::apply_cycle(tx, &batch, now, &guard))?;
        for h in &report.apply.purged_hashes {
            self.store.remove_eml_if_unused(h)?;
        }
        for d in &report.apply.dropped_drafts {
            self.store.delete_draft(*d)?;
        }
        if !report.apply.dropped_drafts.is_empty() {
            self.emit(Event::DraftsChanged);
        }
        if let Some(search) = &self.search
            && !report.apply.purged.is_empty()
        {
            for id in &report.apply.purged {
                search.delete(*id as u64)?;
            }
            search.commit()?;
        }
        for (folder_id, ids) in &report.apply.touched {
            self.emit(Event::MessagesChanged {
                folder_id: *folder_id,
                message_ids: ids.clone(),
            });
        }
        report.elapsed = started.elapsed();
        Ok(report)
    }

    async fn collect_items(
        &self,
        only: Option<&HashSet<i64>>,
        report: &mut ItemsReport,
    ) -> Result<Vec<FolderChanges>> {
        let folders: Vec<_> = self
            .store
            .syncable_folders()?
            .into_iter()
            .filter(|f| only.is_none_or(|o| o.contains(&f.id)))
            .collect();
        report.folders = folders.len();
        let mut batch = Vec::new();
        for f in &folders {
            let Some(server_id) = &f.server_id else {
                continue;
            };
            let folder = FolderRef::Id(server_id.clone());
            let mut state = f.sync_state.clone();
            let mut changes = Vec::new();
            let result: Result<()> = async {
                loop {
                    let s = self
                        .client
                        .sync_folder_items(&folder, state.as_deref())
                        .await?;
                    report.calls += 1;
                    changes.extend(s.changes);
                    state = Some(s.sync_state);
                    if s.includes_last {
                        return Ok(());
                    }
                }
            }
            .await;
            match result {
                Ok(()) => {
                    report.fetched_changes += changes.len();
                    batch.push(FolderChanges {
                        folder_id: f.id,
                        changes,
                        new_state: state.unwrap_or_default(),
                    });
                }
                Err(Error::Owa(eo_owa::Error::LoginRequired)) => {
                    self.emit(Event::LoginRequired);
                    return Err(Error::Owa(eo_owa::Error::LoginRequired));
                }
                Err(e) => report.errors.push(format!("{}: {e}", f.name)),
            }
        }
        Ok(batch)
    }

    /// 원문 큐: 원문이 없는 서버 메일을 최근 것부터 받는다. `limit`까지만 받고 멈출 수 있다(이어받기 확인용).
    /// 429/503이 오면 동시 요청 수를 절반으로 줄인다(계획서 7장 제한).
    pub async fn fetch_eml(&self, limit: Option<usize>) -> Result<EmlReport> {
        let started = Instant::now();
        let mut todo = self.store.eml_todo(usize::MAX)?;
        if let Some(l) = limit {
            todo.truncate(l);
        }
        let total = todo.len();
        let mut report = EmlReport {
            queued: total,
            final_concurrency: self.cfg.eml_concurrency.max(1),
            ..EmlReport::default()
        };
        let mut concurrency = self.cfg.eml_concurrency.max(1);
        let batch = self.cfg.eml_batch.max(1);
        let mut done = 0;
        for round in todo.chunks(batch * 32) {
            // 묶음을 소유 값으로 만들어 둔다(슬라이스를 받는 클로저는 `spawn`의 Send 판정을 통과하지 못한다).
            let chunks: Vec<Vec<EmlTodo>> = round.chunks(batch).map(<[EmlTodo]>::to_vec).collect();
            let requests = chunks.into_iter().map(|chunk| {
                let client = self.client.clone();
                async move {
                    let ids: Vec<ItemId> = chunk
                        .iter()
                        .map(|t| ItemId {
                            id: t.server_item_id.clone(),
                            change_key: None,
                        })
                        .collect();
                    let r = client.get_mime(&ids).await;
                    (chunk, r)
                }
            });
            let mut results = stream::iter(requests).buffer_unordered(concurrency);
            let mut throttled = false;
            while let Some((chunk, result)) = results.next().await {
                match result {
                    Ok(mimes) => {
                        for (t, mime) in chunk.into_iter().zip(mimes) {
                            match mime {
                                Ok(bytes) => {
                                    report.bytes += bytes.len() as u64;
                                    if self.store_eml(t, bytes).await? {
                                        report.stored += 1;
                                    }
                                }
                                Err(e) => *report.failures.entry(e.code).or_default() += 1,
                            }
                            done += 1;
                        }
                    }
                    Err(eo_owa::Error::LoginRequired) => {
                        self.emit(Event::LoginRequired);
                        return Err(Error::Owa(eo_owa::Error::LoginRequired));
                    }
                    Err(e) => {
                        if matches!(
                            e,
                            eo_owa::Error::Status {
                                status: 429 | 503,
                                ..
                            }
                        ) {
                            throttled = true;
                        }
                        *report.failures.entry(e.to_string()).or_default() += chunk.len();
                        done += chunk.len();
                    }
                }
                self.emit(Event::EmlProgress { done, total });
            }
            if throttled && concurrency > 1 {
                concurrency /= 2;
                report.final_concurrency = concurrency;
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
        report.elapsed = started.elapsed();
        Ok(report)
    }

    async fn store_eml(&self, t: EmlTodo, bytes: Vec<u8>) -> Result<bool> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || {
            store.attach_eml(t.message_id, &t.server_item_id, &bytes)
        })
        .await
        .map_err(|e| Error::Task(e.to_string()))?
        .map_err(Error::from)
    }

    /// 한 주기: 계층 → 전 폴더 항목 → 원문 → 색인.
    pub async fn cycle(&self, eml_limit: Option<usize>) -> Result<CycleReport> {
        Ok(CycleReport {
            hierarchy: self.sync_hierarchy().await?,
            items: self.sync_items(None).await?,
            eml: self.fetch_eml(eml_limit).await?,
            index: self.index_pending().await?,
        })
    }

    /// 시도할 작업이 있으면 한 회차 처리한다. 네트워크 오류로 멈췄으면 true.
    async fn run_ops(&self) -> Result<bool> {
        if self.store.due_ops(now_ms(), 1)?.is_empty() {
            return Ok(false);
        }
        Ok(self.process_ops().await?.offline)
    }

    /// 주기 실행(계획서 7장): 받은 편지함 10초, 나머지 폴더·계층 60초(설정으로 바꿈, [`Engine::set_intervals`]),
    /// [`Engine::wake`] 시 즉시 전체.
    /// 서버 반영 작업은 매초(그리고 [`Engine::wake_ops`] 즉시) 먼저 처리한다. 연결이 끊겼다가 동기화가
    /// 다시 성공하면 기다리던 작업을 바로 다시 시도한다(계획서 10장 5단계 수용: 오프라인 조작 → 복구 후 반영).
    /// `stop`이 true가 되거나 로그인이 필요해지면 끝난다.
    pub async fn run(&self, mut stop: watch::Receiver<bool>) {
        let mut last_inbox: Option<Instant> = None;
        let mut last_others: Option<Instant> = None;
        let mut offline = false;
        loop {
            let mut forced = false;
            tokio::select! {
                _ = stop.changed() => {}
                () = self.wake.notified() => forced = true,
                () = self.ops_wake.notified() => {}
                () = tokio::time::sleep(Duration::from_secs(1)) => {}
            }
            if *stop.borrow() {
                return;
            }
            match self.run_ops().await {
                Ok(off) => offline |= off,
                Err(Error::Owa(eo_owa::Error::LoginRequired)) => return,
                Err(e) => self.emit(Event::Error(e.to_string())),
            }
            let due = |last: Option<Instant>, every: Duration| {
                forced || last.is_none_or(|t| t.elapsed() >= every)
            };
            let (inbox_every, others_every) = self.intervals();
            let others_due = due(last_others, others_every);
            let inbox_due = due(last_inbox, inbox_every);
            if !others_due && !inbox_due {
                continue;
            }
            self.emit(Event::Syncing(true));
            let result: Result<()> = async {
                if others_due {
                    self.sync_hierarchy().await?;
                    self.sync_items(None).await?;
                } else {
                    let inbox: HashSet<i64> = self
                        .store
                        .folders()?
                        .into_iter()
                        .filter(|f| f.well_known.as_deref() == Some("inbox"))
                        .map(|f| f.id)
                        .collect();
                    self.sync_items(Some(&inbox)).await?;
                }
                self.fetch_eml(None).await?;
                self.index_pending().await?;
                Ok(())
            }
            .await;
            self.emit(Event::Syncing(false));
            last_inbox = Some(Instant::now());
            if others_due {
                last_others = Some(Instant::now());
            }
            match result {
                Ok(()) => {
                    if offline {
                        // 다시 연결됐다: 백오프로 기다리던 작업을 지금 보낸다.
                        offline = false;
                        let _ = self.store.retry_ops_now(now_ms());
                        match self.run_ops().await {
                            Ok(off) => offline = off,
                            Err(Error::Owa(eo_owa::Error::LoginRequired)) => return,
                            Err(e) => self.emit(Event::Error(e.to_string())),
                        }
                    }
                }
                Err(Error::Owa(eo_owa::Error::LoginRequired)) => return,
                Err(e) => {
                    offline |= crate::ops::transient(&e);
                    self.emit(Event::Error(e.to_string()));
                }
            }
        }
    }
}
