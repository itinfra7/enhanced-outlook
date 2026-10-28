//! 가져오기(계획서 10장 7단계)의 저장소 쪽: 백업의 폴더 경로를 로컬 폴더에 맞추고, 로컬 전용 메일로 넣는다.
//!
//! - 사용자 결정(2026-10-16): **같은 이름의 기존 폴더에 합친다.** 백업의 "받은 편지함"은 지금의 받은 편지함으로
//!   (Inbox 같은 다른 이름도 잘 알려진 폴더로 맞춘다). 없으면 로컬 폴더(서버 ID 없음)를 만든다.
//! - 가져온 메일은 `server_state = 2`(로컬 전용), 서버에 올리지 않는다. 출처는 `origin`(`import`, `html`).
//! - 같은 원문(바이트가 같은 EML)이 이미 있거나 같은 폴더에 같은 Message-ID가 있으면 건너뛴다.

use rusqlite::{OptionalExtension, params};

use crate::{MessageProps, Result, Store, server_state};

/// 잘 알려진 폴더의 다른 이름들(영문·띄어쓰기 없는 표기).
const ALIASES: &[(&str, &[&str])] = &[
    ("inbox", &["inbox", "받은 편지함", "받은편지함"]),
    (
        "sentitems",
        &[
            "sent items",
            "sent",
            "보낸 편지함",
            "보낸편지함",
            "보낸 메일",
        ],
    ),
    (
        "deleteditems",
        &[
            "deleted items",
            "지운 편지함",
            "지운편지함",
            "삭제된 편지함",
            "삭제된 항목",
        ],
    ),
    ("drafts", &["drafts", "임시 보관함", "임시보관함"]),
    (
        "junkemail",
        &["junk email", "junk", "정크 메일", "정크메일", "스팸 메일"],
    ),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportOutcome {
    Imported(i64),
    Duplicate,
}

fn norm(s: &str) -> String {
    s.trim().to_lowercase()
}

impl Store {
    /// 폴더 경로(위에서부터 이름들)를 로컬 폴더로 맞춘다. 없으면 만든다. (폴더 번호, 새로 만든 경로들)
    pub fn find_or_create_folder_path(&self, path: &[String]) -> Result<(i64, Vec<String>)> {
        let path: Vec<&str> = path
            .iter()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect();
        let path = if path.is_empty() {
            vec!["가져온 메일"]
        } else {
            path
        };
        self.transaction(|tx| {
            let mut created = Vec::new();
            let mut parent: Option<i64> = None;
            for (depth, name) in path.iter().enumerate() {
                let key = norm(name);
                let found: Option<i64> = match parent {
                    None => {
                        // 1) 맨 위의 같은 이름 2) 잘 알려진 폴더의 다른 이름 3) 어디든 같은 이름(하나만)
                        let top: Option<i64> = tx
                            .tx
                            .query_row(
                                "SELECT id FROM folders WHERE parent_id IS NULL AND is_mail = 1 \
                                 AND lower(trim(name)) = ?1 ORDER BY server_id IS NULL, sort_order, id LIMIT 1",
                                [&key],
                                |r| r.get(0),
                            )
                            .optional()?;
                        let wk = ALIASES
                            .iter()
                            .find(|(_, names)| names.contains(&key.as_str()))
                            .map(|(wk, _)| *wk);
                        let by_alias: Option<i64> = match (top, wk) {
                            (None, Some(wk)) => tx
                                .tx
                                .query_row(
                                    "SELECT id FROM folders WHERE well_known = ?1 AND is_mail = 1 \
                                     ORDER BY server_id IS NULL LIMIT 1",
                                    [wk],
                                    |r| r.get(0),
                                )
                                .optional()?,
                            _ => None,
                        };
                        let anywhere: Option<i64> = if top.is_none() && by_alias.is_none() {
                            let ids: Vec<i64> = {
                                let mut stmt = tx.tx.prepare(
                                    "SELECT id FROM folders WHERE is_mail = 1 AND lower(trim(name)) = ?1",
                                )?;
                                stmt.query_map([&key], |r| r.get(0))?
                                    .collect::<rusqlite::Result<_>>()?
                            };
                            (ids.len() == 1).then(|| ids[0])
                        } else {
                            None
                        };
                        top.or(by_alias).or(anywhere)
                    }
                    Some(p) => tx
                        .tx
                        .query_row(
                            "SELECT id FROM folders WHERE parent_id = ?1 AND is_mail = 1 \
                             AND lower(trim(name)) = ?2 ORDER BY server_id IS NULL, id LIMIT 1",
                            params![p, key],
                            |r| r.get(0),
                        )
                        .optional()?,
                };
                let id = match found {
                    Some(id) => id,
                    None => {
                        tx.tx.execute(
                            "INSERT INTO folders(server_id, parent_id, name, folder_class, is_mail, sort_order) \
                             VALUES (NULL, ?1, ?2, 'IPF.Note', 1, 200)",
                            params![parent, name],
                        )?;
                        created.push(path[..=depth].join("/"));
                        tx.tx.last_insert_rowid()
                    }
                };
                parent = Some(id);
            }
            Ok((parent.unwrap_or_default(), created))
        })
    }

    /// 로컬 전용 메일로 넣는다. 같은 원문이 이미 있거나 같은 폴더에 같은 Message-ID가 있으면 건너뛴다.
    pub fn import_message(
        &self,
        folder_id: i64,
        raw: &[u8],
        p: &MessageProps,
        origin: &str,
    ) -> Result<ImportOutcome> {
        let hash = crate::sha256_hex(raw);
        {
            let conn = self.conn();
            let same_raw: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM messages WHERE eml_hash = ?1)",
                [&hash],
                |r| r.get(0),
            )?;
            // HCL Domino(노츠)는 다시 보낸 메일 등 서로 다른 메일이 같은 Message-ID를 쓰기도 한다(실데이터 보낸 메일 57통).
            // 그래서 HCL Domino 백업은 받은 날짜까지 같아야 같은 메일로 본다(같은 메일을 두 번 저장한 사본만 합쳐진다).
            let by_date = origin.starts_with("domino");
            let same_id = match &p.internet_msg_id {
                Some(imid) if by_date => conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM messages WHERE folder_id = ?1 AND internet_msg_id = ?2 \
                     AND date_received = ?3)",
                    params![folder_id, imid, p.date_received],
                    |r| r.get::<_, bool>(0),
                )?,
                Some(imid) => conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM messages WHERE folder_id = ?1 AND internet_msg_id = ?2)",
                    params![folder_id, imid],
                    |r| r.get::<_, bool>(0),
                )?,
                None => false,
            };
            if same_raw || same_id {
                return Ok(ImportOutcome::Duplicate);
            }
        }
        let stored = self.eml().put(raw)?;
        let conn = self.conn();
        conn.execute(
            "INSERT INTO messages(folder_id, server_item_id, change_key, server_state, internet_msg_id, \
               conversation_id, subject, from_name, from_addr, to_list, cc_list, date_received, date_sent, \
               size, has_attachments, is_read, flag, importance, is_draft, item_class, preview, eml_hash, \
               indexed, origin) \
             VALUES (?1, NULL, NULL, ?2, ?3, NULL, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, 0, \
               ?16, ?17, ?18, 0, ?19)",
            params![
                folder_id,
                server_state::LOCAL_ONLY,
                p.internet_msg_id,
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
                p.item_class,
                p.preview,
                stored,
                origin
            ],
        )?;
        Ok(ImportOutcome::Imported(conn.last_insert_rowid()))
    }
}

