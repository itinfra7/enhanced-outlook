//! 로컬 저장소: SQLite(WAL) 스키마·마이그레이션, zstd 압축 EML 저장소, 서버 반영 작업 큐.
//!
//! 계획서 6장. 원문은 DB BLOB이 아니라 파일 시스템에 저장한다. 위치는
//! 실행 파일이 있는 폴더의 `EnhancedOutlook\`(`db.sqlite`, `eml\`, `index\`, `logs\`).

mod activity;
mod domino;
mod drafts;
mod dupes;
mod eml;
mod folder_ops;
mod import;
mod local;
mod model;
mod ops;
mod schema;
mod time;

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use rusqlite::{Connection, OptionalExtension, Row, params};

pub use activity::ActivityOp;
pub use domino::{
    DOMINO_SYNC_ORIGIN, DominoCopy, DominoItem, DominoOp, RedirectDomino, RedirectOutlook,
    domino_op,
};
pub use drafts::{
    Draft, DraftContent, DraftLink, DraftOptions, Recipient, draft_state, orig_key,
    parse_address_list,
};
pub use dupes::{DupProgress, DupRow, DupStage};
pub use eml::{EmlStore, sha256_hex};
pub use import::ImportOutcome;
pub use local::Touched;
pub use model::{
    AddressLists, BatchProgress, EmlTodo, FolderCounts, FolderRow, IndexTodo, MessageProps,
    MessageRow, OpRow, OpSummary, PendingGuard, RowLite, ServerFolder, server_state,
};
pub use ops::{
    MAX_ITEM_ERRORS, OP_COPY, OP_DELETE, OP_DRAFT_SAVE, OP_MOVE, OP_SEND, OP_SET_FLAG, OP_SET_PIN,
    OP_SET_READ, op_backoff_ms,
};
pub use time::parse_rfc3339_ms;

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("DB 오류: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("파일 오류: {0}")]
    Io(#[from] std::io::Error),
    #[error("작업 큐 내용 오류: {0}")]
    Json(#[from] serde_json::Error),
    /// 할 수 없는 조작(사용자에게 그대로 보여 줄 문장).
    #[error("{0}")]
    Invalid(String),
}

/// 외부 이미지 허용 목록의 meta 키 앞부분(보낸 사람 주소, 도메인).
const REMOTE_SENDER: &str = "remote_images:";
const REMOTE_DOMAIN: &str = "remote_domain:";

/// 데이터 폴더 이름.
pub const DATA_DIR: &str = "EnhancedOutlook";

/// 기본 저장 위치: 실행 파일이 있는 폴더의 `EnhancedOutlook`(포터블). 실행 파일 위치를 알 수 없으면 현재 폴더.
pub fn default_root() -> PathBuf {
    default_root_migrating().0
}

/// [`default_root`] + 예전 데이터 폴더를 옮겼으면 그 예전 위치. 새 폴더가 없고, 실행 파일 옆에 메일 저장소
/// (`db.sqlite`와 `eml\`)가 든 폴더가 하나만 있으면 앞 버전(다른 이름)의 데이터로 보고 새 이름으로 옮긴다.
/// 옮기지 못하면(다른 프로그램이 쓰는 중 등) 그 폴더를 그대로 쓴다.
pub fn default_root_migrating() -> (PathBuf, Option<PathBuf>) {
    let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
    else {
        return (PathBuf::from(DATA_DIR), None);
    };
    let root = dir.join(DATA_DIR);
    if root.exists() {
        return (root, None);
    }
    match previous_root(&dir) {
        Some(old) => match std::fs::rename(&old, &root) {
            Ok(()) => (root, Some(old)),
            Err(_) => (old, None),
        },
        None => (root, None),
    }
}

/// `dir` 안의 앞 버전 데이터 폴더(메일 저장소가 든 폴더가 하나뿐일 때만).
fn previous_root(dir: &Path) -> Option<PathBuf> {
    let found: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir() && p.join("db.sqlite").is_file() && p.join("eml").is_dir())
        .collect();
    (found.len() == 1).then(|| found.into_iter().next())?
}

pub struct Store {
    root: PathBuf,
    conn: Mutex<Connection>,
    eml: EmlStore,
}

const FOLDER_COLS: &str = "id, server_id, server_parent_id, parent_id, name, folder_class, \
     well_known, is_mail, archived_at, sync_state, sort_order";

const MESSAGE_COLS: &str = "id, folder_id, server_item_id, change_key, server_state, eml_hash, \
     archived_at, internet_msg_id, conversation_id, subject, from_name, from_addr, to_list, \
     cc_list, date_received, date_sent, size, has_attachments, is_read, flag, importance, \
     is_draft, item_class, preview, origin, pinned";

const ROW_COLS: &str = "id, folder_id, subject, from_name, from_addr, to_list, date_received, \
     size, is_read, flag, has_attachments, importance, server_state, eml_hash IS NOT NULL, origin, \
     pinned";

/// 목록 정렬 기준.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SortKey {
    Date,
    From,
    To,
    Size,
    Subject,
    Importance,
    Flag,
    Attachments,
    Unread,
}

/// 제목 비교용: 앞의 "RE:"·"FW:"·"FWD:"·"회신:"·"전달:"·"답장:"(여러 번, 대괄호 없이)을 빼고 소문자로.
pub fn normalized_subject(s: &str) -> String {
    let mut t = s.trim();
    loop {
        let lower = t.to_lowercase();
        let cut = [
            "re:", "fw:", "fwd:", "회신:", "전달:", "답장:", "re :", "fw :",
        ]
        .iter()
        .find(|p| lower.starts_with(*p))
        .map(|p| p.len());
        match cut {
            Some(n) => t = t[n..].trim_start(),
            None => return lower,
        }
    }
}

fn row_lite(r: &Row<'_>) -> rusqlite::Result<RowLite> {
    Ok(RowLite {
        id: r.get(0)?,
        folder_id: r.get(1)?,
        subject: r.get(2)?,
        from_name: r.get(3)?,
        from_addr: r.get(4)?,
        to_list: r.get(5)?,
        date_received: r.get(6)?,
        size: r.get(7)?,
        is_read: r.get(8)?,
        flag: r.get(9)?,
        has_attachments: r.get(10)?,
        importance: r.get(11)?,
        archived: r.get::<_, i64>(12)? == server_state::ARCHIVED,
        has_eml: r.get(13)?,
        origin: r.get(14)?,
        pinned: r.get(15)?,
    })
}

