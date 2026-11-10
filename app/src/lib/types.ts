// Rust `view.rs`·`commands.rs`·`state.rs`의 직렬화 형태와 맞춘다.

export interface FolderView {
  id: number;
  parentId: number | null;
  name: string;
  wellKnown: string | null;
  depth: number;
  total: number;
  unread: number;
  /** 서버에서 지워져 로컬에만 남은 메일 수 */
  archivedMail: number;
  /** 폴더 자체가 서버에서 사라짐 */
  archived: boolean;
  /** 가져오기로 만든 로컬 폴더(서버에 없음) */
  local: boolean;
  /** HCL Domino 웹메일 메일함과 이어진 폴더(또는 'HCL Domino 메일' 맨 위) */
  domino: boolean;
  /** 즐겨찾기(폴더 목록 맨 위에도 보인다) */
  favorite: boolean;
}

export interface RowView {
  id: number;
  folderId: number;
  subject: string;
  from: string;
  to: string;
  date: number;
  size: number;
  read: boolean;
  flag: number;
  attach: boolean;
  importance: number;
  archived: boolean;
  hasEml: boolean;
  /** 가져온 메일의 출처: 'import'(EML), 'html'(HTML 백업에서 복원) */
  origin: string | null;
  /** 임시 보관함에서 쓰던 메일: 0 작성 중, 1 보내는 중, 2 발송 실패 */
  draft: number | null;
  /** 고정(목록 맨 위) */
  pinned: boolean;
}

export interface Page {
  total: number;
  offset: number;
  rows: RowView[];
  /** Rust 쪽 처리 시간(ms) */
  ms: number;
}

export interface AttachmentView {
  index: number;
  name: string;
  mime: string;
  size: number;
}

export interface MessageView {
  id: number;
  folderId: number;
  subject: string;
  from: string;
  sender: string;
  to: string;
  cc: string;
  bcc: string;
  replyTo: string;
  date: number;
  html: string;
  hasRemoteImages: boolean;
  remoteAllowed: boolean;
  attachments: AttachmentView[];
  archivedAt: number | null;
  /** 가져온 메일의 출처: 'import'(EML), 'html'(HTML 백업에서 복원) */
  origin: string | null;
  hasEml: boolean;
  /** 본문이 쓰는 인라인 이미지 수 */
  inlineImages: number;
}

export interface Opened {
  msg: MessageView;
  cached: boolean;
  ms: number;
}

export interface Status {
  loggedIn: boolean;
  connecting: boolean;
  syncing: boolean;
  loginRequired: boolean;
  lastSync: number | null;
  lastError: string | null;
  emlDone: number;
  emlTotal: number;
  quotaUsed: number | null;
  quotaLimit: number | null;
  /** 서버 반영 대기 작업 수 */
  pendingOps: number;
  domino: DominoStatus;
}

/** HCL Domino 계정 연동 상태 */
export interface DominoStatus {
  loggedIn: boolean;
  connecting: boolean;
  syncing: boolean;
  loginRequired: boolean;
  /** 이번 실행 중 로그인한 아이디(메모리에만) */
  username: string | null;
  lastSync: number | null;
  lastError: string | null;
  done: number;
  total: number;
  quotaUsed: number | null;
  quotaLimit: number | null;
  /** 웹메일에 보낼 작업 수 */
  pending: number;
}

export interface ActionResult {
  changed: number;
  queued: number;
  note: string | null;
}

/** 도구 대상: 현재 폴더, 시작·마지막 날짜(yyyymmdd) 또는 전체 */
export interface Range {
  folderId: number;
  from: string | null;
  to: string | null;
  all: boolean;
}

export interface Preview {
  count: number;
  bytes: number;
  withoutEml: number;
  onServer: number;
}

export interface ToolStart {
  batch: number | null;
  count: number;
  queued: number;
  skipped: number;
  note: string | null;
}

export interface Progress {
  pending: number;
  done: number;
  abandoned: number;
}

export interface SpacePreview {
  used: number;
  limit: number;
  need: number;
  count: number;
  bytes: number;
  withoutEml: number;
  oldest: number | null;
  newest: number | null;
}

export interface ExportProgress {
  done: number;
  total: number;
  skipped: number;
  finished: boolean;
  error: string | null;
  path: string;
}

/** 가져오기(7단계) 결과 */
export interface ImportSummary {
  total: number;
  imported: number;
  restoredHtml: number;
  /** HCL Domino 백업에서 복원한 수 */
  restoredDomino: number;
  duplicates: number;
  skipped: number;
  createdFolders: string[];
  /** [폴더 경로("상위/하위"), 가져온 통수] */
  perFolder: [string, number][];
  missingFiles: number;
  extraFiles: number;
  /** 첨부 칸에 이름만 있던 첨부(내려받지 않고 저장한 백업) */
  notDownloaded: number;
  /** 로그인 전이라 넣지 않은 HCL Domino 아닌 메일 수 */
  needLogin: number;
  errors: string[];
  cancelled: boolean;
}

