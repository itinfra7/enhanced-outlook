//! HCL Domino 계정 연동(사용자 요청 2026-10-21): 웹메일의 메일을 EML로 되살려 'HCL Domino 메일' 아래 로컬 전용으로 넣는다.
//!
//! - 메일함: 받은 메일·보낸 메일·외부 메일·미발송 메일·지운 메일과 개인 폴더. 같은 이름의 로컬 폴더에 넣는다
//!   (HTML 백업으로 가져온 'HCL Domino 메일/받은 메일' 등과 같은 곳).
//! - 이미 가져온 문서(UNID)는 다시 받지 않는다. 서버에서 지워져도 로컬 사본은 보관으로 남는다.
//! - 양방향(0.6.0, 사용자 요청): 읽음·★·메일함 이동·삭제를 웹메일과 맞춘다. 앱에서 바꾼 것은 대기열로
//!   [`push_domino_ops`], 웹메일에서 바뀐 것은 모든 메일함을 볼 때 지난번에 본 서버 상태와 비교해 반영한다.
//! - 메일을 열면 서버가 'new' 표시를 지운다. 'new'였던 메일은 연 직후 웹메일의 안읽음 표시로 되돌리고(사용자 결정),
//!   앱 안에서도 안읽음으로 둔다.
//! - PST·HTML 백업으로 이미 가져온 같은 메일(제목이 같고 받은 시각이 10초 안)은 건너뛴다. 웹메일은 Message-ID를 주지 않는다.
//! - EML: 머리글은 Memo 양식의 입력란(RFC 2047 그대로), 본문은 MIME 조각(HTML, 없으면 일반 글), 본문 그림은 조각을
//!   Content-ID로, 첨부는 `$FILE`. MIME이 아닌(서식 있는 글) 본문은 양식에 그려진 HTML을 쓴다.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use eo_mime::backup::{NewEml, build_eml, mime_by_name, parse_address_value, parse_kst, summarize};
use eo_mime::compose::{OriginalPart, Person};
use eo_owa::domino::{Domino, Fetched, MAIL_VIEWS, MailLocation, Mailbox};
use eo_owa::notes::{Memo, ViewEntry, file_name, url_token, xml_unescape};
use eo_store::{DOMINO_SYNC_ORIGIN, ImportOutcome, Store};

use crate::import::{DOMINO_FOLDER, props_from};
use crate::{Error, Result};

/// 빠른 확인에서 보는 받은 메일 수(최근 것부터).
const QUICK_COUNT: usize = 100;
/// 같은 메일로 볼 받은 시각 차이(ms).
const SAME_MAIL_MS: i64 = 10_000;
/// 한 메일이 이만큼 연달아 실패하면 앱을 다시 켤 때까지 건너뛴다(열 때마다 'new'를 건드리므로).
const MAX_FAILURES: u32 = 3;
const MAX_ERRORS: usize = 20;

/// 확인 범위.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DominoScan {
    /// 받은 메일 첫 쪽(최근 100통)만.
    Quick,
    /// 모든 메일함·개인 폴더 전부.
    Full,
}

/// 시험용 제한.
#[derive(Debug, Clone, Default)]
pub struct DominoOptions {
    /// 이만큼만 가져온다.
    pub max_fetch: Option<usize>,
    /// 이 보기만(예: `XML_Sent`).
    pub only_view: Option<String>,
}

/// 목록에 있던 메일 하나.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedMail {
    pub unid: String,
    pub noteid: String,
    pub view: String,
    /// 웹메일 메일함 열쇠(`XML_Inbox`, `XML_Category:이름`).
    pub box_key: String,
    /// 로컬 폴더 경로('HCL Domino 메일' 아래).
    pub folder: Vec<String>,
    pub date: Option<i64>,
    pub size: Option<i64>,
    pub is_new: bool,
    /// ★(모르면 없음).
    pub star: Option<bool>,
}

#[derive(Debug, Clone, Default)]
pub struct DominoReport {
    pub listed: usize,
    pub new: usize,
    pub imported: usize,
    /// 백업으로 이미 가져온 같은 메일이라 건너뜀.
    pub duplicates: usize,
    pub failed: usize,
    /// 'new'를 되돌린 수, 되돌리지 못한 수.
    pub unread_restored: usize,
    pub unread_restore_failed: usize,
    /// 그림·첨부를 다 받지 못하고 넣은 메일 수, 받지 못한 그림·첨부 수(서버 쪽 빈 조각 등).
    pub with_missing: usize,
    pub missing_parts: usize,
    /// 새 메일이 들어간 로컬 폴더.
    pub folders: Vec<i64>,
    /// 메일함을 새로 만들었거나 순서를 바꿨다(폴더 목록을 다시 그려야 함).
    pub folders_changed: bool,
    /// 웹메일에서 바뀐 것(읽음·★·메일함·삭제)을 로컬에 반영한 메일 수.
    pub updated: usize,
    pub errors: Vec<String>,
    /// 받은 메일 목록 전부(전부 확인일 때만): 자동 정리 후보를 고를 때.
    pub inbox: Option<Vec<ListedMail>>,
    pub cancelled: bool,
}

static FAILURES: Mutex<Option<HashMap<String, u32>>> = Mutex::new(None);

fn failures<R>(f: impl FnOnce(&mut HashMap<String, u32>) -> R) -> R {
    let mut g = FAILURES.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(HashMap::new))
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

