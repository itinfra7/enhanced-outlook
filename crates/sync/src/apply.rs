//! 한 동기화 주기의 항목 변경을 저장소에 반영한다(계획서 6.3 서버 삭제 판정).
//!
//! 1. 모든 폴더의 변경을 먼저 모은다(호출한 쪽).
//! 2. 삭제된 항목마다, 같은 주기에 생긴 항목과 Message-ID + 받은 시각으로 짝을 짓는다.
//!    짝이 있으면 이동: 같은 행의 폴더·항목 ID만 바꾸고 원문은 그대로 둔다.
//! 3. 짝이 없으면 원래 폴더에 그대로 두고 "보관"(server_state=1)으로 표시한다.
//!
//! 서버 반영을 기다리는 로컬 조작([`PendingGuard`])은 덮어쓰지 않는다: 이동 대기 메일의 폴더, 읽음·플래그
//! 대기 값, 서버 삭제 대기의 보관 표시를 지키고, 완전 삭제 대기 항목은 다시 만들지 않는다.
//! 서버의 새 항목이 서버 반영 전 사본과 같은 메일이면(복사가 새 ID를 주지 않은 경우) 그 사본과 잇는다.

use std::collections::BTreeMap;

use eo_owa::{ChangeKind, ItemChange};
use eo_store::{MessageProps, MessageRow, PendingGuard, Result, Tx};

use crate::convert;

