//! 임시 보관함 저장과 발송(계획서 5.7, 5.8, 9.3, 0.7.0).
//!
//! - 작성 중인 메일은 서버 임시 보관함의 초안 하나와 짝이다(0.7.0, 사용자 요청: 웹메일에서도 보이게).
//!   처음 올릴 때 초안을 만든다: 새 메일 `CreateItem`, 회신·전달(원문이 서버에 있음) `ReplyToItem`·
//!   `ReplyAllToItem`·`ForwardItem`, 로컬 보관본 회신·전달(서버에 없음) 새 메일 + 확장 속성
//!   In-Reply-To(0x1042)·References(0x1039).
//! - 올릴 때마다 첨부를 맞추고(새 것만 올리고 뺀 것은 지운다) 내용을 `UpdateItem(SaveOnly)`로 지정한다.
//! - 본문은 작성 화면의 새 내용 + 인용문(로컬 원문으로 만든 웹메일 형식)으로 통째로 지정한다. 인용문의 인라인
//!   이미지는 같은 Content-ID로 붙인다. 서버 전달은 서버가 원본 첨부를 함께 넣고, 로컬 보관본 전달은 원본 첨부도 붙인다.
//! - 보내기: 서버 초안을 맞춘 뒤 `UpdateItem(SendAndSaveCopy)`. 중복 발송 방지: 보내기 요청 직전에 `send_started`를
//!   적는다. 다시 시도할 때 그 초안이 없어졌으면 이미 보낸 것으로 본다.

use std::collections::HashSet;
use std::path::Path;

use eo_mime::compose::{self, ReplyMode};
use eo_owa::requests::{DraftFields, NewMail, PROP_IN_REPLY_TO, PROP_REFERENCES, ReplyDraft};
use eo_owa::{Address, FileAttachment, ItemId, ReplyKind};
use eo_store::{Draft, MessageRow, Recipient, draft_state, orig_key};

use crate::engine::{Engine, Event, now_ms};
use crate::ops::Outcome;
use crate::{Error, Result};

const NOT_FOUND: &str = "ErrorItemNotFound";
const NEW_MAIL: &str = "newMail";
/// 작성 창 인용문 문서의 본문 그림 주소 앞부분(+ 원문 메일 번호 + "/"). 앱의 `app-cid` 프로토콜과 같다.
pub const QUOTE_CID_BASE: &str = "http://app-cid.localhost/";

fn bare(id: &str) -> ItemId {
    ItemId {
        id: id.to_owned(),
        change_key: None,
    }
}

fn addrs(list: &[Recipient]) -> Vec<Address> {
    list.iter()
        .map(|r| Address {
            name: r.name.clone(),
            email: r.email.clone(),
        })
        .collect()
}

/// 확장자로 MIME 형식(모르면 application/octet-stream).
pub fn content_type_of(name: &str) -> &'static str {
    let ext = Path::new(name)
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "pdf" => "application/pdf",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "hwp" => "application/x-hwp",
        "hwpx" => "application/hwp+zip",
        "zip" => "application/zip",
        "txt" | "log" => "text/plain",
        "csv" => "text/csv",
        "htm" | "html" => "text/html",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        "eml" => "message/rfc822",
        _ => "application/octet-stream",
    }
}

fn mode_of(kind: &str) -> Option<ReplyMode> {
    match kind {
        "reply" => Some(ReplyMode::Reply),
        "reply_all" => Some(ReplyMode::ReplyAll),
        "forward" => Some(ReplyMode::Forward),
        _ => None,
    }
}

fn reply_kind(m: ReplyMode) -> ReplyKind {
    match m {
        ReplyMode::Reply => ReplyKind::Reply,
        ReplyMode::ReplyAll => ReplyKind::ReplyAll,
        ReplyMode::Forward => ReplyKind::Forward,
    }
}

