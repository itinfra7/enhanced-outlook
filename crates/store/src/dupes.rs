//! 중복 메일 후보(0.9.0 사용자 요청: 도구 → 중복 메일, 설정 → 자동 정리 "중복 메일 자동 삭제").
//!
//! 제목·보낸 사람·받는 사람·참조가 같은 메일을 묶는다. 본문은 원문으로 `eo_sync::exact_duplicates`가 비교한다.
//! - 제목은 공백만 하나로(대소문자는 그대로), 보낸 사람은 주소(소문자), 받는 사람·참조는 주소 모음(순서 무시).
//! - 지운 편지함·정크 메일·임시 보관함·보낼 편지함·HCL Domino 지운 메일·미발송 메일은 보지 않는다.
//! - 보낸 편지함(HCL Domino 보낸 메일 포함)은 다른 폴더와 따로 묶는다: 나를 참조로 넣어 보내면 받은 메일이 보낸
//!   메일과 머리글·본문이 같다(같은 메일을 보낸 쪽과 받은 쪽에 하나씩 가진 것이지 중복이 아니다).
//!
//! 0.7.0~0.8.4의 같은 폴더 중복 규칙과 HCL Domino 전달 사본 규칙은 0.9.0에서 뺐다(사용자 요청).

use std::collections::{HashMap, HashSet};

use crate::model::server_state;
use crate::{Result, Store};

/// 중복 후보 한 통.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DupRow {
    pub id: i64,
    pub date: i64,
    /// 서버에 있다(서버에서 지워진 보관본·가져온 메일이 아님).
    pub on_server: bool,
    /// 원문(없으면 본문을 비교할 수 없다).
    pub eml_hash: Option<String>,
}

impl DupRow {
    /// 남길 순서: 받은 시각이 이른 것, 같으면 서버에 있는 것, 같으면 먼저 들어온 것. 사용자 데이터에서 서버의 다른
    /// 폴더로 옮긴 메일과 받은 편지함에 남은 보관본이 같은 시각이었는데, 번호만 보면 보관본을 남길 뻔했다.
    fn order(&self) -> (i64, bool, i64) {
        (self.date, !self.on_server, self.id)
    }
}

/// 찾기 단계(화면 진행 표시용).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DupStage {
    /// 메일 목록 읽기(한 것·전체 = 메일 수)
    Read,
    /// 본문 비교(원문 읽은 메일 수)
    Body,
}

/// 진행 알림: 단계, 한 것, 전체.
pub type DupProgress<'a> = &'a mut dyn FnMut(DupStage, usize, usize);

/// 진행을 알릴 간격(메일 수).
const PROGRESS_EVERY: usize = 2000;

/// 공백을 하나로, 앞뒤 공백 없이.
fn squeeze(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 받는 사람·참조 → 주소 모음(소문자, 순서·중복 무시). "이름 <주소>; 주소; …".
fn address_key(list: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    for part in list.split(';') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        match (part.rfind('<'), part.rfind('>')) {
            (Some(a), Some(b)) if a < b => out.push(part[a + 1..b].trim().to_lowercase()),
            // 쉼표로 이은 주소들.
            _ if part.contains(',') && part.contains('@') => out.extend(
                part.split(',')
                    .map(|x| x.trim().to_lowercase())
                    .filter(|x| !x.is_empty()),
            ),
            _ => out.push(squeeze(part).to_lowercase()),
        }
    }
    out.sort();
    out.dedup();
    out.join(";")
}

/// 묶는 열쇠: (보낸 편지함 여부, 제목, 보낸 사람, 받는 사람, 참조).
type Key = (bool, String, String, String, String);

/// 머리글 열쇠(맞춘 값을 이은 글)와 그 묶음.
pub type KeyedRows = (String, Vec<DupRow>);

impl Store {
    /// 중복을 찾을 때 빼는 폴더와 보낸 편지함들.
    fn dup_folders(&self) -> Result<(HashSet<i64>, HashSet<i64>)> {
        let mut skip = HashSet::new();
        for wk in ["deleteditems", "junkemail", "drafts", "outbox"] {
            if let Some(f) = self.folder_by_well_known(wk)? {
                skip.insert(f.id);
            }
        }
        let mut sent = HashSet::new();
        if let Some(f) = self.folder_by_well_known("sentitems")? {
            sent.insert(f.id);
        }
        for (f, b) in self.domino_folder_boxes()? {
            match b.as_str() {
                "XML_Trash" | "XML_Draft" => {
                    skip.insert(f);
                }
                "XML_Sent" => {
                    sent.insert(f);
                }
                _ => {}
            }
        }
        Ok((skip, sent))
    }