fn listed(e: &ViewEntry, mailbox: &Mailbox, folder: &[String]) -> Option<ListedMail> {
    let unid = e.unid.as_deref()?.to_ascii_uppercase();
    Some(ListedMail {
        unid,
        noteid: e.noteid.clone(),
        view: mailbox.view().to_owned(),
        box_key: mailbox.key(),
        folder: folder.to_vec(),
        date: e.full_date().and_then(parse_kst),
        size: e.size(),
        is_new: e.is_new(),
        star: e.star(),
    })
}

/// 개인 폴더 트리 한 줄의 (범주 이름, 보이는 이름). 링크 `folder_link('이름')`이 있으면 그 이름이 범주다.
fn folder_key(text: &str) -> Option<(String, String)> {
    let shown = strip_tags(text).trim().to_owned();
    let key = text
        .find("folder_link(")
        .and_then(|i| {
            let rest = &text[i + "folder_link(".len()..];
            let rest = rest.trim_start_matches(['\\', ' ']);
            let q = rest.chars().next().filter(|c| matches!(c, '\'' | '"'))?;
            let body = &rest[1..];
            let end = body.find(q)?;
            Some(body[..end].trim_end_matches('\\').to_owned())
        })
        .unwrap_or_else(|| shown.clone());
    if key.is_empty() {
        return None;
    }
    let shown = if shown.is_empty() {
        key_name(&key)
    } else {
        shown
    };
    Some((key, shown))
}

fn key_name(key: &str) -> String {
    key.rsplit('\\').next().unwrap_or(key).to_owned()
}

fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    xml_unescape(&out)
}

/// 확인할 목록: (웹메일 메일함, 로컬 폴더 경로).
async fn sources(
    client: &Domino,
    scan: DominoScan,
    report: &mut DominoReport,
) -> Result<Vec<(Mailbox, Vec<String>)>> {
    let mut out: Vec<(Mailbox, Vec<String>)> = MAIL_VIEWS
        .iter()
        .filter_map(|(v, name)| Some((Mailbox::from_key(v)?, vec![(*name).to_owned()])))
        .collect();
    if scan == DominoScan::Quick {
        out.truncate(1);
        return Ok(out);
    }
    match client.view_all("FolderTree", None).await {
        Ok(entries) => {
            let mut names: HashMap<String, String> = HashMap::new();
            for e in &entries {
                let Some((key, shown)) = e.first_value().and_then(folder_key) else {
                    continue;
                };
                names.insert(e.position.clone(), shown);
                // 위치 `1.2`의 경로: `1`, `1.2`의 이름들.
                let mut path = Vec::new();
                let parts: Vec<&str> = e.position.split('.').collect();
                for k in 1..=parts.len() {
                    if let Some(n) = names.get(&parts[..k].join(".")) {
                        path.push(n.clone());
                    }
                }
                out.push((Mailbox::Folder(key), path));
            }
        }
        Err(e @ eo_owa::Error::LoginRequired) => return Err(e.into()),
        Err(e) => push_error(report, format!("개인 폴더 목록: {e}")),
    }
    Ok(out)
}

fn push_error(report: &mut DominoReport, e: String) {
    if report.errors.len() < MAX_ERRORS {
        report.errors.push(e);
    }
}

