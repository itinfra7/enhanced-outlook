//! 작성 중인 메일과 보낼 편지함(계획서 9.3), 로컬 주소록(5.9).
//!
//! - 작성 화면이 5초마다 로컬에 저장하고, 서버 임시 보관함에는 1분마다·닫을 때·Ctrl+S에 올린다
//!   (0.7.0, 사용자 요청: "작성 중"을 따로 두지 않고 웹메일에서도 보이게). 올리기는 작업 큐(`draft_save`).
//! - 임시 보관함에는 작성 중인 메일마다 메일 행이 하나 있다(`message_id`). 서버에 올리기 전에는 "서버 반영
//!   대기" 행이고, 올리면 서버 항목 ID가 붙어 동기화가 같은 행을 갱신한다.
//! - 보내기를 누르면 "보낼 편지함" 상태가 되고 발송 작업(`send`)을 쌓는다. 실패하면 "발송 실패"로 남아
//!   임시 보관함에서 다시 열 수 있다.
//! - 첨부 파일은 `EnhancedOutlook\drafts\{id}\`에 복사해 둔다(원본 파일이 지워져도 보낼 수 있게, 포터블 루트 안).
//! - 주소록은 보낸 메일의 받는 사람·참조(가중 3)와 받은 메일의 보낸 사람(가중 1)로 만든다.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::model::server_state;
use crate::ops::{OP_DRAFT_SAVE, OP_SEND};
use crate::{Error, Result, Store};

/// 작성 상태.
pub mod draft_state {
    pub const EDITING: i64 = 0;
    pub const OUTBOX: i64 = 1;
    pub const FAILED: i64 = 2;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recipient {
    pub name: String,
    pub email: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Draft {
    pub id: i64,
    /// new, reply, reply_all, forward
    pub kind: String,
    pub ref_message_id: Option<i64>,
    pub to: Vec<Recipient>,
    pub cc: Vec<Recipient>,
    pub bcc: Vec<Recipient>,
    pub subject: String,
    /// 새로 쓴 부분(인용문은 보낼 때 원문으로 다시 만든다).
    pub body_html: String,
    pub include_quote: bool,
    /// 작성 창에서 고친 인용문(작성 창 문서의 본문, 0.9.0). 없으면 보낼 때 원문으로 만든다.
    pub quote_html: Option<String>,
    pub state: i64,
    pub last_error: Option<String>,
    pub server_draft_id: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    /// 임시 보관함의 그 메일 행.
    pub message_id: Option<i64>,
    pub options: DraftOptions,
    /// 서버 임시 보관함에 올릴 바뀐 내용이 있다.
    pub server_dirty: bool,
    /// 보내기 요청을 서버에 보냈다(그 뒤 서버 초안이 없으면 이미 보낸 것).
    pub send_started: bool,
    /// 서버 초안을 만든 방식(ComposeOperation: newMail·reply·replyAll·forward).
    pub server_kind: Option<String>,
    /// 전달에서 뺀 원본 첨부("순번:이름", 1.1.0).
    pub excluded_orig: Vec<String>,
}

/// 전달할 원본 첨부의 키("순번:이름"). 이름이 같은 첨부가 있어도 순번으로 가른다.
pub fn orig_key(index: usize, name: &str) -> String {
    format!("{index}:{name}")
}

/// 웹메일 작성 화면의 "기타 작업"·"메시지 옵션"(0.7.0).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DraftOptions {
    /// 0 낮음, 1 보통, 2 높음.
    pub importance: i64,
    /// 우편물 종류: 0 보통, 1 개인, 2 비공개, 3 비밀 우편.
    pub sensitivity: i64,
    pub read_receipt: bool,
    pub delivery_receipt: bool,
    /// 일반 텍스트로 보낸다.
    pub plain_text: bool,
}

impl Default for DraftOptions {
    fn default() -> Self {
        Self {
            importance: 1,
            sensitivity: 0,
            read_receipt: false,
            delivery_receipt: false,
            plain_text: false,
        }
    }
}

/// 저장할 내용(작성 화면에서).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DraftContent {
    pub to: Vec<Recipient>,
    pub cc: Vec<Recipient>,
    pub bcc: Vec<Recipient>,
    pub subject: String,
    pub body_html: String,
    pub include_quote: bool,
    /// 고친 인용문(없으면 원문 그대로).
    pub quote_html: Option<String>,
    pub options: DraftOptions,
}

const DRAFT_COLS: &str = "id, kind, ref_message_id, to_json, cc_json, bcc_json, subject, body_html, \
     include_quote, state, last_error, server_draft_id, created_at, updated_at, message_id, \
     importance, sensitivity, read_receipt, delivery_receipt, plain_text, server_dirty, send_started, \
     server_kind, quote_html, excluded_orig";

