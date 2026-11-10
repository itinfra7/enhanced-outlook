use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// 항목 ID(`Id` 약 152자 base64) + `ChangeKey`(40자). 변경 작업 응답의 최신 ChangeKey를 이어서 쓴다.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct ItemId {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct FolderId {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change_key: Option<String>,
}

/// 요청에 넣는 폴더 지정: 잘 알려진 이름(`inbox`, `msgfolderroot` 등) 또는 서버 ID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FolderRef {
    Distinguished(String),
    Id(String),
}

impl FolderRef {
    pub fn distinguished(name: &str) -> Self {
        Self::Distinguished(name.to_owned())
    }
}

impl From<&FolderId> for FolderRef {
    fn from(id: &FolderId) -> Self {
        Self::Id(id.id.clone())
    }
}

/// `DeleteItem`의 삭제 방식(계획서 5.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteType {
    /// 지운 편지함으로 이동.
    MoveToDeletedItems,
    /// 복구 가능한 항목으로(웹메일 "삭제된 항목 복구"로 되살릴 수 있음).
    SoftDelete,
    /// 영구 삭제.
    HardDelete,
}

impl DeleteType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MoveToDeletedItems => "MoveToDeletedItems",
            Self::SoftDelete => "SoftDelete",
            Self::HardDelete => "HardDelete",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Folder {
    pub folder_id: FolderId,
    #[serde(default)]
    pub parent_folder_id: Option<FolderId>,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub folder_class: Option<String>,
    #[serde(default)]
    pub distinguished_folder_id: Option<String>,
    #[serde(default)]
    pub total_count: Option<i64>,
    #[serde(default)]
    pub child_folder_count: Option<i64>,
    #[serde(default)]
    pub unread_count: Option<i64>,
}

impl Folder {
    /// 메일 폴더인가: `FolderClass`가 비었거나 `^IPF(\.Note(\..*)?)?$`(계획서 5.3).
    pub fn is_mail(&self) -> bool {
        let class = self.folder_class.as_deref().unwrap_or("");
        let lower = class.to_ascii_lowercase();
        lower.is_empty() || lower == "ipf" || lower == "ipf.note" || lower.starts_with("ipf.note.")
    }
}

/// 메일 폴더 트리의 한 노드(깊이 우선 순서).
#[derive(Debug, Clone)]
pub struct FolderNode {
    pub folder: Folder,
    pub depth: usize,
    /// "상위 / 하위" 형식의 표시 경로.
    pub path: String,
}

/// 루트 아래 메일 폴더만 깊이 우선으로 나열한다. 메일이 아닌 폴더는 하위까지 제외한다.
pub fn mail_folder_tree(folders: &[Folder], root_id: &str) -> Vec<FolderNode> {
    let mut children: HashMap<&str, Vec<&Folder>> = HashMap::new();
    for f in folders {
        let parent = f
            .parent_folder_id
            .as_ref()
            .map_or(root_id, |p| p.id.as_str());
        children.entry(parent).or_default().push(f);
    }
    let mut out = Vec::new();
    let mut stack = Vec::new();
    push_mail_children(&children, &mut stack, root_id, 0, "");
    while let Some((f, depth, path)) = stack.pop() {
        push_mail_children(&children, &mut stack, &f.folder_id.id, depth + 1, &path);
        out.push(FolderNode {
            folder: f.clone(),
            depth,
            path,
        });
    }
    out
}

fn push_mail_children<'a>(
    children: &HashMap<&str, Vec<&'a Folder>>,
    stack: &mut Vec<(&'a Folder, usize, String)>,
    parent: &str,
    depth: usize,
    path: &str,
) {
    let Some(kids) = children.get(parent) else {
        return;
    };
    // 스택이라 역순으로 넣어야 서버 순서대로 나온다.
    for &f in kids.iter().rev() {
        if f.is_mail() {
            let p = if path.is_empty() {
                f.display_name.clone()
            } else {
                format!("{path} / {}", f.display_name)
            };
            stack.push((f, depth, p));
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct Mailbox {
    pub name: Option<String>,
    pub email_address: Option<String>,
    pub routing_type: Option<String>,
    pub mailbox_type: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct Recipient {
    pub mailbox: Option<Mailbox>,
}

/// 받는 사람 목록의 한 항목. 주소가 바로 오는 형태와 `Mailbox` 안에 오는 형태를 모두 받는다.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct RecipientEntry {
    pub name: Option<String>,
    pub email_address: Option<String>,
    pub routing_type: Option<String>,
    pub mailbox: Option<Mailbox>,
}

impl RecipientEntry {
    pub fn email(&self) -> Option<&str> {
        self.email_address
            .as_deref()
            .or_else(|| self.mailbox.as_ref()?.email_address.as_deref())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct Flag {
    pub flag_status: Option<String>,
}

/// 회신 종류(계획서 5.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplyKind {
    Reply,
    ReplyAll,
    Forward,
}

impl ReplyKind {
    pub fn item_type(self) -> &'static str {
        match self {
            Self::Reply => "ReplyToItem:#Exchange",
            Self::ReplyAll => "ReplyAllToItem:#Exchange",
            Self::Forward => "ForwardItem:#Exchange",
        }
    }

    pub fn subject_prefix(self) -> &'static str {
        match self {
            Self::Reply | Self::ReplyAll => "RE: ",
            Self::Forward => "FW: ",
        }
    }

    pub fn compose_operation(self) -> &'static str {
        match self {
            Self::Reply => "reply",
            Self::ReplyAll => "replyAll",
            Self::Forward => "forward",
        }
    }
}

/// 보낼 주소(이름은 비어도 됨).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Address {
    pub name: String,
    pub email: String,
}