    /// 제목·보낸 사람·받는 사람·참조가 같은 메일 묶음(두 통 이상, 남길 순서 [`DupRow::order`])과 살펴본 메일 수.
    /// 묶음마다 머리글 열쇠(맞춘 값을 이은 글, 다시 찾아도 같다: 화면의 "제외"를 저장하는 데 쓴다)와 함께.
    pub fn duplicate_candidates(
        &self,
        progress: DupProgress<'_>,
    ) -> Result<(Vec<KeyedRows>, usize)> {
        let (skip, sent) = self.dup_folders()?;
        let conn = self.conn();
        let from = "FROM messages m JOIN folders f ON f.id = m.folder_id \
                    WHERE f.is_mail = 1 AND m.is_draft = 0";
        let total: i64 = conn.query_row(&format!("SELECT COUNT(*) {from}"), [], |r| r.get(0))?;
        let total = usize::try_from(total).unwrap_or(0);
        progress(DupStage::Read, 0, total);
        let mut stmt = conn.prepare(&format!(
            "SELECT m.id, m.folder_id, COALESCE(m.subject, ''), \
               COALESCE(NULLIF(trim(m.from_addr), ''), m.from_name, ''), \
               COALESCE(m.to_list, ''), COALESCE(m.cc_list, ''), m.date_received, m.eml_hash, \
               m.server_item_id IS NOT NULL AND m.server_state != {} {from}",
            server_state::ARCHIVED
        ))?;
        let mut rows = stmt.query([])?;
        let mut groups: HashMap<Key, Vec<DupRow>> = HashMap::new();
        let mut seen = 0usize;
        while let Some(r) = rows.next()? {
            seen += 1;
            if seen.is_multiple_of(PROGRESS_EVERY) {
                progress(DupStage::Read, seen, total.max(seen));
            }
            let folder: i64 = r.get(1)?;
            if skip.contains(&folder) {
                continue;
            }
            let subject: String = r.get(2)?;
            let from: String = r.get(3)?;
            let to: String = r.get(4)?;
            let cc: String = r.get(5)?;
            let key = (
                sent.contains(&folder),
                squeeze(&subject),
                from.trim().to_lowercase(),
                address_key(&to),
                address_key(&cc),
            );
            groups.entry(key).or_default().push(DupRow {
                id: r.get(0)?,
                date: r.get(6)?,
                eml_hash: r.get(7)?,
                on_server: r.get(8)?,
            });
        }
        progress(DupStage::Read, seen, seen);
        let mut out: Vec<KeyedRows> = groups
            .into_iter()
            .filter(|(_, g)| g.len() > 1)
            .map(|((sent, subject, from, to, cc), mut g)| {
                g.sort_by_key(DupRow::order);
                let key = format!(
                    "{}\u{1f}{subject}\u{1f}{from}\u{1f}{to}\u{1f}{cc}",
                    u8::from(sent)
                );
                (key, g)
            })
            .collect();
        out.sort_by_key(|(_, g)| g[0].order());
        Ok((out, seen))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MessageProps, ServerFolder};

    fn p(subject: &str, from: &str, to: &str, cc: &str, at: i64) -> MessageProps {
        MessageProps {
            subject: Some(subject.into()),
            from_addr: Some(from.into()),
            to_list: Some(to.into()),
            cc_list: Some(cc.into()),
            date_received: at,
            ..MessageProps::default()
        }
    }

