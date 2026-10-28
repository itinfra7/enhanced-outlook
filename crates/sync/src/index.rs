//! 색인 큐(계획서 7장 마지막 단계, 8.4 묶음 커밋): 아직 색인하지 않은 메일을 최근 것부터 500통씩
//! 원문 해석 → 색인 → 확정 → 색인 완료 표시. 로그인 없이도 돈다.
//! 원문이 없는 메일(아직 못 받음 등)도 목록 속성(제목·보낸 사람·받는 사람·참조)으로 먼저 색인한다(0.7.0 사용자
//! 지적: 어떤 메일은 검색이 안 됨). 원문이 오면 다시 색인한다.

use std::time::{Duration, Instant};

use eo_search::{MailDoc, SearchIndex};
use eo_store::{AddressLists, IndexTodo, Store};

use crate::Result;

/// 한 번에 확정할 묶음(계획서 8.4: 최대 500통).
const BATCH: usize = 500;

#[derive(Debug, Clone, Default)]
pub struct IndexReport {
    pub indexed: usize,
    /// 원문을 해석하지 못해 목록 속성만 색인한 수.
    pub parse_failed: usize,
    pub elapsed: Duration,
}

fn doc_for(store: &Store, t: &IndexTodo) -> (MailDoc, Option<AddressLists>) {
    let db_from = match (&t.from_name, &t.from_addr) {
        (Some(n), Some(a)) => format!("{n} <{a}>"),
        (n, a) => n.clone().or_else(|| a.clone()).unwrap_or_default(),
    };
    let mut doc = MailDoc {
        id: t.message_id as u64,
        folder: t.folder_id as u64,
        date: t.date_received,
        subject: t.subject.clone().unwrap_or_default(),
        from: db_from,
        to: t.to_list.clone().unwrap_or_default(),
        cc: t.cc_list.clone().unwrap_or_default(),
        ..MailDoc::default()
    };
    let extracted = t
        .eml_hash
        .as_deref()
        .and_then(|h| store.eml().get(h).ok())
        .and_then(|raw| eo_mime::extract(&raw));
    let Some(x) = extracted else {
        return (doc, None);
    };
    if !x.subject.is_empty() {
        doc.subject = x.subject;
    }
    if !x.from.is_empty() {
        doc.from = x.from;
    }
    doc.body = x.body_text;
    doc.to = x.to.clone();
    doc.cc = x.cc.clone();
    doc.bcc = x.bcc.clone();
    doc.attach = x.attachment_names.join(" ");
    (
        doc,
        Some(AddressLists {
            to: x.to,
            cc: x.cc,
            bcc: x.bcc,
        }),
    )
}

/// 색인 작업은 한 번에 하나만(앱 시작·동기화 주기·로컬 조작이 동시에 부를 수 있다).
static INDEX_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn index_lock() -> std::sync::MutexGuard<'static, ()> {
    INDEX_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// DB와 색인을 대조해 고친다: 완료로 표시됐는데 색인에 없는 메일은 다시 색인하게 표시하고,
/// DB에 없는 색인 문서는 지운다. (다시 색인할 수, 지운 문서 수)
/// 6단계에서 완료 표시만 되고 색인에 없는 메일이 생긴 일이 있어(원인 미확정) 앱 시작 때 한 번 돈다.
pub fn reconcile_index(store: &Store, search: &SearchIndex) -> Result<(usize, usize)> {
    let _g = index_lock();
    let in_index: std::collections::HashSet<u64> = search.all_ids()?.into_iter().collect();
    let marked = store.indexed_ids()?;
    let missing: Vec<i64> = marked
        .iter()
        .copied()
        .filter(|id| !in_index.contains(&(*id as u64)))
        .collect();
    // DB의 ID를 한 번에 읽어 비교한다. 문서마다 DB를 조회하면 원문 없이 색인된 메일이 많을 때
    // (합성 저장소 11만 통) 시작 직후 수 초 동안 DB 잠금을 화면과 다퉈 폴더 전환이 느려졌다(8단계 측정).
    let existing: std::collections::HashSet<u64> =
        store.message_ids()?.into_iter().map(|i| i as u64).collect();
    let mut stale = 0;
    for id in in_index.iter().filter(|id| !existing.contains(id)) {
        search.delete(*id)?;
        stale += 1;
    }
    if stale > 0 {
        search.commit()?;
    }
    store.mark_unindexed(&missing)?;
    Ok((missing.len(), stale))
}