/// 한 번 확인한다: 목록을 보고 아직 없는 메일을 가져온다. `progress(끝낸 수, 가져올 수)`.
pub async fn sync_domino(
    client: &Domino,
    store: &Store,
    scan: DominoScan,
    opts: &DominoOptions,
    cancel: &AtomicBool,
    mut progress: impl FnMut(usize, usize),
) -> Result<DominoReport> {
    let mut report = DominoReport::default();
    let mut all: Vec<ListedMail> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let srcs = sources(client, scan, &mut report).await?;
    // 웹메일 메일함 열쇠 → 로컬 폴더.
    let mut folder_of_box: HashMap<String, i64> = HashMap::new();
    let whole = scan == DominoScan::Full && opts.only_view.is_none();
    if whole {
        // 메일이 없어도 웹메일의 메일함은 다 보이게, 순서는 웹메일 왼쪽 메뉴대로(받은 메일 → … → 지운 메일 → 개인 폴더).
        // 백업으로 만든 다른 하위 폴더(순서 200)는 그 아래 이름순(사용자 요청 2026-10-21).
        for (i, (mailbox, folder)) in srcs.iter().enumerate() {
            let mut path = vec![DOMINO_FOLDER.to_owned()];
            path.extend(folder.iter().cloned());
            let (id, created) = store.find_or_create_folder_path(&path)?;
            let order = 10 + i64::try_from(i).unwrap_or(0);
            if store.set_folder_sort_order(id, order)? || !created.is_empty() {
                report.folders_changed = true;
            }
            store.domino_set_folder_box(id, &mailbox.key())?;
            folder_of_box.insert(mailbox.key(), id);
        }
    }
    // 메일함 하나라도 못 읽었으면 "웹메일에서 지워짐"을 판단하지 않는다.
    let mut complete = whole;
    for (mailbox, folder) in srcs {
        let view = mailbox.view();
        if opts.only_view.as_ref().is_some_and(|v| v != view) {
            continue;
        }
        let category = match &mailbox {
            Mailbox::Folder(k) => Some(k.as_str()),
            _ => None,
        };
        let entries = if scan == DominoScan::Quick {
            client
                .view_page(view, None, 1, QUICK_COUNT)
                .await
                .map(|p| p.entries)
        } else {
            client.view_all(view, category).await
        };
        let entries = match entries {
            Ok(e) => e,
            Err(e @ eo_owa::Error::LoginRequired) => return Err(e.into()),
            Err(e) if category.is_some() => {
                complete = false;
                push_error(&mut report, format!("{}: {e}", folder.join("/")));
                continue;
            }
            Err(e) => return Err(e.into()),
        };
        let mails: Vec<ListedMail> = entries
            .iter()
            .filter_map(|e| listed(e, &mailbox, &folder))
            .collect();
        if mailbox == Mailbox::Inbox && scan == DominoScan::Full {
            report.inbox = Some(mails.clone());
        }
        for m in mails {
            if seen.insert(m.unid.clone()) {
                all.push(m);
            }
        }
    }
    report.listed = all.len();
    let known = store.domino_known()?;
    // 'new' 메일: 목록의 열과 미확인 보기(개인 폴더 보기에는 'new' 열이 없을 수 있다).
    // 모든 메일함을 볼 때는 늘(양방향 읽음 동기화), 빠른 확인에서는 새 메일이 있을 때만 받는다.
    let need_unread = whole || all.iter().any(|m| !known.contains(&m.unid));
    let unread: HashSet<String> = if need_unread {
        client
            .view_all("XML_Unread", None)
            .await?
            .into_iter()
            .filter_map(|e| e.unid.map(|u| u.to_ascii_uppercase()))
            .collect()
    } else {
        HashSet::new()
    };
    if whole {
        // 웹메일에서 바뀐 것(읽음·★·메일함·삭제)을 로컬에 반영한다(0.6.0 양방향).
        let in_all: Option<HashSet<String>> = if complete {
            Some(
                client
                    .view_all("XML_All", None)
                    .await?
                    .into_iter()
                    .filter_map(|e| e.unid.map(|u| u.to_ascii_uppercase()))
                    .collect(),
            )
        } else {
            None
        };
        apply_server_state(
            store,
            &all,
            &unread,
            in_all.as_ref(),
            &folder_of_box,
            &mut report,
        )?;
    }
    let mut todo: Vec<ListedMail> = all
        .into_iter()
        .filter(|m| !known.contains(&m.unid))
        .filter(|m| failures(|f| f.get(&m.unid).copied().unwrap_or(0)) < MAX_FAILURES)
        .collect();
    // 최근 것부터.
    todo.sort_by_key(|m| std::cmp::Reverse(m.date.unwrap_or(0)));
    if let Some(max) = opts.max_fetch {
        todo.truncate(max);
    }
    report.new = todo.len();
    if todo.is_empty() {
        return Ok(report);
    }
    let loc = client.location().ok_or(eo_owa::Error::LoginRequired)?;
    let mut locals = LocalCopies::load(store)?;
    let mut folders: HashMap<Vec<String>, i64> = HashMap::new();
    let total = todo.len();
    for (k, m) in todo.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            report.cancelled = true;
            break;
        }
        progress(k, total);
        let was_new = m.is_new || unread.contains(&m.unid);
        let raw = match fetch(client, &loc, m, was_new, &mut report).await {
            Ok(raw) => raw,
            Err(Error::Owa(e @ eo_owa::Error::LoginRequired)) => return Err(e.into()),
            Err(e) => {
                report.failed += 1;
                failures(|f| *f.entry(m.unid.clone()).or_default() += 1);
                push_error(&mut report, format!("{} {}: {e}", m.view, m.unid));
                continue;
            }
        };
        failures(|f| f.remove(&m.unid));
        let Some(summary) = summarize(&raw) else {
            report.failed += 1;
            push_error(
                &mut report,
                format!("{} {}: 되살린 원문을 읽을 수 없음", m.view, m.unid),
            );
            continue;
        };
        let date = m.date.or(summary.date).unwrap_or_else(now_ms);
        if let Some(id) = locals.take(&summary.subject, date) {
            store.domino_record(&m.unid, Some(id), &m.noteid, &m.box_key, now_ms())?;
            store.domino_set_srv(&m.unid, Some(&m.box_key), Some(was_new), m.star)?;
            report.duplicates += 1;
            continue;
        }
        let mut path = vec![DOMINO_FOLDER.to_owned()];
        path.extend(m.folder.iter().cloned());
        let folder = match folders.get(&path) {
            Some(id) => *id,
            None => {
                let (id, _) = store.find_or_create_folder_path(&path)?;
                folders.insert(path, id);
                id
            }
        };
        let mut props = props_from(&summary, raw.len(), date);
        props.is_read = !was_new;
        props.flag = i64::from(m.star == Some(true));
        let id = match store.import_message(folder, &raw, &props, DOMINO_SYNC_ORIGIN)? {
            ImportOutcome::Imported(id) => {
                report.imported += 1;
                if !report.folders.contains(&folder) {
                    report.folders.push(folder);
                }
                Some(id)
            }
            ImportOutcome::Duplicate => {
                report.duplicates += 1;
                None
            }
        };
        store.domino_record(&m.unid, id, &m.noteid, &m.box_key, now_ms())?;
        store.domino_set_srv(&m.unid, Some(&m.box_key), Some(was_new), m.star)?;
    }
    progress(total, total);
    Ok(report)
}