fn draft_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<(Draft, [String; 3])> {
    let excluded: Option<String> = r.get(24)?;
    Ok((
        Draft {
            id: r.get(0)?,
            kind: r.get(1)?,
            ref_message_id: r.get(2)?,
            to: Vec::new(),
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: r.get(6)?,
            body_html: r.get(7)?,
            include_quote: r.get(8)?,
            quote_html: r.get(23)?,
            state: r.get(9)?,
            last_error: r.get(10)?,
            server_draft_id: r.get(11)?,
            created_at: r.get(12)?,
            updated_at: r.get(13)?,
            message_id: r.get(14)?,
            options: DraftOptions {
                importance: r.get(15)?,
                sensitivity: r.get(16)?,
                read_receipt: r.get(17)?,
                delivery_receipt: r.get(18)?,
                plain_text: r.get(19)?,
            },
            server_dirty: r.get(20)?,
            send_started: r.get(21)?,
            server_kind: r.get(22)?,
            excluded_orig: excluded
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default(),
        },
        [r.get(3)?, r.get(4)?, r.get(5)?],
    ))
}

/// 목록 미리 보기 글(태그를 빼고 공백을 줄인 앞부분).
fn preview_text(html: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    let mut space = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                space = true;
            }
            _ if in_tag => {}
            c if c.is_whitespace() => space = true,
            c => {
                if space && !out.is_empty() {
                    out.push(' ');
                }
                space = false;
                out.push(c);
            }
        }
        if out.chars().count() >= 200 {
            break;
        }
    }
    out.replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// "이름 <주소>; 주소2"(목록 표시용).
fn address_list(list: &[Recipient]) -> Option<String> {
    (!list.is_empty()).then(|| {
        list.iter()
            .map(|r| {
                if r.name.is_empty() {
                    r.email.clone()
                } else {
                    format!("{} <{}>", r.name, r.email)
                }
            })
            .collect::<Vec<_>>()
            .join("; ")
    })
}

/// 임시 보관함 메일 행에 이어진 작성 중 메일(동기화가 서버에서 사라진 초안을 처리할 때).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DraftLink {
    pub draft_id: i64,
    /// 보낼 편지함(보내는 중).
    pub sending: bool,
    /// 서버에 아직 올리지 않은 고침이 있다.
    pub dirty: bool,
}

impl crate::Tx<'_> {
    pub fn draft_link(&self, message_id: i64) -> Result<Option<DraftLink>> {
        Ok(self
            .tx
            .query_row(
                "SELECT id, state, server_dirty FROM drafts WHERE message_id = ?1",
                [message_id],
                |r| {
                    Ok(DraftLink {
                        draft_id: r.get(0)?,
                        sending: r.get::<_, i64>(1)? == draft_state::OUTBOX,
                        dirty: r.get(2)?,
                    })
                },
            )
            .optional()?)
    }

    /// 서버 초안이 사라졌지만 올리지 않은 고침이 있다: 행을 "서버 반영 대기"로 돌리고 다시 올린다.
    pub fn unlink_server_draft(&self, draft_id: i64, message_id: i64, now_ms: i64) -> Result<()> {
        self.tx.execute(
            "UPDATE drafts SET server_draft_id = NULL, server_kind = NULL, server_dirty = 1 \
             WHERE id = ?1",
            [draft_id],
        )?;
        self.tx
            .execute("DELETE FROM draft_uploads WHERE draft_id = ?1", [draft_id])?;
        self.tx.execute(
            "UPDATE messages SET server_item_id = NULL, change_key = NULL, server_state = ?1 \
             WHERE id = ?2",
            params![server_state::PENDING, message_id],
        )?;
        self.enqueue_op(
            OP_DRAFT_SAVE,
            None,
            &json!({"draft": draft_id}),
            now_ms,
            None,
        )?;
        Ok(())
    }
}

fn finish(row: (Draft, [String; 3])) -> Result<Draft> {
    let (mut d, [to, cc, bcc]) = row;
    d.to = serde_json::from_str(&to)?;
    d.cc = serde_json::from_str(&cc)?;
    d.bcc = serde_json::from_str(&bcc)?;
    Ok(d)
}

/// "이름 <주소>; 주소2" → (이름, 주소)들.
pub fn parse_address_list(list: &str) -> Vec<Recipient> {
    list.split([';', ','])
        .map(str::trim)
        .filter(|s| s.contains('@'))
        .map(|s| match (s.find('<'), s.rfind('>')) {
            (Some(a), Some(b)) if a < b => Recipient {
                name: s[..a].trim().trim_matches('"').to_owned(),
                email: s[a + 1..b].trim().to_owned(),
            },
            _ => Recipient {
                name: String::new(),
                email: s.trim_matches(['<', '>']).to_owned(),
            },
        })
        .collect()
}