export interface ImportProgress {
  done: number;
  total: number;
  imported: number;
  duplicates: number;
  skipped: number;
  current: string;
  /** 가져오기가 끝나고 색인 중 */
  indexing: boolean;
  finished: boolean;
  report: ImportSummary | null;
  error: string | null;
}

export interface Recipient {
  name: string;
  email: string;
}

export interface FileView {
  name: string;
  size: number;
}

/** 전달할 원본 첨부(작성 창에서 하나씩 뺄 수 있다) */
export interface OrigFileView {
  /** "순번:이름" */
  key: string;
  name: string;
  size: number;
  excluded: boolean;
}

/** 작성 창(6단계) */
export interface DraftView {
  id: number;
  kind: 'new' | 'reply' | 'reply_all' | 'forward';
  to: Recipient[];
  cc: Recipient[];
  bcc: Recipient[];
  subject: string;
  bodyHtml: string;
  includeQuote: boolean;
  /** 인용문 문서(작성 창에서 고칠 수 있다) */
  quoteDoc: string | null;
  /** 고친 인용문(고친 적이 있으면) */
  quoteHtml: string | null;
  attachments: FileView[];
  /** 전달할 원본 첨부(뺀 것은 excluded) */
  forwardAttachments: OrigFileView[];
  /** 0 작성 중, 1 보낼 편지함, 2 발송 실패 */
  state: number;
  lastError: string | null;
  maxBytes: number;
  localOriginal: boolean;
  /** HCL Domino 메일에 회신·전달: "보내는 계정은 … 입니다." */
  senderNote: string | null;
  options: DraftOptions;
  /** 보낸 사람(웹메일 "보낸 사람" 칸) */
  from: string | null;
  /** 서버 임시 보관함에 지금 내용이 올라가 있다 */
  serverSaved: boolean;
  /** 웹메일 설정의 서명("서명 삽입", 설정했을 때만) */
  signature: string | null;
}

/** 내 템플릿(자주 쓰는 문구·그림) */
export interface Template {
  name: string;
  html: string;
}

/** 웹메일 작성 화면의 "기타 작업"·"메시지 옵션"(0.7.0) */
export interface DraftOptions {
  /** 0 낮음, 1 보통, 2 높음 */
  importance: number;
  /** 우편물 종류: 0 보통, 1 개인, 2 비공개, 3 비밀 우편 */
  sensitivity: number;
  readReceipt: boolean;
  deliveryReceipt: boolean;
  plainText: boolean;
}

export interface DraftSave {
  to: Recipient[];
  cc: Recipient[];
  bcc: Recipient[];
  subject: string;
  bodyHtml: string;
  includeQuote: boolean;
  /** 고친 인용문(고치지 않았으면 null: 보낼 때 원문으로 만든다) */
  quoteHtml: string | null;
  options: DraftOptions;
}

export interface SendResult {
  draftId: number;
  error: string | null;
}

export interface MenuItem {
  label: string;
  shortcut?: string;
  danger?: boolean;
  disabled?: boolean;
  separator?: boolean;
  run?: () => void;
}

export interface OpFailed {
  what: string;
  error: string;
}

/** 프로세스 메모리(바이트): private 전용(커밋), workingSet 작업 집합. */
export interface ProcMem {
  private: number;
  workingSet: number;
}

/** 앱 본체와 그 아래 WebView2 프로세스들의 메모리(--mem-test). */
export interface MemoryReport {
  main: ProcMem;
  webview: ProcMem;
  webviewCount: number;
}

export interface Bootstrap {
  folders: FolderView[];
  status: Status;
  username: string | null;
  perfTest: boolean;
  /** --mem-test(성능 측정 대신 메모리 반복 점검) */
  memTest: boolean;
  root: string;
  indexed: number;
  /** 폴더 트리·건수 계산 시간(ms) */
  ms: number;
  /** 시작 단계별 시각(프로세스 시작 후 ms) */
  marks: [string, number][];
  /** 받은 편지함 첫 페이지(첫 화면용) */
  first: { folderId: number; page: Page } | null;
  /** 화면 자체 점검(합성 저장소에서만) */
  uiTest: boolean;
  /** --image-check(본문 그림 점검, 읽기만) */
  imageCheck: boolean;
  /** 지난 실행이 비정상 종료됨 */
  crashedBefore: boolean;
  /** 폴더 목록·메일 목록 폭(px, 지난번에 끌어 바꾼 것) */
  paneWidths: [number, number] | null;
  /** 메일 목록 정렬(지난번에 고른 것) */
  listSort: ListSort | null;
  /** HCL Domino 계정 연동을 켜 둠 */
  dominoEnabled: boolean;
  /** Outlook 웹메일 주소(비면 처음 실행) */
  outlookUrl: string;
  /** 로그인 창에 채울 아이디(지난번에 로그인한 것) */
  rememberedUser: string | null;
  rememberedDominoUser: string | null;
}