/// 웹메일에서 바뀐 것을 로컬에 반영한다(0.6.0 양방향): 지난번에 본 서버 상태(`srv_*`)와 달라진 것만.
/// 앱에서 바꾼 것이 아직 웹메일로 가는 중인 메일은 건드리지 않는다(서로 덮어쓰지 않게).
/// 처음 보는(지난 상태를 모르는) 것은 적기만 한다. `in_all`: 모든 메일 보기(있으면 "웹메일에서 지워짐"도 판단).
fn apply_server_state(
    store: &Store,
    listed: &[ListedMail],
    unread: &HashSet<String>,
    in_all: Option<&HashSet<String>>,
    folder_of_box: &HashMap<String, i64>,
    report: &mut DominoReport,
) -> Result<()> {
    let by_unid: HashMap<&str, &ListedMail> = listed.iter().map(|m| (m.unid.as_str(), m)).collect();
    let pending = store.domino_pending_unids()?;
    let now = now_ms();
    for item in store.domino_items()? {
        let Some(mid) = item.message_id else { continue };
        if item.server_deleted || pending.contains(&item.unid) {
            continue;
        }
        let Some(m) = by_unid.get(item.unid.as_str()) else {
            // 어느 메일함에도, 모든 메일에도 없다: 웹메일에서 지웠다. 로컬은 보관으로 남긴다.
            if in_all.is_some_and(|all| !all.contains(&item.unid)) {
                let changed = store.domino_archive_local(&[mid], now)?;
                store.domino_mark_server_deleted(std::slice::from_ref(&item.unid), now)?;
                if !changed.is_empty() {
                    report.updated += 1;
                    touch(store, mid, report)?;
                }
            }
            continue;
        };
        let is_new = m.is_new || unread.contains(&item.unid);
        let mut changed = false;
        store.transaction(|tx| {
            let Some(row) = tx.message(mid)? else {
                return Ok(());
            };
            if item.srv_box.is_some()
                && item.srv_box.as_deref() != Some(m.box_key.as_str())
                && let Some(&to) = folder_of_box.get(&m.box_key)
                && to != row.folder_id
            {
                tx.set_folder(mid, to)?;
                report.folders.push(row.folder_id);
                report.folders.push(to);
                changed = true;
            }
            if item.srv_new.is_some_and(|n| n != is_new) && row.props.is_read == is_new {
                tx.set_read(mid, !is_new)?;
                changed = true;
            }
            if let Some(star) = m.star
                && item.srv_star.is_some_and(|s| s != star)
            {
                // 완료(2)는 웹메일에 없다: ★이 꺼졌을 때 완료는 그대로 둔다.
                let flag = match (star, row.props.flag) {
                    (true, _) => 1,
                    (false, 2) => 2,
                    (false, _) => 0,
                };
                if flag != row.props.flag {
                    tx.set_flag(mid, flag)?;
                    changed = true;
                }
            }
            if changed {
                report.folders.push(row.folder_id);
            }
            Ok(())
        })?;
        if changed {
            report.updated += 1;
        }
        store.domino_set_srv(&item.unid, Some(&m.box_key), Some(is_new), m.star)?;
    }
    report.folders.sort_unstable();
    report.folders.dedup();
    Ok(())
}

fn touch(store: &Store, message_id: i64, report: &mut DominoReport) -> Result<()> {
    if let Some(m) = store.message(message_id)?
        && !report.folders.contains(&m.folder_id)
    {
        report.folders.push(m.folder_id);
    }
    Ok(())
}

/// 앱에서 바꾼 것을 웹메일로 보낸 결과.
#[derive(Debug, Clone, Default)]
pub struct DominoOpsReport {
    pub done: usize,
    pub retried: usize,
    /// 끝내 거부돼 로컬을 되돌린 작업: (종류, 오류).
    pub abandoned: Vec<(String, String)>,
    /// 되돌리느라 바뀐 로컬 폴더.
    pub folders: Vec<i64>,
}

/// 보낸 작업이 바꾼 서버 상태: (메일함 열쇠, 'new', ★, 웹메일에서 완전히 지움).
type Pushed = (Option<String>, Option<bool>, Option<bool>, bool);

/// 이만큼 실패하면 포기하고 로컬을 되돌린다.
const MAX_OP_ATTEMPTS: i64 = 5;