    #[test]
    fn candidates_share_subject_sender_and_recipients() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        let ids = s
            .transaction(|tx| {
                let f = |id: &str| ServerFolder {
                    server_id: id.into(),
                    server_parent_id: Some("ROOT".into()),
                    name: id.into(),
                    folder_class: Some("IPF.Note".into()),
                };
                let inbox = tx.upsert_folder(&f("INBOX"))?;
                let other = tx.upsert_folder(&f("OTHER"))?;
                let sent = tx.upsert_folder(&f("SENT"))?;
                let trash = tx.upsert_folder(&f("TRASH"))?;
                tx.rebuild_tree("ROOT")?;
                tx.set_well_known("SENT", "sentitems", 2)?;
                tx.set_well_known("TRASH", "deleteditems", 3)?;
                let to = "김 <Kim@x.com>; lee@x.com";
                Ok(vec![
                    // 0·1·2: 같은 메일(다른 폴더, 받는 사람 순서·이름 표기가 달라도).
                    tx.insert_message(
                        inbox,
                        "A1",
                        None,
                        &p("주간  보고", "Me@x.com", to, "", 2_000),
                    )?,
                    tx.insert_message(
                        other,
                        "A2",
                        None,
                        &p(
                            "주간 보고",
                            "me@x.com",
                            "lee@x.com; Kim <kim@x.com>",
                            "",
                            1_000,
                        ),
                    )?,
                    tx.insert_message(
                        inbox,
                        "A3",
                        None,
                        &p("주간 보고 ", "me@x.com", to, "", 1_000),
                    )?,
                    // 3: 참조가 다름. 4: 제목 대소문자가 다름.
                    tx.insert_message(
                        inbox,
                        "B",
                        None,
                        &p("주간 보고", "me@x.com", to, "park@x.com", 3_000),
                    )?,
                    tx.insert_message(
                        inbox,
                        "C",
                        None,
                        &p("주간 Report", "me@x.com", to, "", 4_000),
                    )?,
                    tx.insert_message(
                        inbox,
                        "C2",
                        None,
                        &p("주간 report", "me@x.com", to, "", 5_000),
                    )?,
                    // 6: 보낸 편지함(받은 쪽과 따로), 7: 지운 편지함(보지 않음).
                    tx.insert_message(sent, "S", None, &p("주간 보고", "me@x.com", to, "", 500))?,
                    tx.insert_message(trash, "T", None, &p("주간 보고", "me@x.com", to, "", 400))?,
                ])
            })
            .unwrap();
        let mut seen = Vec::new();
        let (groups, n) = s
            .duplicate_candidates(&mut |st, done, total| seen.push((st, done, total)))
            .unwrap();
        assert_eq!(n, 8);
        let got: Vec<Vec<i64>> = groups
            .iter()
            .map(|(_, g)| g.iter().map(|r| r.id).collect())
            .collect();
        assert_eq!(
            got,
            vec![vec![ids[1], ids[2], ids[0]]],
            "받은 시각·번호 순, 같은 시각이면 먼저 들어온 것부터"
        );
        assert_eq!(
            groups[0].0, "0\u{1f}주간 보고\u{1f}me@x.com\u{1f}kim@x.com;lee@x.com\u{1f}",
            "머리글 열쇠는 맞춘 값(다시 찾아도 같다)"
        );
        assert_eq!(seen.last(), Some(&(DupStage::Read, 8, 8)));
        assert_eq!(
            address_key("\"Kim, J\" <K@x.com>; a@x.com, b@x.com"),
            "a@x.com;b@x.com;k@x.com"
        );
    }

    #[test]
    fn same_time_keeps_the_server_copy_first() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        let inbox = s
            .transaction(|tx| {
                let f = tx.upsert_folder(&ServerFolder {
                    server_id: "INBOX".into(),
                    server_parent_id: Some("ROOT".into()),
                    name: "받은 편지함".into(),
                    folder_class: Some("IPF.Note".into()),
                })?;
                tx.rebuild_tree("ROOT")?;
                Ok(f)
            })
            .unwrap();
        let props = p("회의", "kim@x.com", "me@x.com", "", 1_000);
        // 먼저 들어온 가져온 사본(번호가 작음), 나중에 동기화한 서버 메일(같은 시각).
        let crate::ImportOutcome::Imported(local) =
            s.import_message(inbox, b"x", &props, "eml").unwrap()
        else {
            panic!("가져오지 못함")
        };
        let server = s
            .transaction(|tx| tx.insert_message(inbox, "S1", None, &props))
            .unwrap();
        assert!(local < server);
        let (groups, _) = s.duplicate_candidates(&mut |_, _, _| {}).unwrap();
        let got: Vec<i64> = groups[0].1.iter().map(|r| r.id).collect();
        assert_eq!(got, vec![server, local]);
    }
}
