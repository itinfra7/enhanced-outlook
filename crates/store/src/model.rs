//! 저장소 행 타입. `owa` 크레이트와 독립적이다(변환은 `sync`가 한다).

/// `messages.server_state` 값(계획서 6.1).
pub mod server_state {
    pub const SYNCED: i64 = 0;
    pub const ARCHIVED: i64 = 1;
    pub const LOCAL_ONLY: i64 = 2;
    pub const PENDING: i64 = 3;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderRow {
    pub id: i64,
    pub server_id: Option<String>,
    pub server_parent_id: Option<String>,
    pub parent_id: Option<i64>,
    pub name: String,
    pub folder_class: Option<String>,
    pub well_known: Option<String>,
    pub is_mail: bool,
    pub archived_at: Option<i64>,
    pub sync_state: Option<String>,
    pub sort_order: i64,
}

/// 계층 동기화에서 받은 서버 폴더.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerFolder {
    pub server_id: String,
    pub server_parent_id: Option<String>,
    pub name: String,
    pub folder_class: Option<String>,
}

/// 목록에 쓰는 메일 속성(서버 목록 속성에서 온다).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MessageProps {
    pub internet_msg_id: Option<String>,
    pub conversation_id: Option<String>,
    pub subject: Option<String>,
    pub from_name: Option<String>,
    pub from_addr: Option<String>,
    pub to_list: Option<String>,
    pub cc_list: Option<String>,
    pub date_received: i64,
    pub date_sent: Option<i64>,
    pub size: Option<i64>,
    pub has_attachments: bool,
    pub is_read: bool,
    /// 0 없음, 1 플래그, 2 완료.
    pub flag: i64,
    /// 0 낮음, 1 보통, 2 높음.
    pub importance: i64,
    pub is_draft: bool,
    pub item_class: Option<String>,
    pub preview: Option<String>,
    /// 고정(1.1.0). `None`이면 모른다: 저장된 값을 그대로 둔다(고정 속성을 요청하지 않은 응답 등).
    pub pinned: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageRow {
    pub id: i64,
    pub folder_id: i64,
    pub server_item_id: Option<String>,
    pub change_key: Option<String>,
    pub server_state: i64,
    pub eml_hash: Option<String>,
    pub archived_at: Option<i64>,
    pub props: MessageProps,
    /// 가져온 메일의 출처: `import`(EML), `html`(HTML 백업에서 복원). 서버 메일은 없음.
    pub origin: Option<String>,
}

/// 목록 화면 한 행(필요한 열만, 계획서 4.1-5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowLite {
    pub id: i64,
    pub folder_id: i64,
    pub subject: Option<String>,
    pub from_name: Option<String>,
    pub from_addr: Option<String>,
    /// 보낸 편지함에서는 보낸 사람 대신 받는 사람을 보여 준다.
    pub to_list: Option<String>,
    pub date_received: i64,
    pub size: Option<i64>,
    pub is_read: bool,
    pub flag: i64,
    pub has_attachments: bool,
    pub importance: i64,
    /// 서버에서 지워져 로컬에만 있음.
    pub archived: bool,
    pub has_eml: bool,
    /// 가져온 메일의 출처(`import`, `html`).
    pub origin: Option<String>,
    /// 고정(맨 위, 1.1.0).
    pub pinned: bool,
}

/// 폴더별 건수(상태 표시용).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FolderCounts {
    pub folder_id: i64,
    pub total: i64,
    pub unread: i64,
    pub archived: i64,
    pub missing_eml: i64,
}

/// 원문을 받아야 할 메일.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmlTodo {
    pub message_id: i64,
    pub server_item_id: String,
    pub date_received: i64,
}

/// 색인해야 할 메일(원문 있음).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexTodo {
    pub message_id: i64,
    pub folder_id: i64,
    pub date_received: i64,
    /// 원문이 없으면(아직 못 받음 등) 목록 속성만 색인한다(0.7.0).
    pub eml_hash: Option<String>,
    pub subject: Option<String>,
    pub from_name: Option<String>,
    pub from_addr: Option<String>,
    pub to_list: Option<String>,
    pub cc_list: Option<String>,
}

/// 원문에서 얻은 받는 사람·참조·숨은 참조("이름 <주소>; ..." 형식).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AddressLists {
    pub to: String,
    pub cc: String,
    pub bcc: String,
}

/// 서버 반영 대기 작업(계획서 6.1 ops, 7장 작업 큐).
#[derive(Debug, Clone, PartialEq)]
pub struct OpRow {
    pub id: i64,
    pub kind: String,
    pub message_id: Option<i64>,
    pub payload: serde_json::Value,
    pub created_at: i64,
    pub attempts: i64,
    pub next_attempt_at: i64,
    pub last_error: Option<String>,
    /// 서버가 항목을 거부한 횟수(네트워크 오류는 세지 않음).
    pub item_errors: i64,
    /// 도구의 일괄 작업 번호.
    pub batch: Option<i64>,
}

/// 대기 중인 작업이 있어 동기화가 덮어쓰면 안 되는 것(계획서 4.1-2 낙관적 반영).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PendingGuard {
    /// 이동·삭제(지운 편지함으로) 대기: 서버 변경이 와도 폴더를 바꾸지 않는다.
    pub keep_folder: std::collections::HashSet<i64>,
    pub keep_read: std::collections::HashSet<i64>,
    pub keep_flag: std::collections::HashSet<i64>,
    /// 고정·해제 대기(1.1.0).
    pub keep_pin: std::collections::HashSet<i64>,
    /// 서버에서 삭제(로컬 보관) 대기: 보관 표시를 풀지 않는다.
    pub keep_state: std::collections::HashSet<i64>,
    /// 완전 삭제 대기: 이 서버 항목이 다시 들어와도 만들지 않는다.
    pub ignore_server_ids: std::collections::HashSet<String>,
}

/// 작업 큐 요약 한 줄(진단용).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpSummary {
    pub kind: String,
    /// 0 대기, 1 완료, 2 포기·중지.
    pub state: i64,
    pub count: i64,
    pub last_error: Option<String>,
}

/// 일괄 작업 진행(대기·완료·포기 수).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BatchProgress {
    pub pending: i64,
    pub done: i64,
    pub abandoned: i64,
}