/// 첨부 열쇠용 짧은 내용 지문(FNV-1a 64).
fn fingerprint(data: &[u8]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in data {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// 서버 초안에 붙일 것 하나. 열쇠가 같으면 이미 올린 것으로 보고 다시 올리지 않는다.
struct Upload {
    key: String,
    source: Source,
}

enum Source {
    Part(FileAttachment),
    /// 작성 화면에서 붙인 파일(올릴 때만 읽는다).
    File(String),
}

impl Upload {
    fn part(key: String, att: FileAttachment) -> Self {
        Self {
            key,
            source: Source::Part(att),
        }
    }
}

/// 올릴 내용: 받을 본문과 붙일 것들.
struct Material {
    body: String,
    uploads: Vec<Upload>,
    mode: Option<ReplyMode>,
    reference: Option<MessageRow>,
    original: Option<compose::Original>,
}

fn inline_part(p: &compose::OriginalPart) -> FileAttachment {
    FileAttachment {
        name: p.name.clone(),
        content_type: p.mime.clone(),
        content: p.data.clone(),
        is_inline: true,
        content_id: p.content_id.clone(),
    }
}

impl Engine {
    /// 내 이름·주소(웹메일 설정, 엔진마다 한 번 받는다).
    pub(crate) async fn me(&self) -> Result<Address> {
        if let Some(me) = self.me_cache() {
            return Ok(me);
        }
        let cfg = self.client().user_config().await?;
        let me = Address {
            name: cfg.display_name.unwrap_or_default(),
            email: cfg.email_address.ok_or_else(|| {
                Error::Owa(eo_owa::Error::Decode {
                    action: "GetOwaUserConfiguration".into(),
                    detail: "내 주소 없음".into(),
                })
            })?,
        };
        self.set_me_cache(me.clone());
        Ok(me)
    }

    fn finish_sent(&self, d: &Draft) -> Result<()> {
        let people: Vec<Recipient> = d.to.iter().chain(&d.cc).chain(&d.bcc).cloned().collect();
        let _ = self.store().bump_contacts(&people, now_ms());
        // 서버는 보낸 초안을 임시 보관함에서 지운다. 동기화를 기다리지 않고 행을 바로 뺀다.
        if let Some(mid) = d.message_id
            && let Some(row) = self.store().message(mid)?
        {
            self.store().purge_message(mid)?;
            if let Some(search) = self.search() {
                search.delete(mid as u64)?;
                search.commit()?;
            }
            self.emit(Event::MessagesChanged {
                folder_id: row.folder_id,
                message_ids: vec![mid],
            });
        }
        self.store().delete_draft(d.id)?;
        self.emit(Event::SendResult {
            draft_id: d.id,
            error: None,
        });
        // 보낸 편지함·받은 편지함(본인에게 보낸 경우)을 곧 다시 본다.
        self.wake();
        Ok(())
    }

    /// 보낼 본문과 붙일 것(인용문 그림·로컬 보관본 전달의 원본 첨부·붙여 넣은 그림·파일).
    fn material(&self, d: &Draft, server_kind: Option<&str>) -> Result<Material> {
        let mode = mode_of(&d.kind);
        let reference = match d.ref_message_id {
            Some(id) => self.store().message(id)?,
            None => None,
        };
        let original = reference
            .as_ref()
            .and_then(|r| r.eml_hash.as_deref())
            .and_then(|h| self.store().eml().get(h).ok())
            .and_then(|raw| compose::original(&raw));
        // 인용문: 작성 창에서 고쳤으면 고친 것(0.9.0), 아니면 원문으로 만든다.
        let quote = match (&original, d.include_quote, mode) {
            (Some(o), true, Some(_)) => match (d.quote_html.as_deref(), d.ref_message_id) {
                (Some(edited), Some(ref_id)) => {
                    compose::edited_quote(edited, o, &format!("{QUOTE_CID_BASE}{ref_id}/"))
                }
                _ => compose::quote_html(o),
            },
            _ => String::new(),
        };
        // 새로 쓴 부분에 기본 글꼴을 입힌다(받는 쪽에서 앱 글꼴과 같게). 빈 문단은 줄로 남게.
        // 작성 화면에 붙여 넣은 그림(data:)은 본문 그림(cid) 인라인 첨부로 바꿔 함께 올린다(고친 인용문에 붙인 것도).
        let (user, mut pasted) =
            compose::inline_data_images(&d.body_html.replace("<p></p>", "<p><br></p>"));
        let (quote, quote_pasted) = compose::inline_data_images(&quote);
        for p in quote_pasted {
            if !pasted.iter().any(|x| x.content_id == p.content_id) {
                pasted.push(p);
            }
        }
        let html = format!(
            "<div style=\"font-family:'맑은 고딕','Malgun Gothic',sans-serif;font-size:10pt;color:#000000\">{user}</div>{quote}"
        );
        let plain = d.options.plain_text;
        let mut uploads = Vec::new();
        // 서버 전달(ForwardItem)은 서버가 원본의 그림·첨부를 함께 넣는다.
        let server_forward = mode == Some(ReplyMode::Forward) && server_kind == Some("forward");
        if let Some(o) = &original {
            if !quote.is_empty() && !plain && !server_forward {
                // 고친 인용문에서 지운 그림은 붙이지 않는다.
                let lower = quote.to_ascii_lowercase();
                uploads.extend(
                    o.inline
                        .iter()
                        .filter(|p| {
                            p.content_id.as_deref().is_none_or(|c| {
                                lower.contains(&format!("cid:{}", c.to_ascii_lowercase()))
                            })
                        })
                        .map(|p| {
                            let cid = p.content_id.as_deref().unwrap_or(&p.name);
                            Upload::part(format!("quote:{cid}"), inline_part(p))
                        }),
                );
            }
            // 새 메일로 만든 전달(원본이 서버에 없음)은 원본 첨부를 직접 붙인다(작성 창에서 뺀 것은 빼고, 1.1.0).
            if mode == Some(ReplyMode::Forward) && server_kind == Some(NEW_MAIL) {
                uploads.extend(
                    o.attachments
                        .iter()
                        .enumerate()
                        .filter(|(i, a)| !d.excluded_orig.contains(&orig_key(*i, &a.name)))
                        .map(|(i, a)| {
                            Upload::part(
                                format!("orig:{i}:{}", a.name),
                                FileAttachment {
                                    name: a.name.clone(),
                                    content_type: a.mime.clone(),
                                    content: a.data.clone(),
                                    is_inline: false,
                                    content_id: None,
                                },
                            )
                        }),
                );
            }
        }
        if !plain {
            uploads.extend(pasted.iter().map(|p| {
                let cid = p.content_id.as_deref().unwrap_or(&p.name);
                Upload::part(
                    format!("img:{cid}:{}", fingerprint(&p.data)),
                    inline_part(p),
                )
            }));
        }
        for (name, size) in self.store().draft_files(d.id)? {
            uploads.push(Upload {
                key: format!("file:{name}:{size}"),
                source: Source::File(name),
            });
        }
        Ok(Material {
            body: if plain {
                compose::html_to_text(&html)
            } else {
                html
            },
            uploads,
            mode,
            reference,
            original,
        })
    }

    /// 원본 참조(ReferenceItemId)에는 최신 ChangeKey가 있어야 한다(없으면
    /// ErrorChangeKeyRequiredForWriteOperations, 6단계 실서버 확인). 초안을 만들기 직전에 서버에서 읽는다.
    /// 그사이 서버에서 사라졌으면 로컬 보관본처럼 새 메일 + 스레드 머리글로 만든다.
    async fn server_reference(&self, m: &Material) -> Result<Option<ItemId>> {
        let (Some(_), Some(sid)) = (
            m.mode,
            m.reference.as_ref().and_then(|r| r.server_item_id.clone()),
        ) else {
            return Ok(None);
        };
        match self
            .client()
            .get_items(&[bare(&sid)], &["Subject"])
            .await?
            .into_iter()
            .next()
        {
            Some(Ok(item)) => Ok(Some(item.item_id.unwrap_or_else(|| bare(&sid)))),
            Some(Err(e)) if e.code == NOT_FOUND => Ok(None),
            Some(Err(e)) => Err(Error::Owa(eo_owa::Error::Response {
                action: "GetItem".into(),
                code: e.code,
                message: e.message,
            })),
            None => Ok(None),
        }
    }

    /// 서버 초안을 새로 만든다(내용은 뒤에 `UpdateItem`으로 지정). 초안 ID와 만든 방식.
    async fn create_server_draft(&self, d: &Draft, m: &Material) -> Result<(ItemId, &'static str)> {
        let me = self.me().await?;
        let (to, cc, bcc) = (addrs(&d.to), addrs(&d.cc), addrs(&d.bcc));
        let (item, kind) = match (m.mode, self.server_reference(m).await?) {
            (Some(mode), Some(reference)) => {
                let kind = reply_kind(mode);
                let item = self
                    .client()
                    .create_reply_draft(&ReplyDraft {
                        kind,
                        reference: &reference,
                        from: &me,
                        to: &to,
                        cc: &cc,
                        bcc: &bcc,
                        subject: &d.subject,
                        body_html: "",
                    })
                    .await?;
                (item, kind.compose_operation())
            }
            (mode, _) => {
                let extended: Vec<(&str, String)> = match (mode, &m.original) {
                    (Some(ReplyMode::Reply | ReplyMode::ReplyAll), Some(o)) => {
                        compose::thread_headers(o)
                            .map(|(irt, refs)| {
                                vec![(PROP_IN_REPLY_TO, irt), (PROP_REFERENCES, refs)]
                            })
                            .unwrap_or_default()
                    }
                    _ => Vec::new(),
                };
                let item = self
                    .client()
                    .create_new_mail_draft(&NewMail {
                        from: &me,
                        to: &to,
                        cc: &cc,
                        bcc: &bcc,
                        subject: &d.subject,
                        body_html: "",
                        extended: &extended,
                    })
                    .await?;
                (item, NEW_MAIL)
            }
        };
        self.store().set_draft_server_id(d.id, &item.id, kind)?;
        Ok((item, kind))
    }

    /// 서버 초안을 준비한다: 없으면 만들고, 첨부를 작성 화면과 맞춘다. 최신 초안 ID·만든 방식·보낼 본문.
    async fn prepare_server_draft(&self, d: &Draft) -> Result<(ItemId, String, String)> {
        let mut existing = None;
        if let Some(sid) = &d.server_draft_id {
            match self
                .client()
                .get_items(&[bare(sid)], &["Subject"])
                .await?
                .into_iter()
                .next()
            {
                Some(Ok(item)) => existing = Some(item.item_id.unwrap_or_else(|| bare(sid))),
                // 웹메일에서 지웠다: 새로 만든다.
                Some(Err(e)) if e.code == NOT_FOUND => self.store().forget_server_draft(d.id)?,
                Some(Err(e)) => {
                    return Err(Error::Owa(eo_owa::Error::Response {
                        action: "GetItem".into(),
                        code: e.code,
                        message: e.message,
                    }));
                }
                None => existing = Some(bare(sid)),
            }
        }
        let kind_now = existing
            .as_ref()
            .map(|_| d.server_kind.clone().unwrap_or_else(|| NEW_MAIL.into()));
        // 만들기 전에는 만든 방식을 아직 모른다: 원본 첨부를 붙일지만 달라지므로 만든 뒤에 다시 고른다.
        let mut m = self.material(d, kind_now.as_deref())?;
        let (mut item, kind) = match (existing, kind_now) {
            (Some(item), Some(kind)) => (item, kind),
            _ => {
                let (item, kind) = self.create_server_draft(d, &m).await?;
                m = self.material(d, Some(kind))?;
                (item, kind.to_owned())
            }
        };

        // 첨부 맞추기: 뺀 것은 지우고 새 것만 올린다. 서버 전달의 원본 첨부(`orig`로 시작하는 기록)는 서버가
        // 넣은 것이라 아래 `sync_forward_originals`가 따로 맞춘다.
        let server_forward = kind == "forward" && m.mode == Some(ReplyMode::Forward);
        let have = self.store().draft_uploads(d.id)?;
        let want: HashSet<&str> = m.uploads.iter().map(|u| u.key.as_str()).collect();
        let stale: Vec<(String, String)> = have
            .iter()
            .filter(|(k, _)| !(server_forward && k.starts_with("orig")))
            .filter(|(k, _)| !want.contains(k.as_str()))
            .map(|(k, a)| (k.clone(), a.clone()))
            .collect();
        if !stale.is_empty() {
            let ids: Vec<String> = stale.iter().map(|(_, a)| a.clone()).collect();
            let results = self.client().delete_attachments(&ids).await?;
            for ((key, _), r) in stale.iter().zip(results) {
                match r {
                    Ok(ck) => {
                        if ck.is_some() {
                            item.change_key = ck;
                        }
                    }
                    // 이미 없는 첨부(웹메일에서 뺐다 등)는 기록만 지운다.
                    Err(e) if e.code.contains("NotFound") => {}
                    Err(e) => {
                        return Err(Error::Owa(eo_owa::Error::Response {
                            action: "DeleteAttachment".into(),
                            code: e.code,
                            message: e.message,
                        }));
                    }
                }
                self.store().set_draft_upload(d.id, key, None)?;
            }
        }
        for u in m.uploads.iter().filter(|u| !have.contains_key(&u.key)) {
            let att = match &u.source {
                Source::Part(a) => a.clone(),
                Source::File(name) => FileAttachment {
                    content_type: content_type_of(name).to_owned(),
                    name: name.clone(),
                    content: std::fs::read(self.store().draft_dir(d.id).join(name)).map_err(
                        |e| Error::Task(format!("첨부 파일 '{name}'을(를) 읽을 수 없습니다: {e}")),
                    )?,
                    is_inline: false,
                    content_id: None,
                },
            };
            let (aid, _) = self.client().create_attachment(&item, &att).await?;
            if let Some(ck) = aid.root_item_change_key.clone() {
                item.change_key = Some(ck);
            }
            self.store().set_draft_upload(d.id, &u.key, Some(&aid.id))?;
        }
        if server_forward {
            self.sync_forward_originals(d, &m, &mut item).await?;
        }
        Ok((item, kind, m.body))
    }

    /// 서버 전달(ForwardItem) 초안의 원본 첨부를 작성 창과 맞춘다(1.1.0): 뺀 원본 첨부는 서버 초안에서 지우고,
    /// 뺐다가 되돌린 것은 로컬 원문에서 다시 올린다. 서버 첨부는 원본 첨부와 순서대로 이름으로 짝짓는다.
    /// 기록: `orig:순번:이름` = 다시 올린 첨부 ID, `origdel:순번:이름` = 서버에서 지웠음(되돌리면 다시 올린다).
    async fn sync_forward_originals(
        &self,
        d: &Draft,
        m: &Material,
        item: &mut ItemId,
    ) -> Result<()> {
        let Some(o) = &m.original else {
            return Ok(());
        };
        let have = self.store().draft_uploads(d.id)?;
        if d.excluded_orig.is_empty() && !have.keys().any(|k| k.starts_with("orig")) {
            return Ok(());
        }
        let server = self
            .client()
            .get_items(std::slice::from_ref(item), &["Attachments"])
            .await?
            .into_iter()
            .next()
            .and_then(std::result::Result::ok)
            .and_then(|i| i.attachments)
            .unwrap_or_default();
        // (이름, ID, 본문 그림)
        let mut free: Vec<(String, String, bool)> = server
            .into_iter()
            .filter_map(|a| {
                Some((
                    a.name.unwrap_or_default(),
                    a.attachment_id?.id,
                    a.is_inline.unwrap_or(false),
                ))
            })
            .collect();
        let keys: Vec<String> = o
            .attachments
            .iter()
            .enumerate()
            .map(|(i, a)| orig_key(i, &a.name))
            .collect();
        // 1) 앱이 다시 올린 것은 기록한 ID로, 2) 나머지는 이름으로(일반 첨부 먼저).
        let mut found: Vec<Option<String>> = vec![None; keys.len()];
        for (i, key) in keys.iter().enumerate() {
            if let Some(aid) = have.get(&format!("orig:{key}"))
                && let Some(p) = free.iter().position(|(_, id, _)| id == aid)
            {
                found[i] = Some(free.remove(p).1);
            }
        }
        for inline in [false, true] {
            for (i, a) in o.attachments.iter().enumerate() {
                if found[i].is_some() || have.contains_key(&format!("origdel:{}", keys[i])) {
                    continue;
                }
                if let Some(p) = free
                    .iter()
                    .position(|(n, _, il)| *il == inline && n == &a.name)
                {
                    found[i] = Some(free.remove(p).1);
                }
            }
        }
        let mut drop: Vec<(usize, String)> = Vec::new();
        for (i, key) in keys.iter().enumerate() {
            let excluded = d.excluded_orig.contains(key);
            match (&found[i], excluded) {
                (Some(aid), true) => drop.push((i, aid.clone())),
                (None, false) if have.contains_key(&format!("origdel:{key}")) => {
                    let a = &o.attachments[i];
                    let att = FileAttachment {
                        name: a.name.clone(),
                        content_type: a.mime.clone(),
                        content: a.data.clone(),
                        is_inline: false,
                        content_id: None,
                    };
                    let (aid, _) = self.client().create_attachment(item, &att).await?;
                    if let Some(ck) = aid.root_item_change_key.clone() {
                        item.change_key = Some(ck);
                    }
                    self.store()
                        .set_draft_upload(d.id, &format!("orig:{key}"), Some(&aid.id))?;
                    self.store()
                        .set_draft_upload(d.id, &format!("origdel:{key}"), None)?;
                }
                _ => {}
            }
        }
        if !drop.is_empty() {
            let ids: Vec<String> = drop.iter().map(|(_, a)| a.clone()).collect();
            let results = self.client().delete_attachments(&ids).await?;
            for ((i, _), r) in drop.iter().zip(results) {
                match r {
                    Ok(ck) => {
                        if ck.is_some() {
                            item.change_key = ck;
                        }
                    }
                    Err(e) if e.code.contains("NotFound") => {}
                    Err(e) => {
                        return Err(Error::Owa(eo_owa::Error::Response {
                            action: "DeleteAttachment".into(),
                            code: e.code,
                            message: e.message,
                        }));
                    }
                }
                let key = &keys[*i];
                self.store()
                    .set_draft_upload(d.id, &format!("orig:{key}"), None)?;
                self.store()
                    .set_draft_upload(d.id, &format!("origdel:{key}"), Some("-"))?;
            }
        }
        Ok(())
    }

    /// 작성 중 메일을 서버 임시 보관함에 올린다(작업 큐 `draft_save`). 네트워크 오류는 `Err`(다시 시도),
    /// 서버가 거부하면 `Rejected`.
    pub(crate) async fn save_draft_to_server(&self, draft_id: i64) -> Result<Outcome> {
        let Some(d) = self.store().draft(draft_id)? else {
            return Ok(Outcome::Done); // 보냈거나 버림
        };
        if d.state == draft_state::OUTBOX || (!d.server_dirty && d.server_draft_id.is_some()) {
            return Ok(Outcome::Done);
        }
        let version = d.updated_at;
        let result = async {
            let (item, kind, body) = self.prepare_server_draft(&d).await?;
            let (to, cc, bcc) = (addrs(&d.to), addrs(&d.cc), addrs(&d.bcc));
            let saved = self
                .client()
                .update_draft(&item, &fields(&d, &to, &cc, &bcc, &body), &kind, false)
                .await?;
            Ok::<ItemId, Error>(saved.unwrap_or(item))
        }
        .await;
        match result {
            Ok(id) => {
                self.store().finish_draft_upload(
                    d.id,
                    &id.id,
                    id.change_key.as_deref(),
                    version,
                )?;
                if let Some(mid) = d.message_id
                    && let Some(row) = self.store().message(mid)?
                {
                    self.emit(Event::MessagesChanged {
                        folder_id: row.folder_id,
                        message_ids: vec![mid],
                    });
                }
                self.emit(Event::DraftsChanged);
                Ok(Outcome::Done)
            }
            Err(e) if crate::ops::transient(&e) => Err(e),
            Err(Error::Owa(eo_owa::Error::LoginRequired)) => {
                Err(Error::Owa(eo_owa::Error::LoginRequired))
            }
            Err(e) => Ok(Outcome::Rejected(e.to_string())),
        }
    }

    /// 작성 중 메일 하나를 보낸다. 네트워크 오류는 `Err`(다시 시도), 서버가 거부하면 `Fatal`.
    pub(crate) async fn send_draft(&self, draft_id: i64) -> Result<Outcome> {
        let Some(d) = self.store().draft(draft_id)? else {
            return Ok(Outcome::Done); // 이미 보냈거나 버림
        };
        if d.state != draft_state::OUTBOX {
            return Ok(Outcome::Done);
        }
        // 지난 시도에서 보내기 요청까지 갔다: 초안이 사라졌으면 응답만 못 받고 이미 보낸 것이다.
        if d.send_started
            && let Some(sid) = &d.server_draft_id
        {
            let found = self.client().get_items(&[bare(sid)], &["Subject"]).await?;
            if matches!(found.first(), Some(Err(e)) if e.code == NOT_FOUND) {
                self.finish_sent(&d)?;
                return Ok(Outcome::Done);
            }
        }
        match self.send_once(&d).await {
            Ok(()) => {
                self.finish_sent(&d)?;
                Ok(Outcome::Done)
            }
            Err(e) if crate::ops::transient(&e) => Err(e),
            Err(Error::Owa(eo_owa::Error::LoginRequired)) => {
                Err(Error::Owa(eo_owa::Error::LoginRequired))
            }
            Err(e) => {
                // 서버가 거부: 초안은 임시 보관함에 그대로 두고 사용자에게 돌려준다.
                let why = e.to_string();
                self.store().set_draft_send_started(draft_id, false)?;
                self.store().mark_draft_failed(draft_id, Some(&why))?;
                self.emit(Event::SendResult {
                    draft_id,
                    error: Some(why.clone()),
                });
                Ok(Outcome::Fatal(why))
            }
        }
    }

    async fn send_once(&self, d: &Draft) -> Result<()> {
        let (item, kind, body) = self.prepare_server_draft(d).await?;
        let (to, cc, bcc) = (addrs(&d.to), addrs(&d.cc), addrs(&d.bcc));
        self.store().set_draft_send_started(d.id, true)?;
        self.client()
            .update_draft(&item, &fields(d, &to, &cc, &bcc, &body), &kind, true)
            .await?;
        Ok(())
    }
}

fn fields<'a>(
    d: &'a Draft,
    to: &'a [Address],
    cc: &'a [Address],
    bcc: &'a [Address],
    body: &'a str,
) -> DraftFields<'a> {
    DraftFields {
        to,
        cc,
        bcc,
        subject: &d.subject,
        body,
        plain_text: d.options.plain_text,
        importance: d.options.importance,
        sensitivity: d.options.sensitivity,
        read_receipt: d.options.read_receipt,
        delivery_receipt: d.options.delivery_receipt,
    }
}

#[cfg(test)]
mod tests {
    use super::{content_type_of, fingerprint};

    #[test]
    fn content_types() {
        assert_eq!(content_type_of("보고서.PDF"), "application/pdf");
        assert_eq!(content_type_of("a.hwp"), "application/x-hwp");
        assert_eq!(content_type_of("noext"), "application/octet-stream");
    }

    #[test]
    fn fingerprint_is_stable() {
        assert_eq!(fingerprint(b""), "cbf29ce484222325");
        assert_ne!(fingerprint(b"a"), fingerprint(b"b"));
    }
}