/// 쌓인 작업을 웹메일에 보낸다(쌓인 순서대로). 보낸 뒤 서버 상태(`srv_*`)를 적어 다음 확인 때 "웹메일에서 바뀜"으로 보지 않게 한다.
pub async fn push_domino_ops(client: &Domino, store: &Store) -> Result<DominoOpsReport> {
    use eo_store::domino_op as op;
    let mut report = DominoOpsReport::default();
    let now = now_ms();
    for o in store.domino_due_ops(now, 200)? {
        let Some(item) = store.domino_item(&o.unid)? else {
            store.domino_op_abandon(o.id, "문서 기록 없음")?;
            continue;
        };
        let ids = [item.noteid.clone()];
        // (바뀐 메일함, 'new', ★, 웹메일에서 완전히 지움)
        let r: eo_owa::Result<Pushed> = match o.kind.as_str() {
            op::READ => {
                let read = o.arg == "1";
                client
                    .set_read(&ids, read)
                    .await
                    .map(|()| (None, Some(!read), None, false))
            }
            op::STAR => {
                let on = o.arg == "1";
                client
                    .set_star(&item.noteid, on)
                    .await
                    .map(|()| (None, None, Some(on), false))
            }
            op::DELETE => {
                let from = Mailbox::from_key(&o.arg).unwrap_or(Mailbox::Inbox);
                client.delete_docs(&from, &ids).await.map(|()| {
                    if from == Mailbox::Trash {
                        (None, None, None, true)
                    } else {
                        (Some(Mailbox::Trash.key()), None, None, false)
                    }
                })
            }
            op::RECOVER => client
                .recover(&ids)
                .await
                .map(|()| (Some(Mailbox::Inbox.key()), None, None, false)),
            op::FOLDER_MOVE => client.move_to_folder(&ids, &o.arg).await.map(|()| {
                (
                    Some(Mailbox::Folder(o.arg.clone()).key()),
                    None,
                    None,
                    false,
                )
            }),
            op::MOVE_BACK => client
                .move_back(&ids)
                .await
                .map(|()| (Some(Mailbox::Inbox.key()), None, None, false)),
            other => {
                store.domino_op_abandon(o.id, &format!("모르는 작업 {other}"))?;
                continue;
            }
        };
        match r {
            Ok((srv_box, srv_new, srv_star, deleted)) => {
                store.domino_set_srv(&o.unid, srv_box.as_deref(), srv_new, srv_star)?;
                if deleted {
                    store.domino_mark_server_deleted(std::slice::from_ref(&o.unid), now)?;
                }
                store.domino_op_done(o.id)?;
                report.done += 1;
            }
            Err(e @ eo_owa::Error::LoginRequired) => return Err(e.into()),
            Err(e) if o.attempts + 1 >= MAX_OP_ATTEMPTS => {
                store.domino_op_abandon(o.id, &e.to_string())?;
                rollback(store, &item, &o.kind, &mut report)?;
                report.abandoned.push((o.kind.clone(), e.to_string()));
            }
            Err(e) => {
                // 30초, 1분, 2분, 4분 뒤.
                let wait = 30_000_i64 << o.attempts.clamp(0, 6);
                store.domino_op_retry(o.id, &e.to_string(), now + wait)?;
                report.retried += 1;
            }
        }
    }
    report.folders.sort_unstable();
    report.folders.dedup();
    Ok(report)
}

/// 포기한 작업의 로컬 반영을 마지막으로 본 서버 상태로 되돌린다.
fn rollback(
    store: &Store,
    item: &eo_store::DominoItem,
    kind: &str,
    report: &mut DominoOpsReport,
) -> Result<()> {
    use eo_store::domino_op as op;
    let Some(mid) = item.message_id else {
        return Ok(());
    };
    let Some(row) = store.message(mid)? else {
        return Ok(());
    };
    report.folders.push(row.folder_id);
    match kind {
        op::READ => {
            if let Some(new) = item.srv_new {
                store.transaction(|tx| tx.set_read(mid, !new))?;
            }
        }
        op::STAR => {
            if let Some(star) = item.srv_star {
                store.transaction(|tx| tx.set_flag(mid, i64::from(star)))?;
            }
        }
        _ => {
            let boxes = store.domino_folder_boxes()?;
            let back = item
                .srv_box
                .as_ref()
                .and_then(|b| boxes.iter().find(|(_, v)| *v == b).map(|(f, _)| *f));
            if let Some(f) = back {
                store.transaction(|tx| tx.set_folder(mid, f))?;
                report.folders.push(f);
            }
            if !item.server_deleted {
                store.domino_unarchive_local(mid)?;
            }
        }
    }
    Ok(())
}

/// 백업으로 가져온 같은 메일 찾기(제목이 같고 받은 시각이 10초 안, 가장 가까운 것, 한 번씩만).
struct LocalCopies {
    items: Vec<(i64, String, i64)>,
    used: HashSet<i64>,
}

impl LocalCopies {
    fn load(store: &Store) -> Result<Self> {
        Ok(Self {
            items: store
                .domino_local_mails()?
                .into_iter()
                .map(|(id, s, d)| (id, s.trim().to_lowercase(), d))
                .collect(),
            used: HashSet::new(),
        })
    }

    fn take(&mut self, subject: &str, date: i64) -> Option<i64> {
        let s = subject.trim().to_lowercase();
        let id = self
            .items
            .iter()
            .filter(|(id, subj, d)| {
                *subj == s && (d - date).abs() <= SAME_MAIL_MS && !self.used.contains(id)
            })
            .min_by_key(|(_, _, d)| (d - date).abs())
            .map(|(id, _, _)| *id)?;
        self.used.insert(id);
        Some(id)
    }
}

/// 메일 하나를 열어 EML로. 'new'였으면 연 직후 되돌린다.
async fn fetch(
    client: &Domino,
    loc: &MailLocation,
    m: &ListedMail,
    was_new: bool,
    report: &mut DominoReport,
) -> Result<Vec<u8>> {
    let memo = client.memo(&m.unid).await?;
    if was_new {
        match client.mark_unread(std::slice::from_ref(&m.noteid)).await {
            Ok(()) => report.unread_restored += 1,
            Err(e @ eo_owa::Error::LoginRequired) => return Err(e.into()),
            Err(e) => {
                report.unread_restore_failed += 1;
                push_error(
                    report,
                    format!("{} {}: 'new' 되돌리기 실패: {e}", m.view, m.unid),
                );
            }
        }
    }
    let body = fetch_body(client, loc, &m.unid, &memo).await?;
    if body.missing > 0 {
        report.missing_parts += body.missing;
        report.with_missing += 1;
    }
    Ok(build_eml(&new_eml(&memo, m, body)))
}

/// 되살린 본문: HTML, 본문 그림, 첨부.
#[derive(Debug, Default)]
struct Body {
    html: String,
    inline: Vec<OriginalPart>,
    attachments: Vec<OriginalPart>,
    /// 받지 못한 그림·첨부 수.
    missing: usize,
}