impl Store {
    /// 작성 중 메일의 첨부 파일 폴더.
    pub fn draft_dir(&self, id: i64) -> PathBuf {
        self.root().join("drafts").join(id.to_string())
    }

    pub fn create_draft(
        &self,
        kind: &str,
        ref_message_id: Option<i64>,
        content: &DraftContent,
        now_ms: i64,
    ) -> Result<i64> {
        let conn = self.conn();
        let o = &content.options;
        conn.execute(
            "INSERT INTO drafts(kind, ref_message_id, to_json, cc_json, bcc_json, subject, body_html, \
               include_quote, created_at, updated_at, importance, sensitivity, read_receipt, \
               delivery_receipt, plain_text) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                kind,
                ref_message_id,
                serde_json::to_string(&content.to)?,
                serde_json::to_string(&content.cc)?,
                serde_json::to_string(&content.bcc)?,
                content.subject,
                content.body_html,
                content.include_quote,
                now_ms,
                o.importance,
                o.sensitivity,
                o.read_receipt,
                o.delivery_receipt,
                o.plain_text
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// 임시 보관함의 메일 행에 이어진 작성 중 메일.
    pub fn draft_by_message(&self, message_id: i64) -> Result<Option<Draft>> {
        let row = self
            .conn()
            .query_row(
                &format!("SELECT {DRAFT_COLS} FROM drafts WHERE message_id = ?1"),
                [message_id],
                draft_row,
            )
            .optional()?;
        row.map(finish).transpose()
    }

    /// 서버 초안 항목 ID로 작성 중 메일 번호.
    pub fn draft_by_server_id(&self, server_id: &str) -> Result<Option<i64>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT id FROM drafts WHERE server_draft_id = ?1",
                [server_id],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// 메일 행들 → (작성 중 메일 번호, 상태). 임시 보관함 목록의 "보내는 중"·"발송 실패" 표시용.
    pub fn draft_states(&self, message_ids: &[i64]) -> Result<HashMap<i64, (i64, i64)>> {
        if message_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let marks = vec!["?"; message_ids.len()].join(",");
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT message_id, id, state FROM drafts WHERE message_id IN ({marks})"
        ))?;
        let rows = stmt.query_map(rusqlite::params_from_iter(message_ids), |r| {
            Ok((
                r.get::<_, i64>(0)?,
                (r.get::<_, i64>(1)?, r.get::<_, i64>(2)?),
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// 임시 보관함의 메일 행을 작성 중 메일 내용으로 만들거나 고친다. 임시 보관함 폴더를 아직 모르면
    /// (한 번도 로그인하지 않음) 아무것도 하지 않는다. 행 번호.
    pub fn sync_draft_row(&self, id: i64, me: Option<&Recipient>) -> Result<Option<i64>> {
        let Some(d) = self.draft(id)? else {
            return Ok(None);
        };
        let size = d.body_html.len() as i64
            + self
                .draft_files(id)?
                .iter()
                .map(|(_, s)| *s as i64)
                .sum::<i64>();
        let has_files = !self.draft_files(id)?.is_empty();
        let preview = preview_text(&d.body_html);
        self.transaction(|tx| {
            let Some(folder) = tx.well_known_folder_id("drafts")? else {
                return Ok(None);
            };
            let existing = match d.message_id {
                Some(mid) => tx.message(mid)?,
                None => None,
            };
            let props = crate::MessageProps {
                internet_msg_id: existing
                    .as_ref()
                    .and_then(|r| r.props.internet_msg_id.clone()),
                conversation_id: existing
                    .as_ref()
                    .and_then(|r| r.props.conversation_id.clone()),
                subject: Some(d.subject.clone()),
                from_name: me.map(|m| m.name.clone()),
                from_addr: me.map(|m| m.email.clone()),
                to_list: address_list(&d.to),
                cc_list: address_list(&d.cc),
                date_received: d.updated_at,
                date_sent: Some(d.updated_at),
                size: Some(size),
                has_attachments: has_files,
                is_read: true,
                flag: 0,
                importance: d.options.importance,
                is_draft: true,
                item_class: Some("IPM.Note".into()),
                preview: Some(preview.clone()),
                pinned: None,
            };
            let mid = match existing {
                Some(row) => {
                    tx.update_message(row.id, None, row.change_key.as_deref(), &props)?;
                    tx.tx.execute(
                        "UPDATE messages SET folder_id = ?1, indexed = 0 WHERE id = ?2",
                        params![folder, row.id],
                    )?;
                    row.id
                }
                None => {
                    tx.tx.execute(
                        "INSERT INTO messages(folder_id, server_item_id, server_state, subject, from_name, \
                           from_addr, to_list, cc_list, date_received, date_sent, size, has_attachments, \
                           is_read, flag, importance, is_draft, item_class, preview) \
                         VALUES (?1, NULL, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 1, 0, ?12, 1, ?13, ?14)",
                        params![
                            folder,
                            server_state::PENDING,
                            props.subject,
                            props.from_name,
                            props.from_addr,
                            props.to_list,
                            props.cc_list,
                            props.date_received,
                            props.date_sent,
                            props.size,
                            props.has_attachments,
                            props.importance,
                            props.item_class,
                            props.preview
                        ],
                    )?;
                    let mid = tx.tx.last_insert_rowid();
                    tx.tx.execute(
                        "UPDATE drafts SET message_id = ?1 WHERE id = ?2",
                        params![mid, id],
                    )?;
                    mid
                }
            };
            Ok(Some(mid))
        })
    }

    /// 서버 임시 보관함에 올릴 작업을 쌓는다(대기 중인 것이 있으면 그대로). 보낼 편지함에 든 것은 발송 작업이
    /// 올린다. 쌓았으면 true.
    pub fn queue_draft_upload(&self, id: i64, now_ms: i64) -> Result<bool> {
        self.transaction(|tx| {
            let waiting: bool = tx.tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM ops WHERE state = 0 AND kind IN (?1, ?2) \
                   AND json_extract(payload, '$.draft') = ?3)",
                params![OP_DRAFT_SAVE, OP_SEND, id],
                |r| r.get(0),
            )?;
            let editable: bool = tx
                .tx
                .query_row(
                    "SELECT state != ?1 FROM drafts WHERE id = ?2",
                    params![draft_state::OUTBOX, id],
                    |r| r.get(0),
                )
                .optional()?
                .unwrap_or(false);
            if waiting || !editable {
                return Ok(false);
            }
            tx.enqueue_op(OP_DRAFT_SAVE, None, &json!({"draft": id}), now_ms, None)?;
            Ok(true)
        })
    }

    /// 임시 보관함 행이 아직 없는 작성 중 메일(예전 판에서 쓰던 것 등).
    pub fn drafts_without_row(&self) -> Result<Vec<i64>> {
        let conn = self.conn();
        let mut stmt =
            conn.prepare("SELECT id FROM drafts WHERE message_id IS NULL ORDER BY id")?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// 서버에 올릴 것이 남은 작성 중 메일(보낼 편지함 제외).
    pub fn dirty_drafts(&self) -> Result<Vec<i64>> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare("SELECT id FROM drafts WHERE server_dirty = 1 AND state != ?1 ORDER BY id")?;
        let rows = stmt.query_map([draft_state::OUTBOX], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// 서버에 올렸다: 초안 ID를 적고, 올리는 동안 바뀌지 않았으면(`updated_at` 그대로) 깨끗한 상태로.
    /// 임시 보관함 행에도 서버 항목 ID를 붙인다(동기화가 같은 행을 갱신하게).
    pub fn finish_draft_upload(
        &self,
        id: i64,
        server_id: &str,
        change_key: Option<&str>,
        uploaded_version: i64,
    ) -> Result<()> {
        self.transaction(|tx| {
            tx.tx.execute(
                "UPDATE drafts SET server_draft_id = ?1, \
                   server_dirty = CASE WHEN updated_at = ?2 THEN 0 ELSE server_dirty END WHERE id = ?3",
                params![server_id, uploaded_version, id],
            )?;
            let mid: Option<i64> = tx
                .tx
                .query_row("SELECT message_id FROM drafts WHERE id = ?1", [id], |r| {
                    r.get(0)
                })
                .optional()?
                .flatten();
            let mut hashes = Vec::new();
            if let Some(mid) = mid {
                // 동기화가 이미 같은 서버 항목으로 다른 행을 만들었으면 그 행을 지우고 이 행을 쓴다.
                let dup: Option<i64> = tx
                    .tx
                    .query_row(
                        "SELECT id FROM messages WHERE server_item_id = ?1 AND id != ?2",
                        params![server_id, mid],
                        |r| r.get(0),
                    )
                    .optional()?;
                if let Some(dup) = dup {
                    hashes.extend(tx.purge_row(dup)?);
                }
                tx.set_server_item(mid, server_id, change_key)?;
            }
            Ok(hashes)
        })
        .and_then(|hashes| {
            for h in hashes {
                self.remove_eml_if_unused(&h)?;
            }
            Ok(())
        })
    }

    pub fn set_draft_send_started(&self, id: i64, started: bool) -> Result<()> {
        self.conn().execute(
            "UPDATE drafts SET send_started = ?1 WHERE id = ?2",
            params![started, id],
        )?;
        Ok(())
    }

    /// 서버 초안을 잃었다(웹메일에서 지움 등): 다음에 올릴 때 새로 만든다.
    pub fn forget_server_draft(&self, id: i64) -> Result<()> {
        self.transaction(|tx| {
            tx.tx.execute(
                "UPDATE drafts SET server_draft_id = NULL, server_kind = NULL, server_dirty = 1 \
                 WHERE id = ?1",
                [id],
            )?;
            tx.tx
                .execute("DELETE FROM draft_uploads WHERE draft_id = ?1", [id])?;
            tx.tx.execute(
                "UPDATE messages SET server_item_id = NULL, change_key = NULL, server_state = ?1 \
                 WHERE id = (SELECT message_id FROM drafts WHERE id = ?2)",
                params![server_state::PENDING, id],
            )?;
            Ok(())
        })
    }

    /// 서버 초안에 올린 첨부(열쇠 → 첨부 ID).
    pub fn draft_uploads(&self, id: i64) -> Result<HashMap<String, String>> {
        let conn = self.conn();
        let mut stmt =
            conn.prepare("SELECT key, attachment_id FROM draft_uploads WHERE draft_id = ?1")?;
        let rows = stmt.query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn set_draft_upload(&self, id: i64, key: &str, attachment_id: Option<&str>) -> Result<()> {
        match attachment_id {
            Some(a) => self.conn().execute(
                "INSERT INTO draft_uploads(draft_id, key, attachment_id) VALUES (?1, ?2, ?3) \
                 ON CONFLICT(draft_id, key) DO UPDATE SET attachment_id = excluded.attachment_id",
                params![id, key, a],
            )?,
            None => self.conn().execute(
                "DELETE FROM draft_uploads WHERE draft_id = ?1 AND key = ?2",
                params![id, key],
            )?,
        };
        Ok(())
    }

    pub fn draft(&self, id: i64) -> Result<Option<Draft>> {
        let row = self
            .conn()
            .query_row(
                &format!("SELECT {DRAFT_COLS} FROM drafts WHERE id = ?1"),
                [id],
                draft_row,
            )
            .optional()?;
        row.map(finish).transpose()
    }

    /// 작성 중(또는 발송 실패)인 메일을 저장한다(서버에 올릴 것으로 표시). 보낼 편지함에 들어간 것은 바꾸지 않는다.
    pub fn save_draft(&self, id: i64, c: &DraftContent, now_ms: i64) -> Result<bool> {
        let o = &c.options;
        let n = self.conn().execute(
            "UPDATE drafts SET to_json = ?1, cc_json = ?2, bcc_json = ?3, subject = ?4, body_html = ?5, \
               include_quote = ?6, updated_at = MAX(?7, updated_at + 1), importance = ?8, sensitivity = ?9, \
               read_receipt = ?10, delivery_receipt = ?11, plain_text = ?12, server_dirty = 1, \
               quote_html = ?15 \
             WHERE id = ?13 AND state != ?14",
            params![
                serde_json::to_string(&c.to)?,
                serde_json::to_string(&c.cc)?,
                serde_json::to_string(&c.bcc)?,
                c.subject,
                c.body_html,
                c.include_quote,
                now_ms,
                o.importance,
                o.sensitivity,
                o.read_receipt,
                o.delivery_receipt,
                o.plain_text,
                id,
                draft_state::OUTBOX,
                c.quote_html
            ],
        )?;
        Ok(n == 1)
    }

    /// 첨부를 바꿨다: 서버에 올릴 것으로 표시한다.
    pub fn touch_draft(&self, id: i64, now_ms: i64) -> Result<()> {
        self.conn().execute(
            "UPDATE drafts SET updated_at = MAX(?1, updated_at + 1), server_dirty = 1 \
             WHERE id = ?2 AND state != ?3",
            params![now_ms, id, draft_state::OUTBOX],
        )?;
        Ok(())
    }

    /// 전달에서 뺄 원본 첨부를 바꾼다(`excluded`면 빼고, 아니면 되돌린다). 서버에 올릴 것으로 표시한다.
    pub fn set_orig_excluded(&self, id: i64, key: &str, excluded: bool, now_ms: i64) -> Result<()> {
        let Some(d) = self.draft(id)? else {
            return Ok(());
        };
        let mut list = d.excluded_orig;
        list.retain(|k| k != key);
        if excluded {
            list.push(key.to_owned());
        }
        let value = (!list.is_empty())
            .then(|| serde_json::to_string(&list))
            .transpose()?;
        self.conn().execute(
            "UPDATE drafts SET excluded_orig = ?1, updated_at = MAX(?2, updated_at + 1), server_dirty = 1 \
             WHERE id = ?3 AND state != ?4",
            params![value, now_ms, id, draft_state::OUTBOX],
        )?;
        Ok(())
    }

    /// 작성 중·보낼 편지함·발송 실패 메일, 최근 것부터.
    pub fn list_drafts(&self) -> Result<Vec<Draft>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {DRAFT_COLS} FROM drafts ORDER BY updated_at DESC"
        ))?;
        let rows = stmt.query_map([], draft_row)?;
        rows.map(|r| finish(r?)).collect()
    }

    pub fn set_draft_state(
        &self,
        id: i64,
        state: i64,
        error: Option<&str>,
        now_ms: i64,
    ) -> Result<()> {
        self.conn().execute(
            "UPDATE drafts SET state = ?1, last_error = ?2, updated_at = ?3 WHERE id = ?4",
            params![state, error, now_ms, id],
        )?;
        Ok(())
    }

    /// 발송 실패로 돌린다. `error`가 없으면 이미 적힌 사유를 둔다.
    pub fn mark_draft_failed(&self, id: i64, error: Option<&str>) -> Result<()> {
        self.conn().execute(
            "UPDATE drafts SET state = ?1, last_error = COALESCE(?2, last_error) WHERE id = ?3",
            params![draft_state::FAILED, error, id],
        )?;
        Ok(())
    }

    /// 새로 만든 서버 초안(만든 방식과 함께). 올린 첨부 기록은 새 초안 기준으로 비운다.
    pub fn set_draft_server_id(&self, id: i64, server_id: &str, kind: &str) -> Result<()> {
        self.transaction(|tx| {
            tx.tx.execute(
                "UPDATE drafts SET server_draft_id = ?1, server_kind = ?2 WHERE id = ?3",
                params![server_id, kind, id],
            )?;
            tx.tx
                .execute("DELETE FROM draft_uploads WHERE draft_id = ?1", [id])?;
            Ok(())
        })
    }

    /// 작성 중 메일과 그 첨부 파일을 지운다(보냈거나 버렸을 때). 서버에 아직 없는 임시 보관함 행도 지운다
    /// (서버에 있는 행은 동기화나 삭제 작업이 정리한다).
    pub fn delete_draft(&self, id: i64) -> Result<()> {
        self.transaction(|tx| {
            tx.tx.execute(
                "DELETE FROM messages WHERE id = (SELECT message_id FROM drafts WHERE id = ?1) \
                   AND server_item_id IS NULL",
                [id],
            )?;
            tx.tx.execute("DELETE FROM drafts WHERE id = ?1", [id])?;
            Ok(())
        })?;
        let dir = self.draft_dir(id);
        if dir.exists() {
            std::fs::remove_dir_all(dir)?;
        }
        Ok(())
    }

    /// 첨부 파일들(이름, 크기), 이름순.
    pub fn draft_files(&self, id: i64) -> Result<Vec<(String, u64)>> {
        let dir = self.draft_dir(id);
        let Ok(rd) = std::fs::read_dir(&dir) else {
            return Ok(Vec::new());
        };
        let mut out: Vec<(String, u64)> = rd
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
            .map(|e| {
                (
                    e.file_name().to_string_lossy().into_owned(),
                    e.metadata().map_or(0, |m| m.len()),
                )
            })
            .collect();
        out.sort();
        Ok(out)
    }

    /// 첨부 폴더에서 쓸 이름: 같은 이름이 있으면 "이름 (2).확장자".
    fn free_draft_name(&self, id: i64, name: &str) -> Result<(PathBuf, String)> {
        let dir = self.draft_dir(id);
        std::fs::create_dir_all(&dir)?;
        let p = Path::new(name);
        let stem = p
            .file_stem()
            .map_or_else(|| name.to_owned(), |s| s.to_string_lossy().into_owned());
        let ext = p
            .extension()
            .map(|e| format!(".{}", e.to_string_lossy()))
            .unwrap_or_default();
        let mut candidate = name.to_owned();
        let mut n = 2;
        while dir.join(&candidate).exists() {
            candidate = format!("{stem} ({n}){ext}");
            n += 1;
        }
        Ok((dir.join(&candidate), candidate))
    }

    /// 파일을 첨부 폴더에 복사한다. `name`은 호출한 쪽이 안전한 파일 이름으로 만든 것.
    /// 같은 이름이 있으면 "이름 (2).확장자". 실제로 쓴 이름을 돌려준다.
    pub fn add_draft_file(&self, id: i64, src: &Path, name: &str) -> Result<String> {
        let (path, name) = self.free_draft_name(id, name)?;
        std::fs::copy(src, path)?;
        Ok(name)
    }

    /// 내용으로 첨부를 넣는다(서버 초안을 가져올 때). 실제로 쓴 이름.
    pub fn add_draft_bytes(&self, id: i64, name: &str, data: &[u8]) -> Result<String> {
        let (path, name) = self.free_draft_name(id, name)?;
        std::fs::write(path, data)?;
        Ok(name)
    }

    /// 임시 보관함의 서버 초안(웹메일에서 쓰던 것 등)을 작성 중 메일로 가져온다. 그 행·서버 초안과 잇고,
    /// 이미 서버에 있으니 올릴 것은 없다(첨부 기록은 호출한 쪽이 넣는다).
    pub fn import_server_draft(
        &self,
        message_id: i64,
        server_id: &str,
        content: &DraftContent,
        now_ms: i64,
    ) -> Result<i64> {
        let id = self.create_draft("new", None, content, now_ms)?;
        self.conn().execute(
            "UPDATE drafts SET message_id = ?1, server_draft_id = ?2, server_kind = 'newMail', \
               server_dirty = 0 WHERE id = ?3",
            params![message_id, server_id, id],
        )?;
        Ok(id)
    }

    pub fn remove_draft_file(&self, id: i64, name: &str) -> Result<()> {
        if name.contains(['/', '\\']) || name.starts_with('.') {
            return Err(Error::Invalid("잘못된 첨부 이름".into()));
        }
        let path = self.draft_dir(id).join(name);
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        Ok(())
    }

    /// 보내기: 보낼 편지함으로 옮기고 발송 작업을 쌓는다(계획서 9.3).
    pub fn queue_send(&self, id: i64, now_ms: i64) -> Result<()> {
        self.transaction(|tx| {
            let n = tx.tx.execute(
                "UPDATE drafts SET state = ?1, last_error = NULL, updated_at = ?2 WHERE id = ?3 AND state != ?1",
                params![draft_state::OUTBOX, now_ms, id],
            )?;
            if n == 1 {
                tx.enqueue_op(OP_SEND, None, &json!({"draft": id}), now_ms, None)?;
            }
            Ok(())
        })
    }

    pub fn outbox_count(&self) -> Result<i64> {
        Ok(self.conn().query_row(
            "SELECT COUNT(*) FROM drafts WHERE state = ?1",
            [draft_state::OUTBOX],
            |r| r.get(0),
        )?)
    }

    /// 보낸 편지함에서 가장 많이 보이는 보낸 사람 주소(로그인 전에 "나"를 알아야 할 때).
    pub fn guess_me(&self) -> Result<Option<String>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT m.from_addr FROM messages m JOIN folders f ON f.id = m.folder_id \
                 WHERE f.well_known = 'sentitems' AND m.from_addr IS NOT NULL \
                 GROUP BY lower(m.from_addr) ORDER BY COUNT(*) DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// 주소록을 메일에서 다시 만든다(보낸 메일의 받는 사람·참조 가중 3, 받은 메일의 보낸 사람 가중 1).
    pub fn rebuild_contacts(&self) -> Result<usize> {
        let mut book: HashMap<String, (String, i64, i64)> = HashMap::new();
        {
            let conn = self.conn();
            let mut stmt = conn.prepare(
                "SELECT f.well_known, m.from_name, m.from_addr, m.to_list, m.cc_list, m.date_received \
                 FROM messages m JOIN folders f ON f.id = m.folder_id",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, Option<String>>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, i64>(5)?,
                ))
            })?;
            let mut add = |name: &str, email: &str, score: i64, at: i64| {
                let key = email.trim().to_lowercase();
                if !key.contains('@') {
                    return;
                }
                let e = book.entry(key).or_insert_with(|| (String::new(), 0, 0));
                if e.0.is_empty() && !name.is_empty() {
                    e.0 = name.to_owned();
                }
                e.1 += score;
                e.2 = e.2.max(at);
            };
            for row in rows {
                let (wk, from_name, from_addr, to, cc, at) = row?;
                if wk.as_deref() == Some("sentitems") {
                    for r in [to, cc]
                        .iter()
                        .flatten()
                        .flat_map(|l| parse_address_list(l))
                    {
                        add(&r.name, &r.email, 3, at);
                    }
                } else if let Some(a) = from_addr {
                    add(from_name.as_deref().unwrap_or(""), &a, 1, at);
                }
            }
        }
        self.transaction(|tx| {
            tx.tx.execute("DELETE FROM contacts", [])?;
            for (addr, (name, score, at)) in &book {
                tx.tx.execute(
                    "INSERT INTO contacts(addr, name, score, last_used) VALUES (?1, ?2, ?3, ?4)",
                    params![addr, name, score, at],
                )?;
            }
            Ok(())
        })?;
        Ok(book.len())
    }