fn folder_row(r: &Row<'_>) -> rusqlite::Result<FolderRow> {
    Ok(FolderRow {
        id: r.get(0)?,
        server_id: r.get(1)?,
        server_parent_id: r.get(2)?,
        parent_id: r.get(3)?,
        name: r.get(4)?,
        folder_class: r.get(5)?,
        well_known: r.get(6)?,
        is_mail: r.get(7)?,
        archived_at: r.get(8)?,
        sync_state: r.get(9)?,
        sort_order: r.get(10)?,
    })
}

fn message_row(r: &Row<'_>) -> rusqlite::Result<MessageRow> {
    Ok(MessageRow {
        id: r.get(0)?,
        folder_id: r.get(1)?,
        server_item_id: r.get(2)?,
        change_key: r.get(3)?,
        server_state: r.get(4)?,
        eml_hash: r.get(5)?,
        archived_at: r.get(6)?,
        props: MessageProps {
            internet_msg_id: r.get(7)?,
            conversation_id: r.get(8)?,
            subject: r.get(9)?,
            from_name: r.get(10)?,
            from_addr: r.get(11)?,
            to_list: r.get(12)?,
            cc_list: r.get(13)?,
            date_received: r.get(14)?,
            date_sent: r.get(15)?,
            size: r.get(16)?,
            has_attachments: r.get(17)?,
            is_read: r.get(18)?,
            flag: r.get(19)?,
            importance: r.get(20)?,
            is_draft: r.get(21)?,
            item_class: r.get(22)?,
            preview: r.get(23)?,
            pinned: Some(r.get(25)?),
        },
        origin: r.get(24)?,
    })
}