/// 그림·첨부 하나. 서버가 빈 응답을 주다 끊는 조각이 있어(실계정 2026-10-21, 전달 메일 안의 빈 그림 조각)
/// 받지 못하거나 비어 있으면 건너뛰고 센다. 메일 전체를 버리지 않는다. 로그인이 필요하면 그대로 돌려준다.
fn optional(r: eo_owa::Result<Fetched>, missing: &mut usize) -> Result<Option<Fetched>> {
    match r {
        Ok(f) if !f.data.is_empty() => Ok(Some(f)),
        Err(e @ eo_owa::Error::LoginRequired) => Err(e.into()),
        _ => {
            *missing += 1;
            Ok(None)
        }
    }
}

/// 본문 안의 이 메일 조각·파일 링크.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Ref {
    /// `Body/` 뒤 요소(`M1.2`)와 덧붙일 질의(서식 있는 본문 그림의 `&FieldElemFormat=gif`).
    Element(String, String),
    /// `$FILE` 링크 그대로.
    File(String),
}

/// 본문 HTML에서 이 메일(`unid`)의 조각·파일 링크를 `cid:`로 바꾸고, 나머지 서버 상대 주소는 절대 주소로.
fn rewrite_refs(html: &str, unid: &str, host: &str) -> (String, Vec<(Ref, String)>) {
    let lower = html.to_ascii_lowercase();
    let unid = unid.to_ascii_lowercase();
    let mut refs: Vec<(Ref, String)> = Vec::new();
    let mut out = String::with_capacity(html.len());
    let mut last = 0;
    let mut from = 0;
    while let Some(i) = lower[from..].find(&unid) {
        let at = from + i;
        let (start, end) = url_token(&lower, at);
        from = end.max(at + unid.len());
        if start < last || !lower[start..at].contains(".nsf/") {
            continue;
        }
        let tail = &html[at + unid.len()..end];
        let tail_lower = tail.to_ascii_lowercase();
        let r = if tail_lower.starts_with("/body/") {
            let e = &tail["/body/".len()..];
            let (elem, query) = e.split_once('?').unwrap_or((e, ""));
            if elem.is_empty() || !elem.chars().all(|c| c.is_ascii_alphanumeric() || c == '.') {
                continue;
            }
            let format = query
                .split("&amp;")
                .flat_map(|p| p.split('&'))
                .find_map(|p| p.strip_prefix("FieldElemFormat="))
                .filter(|f| matches!(*f, "gif" | "jpg" | "png"))
                .map(|f| format!("&FieldElemFormat={f}"))
                .unwrap_or_default();
            Ref::Element(elem.to_owned(), format)
        } else if tail_lower.starts_with("/$file/") {
            Ref::File(xml_unescape(&html[start..end]))
        } else {
            continue;
        };
        let cid = match refs.iter().find(|(x, _)| *x == r) {
            Some((_, cid)) => cid.clone(),
            None => {
                let label = match &r {
                    Ref::Element(e, _) => e.clone(),
                    Ref::File(_) => format!("file{}", refs.len() + 1),
                };
                let cid = format!("{label}.{unid}@domino");
                refs.push((r, cid.clone()));
                cid
            }
        };
        out.push_str(&html[last..start]);
        out.push_str("cid:");
        out.push_str(&cid);
        last = end;
    }
    out.push_str(&html[last..]);
    // 남은 서버 상대 주소(`/icons/…`, 다른 메일 링크)는 절대 주소로: 외부 이미지처럼 막힌다.
    let abs = format!("https://{host}/");
    let mut fixed = out;
    for attr in ["src", "href", "background"] {
        for q in ['"', '\''] {
            fixed = fixed.replace(&format!("{attr}={q}/"), &format!("{attr}={q}{abs}"));
        }
    }
    (fixed, refs)
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// `image/png; name=…`의 형식 부분(없으면 이름으로 짐작).
fn mime_or(content_type: &str, name: &str) -> String {
    let t = content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if t.is_empty() || t == "application/octet-stream" || !t.contains('/') {
        mime_by_name(name).to_owned()
    } else {
        t
    }
}

fn ext_for(mime: &str) -> &'static str {
    match mime {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/bmp" => "bmp",
        "text/calendar" => "ics",
        "message/rfc822" => "eml",
        "application/pdf" => "pdf",
        _ => "bin",
    }
}