    /// 주소록 검색(이름·주소 일부, 빈도순).
    pub fn search_contacts(&self, q: &str, limit: usize) -> Result<Vec<Recipient>> {
        let like = format!("%{}%", q.trim().to_lowercase().replace(['%', '_'], ""));
        let conn = self.conn();
        let mut stmt = conn.prepare_cached(
            "SELECT name, addr FROM contacts WHERE addr LIKE ?1 OR lower(name) LIKE ?1 \
             ORDER BY score DESC, last_used DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![like, i64::try_from(limit).unwrap_or(20)], |r| {
            Ok(Recipient {
                name: r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                email: r.get(1)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// 보낸 받는 사람들의 점수를 올린다.
    pub fn bump_contacts(&self, people: &[Recipient], now_ms: i64) -> Result<()> {
        self.transaction(|tx| {
            for p in people {
                tx.tx.execute(
                    "INSERT INTO contacts(addr, name, score, last_used) VALUES (?1, ?2, 5, ?3) \
                     ON CONFLICT(addr) DO UPDATE SET score = score + 5, last_used = ?3, \
                       name = CASE WHEN contacts.name IS NULL OR contacts.name = '' THEN excluded.name ELSE contacts.name END",
                    params![p.email.trim().to_lowercase(), p.name, now_ms],
                )?;
            }
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MessageProps, ServerFolder};

    fn r(name: &str, email: &str) -> Recipient {
        Recipient {
            name: name.into(),
            email: email.into(),
        }
    }

    #[test]
    fn drafts_save_files_and_queue() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        let id = s
            .create_draft(
                "reply",
                Some(7),
                &DraftContent {
                    to: vec![r("김", "kim@x.com")],
                    subject: "RE: a".into(),
                    include_quote: true,
                    ..DraftContent::default()
                },
                1,
            )
            .unwrap();
        let d = s.draft(id).unwrap().unwrap();
        assert_eq!(
            (d.kind.as_str(), d.ref_message_id, d.to.len()),
            ("reply", Some(7), 1)
        );

        let src = dir.path().join("a.txt");
        std::fs::write(&src, b"hello").unwrap();
        assert_eq!(s.add_draft_file(id, &src, "a.txt").unwrap(), "a.txt");
        assert_eq!(s.add_draft_file(id, &src, "a.txt").unwrap(), "a (2).txt");
        assert_eq!(s.draft_files(id).unwrap().len(), 2);
        s.remove_draft_file(id, "a (2).txt").unwrap();
        assert!(s.remove_draft_file(id, "..\\x").is_err());

        let mut c = DraftContent {
            subject: "RE: b".into(),
            ..DraftContent::default()
        };
        assert!(s.save_draft(id, &c, 2).unwrap());
        s.queue_send(id, 3).unwrap();
        s.queue_send(id, 3).unwrap();
        assert_eq!(s.pending_op_count().unwrap(), 1, "두 번 눌러도 한 번만");
        assert_eq!(s.outbox_count().unwrap(), 1);
        c.subject = "바뀌면 안 됨".into();
        assert!(
            !s.save_draft(id, &c, 4).unwrap(),
            "보낼 편지함에 들어간 것은 고치지 않는다"
        );
        assert_eq!(s.draft(id).unwrap().unwrap().subject, "RE: b");
        s.delete_draft(id).unwrap();
        assert!(s.draft(id).unwrap().is_none() && !s.draft_dir(id).exists());
    }

    #[test]
    fn contacts_from_mail_and_me_guess() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        s.transaction(|tx| {
            let f = |id: &str| ServerFolder {
                server_id: id.into(),
                server_parent_id: Some("ROOT".into()),
                name: id.into(),
                folder_class: Some("IPF.Note".into()),
            };
            let inbox = tx.upsert_folder(&f("INBOX"))?;
            let sent = tx.upsert_folder(&f("SENT"))?;
            tx.set_well_known("SENT", "sentitems", 2)?;
            let p = |from: &str, to: &str, n: i64| MessageProps {
                from_name: Some("이름".into()),
                from_addr: Some(from.into()),
                to_list: Some(to.into()),
                date_received: n,
                ..MessageProps::default()
            };
            tx.insert_message(inbox, "A", None, &p("boss@x.com", "me@x.com", 1))?;
            tx.insert_message(
                sent,
                "B",
                None,
                &p("me@x.com", "김민수 <minsu@x.com>; boss@x.com", 2),
            )?;
            tx.insert_message(sent, "C", None, &p("me@x.com", "minsu@x.com", 3))?;
            Ok(())
        })
        .unwrap();
        assert_eq!(s.guess_me().unwrap().as_deref(), Some("me@x.com"));
        assert_eq!(s.rebuild_contacts().unwrap(), 2);
        let found = s.search_contacts("MIN", 5).unwrap();
        assert_eq!(found, vec![r("김민수", "minsu@x.com")]);
        let all = s.search_contacts("x.com", 5).unwrap();
        assert_eq!(all[0].email, "minsu@x.com", "보낸 횟수가 많은 사람이 먼저");
        s.bump_contacts(&[r("새 사람", "New@x.com")], 9).unwrap();
        assert_eq!(s.search_contacts("new", 5).unwrap()[0].email, "new@x.com");
        assert_eq!(
            parse_address_list("\"A\" <a@x>, b@y; 이름없음"),
            vec![r("A", "a@x"), r("", "b@y")]
        );
    }
}