impl Store {
    /// 이 출처(`origin`)로 가져온 메일 ID들.
    pub fn message_ids_by_origin(&self, origin: &str) -> Result<Vec<i64>> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT id FROM messages WHERE origin = ?1 ORDER BY id")?;
        let rows = stmt.query_map([origin], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// 모든 메일 ID(진단용).
    pub fn list_all_ids(&self) -> Result<Vec<i64>> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT id FROM messages ORDER BY id")?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// 제목에 이 글이 든 메일 ID들(진단용, 최근 것부터 `limit`개).
    pub fn message_ids_by_subject(&self, needle: &str, limit: usize) -> Result<Vec<i64>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id FROM messages WHERE instr(subject, ?1) > 0 ORDER BY date_received DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(
            params![needle, i64::try_from(limit).unwrap_or(i64::MAX)],
            |r| r.get(0),
        )?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// 가져온 메일(로컬 전용)의 원문을 바꾼다(글자 깨짐 바로잡기 등). 목록 속성은 새 원문 것으로,
    /// 폴더·읽음·플래그는 그대로 두고 다시 색인하게 표시한다. 가져온 메일이 아니면 false.
    pub fn replace_imported_eml(&self, id: i64, raw: &[u8], p: &MessageProps) -> Result<bool> {
        let Some(old) = self.message(id)? else {
            return Ok(false);
        };
        if old.server_state != server_state::LOCAL_ONLY || old.origin.is_none() {
            return Ok(false);
        }
        let hash = self.eml().put(raw)?;
        self.conn().execute(
            "UPDATE messages SET eml_hash = ?1, internet_msg_id = ?2, subject = ?3, from_name = ?4, from_addr = ?5, \
               to_list = ?6, cc_list = ?7, date_sent = ?8, size = ?9, has_attachments = ?10, preview = ?11, \
               indexed = 0 WHERE id = ?12",
            params![
                hash,
                p.internet_msg_id,
                p.subject,
                p.from_name,
                p.from_addr,
                p.to_list,
                p.cc_list,
                p.date_sent,
                p.size,
                p.has_attachments,
                p.preview,
                id
            ],
        )?;
        if let Some(h) = old.eml_hash
            && h != hash
        {
            self.remove_eml_if_unused(&h)?;
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ServerFolder;

    /// 서버 메일 한 통(가져온 메일이 아닌 것)을 만든다.
    fn id_server_like(s: &Store) -> i64 {
        s.transaction(|tx| {
            let f = tx.upsert_folder(&ServerFolder {
                server_id: "SV".into(),
                server_parent_id: Some("ROOT".into()),
                name: "서버".into(),
                folder_class: None,
            })?;
            tx.insert_message(f, "S-1", None, &MessageProps::default())
        })
        .unwrap()
    }

    #[test]
    fn folders_merge_by_name_and_alias_and_create_missing() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        let inbox = s
            .transaction(|tx| {
                let f = |id: &str, name: &str, parent: &str| ServerFolder {
                    server_id: id.into(),
                    server_parent_id: Some(parent.into()),
                    name: name.into(),
                    folder_class: Some("IPF.Note".into()),
                };
                let inbox = tx.upsert_folder(&f("INBOX", "받은 편지함", "ROOT"))?;
                tx.upsert_folder(&f("PRJ", "프로젝트", "INBOX"))?;
                tx.set_well_known("INBOX", "inbox", 0)?;
                tx.rebuild_tree("ROOT")?;
                Ok(inbox)
            })
            .unwrap();
        let p = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            s.find_or_create_folder_path(&p(&["받은 편지함"])).unwrap(),
            (inbox, vec![])
        );
        assert_eq!(
            s.find_or_create_folder_path(&p(&["Inbox"])).unwrap().0,
            inbox,
            "다른 이름"
        );
        // 받은 편지함 아래의 "프로젝트"는 어디든 하나뿐이라 맨 위에서 찾아도 합친다.
        let (prj, created) = s.find_or_create_folder_path(&p(&["프로젝트"])).unwrap();
        assert!(created.is_empty());
        let (sub, created) = s
            .find_or_create_folder_path(&p(&["프로젝트", "2025 결산"]))
            .unwrap();
        assert_eq!(created, ["프로젝트/2025 결산"]);
        assert_eq!(s.folder(sub).unwrap().unwrap().parent_id, Some(prj));
        let (old, created) = s.find_or_create_folder_path(&p(&["옛날 편지함"])).unwrap();
        assert_eq!(created, ["옛날 편지함"]);
        let f = s.folder(old).unwrap().unwrap();
        assert!(f.server_id.is_none() && f.is_mail && f.parent_id.is_none());
        assert_eq!(
            s.find_or_create_folder_path(&p(&["옛날 편지함"])).unwrap(),
            (old, vec![]),
            "다시 가져와도 같은 폴더"
        );
    }

    #[test]
    fn import_dedupes_by_raw_and_message_id() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        let (f, _) = s.find_or_create_folder_path(&["가져옴".into()]).unwrap();
        let props = MessageProps {
            internet_msg_id: Some("<a@x>".into()),
            subject: Some("제목".into()),
            date_received: 1,
            is_read: true,
            ..MessageProps::default()
        };
        let first = s.import_message(f, b"raw-1", &props, "import").unwrap();
        let ImportOutcome::Imported(id) = first else {
            panic!()
        };
        let m = s.message(id).unwrap().unwrap();
        assert_eq!(
            (m.server_state, m.origin.as_deref(), m.server_item_id),
            (2, Some("import"), None)
        );
        assert_eq!(
            s.import_message(f, b"raw-1", &props, "import").unwrap(),
            ImportOutcome::Duplicate
        );
        assert_eq!(
            s.import_message(f, b"raw-2", &props, "import").unwrap(),
            ImportOutcome::Duplicate,
            "같은 폴더·같은 Message-ID"
        );
        // HCL Domino: 같은 Message-ID라도 날짜가 다르면 다른 메일(다시 보낸 메일), 날짜까지 같으면 사본.
        let (bf, _) = s
            .find_or_create_folder_path(&["HCL Domino".into()])
            .unwrap();
        let resent = MessageProps {
            date_received: 2,
            ..props.clone()
        };
        assert!(matches!(
            s.import_message(bf, b"bn-1", &props, "domino").unwrap(),
            ImportOutcome::Imported(_)
        ));
        assert!(matches!(
            s.import_message(bf, b"bn-2", &resent, "domino").unwrap(),
            ImportOutcome::Imported(_)
        ));
        assert_eq!(
            s.import_message(bf, b"bn-3", &resent, "domino-eml")
                .unwrap(),
            ImportOutcome::Duplicate,
            "같은 Message-ID·같은 날짜"
        );
        // 가져온 메일의 원문 바꾸기: 읽음·플래그는 그대로, 목록 속성은 새것, 옛 원문 파일은 지움, 다시 색인.
        let ImportOutcome::Imported(bn) = s
            .import_message(
                bf,
                b"bn-fix-old",
                &MessageProps {
                    date_received: 9,
                    ..props.clone()
                },
                "domino-eml",
            )
            .unwrap()
        else {
            panic!()
        };
        s.mark_flag(&[bn], 1, 1).unwrap();
        let old_hash = s.message(bn).unwrap().unwrap().eml_hash.unwrap();
        let fixed = MessageProps {
            subject: Some("바로잡은 제목".into()),
            date_received: 9,
            ..props.clone()
        };
        assert_eq!(s.message_ids_by_origin("domino-eml").unwrap(), [bn]);
        assert!(s.replace_imported_eml(bn, b"bn-fix-new", &fixed).unwrap());
        let m = s.message(bn).unwrap().unwrap();
        assert_eq!(
            (m.props.subject.as_deref(), m.props.flag),
            (Some("바로잡은 제목"), 1)
        );
        assert_ne!(m.eml_hash.as_deref(), Some(old_hash.as_str()));
        assert!(s.eml().get(&old_hash).is_err(), "옛 원문은 지움");
        assert!(
            !s.replace_imported_eml(id_server_like(&s), b"x", &fixed)
                .unwrap(),
            "가져온 메일만"
        );
        let other = MessageProps {
            internet_msg_id: None,
            ..props
        };
        assert!(matches!(
            s.import_message(f, b"raw-3", &other, "html").unwrap(),
            ImportOutcome::Imported(_)
        ));
        assert_eq!(
            s.index_todo(10).unwrap().len(),
            6,
            "가져온 메일은 색인 대기(일반 2 + HCL Domino 3) + 원문 없는 서버 메일 1(목록 속성만, 0.7.0)"
        );

        // 조작해도 서버 작업은 쌓이지 않고, 사본도 가져온 메일로 남는다.
        let (g, _) = s.find_or_create_folder_path(&["다른 폴더".into()]).unwrap();
        assert_eq!(s.mark_read(&[id], false, 1).unwrap().queued, 0);
        assert_eq!(s.move_messages(&[id], g, 1, None).unwrap().queued, 0);
        let t = s.copy_messages(&[id], f, 1, None).unwrap();
        let copy = s.message(t.created[0]).unwrap().unwrap();
        assert_eq!(
            (copy.server_state, copy.origin.as_deref(), copy.archived_at),
            (2, Some("import"), None)
        );
        let t = s.server_delete(&[id], 1, None).unwrap();
        assert!(t.is_empty() && t.queued == 0, "서버에서 지울 것이 없다");
        assert_eq!(s.message(id).unwrap().unwrap().server_state, 2);
        assert_eq!(s.pending_op_count().unwrap(), 0);
    }
}