async fn fetch_body(client: &Domino, loc: &MailLocation, unid: &str, memo: &Memo) -> Result<Body> {
    let parts = memo.parts();
    let leaves: Vec<_> = parts.iter().filter(|p| !p.is_multipart()).collect();
    let html_part = leaves.iter().find(|p| p.content_type == "text/html");
    let plain_part = leaves.iter().find(|p| p.content_type == "text/plain");
    let html = if let Some(p) = html_part {
        client
            .body_element(unid, &p.id, "&Charset=UTF-8")
            .await?
            .text()
    } else if let Some(p) = plain_part {
        let t = client
            .body_element(unid, &p.id, "&Charset=UTF-8")
            .await?
            .text();
        format!("<div style=\"white-space:pre-wrap\">{}</div>", escape(&t))
    } else if parts.is_empty() {
        // MIME이 아닌(서식 있는 글) 본문: 양식에 그려진 HTML.
        memo.xmp_body.clone().unwrap_or_default()
    } else {
        String::new()
    };
    let (html, refs) = rewrite_refs(&html, unid, &loc.host);
    let mut body = Body {
        html,
        ..Body::default()
    };
    let file_names: Vec<String> = memo
        .files
        .iter()
        .map(|l| {
            file_name(
                l.rsplit("/$FILE/")
                    .next()
                    .unwrap_or("")
                    .trim_end_matches("?OpenElement"),
            )
        })
        .collect();
    let mut inline_files: HashSet<String> = HashSet::new();
    let mut referenced: HashSet<String> = HashSet::new();
    for (r, cid) in &refs {
        let (fetched, name) = match r {
            Ref::Element(e, format) => {
                referenced.insert(e.clone());
                let Some(f) = optional(
                    client.body_element(unid, e, format).await,
                    &mut body.missing,
                )?
                else {
                    continue;
                };
                let part_name = leaves
                    .iter()
                    .find(|p| p.id == *e)
                    .and_then(|p| p.name.clone());
                let mime = mime_or(&f.content_type, part_name.as_deref().unwrap_or(""));
                let name = part_name.unwrap_or_else(|| format!("{e}.{}", ext_for(&mime)));
                (f, name)
            }
            Ref::File(link) => {
                let name = file_name(
                    link.rsplit("/$FILE/")
                        .next()
                        .unwrap_or("")
                        .trim_end_matches("?OpenElement"),
                );
                inline_files.insert(name.clone());
                let Some(f) = optional(client.file(link).await, &mut body.missing)? else {
                    continue;
                };
                (f, name)
            }
        };
        body.inline.push(OriginalPart {
            mime: mime_or(&fetched.content_type, &name),
            name,
            data: fetched.data,
            content_id: Some(cid.clone()),
        });
    }
    // 본문이 가리키지 않는 조각(그림·파일). 같은 이름의 `$FILE`이 있으면 그쪽으로 받는다.
    for p in &leaves {
        if p.content_type.starts_with("text/plain")
            || p.content_type.starts_with("text/html")
            || referenced.contains(&p.id)
            || p.name.as_ref().is_some_and(|n| file_names.contains(n))
        {
            continue;
        }
        let Some(f) = optional(
            client.body_element(unid, &p.id, "").await,
            &mut body.missing,
        )?
        else {
            continue;
        };
        let mime = mime_or(&f.content_type, p.name.as_deref().unwrap_or(""));
        body.attachments.push(OriginalPart {
            name: p
                .name
                .clone()
                .unwrap_or_else(|| format!("{}.{}", p.id, ext_for(&mime))),
            mime,
            data: f.data,
            content_id: None,
        });
    }
    for (link, name) in memo.files.iter().zip(&file_names) {
        if inline_files.contains(name) || name.is_empty() {
            continue;
        }
        let Some(f) = optional(client.file(link).await, &mut body.missing)? else {
            continue;
        };
        body.attachments.push(OriginalPart {
            mime: mime_or(&f.content_type, name),
            name: name.clone(),
            data: f.data,
            content_id: None,
        });
    }
    Ok(body)
}

/// 노츠 이름(`CN=홍길동 u000001/O=EXAMPLE`)이면 보이는 이름만.
fn notes_name(s: &str) -> String {
    let s = s.trim();
    let s = s.strip_prefix("CN=").unwrap_or(s);
    s.split('/').next().unwrap_or(s).trim().to_owned()
}

fn people(memo: &Memo, field: &str) -> Vec<Person> {
    parse_address_value(memo.field(field).unwrap_or(""))
        .into_iter()
        .map(|p| {
            if p.email.contains('@') {
                p
            } else {
                Person {
                    name: notes_name(if p.name.is_empty() { &p.email } else { &p.name }),
                    email: String::new(),
                }
            }
        })
        .filter(|p| !p.name.is_empty() || !p.email.is_empty())
        .collect()
}

