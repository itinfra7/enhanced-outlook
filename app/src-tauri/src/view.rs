//! 화면에 넘기는 형태(계획서 4.1-5: 필요한 열만). 이름은 프런트엔드 `types.ts`와 맞춘다.

use std::collections::HashMap;

use eo_store::{FolderRow, RowLite, Store};
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderView {
    pub id: i64,
    pub parent_id: Option<i64>,
    pub name: String,
    pub well_known: Option<String>,
    pub depth: usize,
    pub total: i64,
    pub unread: i64,
    /// 서버에서 지워져 로컬에만 남은 메일 수.
    pub archived_mail: i64,
    /// 폴더 자체가 서버에서 사라짐(로컬 보관 폴더).
    pub archived: bool,
    /// 가져오기로 만든 로컬 폴더(서버에 없음, 보관 폴더 아님).
    pub local: bool,
    /// HCL Domino 웹메일 메일함과 이어진 폴더(또는 'HCL Domino 메일' 맨 위): 이름 바꾸기·지우기 없음.
    pub domino: bool,
    /// 즐겨찾기(폴더 목록 맨 위에도 보인다, 0.7.0).
    pub favorite: bool,
}

/// 목록 한 행.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RowView {
    pub id: i64,
    pub folder_id: i64,
    pub subject: String,
    /// 보낸 사람 표시 이름(없으면 주소).
    pub from: String,
    /// 받는 사람 표시(보낸 편지함용, 앞 3명).
    pub to: String,
    pub date: i64,
    pub size: i64,
    pub read: bool,
    pub flag: i64,
    pub attach: bool,
    pub importance: i64,
    pub archived: bool,
    pub has_eml: bool,
    /// 가져온 메일의 출처(`import`, `html`).
    pub origin: Option<String>,
    /// 임시 보관함에서 쓰던 메일의 상태(0 작성 중, 1 보내는 중, 2 발송 실패).
    pub draft: Option<i64>,
    /// 고정(맨 위, 1.1.0).
    pub pinned: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentView {
    pub index: usize,
    pub name: String,
    pub mime: String,
    pub size: usize,
}

/// 읽기 창(계획서 9.2).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageView {
    pub id: i64,
    pub folder_id: i64,
    pub subject: String,
    pub from: String,
    pub sender: String,
    pub to: String,
    pub cc: String,
    pub bcc: String,
    pub reply_to: String,
    /// 보낸 날짜(원문 머리글), 없으면 받은 시각.
    pub date: i64,
    pub html: String,
    pub has_remote_images: bool,
    pub remote_allowed: bool,
    pub attachments: Vec<AttachmentView>,
    /// 서버에서 삭제된 시각(로컬 보관).
    pub archived_at: Option<i64>,
    /// 가져온 메일의 출처: `import`(EML), `html`(HTML 백업에서 복원).
    pub origin: Option<String>,
    pub has_eml: bool,
    /// 본문이 쓰는 인라인 이미지 수.
    pub inline_images: usize,
}

/// 통합 편지함(화면에만 있는 폴더 번호).
pub const UNIFIED_ID: i64 = -1;
/// 'Outlook 메일' 묶음 제목(고를 수 없는 줄).
pub const OUTLOOK_GROUP_ID: i64 = -2;

/// 통합 편지함이 보여 주는 폴더: Outlook 받은 편지함, HCL Domino 받은 메일·외부 메일(있는 것만).
pub fn unified_members(store: &Store) -> eo_store::Result<Vec<i64>> {
    let mut out = Vec::new();
    if let Some(f) = store.folder_by_well_known("inbox")? {
        out.push(f.id);
    }
    let boxes = store.domino_folder_boxes()?;
    for key in ["XML_Inbox", "XML_POP3"] {
        if let Some((id, _)) = boxes.iter().find(|(_, k)| k.as_str() == key) {
            out.push(*id);
        }
    }
    Ok(out)
}