/// 올릴 파일 첨부(계획서 5.7 ②).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileAttachment {
    pub name: String,
    pub content_type: String,
    pub content: Vec<u8>,
    pub is_inline: bool,
    /// 인라인 이미지의 `cid:` 값(꺾쇠 없이).
    pub content_id: Option<String>,
}

/// 첨부 추가 응답(계획서 5.7 ②). `root_item_change_key`가 초안의 새 ChangeKey다.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct AttachmentId {
    pub id: String,
    pub root_item_id: Option<String>,
    pub root_item_change_key: Option<String>,
}

/// 목록용 속성(계획서 5.4). 날짜는 서버 문자열 그대로(`2026-10-15T14:06:38+09:00`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct ItemSummary {
    pub item_id: Option<ItemId>,
    pub subject: Option<String>,
    pub date_time_received: Option<String>,
    pub date_time_sent: Option<String>,
    pub is_read: Option<bool>,
    pub has_attachments: Option<bool>,
    pub size: Option<i64>,
    pub from: Option<Recipient>,
    pub display_to: Option<String>,
    pub display_cc: Option<String>,
    pub to_recipients: Option<Vec<RecipientEntry>>,
    pub cc_recipients: Option<Vec<RecipientEntry>>,
    pub bcc_recipients: Option<Vec<RecipientEntry>>,
    pub flag: Option<Flag>,
    pub importance: Option<String>,
    pub item_class: Option<String>,
    pub internet_message_id: Option<String>,
    pub conversation_id: Option<ItemId>,
    pub preview: Option<String>,
    /// 요청 속성 이름은 `ItemLastModifiedTime`, 응답 키는 `LastModifiedTime`(계획서 5.1).
    pub last_modified_time: Option<String>,
    pub is_draft: Option<bool>,
    /// 웹메일의 고정(1.1.0): 고정한 메일만 4500-09-01([`crate::is_pinned`]).
    pub renew_time: Option<String>,
    /// `Attachments`를 요청했을 때(초안의 첨부 ID, 0.7.0 임시 보관함 저장).
    pub attachments: Option<Vec<ServerAttachment>>,
}

/// 항목의 첨부 한 개(`GetItem` `Attachments`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct ServerAttachment {
    pub attachment_id: Option<AttachmentId>,
    pub name: Option<String>,
    pub content_id: Option<String>,
    pub is_inline: Option<bool>,
    pub size: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum ChangeKind {
    Create,
    Update,
    Delete,
    ReadFlag,
    Other,
}

/// `SyncFolderItems` 변경 한 건. `type_name`은 진단용 원래 `__type`(`:#Exchange` 제외).
#[derive(Debug, Clone, PartialEq)]
pub struct ItemChange {
    pub kind: ChangeKind,
    pub type_name: String,
    pub item: Option<ItemSummary>,
    pub item_id: Option<ItemId>,
    pub is_read: Option<bool>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FolderChange {
    pub kind: ChangeKind,
    pub type_name: String,
    pub folder: Option<Folder>,
    pub folder_id: Option<FolderId>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HierarchySync {
    pub changes: Vec<FolderChange>,
    pub sync_state: String,
    pub includes_last: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ItemSync {
    pub changes: Vec<ItemChange>,
    pub sync_state: String,
    pub includes_last: bool,
    pub total_count: Option<i64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FindItemPage {
    pub items: Vec<ItemSummary>,
    pub includes_last: bool,
    pub indexed_paging_offset: Option<i64>,
    pub total_items_in_view: Option<i64>,
}

/// `GetOwaUserConfiguration`에서 쓰는 값(계획서 5.10). 용량 단위는 바이트.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct UserConfig {
    pub email_address: Option<String>,
    /// 대표 주소 말고 이 계정을 가리키는 주소(로그온 주소·UPN, 서버가 주면). 전체 회신에서 나를 뺄 때 쓴다.
    pub other_addresses: Vec<String>,
    pub display_name: Option<String>,
    pub quota_used: Option<i64>,
    pub quota_warning: Option<i64>,
    pub quota_send: Option<i64>,
    pub max_message_size_kb: Option<i64>,
    pub max_recipients: Option<i64>,
    pub blocked_file_types: Vec<String>,
    /// 웹메일 설정의 서명(설정했을 때만, 0.7.0 "서명 삽입").
    pub signature: Option<Signature>,
}

/// 웹메일 서명(`UserOptions`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Signature {
    pub html: String,
    /// 새 메일에 자동으로 넣기.
    pub auto_new: bool,
    /// 회신·전달에 자동으로 넣기.
    pub auto_reply: bool,
}