fn new_eml(memo: &Memo, m: &ListedMail, body: Body) -> NewEml {
    let mut from = people(memo, "From");
    if from.is_empty() {
        from = people(memo, "SMTPOriginator");
    }
    if from.is_empty()
        && let Some(n) = memo
            .field("hidenFrom")
            .map(notes_name)
            .filter(|n| !n.is_empty())
    {
        from.push(Person {
            name: n,
            email: String::new(),
        });
    }
    let reply_to: Vec<Person> = people(memo, "ReplyTo")
        .into_iter()
        .filter(|r| !from.iter().any(|f| f.email.eq_ignore_ascii_case(&r.email)))
        .collect();
    let mut bcc = people(memo, "BlindCopyTo");
    if bcc.is_empty() {
        bcc = people(memo, "InetBlindCopyTo");
    }
    let date = m.date.or_else(|| {
        memo.field("DspSendDateTime")
            .or_else(|| memo.field("PostedDate"))
            .and_then(parse_kst)
    });
    let mut extra = Vec::new();
    match memo
        .field("Importance")
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("high" | "1") => extra.push(("Importance".to_owned(), "high".to_owned())),
        Some("low" | "3") => extra.push(("Importance".to_owned(), "low".to_owned())),
        _ => {}
    }
    extra.push(("X-EO-Restored".to_owned(), "domino-sync".to_owned()));
    extra.push(("X-Domino-UNID".to_owned(), m.unid.clone()));
    NewEml {
        from,
        to: people(memo, "SendTo"),
        cc: people(memo, "CopyTo"),
        bcc,
        reply_to,
        subject: memo.title.clone(),
        date,
        message_id: format!("<{}@domino.local>", m.unid),
        html: body.html,
        inline: body.inline,
        attachments: body.attachments,
        extra_headers: extra,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const UNID: &str = "D77632AF61A4F0A549258CD60024422D";

    #[test]
    fn body_refs_become_cids_and_relative_urls_absolute() {
        let u = UNID.to_ascii_lowercase();
        let html = format!(
            "<img src=\"/mail9/u000001.nsf/0/{u}/Body/M1.2?OpenElement\"><img src='/mail9/u000001.nsf/0/{u}/Body/M1.2?OpenElement'>\
             <img src=\"https://mail1.example.com/mail9/u000001.nsf/0/{u}/Body/0.1A2?OpenElement&amp;FieldElemFormat=gif\">\
             <a href=\"/mail9/u000001.nsf/all/{UNID}/$FILE/%EB%B3%B4%EA%B3%A0.pdf\">보고</a>\
             <img src=\"/icons/mehtml.gif\"><img src=\"https://plugin.example.com:443/outlook/receive?r=1\">\
             <a href=\"/mail9/u000001.nsf/0/11111111111111111111111111111111/Body/M1?OpenElement\">다른 메일</a>"
        );
        let (out, refs) = rewrite_refs(&html, UNID, "mail1.example.com");
        assert_eq!(
            refs.iter()
                .map(|(r, c)| (r.clone(), c.clone()))
                .collect::<Vec<_>>(),
            [
                (
                    Ref::Element("M1.2".into(), String::new()),
                    format!("M1.2.{u}@domino")
                ),
                (
                    Ref::Element("0.1A2".into(), "&FieldElemFormat=gif".into()),
                    format!("0.1A2.{u}@domino")
                ),
                (
                    Ref::File(format!(
                        "/mail9/u000001.nsf/all/{UNID}/$FILE/%EB%B3%B4%EA%B3%A0.pdf"
                    )),
                    format!("file3.{u}@domino")
                ),
            ],
            "같은 조각은 하나, 다른 메일 링크는 그대로"
        );
        assert_eq!(out.matches(&format!("cid:M1.2.{u}@domino")).count(), 2);
        assert!(out.contains(&format!("src=\"cid:0.1A2.{u}@domino\"")));
        assert!(out.contains("src=\"https://mail1.example.com/icons/mehtml.gif\""));
        assert!(out.contains("https://plugin.example.com:443/outlook/receive?r=1"));
        assert!(out.contains(
            "href=\"https://mail1.example.com/mail9/u000001.nsf/0/11111111111111111111111111111111/Body/M1?OpenElement\""
        ));
    }

    #[test]
    fn folder_tree_keys_and_local_copy_matching() {
        assert_eq!(
            folder_key("<a href=\"javascript:folder_link('팀공지')\">팀 공지</a>"),
            Some(("팀공지".to_owned(), "팀 공지".to_owned()))
        );
        assert_eq!(
            folder_key("프로젝트"),
            Some(("프로젝트".to_owned(), "프로젝트".to_owned()))
        );
        assert_eq!(folder_key("  "), None);
        let mut c = LocalCopies {
            items: vec![
                (1, "주간 보고".into(), 100_000),
                (2, "주간 보고".into(), 108_000),
                (3, "다른 메일".into(), 100_000),
            ],
            used: HashSet::new(),
        };
        assert_eq!(c.take(" 주간 보고 ", 107_000), Some(2), "가장 가까운 것");
        assert_eq!(c.take("주간 보고", 107_000), Some(1), "한 번 쓴 것은 빼고");
        assert_eq!(c.take("주간 보고", 107_000), None);
        assert_eq!(
            c.take("다른 메일", 111_000),
            None,
            "10초 넘게 차이 나면 다른 메일"
        );
    }

    #[test]
    fn eml_from_memo_fields() {
        let memo = Memo {
            title: "Fwd: 협업NAS 조치 방법".into(),
            fields: vec![
                (
                    "From".into(),
                    "\"=?EUC-KR?B?sejDtrz2?=\" <U000002@inbound.example.com>".into(),
                ),
                (
                    "ReplyTo".into(),
                    "\"=?EUC-KR?B?sejDtrz2?=\" <U000002@inbound.example.com>".into(),
                ),
                (
                    "SendTo".into(),
                    "user1@example.com; CN=홍길동 u000001/O=EXAMPLE".into(),
                ),
                ("Importance".into(), "High".into()),
            ],
            ..Memo::default()
        };
        let m = ListedMail {
            unid: UNID.into(),
            noteid: "18B3A".into(),
            view: "XML_Inbox".into(),
            box_key: "XML_Inbox".into(),
            folder: vec!["받은 메일".into()],
            date: parse_kst("2025-07-29 15:36:26"),
            size: Some(1),
            is_new: false,
            star: None,
        };
        let e = new_eml(
            &memo,
            &m,
            Body {
                html: "<p>본문</p>".into(),
                ..Body::default()
            },
        );
        assert_eq!(e.from[0].name, "김철수");
        assert!(e.reply_to.is_empty(), "보낸 사람과 같은 회신 주소는 뺀다");
        assert_eq!(e.to.len(), 2);
        assert_eq!(
            (e.to[1].name.as_str(), e.to[1].email.as_str()),
            ("홍길동 u000001", ""),
            "노츠 이름은 보이는 이름만"
        );
        assert_eq!(e.message_id, format!("<{UNID}@domino.local>"));
        let raw = build_eml(&e);
        let s = summarize(&raw).unwrap();
        assert_eq!(s.subject, "Fwd: 협업NAS 조치 방법");
        assert_eq!(s.importance, 2);
        assert_eq!(s.date, parse_kst("2025-07-29 15:36:26"));
    }
}
