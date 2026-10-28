//! `owa-cli changes`: 전 메일 폴더를 몇 초마다 증분 동기화하며 변경 종류를 보여 준다(계획서 11장 #4).
//! 사용자가 웹메일에서 읽음·이동·삭제를 하면 그 `__type` 이름과, 같은 Message-ID의 삭제·생성 짝(이동, 6.3)을 출력한다.
//! 메일 제목·주소는 출력하지 않고, 항목 ID·Message-ID는 짧은 해시로만 보여 준다.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::Result;
use eo_owa::{ChangeKind, Client, FolderRef};

use crate::util::{fetch_hierarchy, item_sync, now_unix, short_hash};

/// 한국 표준시 HH:MM:SS(일광 절약 시간 없음).
fn clock() -> String {
    let s = now_unix() + 9 * 3600;
    format!("{:02}:{:02}:{:02}", (s / 3600) % 24, (s / 60) % 60, s % 60)
}

struct Watched {
    path: String,
    folder: FolderRef,
    state: String,
}

pub async fn run(client: &Client, interval_secs: u64) -> Result<()> {
    let h = fetch_hierarchy(client).await?;
    let mut watched = Vec::new();
    // 항목 ID → Message-ID. 삭제 변경에는 항목 ID만 오므로 이동 짝짓기에 쓴다.
    let mut message_ids: HashMap<String, String> = HashMap::new();
    for n in h.mail_tree() {
        let folder = FolderRef::from(&n.folder.folder_id);
        let l = item_sync(client, &folder, None).await?;
        for c in &l.changes {
            if let (Some(id), Some(imid)) = (
                &c.item_id,
                c.item.as_ref().and_then(|i| i.internet_message_id.as_ref()),
            ) {
                message_ids.insert(id.id.clone(), imid.clone());
            }
        }
        watched.push(Watched {
            path: n.path,
            folder,
            state: l.sync_state,
        });
    }
    println!(
        "[{}] 변경 감시 시작: 메일 폴더 {}개, {interval_secs}초마다. 웹메일에서 읽음/안읽음·플래그·이동·삭제를 해 보세요. 끝내려면 Ctrl+C.",
        clock(),
        watched.len()
    );

    loop {
        tokio::time::sleep(Duration::from_secs(interval_secs)).await;
        let mut deleted: Vec<(String, String)> = Vec::new();
        let mut created: Vec<(String, String)> = Vec::new();
        for w in &mut watched {
            let l = match item_sync(client, &w.folder, Some(&w.state)).await {
                Ok(l) => l,
                Err(e) => {
                    println!("[{}] {}: 오류 {e}", clock(), w.path);
                    continue;
                }
            };
            w.state = l.sync_state;
            for c in &l.changes {
                let id = c.item_id.as_ref().map(|i| i.id.clone()).unwrap_or_default();
                let imid = c
                    .item
                    .as_ref()
                    .and_then(|i| i.internet_message_id.clone())
                    .or_else(|| message_ids.get(&id).cloned())
                    .unwrap_or_default();
                let flag = c
                    .item
                    .as_ref()
                    .and_then(|i| i.flag.as_ref())
                    .and_then(|f| f.flag_status.as_deref())
                    .unwrap_or("-");
                println!(
                    "[{}] {}: {} ({:?}) 항목 {} · Message-ID {} · 읽음 {} · 플래그 {flag}",
                    clock(),
                    w.path,
                    c.type_name,
                    c.kind,
                    short_hash(&id),
                    if imid.is_empty() {
                        "?".into()
                    } else {
                        short_hash(&imid)
                    },
                    c.is_read.map_or("-".into(), |b| b.to_string())
                );
                match c.kind {
                    ChangeKind::Create => {
                        message_ids.insert(id, imid.clone());
                        created.push((w.path.clone(), imid));
                    }
                    ChangeKind::Delete => deleted.push((w.path.clone(), imid)),
                    _ => {}
                }
            }
        }
        for (from, imid) in &deleted {
            if imid.is_empty() {
                continue;
            }
            match created.iter().find(|(to, m)| m == imid && to != from) {
                Some((to, _)) => println!(
                    "    → Message-ID {}: {from} → {to} 이동으로 판정 (계획서 6.3)",
                    short_hash(imid)
                ),
                None => println!(
                    "    → Message-ID {}: {from}에서 사라짐, 같은 주기에 다른 폴더 생성 없음 → 서버 삭제로 판정",
                    short_hash(imid)
                ),
            }
        }
    }
}