/** 메일 목록 정렬 기준. reverse면 기준의 기본 방향(날짜는 최신순 등)을 뒤집는다. */
export type SortKey = 'date' | 'from' | 'to' | 'subject' | 'size' | 'importance' | 'flag' | 'attachments' | 'unread';

export interface ListSort {
  key: SortKey;
  reverse: boolean;
}

/** 설정(8단계, EnhancedOutlook\settings.json) */
export interface Settings {
  /** Outlook 웹메일 주소 */
  outlookUrl: string;
  inboxEverySecs: number;
  othersEverySecs: number;
  /** 'ask': 막고 보낸 사람별로 허용, 'always': 늘 표시 */
  remoteImages: 'ask' | 'always';
  cleanup: Cleanup;
  /** 글자 크기(화면 확대, %) */
  zoom: number;
  domino: DominoSettings;
  /** 새 메일 알림 내용(읽지 않은 메일 수는 늘 보인다) */
  notify: NotifyMode;
  /** 스프레드시트 붙여 넣기: 매번 묻기·표로·그림으로 */
  sheetPaste: SheetPaste;
  /** 받는 사람 자동 완성에서 숨길 도메인(하위 도메인 포함) */
  hiddenDomains: string[];
}

export type SheetPaste = 'ask' | 'table' | 'image';

export type NotifyMode = 'off' | 'arrived' | 'sender' | 'senderSubject';

/** HCL Domino 계정 연동 */
export interface DominoSettings {
  enabled: boolean;
  /** 포털 주소 */
  portal: string;
  /** HCL Domino 받은 메일 자동 정리(서버에서만 삭제) */
  cleanup: Cleanup;
  /** 통합 편지함(Outlook 받은 편지함 + HCL Domino 받은 메일·외부 메일), Outlook 폴더는 'Outlook 메일'로 묶음 */
  unified: boolean;
  /** Outlook에서 리다이렉트되어 온 HCL Domino 사본을 서버·로컬에서 지운다(Outlook 메일은 그대로) */
  removeRedirects: boolean;
}

/** 리다이렉트 사본 미리 보기 */
export interface RedirectPreview {
  count: number;
  items: { subject: string; from: string; date: number; mailbox: string }[];
  /** 짝 후보가 여럿이라 남기는 수 */
  ambiguous: number;
}

/** 도구 → 중복 메일의 메일 한 통 */
export interface DupMail {
  id: number;
  folderId: number;
  subject: string;
  from: string;
  date: number;
  size: number;
}

/** 중복 묶음: 열쇠(다시 찾아도 같다)와 메일들(첫 메일이 남길 메일) */
export interface DupGroup {
  key: string;
  mails: DupMail[];
}

/** 중복 메일 찾기 결과 */
export interface DupFound {
  groups: DupGroup[];
  /** 저장해 둔 제외 묶음 중 이번에 찾은 것(열쇠) */
  excluded: string[];
  scanned: number;
  withoutBody: number;
}

/** 중복 메일 찾기 진행 */
export interface DupProgress {
  stage: 'read' | 'body';
  done: number;
  total: number;
}

/** 작업 현황 창의 작업 한 줄 */
export interface OpLine {
  source: 'Outlook' | 'HCL Domino';
  what: string;
  subject: string | null;
  detail: string;
  createdAt: number;
  attempts: number;
  nextAt: number | null;
  error: string | null;
}

/** 작업 현황(대기·실패 작업) */
export interface ActivityView {
  outlook: OpLine[];
  outlookTotal: number;
  outlookKinds: { what: string; count: number }[];
  domino: OpLine[];
  dominoTotal: number;
  failed: OpLine[];
  indexPending: number;
  importing: boolean;
}

/** 받은 편지함 자동 정리(서버에서만 삭제) */
export interface Cleanup {
  /** 'age': days일 지난 메일, 'quota': 용량 percent% 이상이면 오래된 메일부터 */
  mode: 'off' | 'age' | 'quota';
  days: number;
  percent: number;
}

export interface SettingsView {
  settings: Settings;
  inboxChoices: number[];
  othersChoices: number[];
  remoteSenders: number;
  root: string;
  version: string;
}

export interface MessagesChanged {
  folderId: number;
  ids: number[];
}