/// 화면의 폴더 목록: 통합 편지함을 켰으면(HCL Domino 연동 중) 맨 위에 통합 편지함, 그 아래 'Outlook 메일' 묶음
/// (Outlook 폴더와 가져오기로 만든 로컬 폴더를 한 단계 들여서), 그다음 'HCL Domino 메일'(사용자 요청·결정 2026-10-21).
pub fn folder_tree_for(store: &Store, unified: bool) -> eo_store::Result<Vec<FolderView>> {
    let tree = folder_tree(store)?;
    if !unified {
        return Ok(tree);
    }
    let members = unified_members(store)?;
    let (total, unread) = tree
        .iter()
        .filter(|f| members.contains(&f.id))
        .fold((0, 0), |(t, u), f| (t + f.total, u + f.unread));
    let virtual_node = |id, name: &str, kind: &str, total, unread| FolderView {
        id,
        parent_id: None,
        name: name.to_owned(),
        well_known: Some(kind.to_owned()),
        depth: 0,
        total,
        unread,
        archived_mail: 0,
        archived: false,
        local: false,
        domino: false,
        favorite: false,
    };
    let mut out = vec![
        virtual_node(UNIFIED_ID, "통합 편지함", "unified", total, unread),
        virtual_node(OUTLOOK_GROUP_ID, "Outlook 메일", "group", 0, 0),
    ];
    let domino_root = tree
        .iter()
        .find(|f| f.depth == 0 && f.name == eo_sync::DOMINO_FOLDER)
        .map(|f| f.id);
    // HCL Domino 트리(루트와 그 아래)는 따로, 나머지는 Outlook 묶음 아래로.
    let mut in_domino = false;
    let mut domino = Vec::new();
    for mut f in tree {
        if f.depth == 0 {
            in_domino = Some(f.id) == domino_root;
        }
        if in_domino {
            domino.push(f);
        } else {
            if f.depth == 0 {
                f.parent_id = Some(OUTLOOK_GROUP_ID);
            }
            f.depth += 1;
            out.push(f);
        }
    }
    out.extend(domino);
    Ok(out)
}

/// 메일 폴더를 트리 순서로(깊이 우선, 같은 단계는 표시 순서·이름순).
pub fn folder_tree(store: &Store) -> eo_store::Result<Vec<FolderView>> {
    fn walk(
        all: &[FolderRow],
        parent: Option<i64>,
        depth: usize,
        counts: &HashMap<i64, eo_store::FolderCounts>,
        boxes: &HashMap<i64, String>,
        out: &mut Vec<FolderView>,
    ) {
        for f in all.iter().filter(|f| f.is_mail && f.parent_id == parent) {
            let c = counts.get(&f.id).cloned().unwrap_or_default();
            out.push(FolderView {
                id: f.id,
                parent_id: f.parent_id,
                name: f.name.clone(),
                well_known: f.well_known.clone(),
                depth,
                total: c.total,
                unread: c.unread,
                archived_mail: c.archived,
                archived: f.archived_at.is_some(),
                local: f.server_id.is_none() && f.archived_at.is_none(),
                domino: boxes.contains_key(&f.id)
                    || (depth == 0 && f.server_id.is_none() && f.name == eo_sync::DOMINO_FOLDER),
                favorite: false,
            });
            if depth < 16 {
                walk(all, Some(f.id), depth + 1, counts, boxes, out);
            }
        }
    }
    let folders = store.folders()?;
    let counts = store
        .folder_counts()?
        .into_iter()
        .map(|c| (c.folder_id, c))
        .collect();
    let boxes = store.domino_folder_boxes()?;
    let mut out = Vec::new();
    walk(&folders, None, 0, &counts, &boxes, &mut out);
    Ok(out)
}

/// 즐겨찾기 표시(폴더 목록은 그대로, 화면이 맨 위에 따로 보인다).
pub fn mark_favorites(mut folders: Vec<FolderView>, favorites: &[i64]) -> Vec<FolderView> {
    for f in &mut folders {
        f.favorite = favorites.contains(&f.id);
    }
    folders
}

fn name_or_addr(name: Option<&str>, addr: Option<&str>) -> String {
    match (name.map(str::trim), addr.map(str::trim)) {
        (Some(n), _) if !n.is_empty() => n.to_owned(),
        (_, Some(a)) => a.to_owned(),
        _ => String::new(),
    }
}

/// "이름 <주소>; ..." → "이름, 이름, 이름 외 N명".
pub fn short_recipients(list: &str) -> String {
    let names: Vec<String> = list
        .split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| match s.find('<') {
            Some(i) if i > 0 => s[..i].trim().trim_matches('"').to_owned(),
            _ => s.trim_matches(['<', '>']).to_owned(),
        })
        .collect();
    match names.len() {
        0..=3 => names.join(", "),
        n => format!("{} 외 {}명", names[..3].join(", "), n - 3),
    }
}

impl From<RowLite> for RowView {
    fn from(r: RowLite) -> Self {
        Self {
            id: r.id,
            folder_id: r.folder_id,
            subject: r.subject.unwrap_or_default(),
            from: name_or_addr(r.from_name.as_deref(), r.from_addr.as_deref()),
            to: r
                .to_list
                .as_deref()
                .map(short_recipients)
                .unwrap_or_default(),
            date: r.date_received,
            size: r.size.unwrap_or(0),
            read: r.is_read,
            flag: r.flag,
            attach: r.has_attachments,
            importance: r.importance,
            archived: r.archived,
            has_eml: r.has_eml,
            origin: r.origin,
            draft: None,
            pinned: r.pinned,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::short_recipients;

    #[test]
    fn recipients_are_shortened() {
        assert_eq!(
            short_recipients("김민수 <a@x>; b@x; \"이지영\" <c@x>; d@x; e@x"),
            "김민수, b@x, 이지영 외 2명"
        );
        assert_eq!(short_recipients(""), "");
    }
}