/// 색인 대기 메일을 색인한다. `limit`이 있으면 그 수까지만.
pub fn index_pending(
    store: &Store,
    search: &SearchIndex,
    limit: Option<usize>,
) -> Result<IndexReport> {
    let _g = index_lock();
    let started = Instant::now();
    let mut report = IndexReport::default();
    loop {
        let want = limit.map_or(BATCH, |l| BATCH.min(l.saturating_sub(report.indexed)));
        if want == 0 {
            break;
        }
        let todo = store.index_todo(want)?;
        if todo.is_empty() {
            break;
        }
        let mut docs = Vec::with_capacity(todo.len());
        let mut marks = Vec::with_capacity(todo.len());
        for t in &todo {
            let (doc, lists) = doc_for(store, t);
            if lists.is_none() && t.eml_hash.is_some() {
                report.parse_failed += 1;
            }
            docs.push(doc);
            marks.push((t.message_id, lists, t.eml_hash.is_some()));
        }
        // 색인을 먼저 확정하고 표시한다(중간에 끊기면 다음에 다시 색인, 같은 ID는 덮어씀).
        search.add(&docs)?;
        search.commit()?;
        store.mark_indexed(&marks)?;
        report.indexed += todo.len();
    }
    report.elapsed = started.elapsed();
    Ok(report)
}

#[cfg(test)]
mod tests {
    use eo_search::SearchOptions;
    use eo_store::{MessageProps, ServerFolder};

    use super::*;

    #[test]
    fn indexes_eml_and_reindexes_after_move() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let search = SearchIndex::in_memory().unwrap();
        let (inbox, other, id) = store
            .transaction(|tx| {
                let f = |s: &str| ServerFolder {
                    server_id: s.into(),
                    server_parent_id: Some("R".into()),
                    name: s.into(),
                    folder_class: None,
                };
                let inbox = tx.upsert_folder(&f("IN"))?;
                let other = tx.upsert_folder(&f("OT"))?;
                let p = MessageProps {
                    subject: Some("목록 제목".into()),
                    date_received: 5,
                    ..MessageProps::default()
                };
                Ok((inbox, other, tx.insert_message(inbox, "S1", None, &p)?))
            })
            .unwrap();
        let hit = |q: &str, folder: Option<u64>| {
            search
                .search(
                    q,
                    &SearchOptions {
                        folder,
                        ..SearchOptions::default()
                    },
                )
                .unwrap()
                .ids
        };
        // 원문이 오기 전에도 목록 속성(제목)으로 찾힌다(0.7.0).
        let r = index_pending(&store, &search, None).unwrap();
        assert_eq!((r.indexed, r.parse_failed), (1, 0));
        assert_eq!(hit("목록", None), [id as u64]);
        assert!(hit("실적", None).is_empty(), "본문은 아직 없다");

        let eml = "From: a@x.com\r\nTo: b@y.com\r\nBcc: audit@z.com\r\nSubject: 월간보고서\r\n\r\n본문 실적";
        assert!(store.attach_eml(id, "S1", eml.as_bytes()).unwrap());
        let r = index_pending(&store, &search, None).unwrap();
        assert_eq!((r.indexed, r.parse_failed), (1, 0), "원문이 오면 다시 색인");
        assert_eq!(hit("보고서 실적", None), [id as u64]);
        assert_eq!(hit("숨은참조:audit", None), [id as u64]);
        let row = store.message(id).unwrap().unwrap();
        assert_eq!(
            row.props.to_list.as_deref(),
            Some("b@y.com"),
            "목록 행은 원문 주소로 보강"
        );
        assert_eq!(row.props.subject.as_deref(), Some("목록 제목"));
        assert_eq!(
            index_pending(&store, &search, None).unwrap().indexed,
            0,
            "다시 색인하지 않음"
        );

        // 이동하면 다시 색인해 폴더 값을 바꾼다.
        store
            .transaction(|tx| tx.update_message(id, Some((other, "S2")), None, &row.props))
            .unwrap();
        assert_eq!(index_pending(&store, &search, None).unwrap().indexed, 1);
        assert_eq!(hit("보고서", Some(other as u64)), [id as u64]);
        assert!(hit("보고서", Some(inbox as u64)).is_empty());

        // 대조: DB에 없는 색인 문서는 지우고, 완료 표시인데 색인에 없는 메일은 다시 색인하게 한다.
        search
            .add(&[MailDoc {
                id: 9_999,
                subject: "유령".into(),
                ..MailDoc::default()
            }])
            .unwrap();
        search.delete(id as u64).unwrap();
        search.commit().unwrap();
        assert_eq!(reconcile_index(&store, &search).unwrap(), (1, 1));
        assert!(hit("유령", None).is_empty());
        assert_eq!(index_pending(&store, &search, None).unwrap().indexed, 1);
        assert_eq!(hit("보고서", None), [id as u64]);
        assert_eq!(reconcile_index(&store, &search).unwrap(), (0, 0));
    }
}
