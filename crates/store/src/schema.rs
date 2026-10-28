//! 스키마와 마이그레이션(계획서 6.1). `PRAGMA user_version`이 적용된 마이그레이션 수다.

use rusqlite::Connection;

use crate::Result;

/// 순서대로 한 번씩 적용한다. 이미 배포된 항목은 고치지 말고 뒤에 추가한다.
pub(crate) const MIGRATIONS: &[&str] = &[
    // 1: 계획서 6.1 + 계층 반영용 열(server_parent_id, folder_class, is_mail, archived_at),
    //    목록용 열(is_draft, item_class), 원문 큐·작업 큐 색인, meta.
    r#"
CREATE TABLE meta (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);

CREATE TABLE folders (
  id               INTEGER PRIMARY KEY,
  server_id        TEXT UNIQUE,          -- 서버에서 사라지면 NULL (로컬 보관 폴더로 유지)
  server_parent_id TEXT,
  parent_id        INTEGER REFERENCES folders(id),
  name             TEXT NOT NULL,
  folder_class     TEXT,
  well_known       TEXT,                 -- inbox, sentitems, drafts, deleteditems, junkemail, outbox
  is_mail          INTEGER NOT NULL DEFAULT 1,  -- 메일 폴더 트리에 속하는가(계층 반영 때 다시 계산)
  archived_at      INTEGER,              -- 서버에서 사라진 시각
  sync_state       TEXT,
  sort_order       INTEGER NOT NULL DEFAULT 100
);

CREATE TABLE messages (
  id               INTEGER PRIMARY KEY,
  folder_id        INTEGER NOT NULL REFERENCES folders(id),
  server_item_id   TEXT UNIQUE,          -- 서버에서 지워지면 NULL
  change_key       TEXT,
  server_state     INTEGER NOT NULL,     -- 0 동기화됨, 1 서버에서 삭제됨(로컬 보관), 2 로컬 전용(가져오기), 3 보류
  internet_msg_id  TEXT,
  conversation_id  TEXT,
  subject          TEXT,
  from_name        TEXT,
  from_addr        TEXT,
  to_list          TEXT,
  cc_list          TEXT,
  bcc_list         TEXT,
  date_received    INTEGER NOT NULL,     -- unix ms
  date_sent        INTEGER,
  size             INTEGER,
  has_attachments  INTEGER NOT NULL DEFAULT 0,
  is_read          INTEGER NOT NULL DEFAULT 0,
  flag             INTEGER NOT NULL DEFAULT 0,  -- 0 없음, 1 플래그, 2 완료
  importance       INTEGER NOT NULL DEFAULT 1,  -- 0 낮음, 1 보통, 2 높음
  is_draft         INTEGER NOT NULL DEFAULT 0,
  item_class       TEXT,
  preview          TEXT,
  eml_hash         TEXT,                 -- NULL이면 원문 미수신
  indexed          INTEGER NOT NULL DEFAULT 0,
  archived_at      INTEGER               -- 서버 삭제 감지 시각
);
CREATE INDEX ix_msg_folder_date ON messages(folder_id, date_received DESC);
CREATE INDEX ix_msg_imid ON messages(internet_msg_id);
CREATE INDEX ix_msg_conv ON messages(conversation_id);
CREATE INDEX ix_msg_eml ON messages(eml_hash);
CREATE INDEX ix_msg_need_eml ON messages(date_received DESC)
  WHERE eml_hash IS NULL AND server_item_id IS NOT NULL;

CREATE TABLE attachments (
  message_id INTEGER REFERENCES messages(id) ON DELETE CASCADE,
  idx        INTEGER,
  name       TEXT,
  mime       TEXT,
  size       INTEGER,
  content_id TEXT,
  is_inline  INTEGER,
  PRIMARY KEY (message_id, idx)
);

CREATE TABLE contacts (
  addr      TEXT PRIMARY KEY,
  name      TEXT,
  score     INTEGER,
  last_used INTEGER
);

CREATE TABLE ops (
  id              INTEGER PRIMARY KEY,
  kind            TEXT NOT NULL,
  message_id      INTEGER,
  payload         TEXT NOT NULL,
  created_at      INTEGER NOT NULL,
  attempts        INTEGER NOT NULL DEFAULT 0,
  next_attempt_at INTEGER NOT NULL DEFAULT 0,
  last_error      TEXT,
  state           INTEGER NOT NULL DEFAULT 0  -- 0 대기, 1 완료, 2 포기
);
CREATE INDEX ix_ops_due ON ops(state, next_attempt_at);
"#,
    // 2: 색인 큐(원문은 있고 아직 색인하지 않은 메일, 최근 것부터).
    r#"
CREATE INDEX ix_msg_need_index ON messages(date_received DESC)
  WHERE indexed = 0 AND eml_hash IS NOT NULL;
"#,
    // 3: 폴더별 건수(전체·안읽음·보관·원문 없음)를 본표를 읽지 않고 색인만으로 센다.
    //    본표 행은 받는 사람 목록·미리 보기로 커서 12만 통이면 전체 읽기가 수백 ms 걸렸다(4단계 측정).
    r#"
CREATE INDEX ix_msg_counts ON messages(folder_id, is_read, server_state, eml_hash);
"#,
    // 4: 작업 큐(5단계). item_errors: 서버가 항목을 거부한 횟수(네트워크 오류와 따로 센다, 5번이면 포기·되돌림).
    //    batch: 도구의 일괄 작업 번호(진행률·중지).
    r#"
ALTER TABLE ops ADD COLUMN item_errors INTEGER NOT NULL DEFAULT 0;
ALTER TABLE ops ADD COLUMN batch INTEGER;
CREATE INDEX ix_ops_batch ON ops(batch, state);
CREATE INDEX ix_ops_msg ON ops(message_id, state);
"#,
    // 5: 작성 중인 메일(6단계, 계획서 9.3 로컬 임시 저장). 첨부 파일은 EnhancedOutlook\drafts\{id}\.
    //    state: 0 작성 중, 1 보낼 편지함(발송 대기), 2 발송 실패. server_draft_id: 서버 초안(중복 발송 방지).
    r#"
CREATE TABLE drafts (
  id              INTEGER PRIMARY KEY,
  kind            TEXT NOT NULL,
  ref_message_id  INTEGER,
  to_json         TEXT NOT NULL DEFAULT '[]',
  cc_json         TEXT NOT NULL DEFAULT '[]',
  bcc_json        TEXT NOT NULL DEFAULT '[]',
  subject         TEXT NOT NULL DEFAULT '',
  body_html       TEXT NOT NULL DEFAULT '',
  include_quote   INTEGER NOT NULL DEFAULT 1,
  state           INTEGER NOT NULL DEFAULT 0,
  last_error      TEXT,
  server_draft_id TEXT,
  created_at      INTEGER NOT NULL,
  updated_at      INTEGER NOT NULL
);
CREATE INDEX ix_contacts_score ON contacts(score DESC);
"#,
    // 6: 가져온 메일(7단계)의 출처. NULL = 서버, 'import' = EML 가져오기, 'html' = HTML 백업에서 복원.
    r#"
ALTER TABLE messages ADD COLUMN origin TEXT;
"#,
    // 7: HCL Domino 계정 연동(0.5.0)으로 가져온 메일(웹메일 문서 UNID). 다시 가져오지 않으려고 둔다:
    //    로컬에서 지운 메일도 기록은 남는다(message_id만 비움). server_deleted_at: 자동 정리로 서버에서 지운 시각.
    r#"
CREATE TABLE domino_items (
  unid              TEXT PRIMARY KEY,
  message_id        INTEGER REFERENCES messages(id) ON DELETE SET NULL,
  noteid            TEXT NOT NULL,
  view              TEXT NOT NULL,
  fetched_at        INTEGER NOT NULL,
  server_deleted_at INTEGER
);
CREATE INDEX ix_domino_msg ON domino_items(message_id);
"#,
    // 8: HCL Domino 양방향 동기화(0.6.0). srv_*: 마지막으로 본 서버 상태(메일함 열쇠·'new'·★) — 서버에서 바뀐 것만
    //    로컬에 반영하려고(NULL은 아직 모름). domino_folders: 웹메일 메일함 ↔ 로컬 폴더.
    //    domino_ops: 앱에서 바꾼 것을 웹메일에 보내는 대기열(0 대기, 1 완료, 2 포기).
    r#"
ALTER TABLE domino_items ADD COLUMN srv_box TEXT;
ALTER TABLE domino_items ADD COLUMN srv_new INTEGER;
ALTER TABLE domino_items ADD COLUMN srv_star INTEGER;
CREATE TABLE domino_folders (
  folder_id INTEGER PRIMARY KEY REFERENCES folders(id) ON DELETE CASCADE,
  box       TEXT NOT NULL UNIQUE
);
CREATE TABLE domino_ops (
  id              INTEGER PRIMARY KEY,
  unid            TEXT NOT NULL,
  message_id      INTEGER,
  kind            TEXT NOT NULL,
  arg             TEXT NOT NULL DEFAULT '',
  created_at      INTEGER NOT NULL,
  attempts        INTEGER NOT NULL DEFAULT 0,
  next_attempt_at INTEGER NOT NULL DEFAULT 0,
  last_error      TEXT,
  state           INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX ix_bops_due ON domino_ops(state, next_attempt_at);
CREATE INDEX ix_bops_unid ON domino_ops(unid, state);
"#,
    // 9: 작성 중인 메일을 서버 임시 보관함에 둔다(0.7.0, 사용자 요청). message_id: 임시 보관함의 그 메일 행.
    //    server_dirty: 서버에 올릴 바뀐 내용이 있음(기존 작성 중 메일도 처음 로그인 때 올린다, 사용자 결정).
    //    send_started: 보내기 요청을 서버에 보냈음(그 뒤 초안이 사라졌으면 이미 보낸 것). 예전 판은 보낼 때만
    //    서버 초안을 만들었으므로, 서버 초안이 있는 보낼 편지함 메일은 보내기를 시작한 것으로 본다.
    //    importance 0 낮음·1 보통·2 높음, sensitivity 0 보통·1 개인·2 비공개·3 비밀 우편(웹메일 메시지 옵션).
    //    server_kind: 서버 초안을 만든 방식(ComposeOperation: newMail·reply·replyAll·forward).
    //    draft_uploads: 서버 초안에 올린 첨부(열쇠 → 첨부 ID). 바뀐 첨부만 올리고 뺀 것은 지운다.
    r#"
ALTER TABLE drafts ADD COLUMN message_id INTEGER REFERENCES messages(id) ON DELETE SET NULL;
ALTER TABLE drafts ADD COLUMN importance INTEGER NOT NULL DEFAULT 1;
ALTER TABLE drafts ADD COLUMN sensitivity INTEGER NOT NULL DEFAULT 0;
ALTER TABLE drafts ADD COLUMN read_receipt INTEGER NOT NULL DEFAULT 0;
ALTER TABLE drafts ADD COLUMN delivery_receipt INTEGER NOT NULL DEFAULT 0;
ALTER TABLE drafts ADD COLUMN plain_text INTEGER NOT NULL DEFAULT 0;
ALTER TABLE drafts ADD COLUMN server_dirty INTEGER NOT NULL DEFAULT 1;
ALTER TABLE drafts ADD COLUMN send_started INTEGER NOT NULL DEFAULT 0;
ALTER TABLE drafts ADD COLUMN server_kind TEXT;
UPDATE drafts SET send_started = 1 WHERE state = 1 AND server_draft_id IS NOT NULL;
CREATE INDEX ix_drafts_msg ON drafts(message_id);
CREATE TABLE draft_uploads (
  draft_id      INTEGER NOT NULL REFERENCES drafts(id) ON DELETE CASCADE,
  key           TEXT NOT NULL,
  attachment_id TEXT NOT NULL,
  PRIMARY KEY (draft_id, key)
);
"#,
    // 10: 원문이 없는 메일도 색인 큐에 넣는다(0.7.0: 목록 속성만으로 먼저 색인, indexed 2 = 목록 속성만).
    r#"
CREATE INDEX ix_msg_need_index_any ON messages(date_received DESC) WHERE indexed = 0;
"#,
    // 11: 외부 계정으로 PGP 암호화 자동 전달(0.8.0 사용자 요청). 메일마다 한 줄, 다시 켜도 같은 메일은 한 번만.
    //     state 0 대기·1 보냄·2 실패(포기)·3 뺌(HCL Domino 자동 전달 사본 등). mode 0 제목만·1 제목과 내용·2 전부(보낼 때의 설정).
    r#"
CREATE TABLE forwards (
  message_id      INTEGER PRIMARY KEY,
  created_at      INTEGER NOT NULL,
  state           INTEGER NOT NULL DEFAULT 0,
  attempts        INTEGER NOT NULL DEFAULT 0,
  next_attempt_at INTEGER NOT NULL DEFAULT 0,
  last_error      TEXT,
  done_at         INTEGER,
  note            TEXT
);
CREATE INDEX ix_forwards_due ON forwards(state, next_attempt_at);
"#,
    // 12: 외부 계정 PGP 자동 전달 취소(0.8.2 사용자 결정: 회사 TLS 검사 장비가 외부 SMTP 연결에 끼어들어 쓸 수 없음).
    //     대기열 표와 기준 번호를 지운다.
    r#"
DROP TABLE IF EXISTS forwards;
DELETE FROM meta WHERE key = 'forward_after_id';
"#,
    // 13: 회신·전달의 인용문을 작성 창에서 고친 것(0.9.0 사용자 요청). NULL이면 보낼 때 원문으로 만든다.
    r#"
ALTER TABLE drafts ADD COLUMN quote_html TEXT;
"#,
];

pub(crate) fn configure(conn: &Connection) -> Result<()> {
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    // 큰 정렬·임시 표를 %TEMP%의 임시 파일이 아니라 메모리에 둔다(포터블 폴더 밖에 쓰지 않음, 계획서 3장).
    conn.pragma_update(None, "temp_store", "MEMORY")?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    Ok(())
}

/// 아직 적용하지 않은 마이그레이션을 한 트랜잭션씩 적용한다. 적용 뒤 버전을 돌려준다.
pub(crate) fn migrate(conn: &mut Connection) -> Result<usize> {
    let current: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    let current = usize::try_from(current).unwrap_or(0);
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(current) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(
            None,
            "user_version",
            i64::try_from(i + 1).unwrap_or(i64::MAX),
        )?;
        tx.commit()?;
    }
    Ok(MIGRATIONS.len())
}
