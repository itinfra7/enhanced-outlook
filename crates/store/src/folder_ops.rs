//! 폴더 오른쪽 클릭 메뉴의 로컬 쪽(0.7.0): 모두 읽음, 가져온 로컬 폴더(서버에 없는 폴더) 만들기·이름 바꾸기·
//! 지우기. 서버 폴더는 웹메일에 바로 요청하고 계층 동기화가 로컬을 맞춘다.

use rusqlite::{OptionalExtension, params};

use crate::local::Touched;
use crate::ops::OP_SET_READ;
use crate::{Error, Result, Store};

impl Store {
    /// 폴더의 안 읽은 메일 번호들.
    pub fn unread_ids_in(&self, folder_id: i64) -> Result<Vec<i64>> {
        let conn = self.conn();
        let mut stmt =
            conn.prepare_cached("SELECT id FROM messages WHERE folder_id = ?1 AND is_read = 0")?;
        let rows = stmt.query_map([folder_id], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// 서버에 "모두 읽음"을 보낸 뒤 로컬도 모두 읽음으로(메일마다 작업을 쌓지 않는다). 그 폴더의 읽음·안읽음
    /// 대기 작업은 없앤다(마지막 조작이 "모두 읽음"이다).
    pub fn mark_folder_read_synced(&self, folder_id: i64) -> Result<Touched> {
        let ids = self.unread_ids_in(folder_id)?;
        self.transaction(|tx| {
            tx.tx.execute(
                "UPDATE ops SET state = 1, last_error = '모두 읽음으로 대체' WHERE state = 0 AND kind = ?1 \
                 AND message_id IN (SELECT id FROM messages WHERE folder_id = ?2)",
                params![OP_SET_READ, folder_id],
            )?;
            tx.tx.execute(
                "UPDATE messages SET is_read = 1 WHERE folder_id = ?1 AND is_read = 0",
                [folder_id],
            )?;
            Ok(())
        })?;
        let mut t = Touched::default();
        for id in ids {
            t.add(folder_id, id);
        }
        Ok(t)
    }

    fn local_folder(&self, id: i64) -> Result<crate::FolderRow> {
        let f = self
            .folders()?
            .into_iter()
            .find(|f| f.id == id)
            .ok_or_else(|| Error::Invalid("폴더가 없습니다".into()))?;
        if f.server_id.is_some() {
            return Err(Error::Invalid("서버 폴더입니다".into()));
        }
        Ok(f)
    }

    fn clean_name(name: &str) -> Result<String> {
        let n = name.trim();
        if n.is_empty() || n.contains(['/', '\\']) {
            return Err(Error::Invalid("폴더 이름이 올바르지 않습니다".into()));
        }
        Ok(n.to_owned())
    }

    /// 로컬 폴더 아래에(없으면 맨 위에) 로컬 폴더를 만든다. 새 폴더 번호.
    pub fn create_local_folder(&self, parent: Option<i64>, name: &str) -> Result<i64> {
        let name = Self::clean_name(name)?;
        if let Some(p) = parent {
            self.local_folder(p)?;
        }
        let conn = self.conn();
        let dup: Option<i64> = conn
            .query_row(
                "SELECT id FROM folders WHERE parent_id IS ?1 AND lower(trim(name)) = lower(?2)",
                params![parent, name],
                |r| r.get(0),
            )
            .optional()?;
        if dup.is_some() {
            return Err(Error::Invalid("같은 이름의 폴더가 있습니다".into()));
        }
        conn.execute(
            "INSERT INTO folders(server_id, parent_id, name, folder_class, is_mail, sort_order) \
             VALUES (NULL, ?1, ?2, 'IPF.Note', 1, 200)",
            params![parent, name],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn rename_local_folder(&self, id: i64, name: &str) -> Result<()> {
        let name = Self::clean_name(name)?;
        self.local_folder(id)?;
        self.conn().execute(
            "UPDATE folders SET name = ?1 WHERE id = ?2",
            params![name, id],
        )?;
        Ok(())
    }

    /// 로컬 폴더와 그 하위 폴더, 안의 메일을 앱에서 지운다(서버와 상관없는 가져온 메일). 지운 메일 수.
    pub fn delete_local_folder(&self, id: i64) -> Result<(usize, Touched)> {
        self.local_folder(id)?;
        let folders = self.folders()?;
        let mut ids = vec![id];
        let mut i = 0;
        while i < ids.len() {
            let p = ids[i];
            ids.extend(
                folders
                    .iter()
                    .filter(|f| f.parent_id == Some(p))
                    .map(|f| f.id),
            );
            i += 1;
        }
        if folders
            .iter()
            .any(|f| ids.contains(&f.id) && f.server_id.is_some())
        {
            return Err(Error::Invalid(
                "서버 폴더가 들어 있어 지울 수 없습니다".into(),
            ));
        }
        let mut t = Touched::default();
        let mut hashes = Vec::new();
        let mut n = 0;
        self.transaction(|tx| {
            for f in &ids {
                let rows: Vec<i64> = {
                    let mut stmt = tx
                        .tx
                        .prepare("SELECT id FROM messages WHERE folder_id = ?1")?;
                    stmt.query_map([f], |r| r.get(0))?
                        .collect::<rusqlite::Result<_>>()?
                };
                for r in rows {
                    hashes.extend(tx.purge_row(r)?);
                    t.add(*f, r);
                    t.removed.push(r);
                    n += 1;
                }
            }
            for f in ids.iter().rev() {
                tx.tx.execute("DELETE FROM folders WHERE id = ?1", [f])?;
            }
            Ok(())
        })?;
        for h in hashes {
            self.remove_eml_if_unused(&h)?;
        }
        Ok((n, t))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MessageProps, ServerFolder};

    #[test]
    fn local_folders_and_mark_read() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        let inbox = s
            .transaction(|tx| {
                tx.upsert_folder(&ServerFolder {
                    server_id: "INBOX".into(),
                    server_parent_id: Some("ROOT".into()),
                    name: "받은 편지함".into(),
                    folder_class: Some("IPF.Note".into()),
                })
            })
            .unwrap();
        let unread = |n: i64| MessageProps {
            date_received: n,
            ..MessageProps::default()
        };
        let a = s
            .transaction(|tx| {
                tx.insert_message(inbox, "A", None, &unread(1))?;
                tx.insert_message(inbox, "B", None, &unread(2))
            })
            .unwrap();
        s.mark_read(&[a], false, 3).unwrap(); // 안읽음 대기 작업 하나
        let t = s.mark_folder_read_synced(inbox).unwrap();
        assert_eq!(t.folders[&inbox].len(), 2);
        assert!(s.unread_ids_in(inbox).unwrap().is_empty());
        assert_eq!(
            s.pending_op_count().unwrap(),
            0,
            "안읽음 대기는 모두 읽음으로 대체"
        );

        let top = s.create_local_folder(None, "옛날 메일").unwrap();
        let sub = s.create_local_folder(Some(top), "2019").unwrap();
        assert!(
            s.create_local_folder(Some(top), " 2019 ").is_err(),
            "같은 이름"
        );
        assert!(
            s.create_local_folder(Some(inbox), "x").is_err(),
            "서버 폴더 아래는 안 됨"
        );
        s.rename_local_folder(sub, "2019년").unwrap();
        assert!(s.rename_local_folder(inbox, "x").is_err());
        assert_eq!(
            s.folders()
                .unwrap()
                .iter()
                .find(|f| f.id == sub)
                .unwrap()
                .name,
            "2019년"
        );
        s.transaction(|tx| {
            let id = tx.insert_message(sub, "L1", None, &unread(5))?;
            tx.tx.execute(
                "UPDATE messages SET server_item_id = NULL, server_state = 2 WHERE id = ?1",
                [id],
            )?;
            Ok(())
        })
        .unwrap();
        let (n, t) = s.delete_local_folder(top).unwrap();
        assert_eq!((n, t.removed.len()), (1, 1));
        assert!(
            !s.folders()
                .unwrap()
                .iter()
                .any(|f| f.id == top || f.id == sub)
        );
        assert!(s.delete_local_folder(inbox).is_err());
    }
}