/// 폴더 하나의 수집된 변경과 반영 뒤 저장할 SyncState.
#[derive(Debug, Clone)]
pub struct FolderChanges {
    pub folder_id: i64,
    pub changes: Vec<ItemChange>,
    pub new_state: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveRecord {
    pub message_id: i64,
    pub from_folder: i64,
    pub to_folder: i64,
    /// 이동 전에 이미 원문이 있었는가(있었으면 다시 받지 않는다).
    pub had_eml: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveRecord {
    pub message_id: i64,
    pub folder_id: i64,
    pub had_eml: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ApplyStats {
    pub created: usize,
    pub updated: usize,
    pub read_flags: usize,
    pub moves: Vec<MoveRecord>,
    pub archives: Vec<ArchiveRecord>,
    /// 모르는 항목에 대한 삭제·읽음 변경(이미 반영됐거나 받기 전의 항목).
    pub ignored: usize,
    /// 폴더별로 바뀐 메일(화면 갱신 이벤트용).
    pub touched: BTreeMap<i64, Vec<i64>>,
    /// 로컬에서도 지운 메일(서버에서 사라진 임시 보관함 초안)과 그 원문 해시(쓰는 행이 없으면 파일도 지운다).
    pub purged: Vec<i64>,
    pub purged_hashes: Vec<String>,
    /// 웹메일에서 지운 초안의 작성 중 메일(첨부 폴더는 호출한 쪽이 지운다).
    pub dropped_drafts: Vec<i64>,
}

impl ApplyStats {
    fn touch(&mut self, folder_id: i64, message_id: i64) {
        self.touched.entry(folder_id).or_default().push(message_id);
    }
}

struct Candidate {
    row: MessageRow,
    matched: bool,
}

/// 같은 메일인가: Message-ID와 받은 시각이 모두 같아야 한다. 본인에게 보낸 메일은 보낸 편지함·
/// 받은 편지함 사본이 같은 Message-ID를 갖지만 받은 시각이 달라 구분된다.
fn same_message(row: &MessageRow, p: &MessageProps) -> bool {
    match (&row.props.internet_msg_id, &p.internet_msg_id) {
        (Some(a), Some(b)) => a == b && row.props.date_received == p.date_received,
        _ => false,
    }
}

/// 대기 중인 로컬 조작 값을 지킨 속성.
fn guarded(row: &MessageRow, mut p: MessageProps, guard: &PendingGuard) -> MessageProps {
    if guard.keep_read.contains(&row.id) {
        p.is_read = row.props.is_read;
    }
    if guard.keep_flag.contains(&row.id) {
        p.flag = row.props.flag;
    }
    p
}

pub fn apply_cycle(
    tx: &Tx<'_>,
    batch: &[FolderChanges],
    now_ms: i64,
    guard: &PendingGuard,
) -> Result<ApplyStats> {
    let mut stats = ApplyStats::default();

    // 삭제 후보: 이번 주기에 서버에서 사라진 항목.
    let mut candidates = Vec::new();
    for fc in batch {
        for c in fc.changes.iter().filter(|c| c.kind == ChangeKind::Delete) {
            match c
                .item_id
                .as_ref()
                .map(|id| tx.message_by_server_id(&id.id))
                .transpose()?
                .flatten()
            {
                Some(row) => candidates.push(Candidate {
                    row,
                    matched: false,
                }),
                None => stats.ignored += 1,
            }
        }
    }

    for fc in batch {
        for c in &fc.changes {
            match c.kind {
                ChangeKind::Create | ChangeKind::Update => {
                    let (Some(item), Some(id)) = (&c.item, &c.item_id) else {
                        stats.ignored += 1;
                        continue;
                    };
                    if guard.ignore_server_ids.contains(&id.id) {
                        stats.ignored += 1; // 완전 삭제 대기
                        continue;
                    }
                    let p = convert::props(item);
                    let ck = id.change_key.as_deref();
                    if let Some(row) = tx.message_by_server_id(&id.id)? {
                        let pinned = guard.keep_folder.contains(&row.id)
                            || guard.keep_state.contains(&row.id);
                        let relocate = (row.folder_id != fc.folder_id && !pinned)
                            .then_some((fc.folder_id, id.id.as_str()));
                        let p = guarded(&row, p, guard);
                        tx.update_message(row.id, relocate, ck, &p)?;
                        stats.updated += 1;
                        stats.touch(row.folder_id, row.id);
                        stats.touch(fc.folder_id, row.id);
                    } else if let Some(copy) = p
                        .internet_msg_id
                        .as_deref()
                        .map(|imid| tx.pending_copy_in(fc.folder_id, imid, p.date_received))
                        .transpose()?
                        .flatten()
                    {
                        // 서버 반영 전 사본과 같은 메일: 새 행을 만들지 않고 잇는다.
                        tx.set_server_item(copy, &id.id, ck)?;
                        if let Some(row) = tx.message(copy)? {
                            let p = guarded(&row, p, guard);
                            tx.update_message(copy, None, ck, &p)?;
                        }
                        stats.updated += 1;
                        stats.touch(fc.folder_id, copy);
                    } else if let Some(cand) = candidates
                        .iter_mut()
                        .find(|cand| !cand.matched && same_message(&cand.row, &p))
                    {
                        cand.matched = true;
                        tx.update_message(
                            cand.row.id,
                            Some((fc.folder_id, id.id.as_str())),
                            ck,
                            &p,
                        )?;
                        stats.moves.push(MoveRecord {
                            message_id: cand.row.id,
                            from_folder: cand.row.folder_id,
                            to_folder: fc.folder_id,
                            had_eml: cand.row.eml_hash.is_some(),
                        });
                        stats.touch(cand.row.folder_id, cand.row.id);
                        stats.touch(fc.folder_id, cand.row.id);
                    } else {
                        let new_id = tx.insert_message(fc.folder_id, &id.id, ck, &p)?;
                        stats.created += 1;
                        stats.touch(fc.folder_id, new_id);
                    }
                }
                ChangeKind::ReadFlag => {
                    let row = match &c.item_id {
                        Some(id) => tx.message_by_server_id(&id.id)?,
                        None => None,
                    };
                    match (row, c.is_read) {
                        (Some(row), Some(_)) if guard.keep_read.contains(&row.id) => {
                            stats.ignored += 1; // 읽음 표시 대기 중
                        }
                        (Some(row), Some(read)) => {
                            tx.set_read(row.id, read)?;
                            stats.read_flags += 1;
                            stats.touch(row.folder_id, row.id);
                        }
                        _ => stats.ignored += 1,
                    }
                }
                ChangeKind::Delete => {} // 후보로 처리
                ChangeKind::Other => stats.ignored += 1,
            }
        }
    }

    let drafts = tx.well_known_folder_id("drafts")?;
    for cand in candidates.into_iter().filter(|c| !c.matched) {
        if drafts == Some(cand.row.folder_id) {
            // 앱에서 쓰던 메일(0.7.0): 보내는 중이 아니고 아직 올리지 않은 고침이 있으면 지키고 다시 올린다.
            // 고침이 없으면 웹메일에서 지운 것이니 작성 중 메일도 지운다.
            match tx.draft_link(cand.row.id)? {
                Some(link) if !link.sending && link.dirty => {
                    tx.unlink_server_draft(link.draft_id, cand.row.id, now_ms)?;
                    stats.touch(cand.row.folder_id, cand.row.id);
                    continue;
                }
                Some(link) if !link.sending => stats.dropped_drafts.push(link.draft_id),
                _ => {}
            }
            // 임시 보관함 초안이 서버에서 사라졌다(보냈거나 버림). 초안은 보관하지 않는다:
            // 보냈으면 보낸 편지함에 발송본이 따로 생긴다(6단계: 앱이 보낼 때 만든 서버 초안이 보관으로 남던 문제).
            if let Some(h) = tx.purge_row(cand.row.id)? {
                stats.purged_hashes.push(h);
            }
            stats.purged.push(cand.row.id);
            stats.touch(cand.row.folder_id, cand.row.id);
            continue;
        }
        tx.archive_message(cand.row.id, now_ms)?;
        stats.archives.push(ArchiveRecord {
            message_id: cand.row.id,
            folder_id: cand.row.folder_id,
            had_eml: cand.row.eml_hash.is_some(),
        });
        stats.touch(cand.row.folder_id, cand.row.id);
    }

    for fc in batch {
        tx.set_folder_sync_state(fc.folder_id, Some(&fc.new_state))?;
    }
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use eo_owa::{ItemId, ItemSummary};
    use eo_store::{ServerFolder, Store};

    use super::*;

    fn item(id: &str, imid: &str, received: &str) -> ItemChange {
        ItemChange {
            kind: ChangeKind::Create,
            type_name: "SyncFolderItemsCreateType".into(),
            item_id: Some(ItemId {
                id: id.into(),
                change_key: Some("ck".into()),
            }),
            item: Some(ItemSummary {
                item_id: Some(ItemId {
                    id: id.into(),
                    change_key: Some("ck".into()),
                }),
                internet_message_id: Some(imid.into()),
                date_time_received: Some(received.into()),
                subject: Some("제목".into()),
                ..ItemSummary::default()
            }),
            is_read: None,
        }
    }

    fn delete(id: &str) -> ItemChange {
        ItemChange {
            kind: ChangeKind::Delete,
            type_name: "SyncFolderItemsDeleteType".into(),
            item_id: Some(ItemId {
                id: id.into(),
                change_key: None,
            }),
            item: None,
            is_read: None,
        }
    }

    fn setup() -> (tempfile::TempDir, Store, i64, i64, i64) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let (inbox, sent, archive) = store
            .transaction(|tx| {
                let f = |id: &str| ServerFolder {
                    server_id: id.into(),
                    server_parent_id: Some("ROOT".into()),
                    name: id.into(),
                    folder_class: Some("IPF.Note".into()),
                };
                Ok((
                    tx.upsert_folder(&f("INBOX"))?,
                    tx.upsert_folder(&f("SENT"))?,
                    tx.upsert_folder(&f("ARCHIVE"))?,
                ))
            })
            .unwrap();
        (dir, store, inbox, sent, archive)
    }

    fn apply(store: &Store, batch: Vec<FolderChanges>) -> ApplyStats {
        let guard = store.pending_guard().unwrap();
        store
            .transaction(|tx| apply_cycle(tx, &batch, 99, &guard))
            .unwrap()
    }

    fn fc(folder_id: i64, changes: Vec<ItemChange>) -> FolderChanges {
        FolderChanges {
            folder_id,
            changes,
            new_state: format!("S{folder_id}"),
        }
    }

    #[test]
    fn move_keeps_row_and_eml_and_unpaired_delete_archives() {
        let (_d, store, inbox, _sent, archive) = setup();
        let s = apply(
            &store,
            vec![fc(
                inbox,
                vec![
                    item("A", "<a@x>", "2026-10-15T10:00:00+09:00"),
                    item("B", "<b@x>", "2026-10-15T11:00:00+09:00"),
                ],
            )],
        );
        assert_eq!(s.created, 2);
        let a = store
            .messages_in_folder(inbox)
            .unwrap()
            .into_iter()
            .find(|m| m.server_item_id.as_deref() == Some("A"))
            .unwrap();
        assert!(store.attach_eml(a.id, "A", b"raw-a").unwrap());

        // 웹메일에서 A를 보관 폴더로 이동, B는 영구 삭제.
        let s = apply(
            &store,
            vec![
                fc(inbox, vec![delete("A"), delete("B")]),
                fc(
                    archive,
                    vec![item("A2", "<a@x>", "2026-10-15T10:00:00+09:00")],
                ),
            ],
        );
        assert_eq!(s.created, 0, "이동은 새 행을 만들지 않는다");
        assert_eq!(
            s.moves,
            [MoveRecord {
                message_id: a.id,
                from_folder: inbox,
                to_folder: archive,
                had_eml: true
            }]
        );
        assert_eq!(s.archives.len(), 1);
        let moved = store.message(a.id).unwrap().unwrap();
        assert_eq!(
            (moved.folder_id, moved.server_item_id.as_deref()),
            (archive, Some("A2"))
        );
        assert!(moved.eml_hash.is_some(), "원문은 그대로");
        let b = store.message(s.archives[0].message_id).unwrap().unwrap();
        assert_eq!(
            (b.folder_id, b.server_state, b.server_item_id),
            (inbox, 1, None)
        );
        assert!(
            store.eml_todo(10).unwrap().is_empty(),
            "다시 받을 원문 없음"
        );
        assert_eq!(
            store
                .folders()
                .unwrap()
                .iter()
                .find(|f| f.id == archive)
                .unwrap()
                .sync_state
                .as_deref(),
            Some(&*format!("S{archive}"))
        );
    }

    #[test]
    fn self_sent_copies_pair_by_received_time() {
        let (_d, store, inbox, sent, archive) = setup();
        apply(
            &store,
            vec![
                fc(sent, vec![item("S", "<m@x>", "2026-10-15T10:00:00+09:00")]),
                fc(inbox, vec![item("I", "<m@x>", "2026-10-15T10:00:02+09:00")]),
            ],
        );
        // 받은 편지함 사본만 이동
        let s = apply(
            &store,
            vec![
                fc(inbox, vec![delete("I")]),
                fc(
                    archive,
                    vec![item("I2", "<m@x>", "2026-10-15T10:00:02+09:00")],
                ),
            ],
        );
        assert_eq!(s.moves.len(), 1);
        assert_eq!(s.moves[0].from_folder, inbox);
        assert_eq!(
            store.messages_in_folder(sent).unwrap()[0]
                .server_item_id
                .as_deref(),
            Some("S")
        );
    }

    #[test]
    fn pending_local_changes_are_not_overwritten() {
        let (_d, store, inbox, sent, archive) = setup();
        apply(
            &store,
            vec![fc(
                inbox,
                vec![
                    item("A", "<a@x>", "2026-10-15T10:00:00Z"),
                    item("B", "<b@x>", "2026-10-15T11:00:00Z"),
                ],
            )],
        );
        let id_of = |sid: &str| {
            store
                .messages_in_folder(inbox)
                .unwrap()
                .into_iter()
                .find(|m| m.server_item_id.as_deref() == Some(sid))
                .unwrap()
                .id
        };
        let (a, b) = (id_of("A"), id_of("B"));
        // 로컬: A를 읽음으로 하고 보관 폴더로 옮김, B는 완전 삭제(서버 반영 전).
        store.mark_read(&[a], true, 1).unwrap();
        store.move_messages(&[a], archive, 1, None).unwrap();
        store.purge_messages(&[b], 1, None).unwrap();
        // 그사이 서버에서: A가 받은 편지함에서 갱신(안읽음), B도 갱신.
        let mut upd_a = item("A", "<a@x>", "2026-10-15T10:00:00Z");
        upd_a.kind = ChangeKind::Update;
        let mut upd_b = item("B", "<b@x>", "2026-10-15T11:00:00Z");
        upd_b.kind = ChangeKind::Update;
        let read_off = ItemChange {
            kind: ChangeKind::ReadFlag,
            type_name: "SyncFolderItemsReadFlagType".into(),
            item_id: Some(ItemId {
                id: "A".into(),
                change_key: None,
            }),
            item: None,
            is_read: Some(false),
        };
        let s = apply(&store, vec![fc(inbox, vec![upd_a, read_off, upd_b])]);
        let row = store.message(a).unwrap().unwrap();
        assert_eq!(
            (row.folder_id, row.props.is_read),
            (archive, true),
            "대기 중 이동·읽음 유지"
        );
        assert_eq!(s.created, 0, "완전 삭제 대기 메일은 다시 만들지 않는다");
        assert!(store.messages_in_folder(inbox).unwrap().is_empty());

        // 복사가 새 ID를 주지 않았을 때: 서버의 새 항목을 서버 반영 전 사본과 잇는다.
        let t = store.copy_messages(&[a], sent, 2, None).unwrap();
        let copy = t.created[0];
        let s = apply(
            &store,
            vec![fc(sent, vec![item("A3", "<a@x>", "2026-10-15T10:00:00Z")])],
        );
        assert_eq!((s.created, s.updated), (0, 1));
        let c = store.message(copy).unwrap().unwrap();
        assert_eq!(
            (c.server_item_id.as_deref(), c.server_state),
            (Some("A3"), 0)
        );
    }

    #[test]
    fn vanished_server_draft_is_purged_not_archived() {
        let (_d, store, inbox, sent, _archive) = setup();
        store
            .transaction(|tx| {
                tx.upsert_folder(&ServerFolder {
                    server_id: "DRAFTS".into(),
                    server_parent_id: Some("ROOT".into()),
                    name: "임시 보관함".into(),
                    folder_class: Some("IPF.Note".into()),
                })?;
                tx.set_well_known("DRAFTS", "drafts", 1)
            })
            .unwrap();
        let drafts = store
            .folders()
            .unwrap()
            .into_iter()
            .find(|f| f.server_id.as_deref() == Some("DRAFTS"))
            .unwrap()
            .id;
        apply(
            &store,
            vec![fc(
                drafts,
                vec![item("D1", "<m@x>", "2026-10-16T14:00:00Z")],
            )],
        );
        let d = store.messages_in_folder(drafts).unwrap()[0].id;
        assert!(store.attach_eml(d, "D1", b"draft-raw").unwrap());
        // 보냄: 초안은 사라지고 보낸 편지함·받은 편지함(본인)에 발송본(받은 시각이 다름).
        let s = apply(
            &store,
            vec![
                fc(drafts, vec![delete("D1")]),
                fc(sent, vec![item("S1", "<m@x>", "2026-10-16T14:00:05Z")]),
                fc(inbox, vec![item("I1", "<m@x>", "2026-10-16T14:00:06Z")]),
            ],
        );
        assert_eq!((s.archives.len(), s.purged, s.created), (0, vec![d], 2));
        assert!(store.messages_in_folder(drafts).unwrap().is_empty());
        assert_eq!(s.purged_hashes.len(), 1);
    }

    /// 앱에서 쓰던 메일의 서버 초안이 웹메일에서 지워졌다(0.7.0): 올리지 않은 고침이 있으면 지키고 다시 올리고,
    /// 없으면 작성 중 메일도 지운다.
    #[test]
    fn vanished_app_draft_is_kept_when_edited_else_dropped() {
        let (_d, store, ..) = setup();
        store
            .transaction(|tx| {
                tx.upsert_folder(&ServerFolder {
                    server_id: "DRAFTS".into(),
                    server_parent_id: Some("ROOT".into()),
                    name: "임시 보관함".into(),
                    folder_class: Some("IPF.Note".into()),
                })?;
                tx.set_well_known("DRAFTS", "drafts", 1)
            })
            .unwrap();
        let content = eo_store::DraftContent {
            subject: "쓰던 메일".into(),
            ..eo_store::DraftContent::default()
        };
        let mut rows = Vec::new();
        for (n, sid) in [(1, "E1"), (2, "C1")] {
            let id = store.create_draft("new", None, &content, n).unwrap();
            let row = store.sync_draft_row(id, None).unwrap().unwrap();
            store.set_draft_server_id(id, sid, "newMail").unwrap();
            store.finish_draft_upload(id, sid, None, n).unwrap();
            rows.push((id, row));
        }
        // E1은 올린 뒤 또 고쳤다(아직 안 올림), C1은 그대로.
        store.save_draft(rows[0].0, &content, 9).unwrap();
        let drafts = store.message(rows[0].1).unwrap().unwrap().folder_id;
        let s = apply(&store, vec![fc(drafts, vec![delete("E1"), delete("C1")])]);
        assert_eq!(s.purged, vec![rows[1].1]);
        assert_eq!(s.dropped_drafts, vec![rows[1].0]);
        let kept = store.message(rows[0].1).unwrap().unwrap();
        assert_eq!(
            (kept.server_item_id, kept.server_state),
            (None, eo_store::server_state::PENDING),
            "고친 메일은 서버 반영 대기로 남는다"
        );
        let d = store.draft(rows[0].0).unwrap().unwrap();
        assert!(d.server_draft_id.is_none() && d.server_dirty);
        assert_eq!(store.pending_op_count().unwrap(), 1, "다시 올린다");
    }

    #[test]
    fn read_flags_updates_and_unknown_deletes() {
        let (_d, store, inbox, ..) = setup();
        apply(
            &store,
            vec![fc(inbox, vec![item("A", "<a@x>", "2026-10-15T10:00:00Z")])],
        );
        let read = ItemChange {
            kind: ChangeKind::ReadFlag,
            type_name: "SyncFolderItemsReadFlagType".into(),
            item_id: Some(ItemId {
                id: "A".into(),
                change_key: None,
            }),
            item: None,
            is_read: Some(true),
        };
        let s = apply(&store, vec![fc(inbox, vec![read, delete("NOPE")])]);
        assert_eq!((s.read_flags, s.ignored), (1, 1));
        assert!(store.messages_in_folder(inbox).unwrap()[0].props.is_read);
    }
}