impl Store {
    /// `root` 아래 `db.sqlite`와 `eml\`을 연다(없으면 만들고 마이그레이션한다).
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        std::fs::create_dir_all(&root)?;
        let mut conn = Connection::open(root.join("db.sqlite"))?;
        schema::configure(&conn)?;
        schema::migrate(&mut conn)?;
        Ok(Self {
            eml: EmlStore::new(root.join("eml")),
            root,
            conn: Mutex::new(conn),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn eml(&self) -> &EmlStore {
        &self.eml
    }

    fn conn(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 한 트랜잭션으로 묶어 실행한다. 클로저가 오류면 되돌린다.
    pub fn transaction<R>(&self, f: impl FnOnce(&Tx<'_>) -> Result<R>) -> Result<R> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let out = f(&Tx { tx: &tx })?;
        tx.commit()?;
        Ok(out)
    }

    pub fn meta(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn()
            .query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
            .optional()?)
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        self.transaction(|tx| tx.set_meta(key, value))
    }

    pub fn folders(&self) -> Result<Vec<FolderRow>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {FOLDER_COLS} FROM folders ORDER BY sort_order, name"
        ))?;
        let rows = stmt.query_map([], folder_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// 항목 동기화 대상: 서버에 있는 메일 폴더.
    pub fn syncable_folders(&self) -> Result<Vec<FolderRow>> {
        Ok(self
            .folders()?
            .into_iter()
            .filter(|f| f.is_mail && f.server_id.is_some() && f.archived_at.is_none())
            .collect())
    }

    pub fn message(&self, id: i64) -> Result<Option<MessageRow>> {
        Ok(self
            .conn()
            .query_row(
                &format!("SELECT {MESSAGE_COLS} FROM messages WHERE id = ?1"),
                [id],
                message_row,
            )
            .optional()?)
    }

    pub fn messages_in_folder(&self, folder_id: i64) -> Result<Vec<MessageRow>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {MESSAGE_COLS} FROM messages WHERE folder_id = ?1 ORDER BY date_received DESC"
        ))?;
        let rows = stmt.query_map([folder_id], message_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// 원문을 아직 받지 않은 서버 메일, 최근 메일부터(계획서 7장 원문 큐).
    /// 폴더의 메일 수(목록 전체 길이).
    pub fn folder_total(&self, folder_id: i64) -> Result<i64> {
        Ok(self.conn().query_row(
            "SELECT COUNT(*) FROM messages WHERE folder_id = ?1",
            [folder_id],
            |r| r.get(0),
        )?)
    }

    /// 목록 창: 폴더의 `offset`부터 `limit`행, 고정한 메일 먼저 최신순(계획서 4.1-5: 200행 단위).
    pub fn list_rows(&self, folder_id: i64, offset: usize, limit: usize) -> Result<Vec<RowLite>> {
        let conn = self.conn();
        // 지연 조인: 건너뛸 행은 색인(folder_id, pinned, date_received)만 훑고, 보여 줄 행만 본표에서 읽는다.
        let mut stmt = conn.prepare_cached(&format!(
            "SELECT {ROW_COLS} FROM ( \
               SELECT id AS pid, pinned AS pn, date_received AS d FROM messages WHERE folder_id = ?1 \
               ORDER BY pinned DESC, date_received DESC, id LIMIT ?2 OFFSET ?3 \
             ) p JOIN messages ON messages.id = p.pid ORDER BY p.pn DESC, p.d DESC, p.pid"
        ))?;
        let rows = stmt.query_map(
            params![
                folder_id,
                i64::try_from(limit).unwrap_or(i64::MAX),
                i64::try_from(offset).unwrap_or(0)
            ],
            row_lite,
        )?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// 폴더 메일 ID를 `key` 순서로 모두(목록 정렬, 사용자 요청 2026-10-21). 같은 값끼리는 최신순.
    /// `reverse`면 기준의 기본 방향을 뒤집는다(기본: 날짜·크기·중요도·플래그·첨부는 큰 것부터,
    /// 보낸 사람·받는 사람·제목은 가나다순, 안읽음은 안읽음 먼저). 제목은 "RE:"·"회신:" 같은 머리말을 빼고 비교한다.
    pub fn sorted_ids(&self, folder_id: i64, key: SortKey, reverse: bool) -> Result<Vec<i64>> {
        self.sorted_ids_in(&[folder_id], key, reverse)
    }

    /// [`Self::sorted_ids`]를 여러 폴더에 걸쳐(통합 편지함, 사용자 요청 2026-10-21).
    pub fn sorted_ids_in(
        &self,
        folder_ids: &[i64],
        key: SortKey,
        reverse: bool,
    ) -> Result<Vec<i64>> {
        if folder_ids.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.conn();
        let within = format!(
            "folder_id IN ({})",
            folder_ids
                .iter()
                .map(i64::to_string)
                .collect::<Vec<_>>()
                .join(",")
        );
        // 고정한 메일은 어느 정렬에서나 맨 위(1.1.0), 고정끼리·나머지끼리는 고른 기준으로.
        if key == SortKey::Subject {
            let mut stmt = conn.prepare(&format!(
                "SELECT id, COALESCE(subject, ''), date_received, pinned FROM messages WHERE {within}"
            ))?;
            let mut rows: Vec<(i64, String, i64, bool)> = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
                .collect::<rusqlite::Result<_>>()?;
            for r in &mut rows {
                r.1 = normalized_subject(&r.1);
            }
            rows.sort_by(|a, b| {
                let k = a.1.cmp(&b.1);
                let k = if reverse { k.reverse() } else { k };
                b.3.cmp(&a.3)
                    .then(k)
                    .then(b.2.cmp(&a.2))
                    .then(b.0.cmp(&a.0))
            });
            return Ok(rows.into_iter().map(|r| r.0).collect());
        }
        let (expr, desc) = match key {
            SortKey::Date => ("date_received", true),
            SortKey::From => (
                "lower(COALESCE(NULLIF(trim(from_name), ''), from_addr, ''))",
                false,
            ),
            SortKey::To => ("lower(COALESCE(to_list, ''))", false),
            SortKey::Size => ("COALESCE(size, 0)", true),
            SortKey::Importance => ("importance", true),
            // 플래그 → 완료 → 없음.
            SortKey::Flag => ("CASE flag WHEN 1 THEN 2 WHEN 2 THEN 1 ELSE 0 END", true),
            SortKey::Attachments => ("has_attachments", true),
            SortKey::Unread => ("is_read", false),
            SortKey::Subject => unreachable!(),
        };
        let dir = if desc != reverse { "DESC" } else { "ASC" };
        let mut stmt = conn.prepare(&format!(
            "SELECT id FROM messages WHERE {within} \
             ORDER BY pinned DESC, {expr} {dir}, date_received DESC, id DESC"
        ))?;
        let ids = stmt
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(ids)
    }

    /// 주어진 ID 순서대로 목록 행(검색 결과용). 없는 ID는 건너뛴다.
    pub fn rows_by_ids(&self, ids: &[i64]) -> Result<Vec<RowLite>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.conn();
        let placeholders = vec!["?"; ids.len()].join(",");
        let mut stmt = conn.prepare(&format!(
            "SELECT {ROW_COLS} FROM messages WHERE id IN ({placeholders})"
        ))?;
        let rows = stmt.query_map(rusqlite::params_from_iter(ids), row_lite)?;
        let mut by_id: std::collections::HashMap<i64, RowLite> = rows
            .map(|r| r.map(|row| (row.id, row)))
            .collect::<rusqlite::Result<_>>()?;
        Ok(ids.iter().filter_map(|id| by_id.remove(id)).collect())
    }

    /// 이 보낸 사람의 외부 이미지를 늘 보여 주는가(계획서 9.2). 보낸 사람 주소, 또는 그 도메인(하위 도메인 포함)을
    /// 허용했으면 true.
    pub fn remote_images_allowed(&self, sender: &str) -> Result<bool> {
        let sender = sender.trim().to_lowercase();
        if self.meta(&format!("{REMOTE_SENDER}{sender}"))?.is_some() {
            return Ok(true);
        }
        // mail.example.com → mail.example.com, example.com(점이 하나 이상 남는 것까지).
        let Some((_, mut domain)) = sender.rsplit_once('@') else {
            return Ok(false);
        };
        while domain.contains('.') {
            if self.meta(&format!("{REMOTE_DOMAIN}{domain}"))?.is_some() {
                return Ok(true);
            }
            domain = domain.split_once('.').map_or("", |x| x.1);
        }
        Ok(false)
    }

    /// 이 보낸 사람 주소의 외부 이미지를 늘 보여 준다.
    pub fn allow_remote_images(&self, sender: &str) -> Result<()> {
        self.set_meta(
            &format!("{REMOTE_SENDER}{}", sender.trim().to_lowercase()),
            "1",
        )
    }

    /// 이 도메인(하위 도메인 포함)에서 온 메일의 외부 이미지를 늘 보여 준다. `@`는 떼고 저장한다.
    pub fn allow_remote_domain(&self, domain: &str) -> Result<()> {
        let d = domain.trim().trim_start_matches('@').to_lowercase();
        self.set_meta(&format!("{REMOTE_DOMAIN}{d}"), "1")
    }

    /// 외부 이미지 허용 목록: (보낸 사람 주소들, 도메인들), 가나다순.
    pub fn remote_image_allowlist(&self) -> Result<(Vec<String>, Vec<String>)> {
        let conn = self.conn();
        let mut stmt =
            conn.prepare("SELECT key FROM meta WHERE key LIKE 'remote%' ORDER BY key")?;
        let keys: Vec<String> = stmt
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let pick = |prefix: &str| {
            keys.iter()
                .filter_map(|k| k.strip_prefix(prefix).map(str::to_owned))
                .collect()
        };
        Ok((pick(REMOTE_SENDER), pick(REMOTE_DOMAIN)))
    }

    /// 허용 목록에서 하나 뺀다(`domain`이면 도메인, 아니면 보낸 사람 주소). 뺐으면 true.
    pub fn remove_remote_allow(&self, domain: bool, value: &str) -> Result<bool> {
        let v = value
            .trim()
            .trim_start_matches(if domain { "@" } else { "" })
            .to_lowercase();
        let key = format!("{}{v}", if domain { REMOTE_DOMAIN } else { REMOTE_SENDER });
        Ok(self
            .conn()
            .execute("DELETE FROM meta WHERE key = ?1", [key])?
            > 0)
    }

    /// 허용한 보낸 사람·도메인 수(설정 화면).
    pub fn remote_image_senders(&self) -> Result<i64> {
        let (s, d) = self.remote_image_allowlist()?;
        Ok((s.len() + d.len()) as i64)
    }

    /// 허용 목록을 모두 지운다. 지운 수.
    pub fn forget_remote_images(&self) -> Result<usize> {
        let conn = self.conn();
        let n = conn.execute(
            "DELETE FROM meta WHERE substr(key, 1, ?1) = ?2 OR substr(key, 1, ?3) = ?4",
            params![
                REMOTE_SENDER.len() as i64,
                REMOTE_SENDER,
                REMOTE_DOMAIN.len() as i64,
                REMOTE_DOMAIN
            ],
        )?;
        Ok(n)
    }

    /// 아직 색인하지 않은 메일, 최근 것부터(계획서 7장 색인 큐). 원문이 없는 메일도 넣는다(0.7.0: 목록
    /// 속성만으로 색인해 제목·보낸 사람으로는 찾히게, 원문이 오면 다시 색인).
    pub fn index_todo(&self, limit: usize) -> Result<Vec<IndexTodo>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, folder_id, date_received, eml_hash, subject, from_name, from_addr, to_list, cc_list \
             FROM messages WHERE indexed = 0 \
             ORDER BY date_received DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map([i64::try_from(limit).unwrap_or(i64::MAX)], |r| {
            Ok(IndexTodo {
                message_id: r.get(0)?,
                folder_id: r.get(1)?,
                date_received: r.get(2)?,
                eml_hash: r.get(3)?,
                subject: r.get(4)?,
                from_name: r.get(5)?,
                from_addr: r.get(6)?,
                to_list: r.get(7)?,
                cc_list: r.get(8)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// 색인 완료 표시. 원문에서 얻은 주소 목록이 있으면 목록 행의 받는 사람·참조·숨은 참조도 채운다.
    /// `full`이 아니면(원문 없이 목록 속성만 색인) 2로 적는다: 원문이 오면 다시 색인한다.
    pub fn mark_indexed(&self, done: &[(i64, Option<AddressLists>, bool)]) -> Result<()> {
        self.transaction(|tx| {
            for (id, lists, full) in done {
                let state = if *full { 1 } else { 2 };
                match lists {
                    Some(l) => tx.tx.execute(
                        "UPDATE messages SET indexed = ?1, to_list = ?2, cc_list = ?3, bcc_list = ?4 \
                         WHERE id = ?5",
                        params![state, l.to, l.cc, l.bcc, id],
                    )?,
                    None => tx.tx.execute(
                        "UPDATE messages SET indexed = ?1 WHERE id = ?2",
                        params![state, id],
                    )?,
                };
            }
            Ok(())
        })
    }

    /// 색인 완료로 표시된 메일 ID(색인과 대조용).
    pub fn indexed_ids(&self) -> Result<Vec<i64>> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT id FROM messages WHERE indexed != 0")?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// 모든 메일을 다시 색인하게 표시한다(색인 형식이 바뀌었을 때).
    pub fn mark_all_unindexed(&self) -> Result<usize> {
        Ok(self.conn().execute("UPDATE messages SET indexed = 0", [])?)
    }

    /// 모든 메일 ID(색인 대조용: 색인에만 있고 DB에 없는 문서 찾기).
    pub fn message_ids(&self) -> Result<Vec<i64>> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT id FROM messages")?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// 다시 색인하게 표시한다.
    pub fn mark_unindexed(&self, ids: &[i64]) -> Result<()> {
        self.transaction(|tx| {
            for id in ids {
                tx.tx
                    .execute("UPDATE messages SET indexed = 0 WHERE id = ?1", [id])?;
            }
            Ok(())
        })
    }

    pub fn unindexed_count(&self) -> Result<i64> {
        Ok(self
            .conn()
            .query_row("SELECT COUNT(*) FROM messages WHERE indexed = 0", [], |r| {
                r.get(0)
            })?)
    }

    pub fn eml_todo(&self, limit: usize) -> Result<Vec<EmlTodo>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, server_item_id, date_received FROM messages \
             WHERE eml_hash IS NULL AND server_item_id IS NOT NULL \
             ORDER BY date_received DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map([i64::try_from(limit).unwrap_or(i64::MAX)], |r| {
            Ok(EmlTodo {
                message_id: r.get(0)?,
                server_item_id: r.get(1)?,
                date_received: r.get(2)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// 원문을 저장하고 메일에 연결한다. 그사이 메일이 다른 항목 ID로 바뀌었으면(이동 등) 연결하지 않는다.
    pub fn attach_eml(&self, message_id: i64, server_item_id: &str, raw: &[u8]) -> Result<bool> {
        let hash = self.eml.put(raw)?;
        // 원문 없이 목록 속성만 색인했던 메일은 본문까지 다시 색인한다.
        let n = self.conn().execute(
            "UPDATE messages SET eml_hash = ?1, indexed = 0 \
             WHERE id = ?2 AND server_item_id = ?3 AND eml_hash IS NULL",
            params![hash, message_id, server_item_id],
        )?;
        Ok(n == 1)
    }

    pub fn folder_counts(&self) -> Result<Vec<FolderCounts>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT folder_id, COUNT(*), \
               SUM(CASE WHEN is_read = 0 THEN 1 ELSE 0 END), \
               SUM(CASE WHEN server_state = 1 THEN 1 ELSE 0 END), \
               SUM(CASE WHEN eml_hash IS NULL THEN 1 ELSE 0 END) \
             FROM messages GROUP BY folder_id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(FolderCounts {
                folder_id: r.get(0)?,
                total: r.get(1)?,
                unread: r.get(2)?,
                archived: r.get(3)?,
                missing_eml: r.get(4)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// 메일 행을 지우고, 그 원문을 쓰는 행이 없으면 파일도 지운다(완전 삭제용).
    pub fn purge_message(&self, id: i64) -> Result<()> {
        let hash: Option<String> = self.transaction(|tx| {
            let hash: Option<String> = tx
                .tx
                .query_row("SELECT eml_hash FROM messages WHERE id = ?1", [id], |r| {
                    r.get(0)
                })
                .optional()?
                .flatten();
            tx.tx.execute("DELETE FROM messages WHERE id = ?1", [id])?;
            let still_used = match &hash {
                Some(h) => tx.tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM messages WHERE eml_hash = ?1)",
                    [h],
                    |r| r.get::<_, bool>(0),
                )?,
                None => true,
            };
            Ok(if still_used { None } else { hash })
        })?;
        if let Some(h) = hash {
            self.eml.remove(&h)?;
        }
        Ok(())
    }
}

/// 트랜잭션 안의 쓰기 작업. [`Store::transaction`]으로 얻는다.
pub struct Tx<'a> {
    tx: &'a rusqlite::Transaction<'a>,
}

impl Tx<'_> {
    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        self.tx.execute(
            "INSERT INTO meta(key, value) VALUES (?1, ?2) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn folder_by_server_id(&self, server_id: &str) -> Result<Option<FolderRow>> {
        Ok(self
            .tx
            .query_row(
                &format!("SELECT {FOLDER_COLS} FROM folders WHERE server_id = ?1"),
                [server_id],
                folder_row,
            )
            .optional()?)
    }

    /// 서버 폴더를 넣거나 갱신한다(보관 표시는 지운다). 로컬 폴더 ID.
    pub fn upsert_folder(&self, f: &ServerFolder) -> Result<i64> {
        if let Some(existing) = self.folder_by_server_id(&f.server_id)? {
            self.tx.execute(
                "UPDATE folders SET server_parent_id = ?1, name = ?2, folder_class = ?3, \
                 archived_at = NULL WHERE id = ?4",
                params![f.server_parent_id, f.name, f.folder_class, existing.id],
            )?;
            return Ok(existing.id);
        }
        self.tx.execute(
            "INSERT INTO folders(server_id, server_parent_id, name, folder_class) \
             VALUES (?1, ?2, ?3, ?4)",
            params![f.server_id, f.server_parent_id, f.name, f.folder_class],
        )?;
        Ok(self.tx.last_insert_rowid())
    }

    /// 서버에서 사라진 폴더: 로컬 보관 폴더로 남긴다(메일은 그대로).
    pub fn archive_folder(&self, server_id: &str, now_ms: i64) -> Result<Option<i64>> {
        let Some(f) = self.folder_by_server_id(server_id)? else {
            return Ok(None);
        };
        self.tx.execute(
            "UPDATE folders SET server_id = NULL, sync_state = NULL, archived_at = ?1 WHERE id = ?2",
            params![now_ms, f.id],
        )?;
        Ok(Some(f.id))
    }

    pub fn set_well_known(&self, server_id: &str, name: &str, sort_order: i64) -> Result<()> {
        self.tx.execute(
            "UPDATE folders SET well_known = ?1, sort_order = ?2 WHERE server_id = ?3",
            params![name, sort_order, server_id],
        )?;
        Ok(())
    }

    /// `server_parent_id`로 `parent_id`를 잇고, 루트 아래 메일 폴더 트리(`is_mail`)를 다시 계산한다.
    /// 메일이 아닌 폴더(`IPF.Contact` 등)와 그 하위는 메일 폴더가 아니다(계획서 5.3).
    pub fn rebuild_tree(&self, root_server_id: &str) -> Result<()> {
        struct Node {
            id: i64,
            server_id: Option<String>,
            server_parent: Option<String>,
            class: Option<String>,
        }
        let nodes: Vec<Node> = {
            let mut stmt = self
                .tx
                .prepare("SELECT id, server_id, server_parent_id, folder_class FROM folders")?;
            let mapped = stmt.query_map([], |r| {
                Ok(Node {
                    id: r.get(0)?,
                    server_id: r.get(1)?,
                    server_parent: r.get(2)?,
                    class: r.get(3)?,
                })
            })?;
            mapped.collect::<rusqlite::Result<_>>()?
        };
        let by_server: std::collections::HashMap<&str, &Node> = nodes
            .iter()
            .filter_map(|n| n.server_id.as_deref().map(|s| (s, n)))
            .collect();
        let class_is_mail = |c: &Option<String>| {
            let c = c.as_deref().unwrap_or("").to_ascii_lowercase();
            c.is_empty() || c == "ipf" || c == "ipf.note" || c.starts_with("ipf.note.")
        };
        for n in &nodes {
            if n.server_id.is_none() {
                continue; // 보관 폴더는 마지막 위치를 유지한다.
            }
            let parent_local = n
                .server_parent
                .as_deref()
                .filter(|p| *p != root_server_id)
                .and_then(|p| by_server.get(p))
                .map(|p| p.id);
            // 위로 올라가며 모두 메일 폴더이고 루트에 닿아야 메일 트리다.
            let mut is_mail = class_is_mail(&n.class);
            let mut cur = n.server_parent.as_deref();
            let mut guard = 0;
            while is_mail {
                match cur {
                    Some(p) if p == root_server_id => break,
                    Some(p) => match by_server.get(p) {
                        Some(parent) => {
                            is_mail = class_is_mail(&parent.class);
                            cur = parent.server_parent.as_deref();
                        }
                        None => is_mail = false,
                    },
                    None => is_mail = false,
                }
                guard += 1;
                if guard > 64 {
                    is_mail = false;
                }
            }
            self.tx.execute(
                "UPDATE folders SET parent_id = ?1, is_mail = ?2 WHERE id = ?3",
                params![parent_local, is_mail, n.id],
            )?;
        }
        Ok(())
    }

    pub fn set_folder_sync_state(&self, folder_id: i64, state: Option<&str>) -> Result<()> {
        self.tx.execute(
            "UPDATE folders SET sync_state = ?1 WHERE id = ?2",
            params![state, folder_id],
        )?;
        Ok(())
    }

    pub fn message_by_server_id(&self, server_item_id: &str) -> Result<Option<MessageRow>> {
        Ok(self
            .tx
            .query_row(
                &format!("SELECT {MESSAGE_COLS} FROM messages WHERE server_item_id = ?1"),
                [server_item_id],
                message_row,
            )
            .optional()?)
    }

    pub fn insert_message(
        &self,
        folder_id: i64,
        server_item_id: &str,
        change_key: Option<&str>,
        p: &MessageProps,
    ) -> Result<i64> {
        self.tx.execute(
            &format!(
                "INSERT INTO messages(id, folder_id, server_item_id, change_key, server_state, \
                   internet_msg_id, conversation_id, subject, from_name, from_addr, to_list, cc_list, \
                   date_received, date_sent, size, has_attachments, is_read, flag, importance, \
                   is_draft, item_class, preview, pinned) \
                 VALUES ({}, ?1, ?2, ?3, 0, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, \
                   ?17, ?18, ?19, ?20, ?21)",
                schema::NEXT_MESSAGE_ID
            ),
            params![
                folder_id,
                server_item_id,
                change_key,
                p.internet_msg_id,
                p.conversation_id,
                p.subject,
                p.from_name,
                p.from_addr,
                p.to_list,
                p.cc_list,
                p.date_received,
                p.date_sent,
                p.size,
                p.has_attachments,
                p.is_read,
                p.flag,
                p.importance,
                p.is_draft,
                p.item_class,
                p.preview,
                p.pinned.unwrap_or(false)
            ],
        )?;
        Ok(self.tx.last_insert_rowid())
    }

    /// 서버 속성 갱신. `relocate`면 폴더·항목 ID도 바꾼다(이동 짝짓기, 원문은 그대로).
    pub fn update_message(
        &self,
        id: i64,
        relocate: Option<(i64, &str)>,
        change_key: Option<&str>,
        p: &MessageProps,
    ) -> Result<()> {
        if let Some((folder_id, server_item_id)) = relocate {
            self.tx.execute(
                // 폴더가 바뀌었으니 색인의 폴더 값도 다시 넣어야 한다.
                "UPDATE messages SET folder_id = ?1, server_item_id = ?2, server_state = 0, \
                 archived_at = NULL, indexed = 0 WHERE id = ?3",
                params![folder_id, server_item_id, id],
            )?;
        }
        self.tx.execute(
            "UPDATE messages SET change_key = ?1, internet_msg_id = ?2, conversation_id = ?3, \
               subject = ?4, from_name = ?5, from_addr = ?6, to_list = ?7, cc_list = ?8, \
               date_received = ?9, date_sent = ?10, size = ?11, has_attachments = ?12, \
               is_read = ?13, flag = ?14, importance = ?15, is_draft = ?16, item_class = ?17, \
               preview = ?18, pinned = COALESCE(?20, pinned) \
             WHERE id = ?19",
            params![
                change_key,
                p.internet_msg_id,
                p.conversation_id,
                p.subject,
                p.from_name,
                p.from_addr,
                p.to_list,
                p.cc_list,
                p.date_received,
                p.date_sent,
                p.size,
                p.has_attachments,
                p.is_read,
                p.flag,
                p.importance,
                p.is_draft,
                p.item_class,
                p.preview,
                id,
                p.pinned
            ],
        )?;
        Ok(())
    }

    pub fn set_read(&self, id: i64, is_read: bool) -> Result<()> {
        self.tx.execute(
            "UPDATE messages SET is_read = ?1 WHERE id = ?2",
            params![is_read, id],
        )?;
        Ok(())
    }

    /// 서버에서 지워진 메일: 원래 폴더에 그대로 두고 "보관"으로 표시한다(계획서 6.3-3).
    pub fn archive_message(&self, id: i64, now_ms: i64) -> Result<()> {
        self.tx.execute(
            "UPDATE messages SET server_state = 1, server_item_id = NULL, archived_at = ?1 \
             WHERE id = ?2",
            params![now_ms, id],
        )?;
        Ok(())
    }

    /// 같은 Message-ID와 받은 시각을 가진, 아직 서버에 있는 메일(이동 짝짓기 후보).
    pub fn messages_by_imid(&self, internet_msg_id: &str) -> Result<Vec<MessageRow>> {
        let mut stmt = self.tx.prepare(&format!(
            "SELECT {MESSAGE_COLS} FROM messages WHERE internet_msg_id = ?1"
        ))?;
        let rows = stmt.query_map([internet_msg_id], message_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn props(imid: &str, received: i64) -> MessageProps {
        MessageProps {
            internet_msg_id: Some(imid.into()),
            subject: Some("제목".into()),
            date_received: received,
            importance: 1,
            ..MessageProps::default()
        }
    }

    fn folder(server_id: &str, parent: &str, class: Option<&str>) -> ServerFolder {
        ServerFolder {
            server_id: server_id.into(),
            server_parent_id: Some(parent.into()),
            name: server_id.into(),
            folder_class: class.map(Into::into),
        }
    }

    #[test]
    fn migrations_are_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        drop(Store::open(dir.path()).unwrap());
        let s = Store::open(dir.path()).unwrap();
        s.set_meta("k", "v").unwrap();
        s.set_meta("k", "w").unwrap();
        assert_eq!(s.meta("k").unwrap().as_deref(), Some("w"));
        let version: i64 = s
            .conn()
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(version, schema::MIGRATIONS.len() as i64);
    }

    #[test]
    fn sorted_ids_by_each_key() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        let mk = |imid: &str,
                  at: i64,
                  subject: &str,
                  from: &str,
                  to: &str,
                  size: i64,
                  read: bool,
                  flag: i64,
                  att: bool,
                  imp: i64| MessageProps {
            internet_msg_id: Some(imid.into()),
            subject: Some(subject.into()),
            from_name: Some(from.into()),
            to_list: Some(to.into()),
            date_received: at,
            size: Some(size),
            is_read: read,
            flag,
            has_attachments: att,
            importance: imp,
            ..MessageProps::default()
        };
        let (f, a, b, c) = s
            .transaction(|tx| {
                let f = tx.upsert_folder(&folder("INBOX", "ROOT", Some("IPF.Note")))?;
                let a = tx.insert_message(
                    f,
                    "A",
                    None,
                    &mk(
                        "<a@x>",
                        3_000,
                        "RE: 나 보고",
                        "김",
                        "z",
                        10,
                        true,
                        0,
                        false,
                        1,
                    ),
                )?;
                let b = tx.insert_message(
                    f,
                    "B",
                    None,
                    &mk("<b@x>", 2_000, "가 회의", "박", "a", 30, false, 1, true, 2),
                )?;
                let c = tx.insert_message(
                    f,
                    "C",
                    None,
                    &mk(
                        "<c@x>",
                        1_000,
                        "회신: 다 일정",
                        "이",
                        "m",
                        20,
                        true,
                        2,
                        false,
                        0,
                    ),
                )?;
                Ok((f, a, b, c))
            })
            .unwrap();
        let ids = |k, r| s.sorted_ids(f, k, r).unwrap();
        assert_eq!(ids(SortKey::Date, false), [a, b, c], "최신순");
        assert_eq!(ids(SortKey::Date, true), [c, b, a]);
        assert_eq!(ids(SortKey::From, false), [a, b, c], "김·박·이");
        assert_eq!(ids(SortKey::To, false), [b, c, a]);
        assert_eq!(ids(SortKey::Size, false), [b, c, a], "큰 것부터");
        assert_eq!(ids(SortKey::Size, true), [a, c, b]);
        assert_eq!(
            ids(SortKey::Subject, false),
            [b, a, c],
            "머리말 빼고 가·나·다"
        );
        assert_eq!(ids(SortKey::Importance, false), [b, a, c]);
        assert_eq!(ids(SortKey::Flag, false), [b, c, a], "플래그·완료·없음");
        assert_eq!(ids(SortKey::Attachments, false), [b, a, c]);
        assert_eq!(
            ids(SortKey::Unread, false),
            [b, a, c],
            "안읽음 먼저, 같으면 최신순"
        );
        assert_eq!(normalized_subject("RE: FW: 회신: Hello"), "hello");
    }

    #[test]
    fn folder_tree_excludes_non_mail_subtrees() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        s.transaction(|tx| {
            tx.upsert_folder(&folder("INBOX", "ROOT", Some("IPF.Note")))?;
            tx.upsert_folder(&folder("SUB", "INBOX", None))?;
            tx.upsert_folder(&folder("CONTACTS", "ROOT", Some("IPF.Contact")))?;
            tx.upsert_folder(&folder("UNDER_CONTACTS", "CONTACTS", Some("IPF.Note")))?;
            tx.rebuild_tree("ROOT")
        })
        .unwrap();
        let by_server = |sid: &str| {
            s.folders()
                .unwrap()
                .into_iter()
                .find(|f| f.server_id.as_deref() == Some(sid))
                .unwrap()
        };
        assert!(by_server("INBOX").is_mail && by_server("INBOX").parent_id.is_none());
        assert!(by_server("SUB").is_mail);
        assert_eq!(by_server("SUB").parent_id, Some(by_server("INBOX").id));
        assert!(!by_server("CONTACTS").is_mail);
        assert!(!by_server("UNDER_CONTACTS").is_mail);
        assert_eq!(s.syncable_folders().unwrap().len(), 2);
    }

    #[test]
    fn eml_queue_attach_and_archive() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        let (old, new) = s
            .transaction(|tx| {
                let f = tx.upsert_folder(&folder("INBOX", "ROOT", Some("IPF.Note")))?;
                Ok((
                    tx.insert_message(f, "S1", Some("C1"), &props("<a@x>", 1_000))?,
                    tx.insert_message(f, "S2", None, &props("<b@x>", 2_000))?,
                ))
            })
            .unwrap();
        let todo = s.eml_todo(10).unwrap();
        assert_eq!(
            todo.iter().map(|t| t.message_id).collect::<Vec<_>>(),
            [new, old]
        );

        assert!(s.attach_eml(new, "S2", b"raw").unwrap());
        assert!(
            !s.attach_eml(old, "S-other", b"raw2").unwrap(),
            "항목 ID가 다르면 연결하지 않는다"
        );
        assert_eq!(s.eml_todo(10).unwrap().len(), 1);

        s.transaction(|tx| tx.archive_message(new, 5)).unwrap();
        let m = s.message(new).unwrap().unwrap();
        assert_eq!(
            (m.server_state, m.server_item_id, m.archived_at),
            (1, None, Some(5))
        );
        assert!(m.eml_hash.is_some(), "보관해도 원문은 남는다");

        let counts = s.folder_counts().unwrap();
        assert_eq!(
            (counts[0].total, counts[0].archived, counts[0].missing_eml),
            (2, 1, 1)
        );

        let hash = m.eml_hash.unwrap();
        s.purge_message(new).unwrap();
        assert!(!s.eml().contains(&hash));
    }

    #[test]
    fn list_window_and_rows_by_ids() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        let (f, ids) = s
            .transaction(|tx| {
                let f = tx.upsert_folder(&folder("INBOX", "ROOT", Some("IPF.Note")))?;
                let mut ids = Vec::new();
                for i in 0..10 {
                    // 시각이 같은 행(5, 6)도 둔다.
                    let t = if i == 6 { 5_000 } else { i * 1_000 };
                    ids.push(tx.insert_message(f, &format!("S{i}"), None, &props("<x>", t))?);
                }
                tx.archive_message(ids[9], 1)?;
                Ok((f, ids))
            })
            .unwrap();
        assert_eq!(s.folder_total(f).unwrap(), 10);
        let all: Vec<i64> = s
            .list_rows(f, 0, 100)
            .unwrap()
            .iter()
            .map(|r| r.id)
            .collect();
        let expected = [9, 8, 7, 5, 6, 4, 3, 2, 1, 0].map(|i| ids[i]);
        assert_eq!(all, expected);
        let window: Vec<i64> = s.list_rows(f, 3, 4).unwrap().iter().map(|r| r.id).collect();
        assert_eq!(window, expected[3..7]);
        let first = &s.list_rows(f, 0, 1).unwrap()[0];
        assert!(first.archived && !first.has_eml);

        let picked = s.rows_by_ids(&[ids[2], 999, ids[7], ids[0]]).unwrap();
        assert_eq!(
            picked.iter().map(|r| r.id).collect::<Vec<_>>(),
            [ids[2], ids[7], ids[0]],
            "주어진 순서를 지키고 없는 ID는 건너뛴다"
        );

        assert!(!s.remote_images_allowed("A@X.com").unwrap());
        s.allow_remote_images("a@x.com").unwrap();
        assert!(s.remote_images_allowed(" A@X.com").unwrap());
        s.allow_remote_images("b@x.com").unwrap();
        assert_eq!(s.remote_image_senders().unwrap(), 2);
        // 도메인 허용: 하위 도메인도, 다른 도메인은 아님.
        assert!(!s.remote_images_allowed("news@mail.partner.co.kr").unwrap());
        s.allow_remote_domain("@Partner.co.kr").unwrap();
        assert!(s.remote_images_allowed("news@mail.partner.co.kr").unwrap());
        assert!(s.remote_images_allowed("a@partner.co.kr").unwrap());
        assert!(!s.remote_images_allowed("a@co.kr").unwrap());
        assert!(!s.remote_images_allowed("a@otherpartner.co.kr").unwrap());
        assert_eq!(
            s.remote_image_allowlist().unwrap(),
            (
                vec!["a@x.com".into(), "b@x.com".into()],
                vec!["partner.co.kr".into()]
            )
        );
        assert!(s.remove_remote_allow(false, "B@x.com").unwrap());
        assert!(s.remove_remote_allow(true, "@partner.co.kr").unwrap());
        assert!(
            !s.remove_remote_allow(true, "partner.co.kr").unwrap(),
            "이미 뺌"
        );
        assert!(!s.remote_images_allowed("a@partner.co.kr").unwrap());
        s.allow_remote_domain("y.com").unwrap();
        assert_eq!(s.forget_remote_images().unwrap(), 2);
        assert!(!s.remote_images_allowed("a@x.com").unwrap());
        assert_eq!(s.remote_image_senders().unwrap(), 0);
    }

    /// 고정(1.1.0): 어느 정렬에서나 맨 위, 동기화는 모르는 값(None)이면 그대로 두고, 대기 작업은 되돌릴 수 있다.
    #[test]
    fn pinned_mails_stay_on_top() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        let (f, old, new, newest) = s
            .transaction(|tx| {
                let f = tx.upsert_folder(&folder("INBOX", "ROOT", Some("IPF.Note")))?;
                let old = tx.insert_message(f, "A", None, &props("<a@x>", 1_000))?;
                let new = tx.insert_message(f, "B", None, &props("<b@x>", 2_000))?;
                let newest = tx.insert_message(
                    f,
                    "C",
                    None,
                    &MessageProps {
                        size: Some(1),
                        ..props("<c@x>", 3_000)
                    },
                )?;
                Ok((f, old, new, newest))
            })
            .unwrap();
        let ids = |rows: Vec<RowLite>| rows.into_iter().map(|r| r.id).collect::<Vec<_>>();
        assert_eq!(ids(s.list_rows(f, 0, 10).unwrap()), [newest, new, old]);
        let t = s.mark_pinned(&[old], true, 10).unwrap();
        assert_eq!(t.queued, 1, "서버 메일이면 서버에도 반영");
        assert_eq!(ids(s.list_rows(f, 0, 10).unwrap()), [old, newest, new]);
        assert_eq!(
            ids(s.list_rows(f, 1, 10).unwrap()),
            [newest, new],
            "페이지 나눔도 같은 순서"
        );
        assert_eq!(s.list_ids(Some(f)).unwrap(), [old, newest, new]);
        assert_eq!(
            s.sorted_ids(f, SortKey::Size, false).unwrap()[0],
            old,
            "다른 정렬에서도 맨 위"
        );
        assert_eq!(s.sorted_ids(f, SortKey::Subject, true).unwrap()[0], old);
        assert!(s.list_rows(f, 0, 1).unwrap()[0].pinned);
        // 동기화: 고정 값을 모르면(None) 그대로, 알면 그 값.
        s.transaction(|tx| tx.update_message(old, None, None, &props("<a@x>", 1_000)))
            .unwrap();
        assert_eq!(s.message(old).unwrap().unwrap().props.pinned, Some(true));
        let guard = s.pending_guard().unwrap();
        assert!(
            guard.keep_pin.contains(&old),
            "대기 중이면 동기화가 덮지 않게"
        );
        // 서버가 거부해 포기하면 되돌린다.
        let op = s
            .pending_ops()
            .unwrap()
            .into_iter()
            .find(|o| o.kind == OP_SET_PIN)
            .unwrap();
        s.rollback_op(&op).unwrap();
        assert_eq!(s.message(old).unwrap().unwrap().props.pinned, Some(false));
        // 서버에서 고정한 메일을 한 번에 표시(업그레이드 뒤).
        let t = s.pin_from_server(&["B".into(), "없음".into()]).unwrap();
        assert_eq!(t.folders.values().map(Vec::len).sum::<usize>(), 1);
        assert_eq!(ids(s.list_rows(f, 0, 10).unwrap()), [new, newest, old]);
        s.transaction(|tx| {
            tx.update_message(
                new,
                None,
                None,
                &MessageProps {
                    pinned: Some(false),
                    ..props("<b@x>", 2_000)
                },
            )
        })
        .unwrap();
        assert_eq!(
            ids(s.list_rows(f, 0, 10).unwrap()),
            [newest, new, old],
            "서버에서 풀림"
        );
    }

    /// 지운 메일·작성 중 메일 번호를 다시 쓰지 않는다(1.1.1 사용자 신고: 복사본을 완전 삭제한 직후 들어온 메일이
    /// 같은 번호를 받아 다른 메일 내용이 보였다).
    #[test]
    fn deleted_ids_are_never_reused() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        let (f, g, a, b) = s
            .transaction(|tx| {
                let f = tx.upsert_folder(&folder("INBOX", "ROOT", Some("IPF.Note")))?;
                let g = tx.upsert_folder(&folder("TEMPLATES", "ROOT", Some("IPF.Note")))?;
                let a = tx.insert_message(f, "A", None, &props("<a@x>", 1_000))?;
                let b = tx.insert_message(f, "B", None, &props("<b@x>", 2_000))?;
                Ok((f, g, a, b))
            })
            .unwrap();
        // 맨 끝 메일을 다른 폴더로 복사한 뒤 사본을 완전 삭제(사용자가 한 그대로).
        let t = s.copy_messages(&[b], g, 5, None).unwrap();
        let copy = *t.folders[&g].iter().max().unwrap();
        assert!(copy > b);
        s.purge_messages(&[copy], 6, None).unwrap();
        let c = s
            .transaction(|tx| tx.insert_message(f, "C", None, &props("<c@x>", 3_000)))
            .unwrap();
        assert!(
            c > copy,
            "지운 번호 {copy}를 다시 쓰면 안 된다(새 번호 {c})"
        );
        assert_eq!(
            s.message(c)
                .unwrap()
                .unwrap()
                .props
                .internet_msg_id
                .as_deref(),
            Some("<c@x>")
        );
        assert!(a < b);
        // 작성 중 메일도.
        let d1 = s
            .create_draft("new", None, &DraftContent::default(), 1)
            .unwrap();
        s.delete_draft(d1).unwrap();
        let d2 = s
            .create_draft("new", None, &DraftContent::default(), 2)
            .unwrap();
        assert!(d2 > d1, "작성 중 메일 번호 {d1}을 다시 쓰면 안 된다");
        // 다시 열어도(번호 기록은 DB에 있다).
        drop(s);
        let s = Store::open(dir.path()).unwrap();
        let e = s
            .transaction(|tx| tx.insert_message(f, "E", None, &props("<e@x>", 4_000)))
            .unwrap();
        assert!(e > c);
    }

    /// 전달에서 뺀 원본 첨부(1.1.0): 빼고 되돌리면 목록이 맞고, 서버에 다시 올릴 것으로 표시한다.
    #[test]
    fn excluded_originals_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        let id = s
            .create_draft("forward", None, &DraftContent::default(), 1)
            .unwrap();
        assert!(s.draft(id).unwrap().unwrap().excluded_orig.is_empty());
        let (a, b) = (orig_key(0, "a.pdf"), orig_key(1, "a.pdf"));
        s.set_orig_excluded(id, &a, true, 2).unwrap();
        s.set_orig_excluded(id, &b, true, 3).unwrap();
        s.set_orig_excluded(id, &a, false, 4).unwrap();
        let d = s.draft(id).unwrap().unwrap();
        assert_eq!(
            d.excluded_orig,
            std::slice::from_ref(&b),
            "이름이 같아도 순번으로 가른다"
        );
        assert!(d.server_dirty);
        s.set_orig_excluded(id, &b, false, 5).unwrap();
        assert!(s.draft(id).unwrap().unwrap().excluded_orig.is_empty());
    }
}
