// Tauri 명령 호출. 인자 이름은 camelCase로 넘기면 Rust의 snake_case로 바뀐다.
import { invoke } from '@tauri-apps/api/core';
import type {
  ActionResult,
  ActivityView,
  Bootstrap,
  Cleanup,
  ListSort,
  DraftSave,
  DraftView,
  DupFound,
  FileView,
  FolderView,
  MemoryReport,
  Recipient,
  Opened,
  Page,
  Preview,
  Progress,
  Range,
  Settings,
  SettingsView,
  SpacePreview,
  Status,
  Template,
  ToolStart,
} from './types';

export const api = {
  bootstrap: () => invoke<Bootstrap>('bootstrap'),
  folders: () => invoke<FolderView[]>('folders'),
  listMessages: (folderId: number, offset: number, limit: number, sort: ListSort | null = null) =>
    invoke<Page>('list_messages', { folderId, offset, limit, sort }),
  setListSort: (sort: ListSort) => invoke<void>('set_list_sort', { sort }),
  search: (query: string, folderId: number | null, offset: number, limit: number) =>
    invoke<Page>('search', { query, folderId, offset, limit }),
  openMessage: (id: number, allowRemote: boolean) =>
    invoke<Opened>('open_message', { id, allowRemote }),
  prefetch: (ids: number[]) => invoke<void>('prefetch', { ids }),
  openMessageWindow: (id: number) => invoke<void>('open_message_window', { id }),
  appStatus: () => invoke<Status>('app_status'),
  // 외부 이미지 허용: kind는 보낸 사람 주소('sender') 또는 도메인('domain'). 맞춘 값을 돌려준다.
  allowRemote: (kind: 'sender' | 'domain', value: string) => invoke<string>('allow_remote', { kind, value }),
  removeRemoteAllow: (kind: 'sender' | 'domain', value: string) =>
    invoke<void>('remove_remote_allow', { kind, value }),
  remoteAllowlist: () => invoke<{ senders: string[]; domains: string[] }>('remote_allowlist'),
  reportBrokenImages: (id: number, failed: string[], collapsed: string[]) =>
    invoke<void>('report_broken_images', { id, failed, collapsed }),
  saveAttachment: (id: number, index: number) =>
    invoke<string | null>('save_attachment', { id, index }),
  saveAllAttachments: (id: number) => invoke<number | null>('save_all_attachments', { id }),
  openAttachment: (id: number, index: number) => invoke<void>('open_attachment', { id, index }),
  login: (username: string, password: string) => invoke<void>('login', { username, password }),
  logout: () => invoke<void>('logout'),
  syncNow: () => invoke<void>('sync_now'),
  // HCL Domino 계정 연동(0.5.0)
  dominoLogin: (username: string, password: string) => invoke<void>('domino_login', { username, password }),
  dominoLogout: () => invoke<void>('domino_logout'),
  dominoSyncNow: () => invoke<void>('domino_sync_now'),
  dominoCleanupPreview: (cleanup: Cleanup) => invoke<number | null>('domino_cleanup_preview', { cleanup }),
  // 메일 조작(5단계): 로컬에 바로 반영되고 서버 작업은 뒤에서
  markRead: (ids: number[], read: boolean) => invoke<ActionResult>('mark_read', { ids, read }),
  setFlag: (ids: number[], flag: number) => invoke<ActionResult>('set_flag', { ids, flag }),
  moveMessages: (ids: number[], to: number) => invoke<ActionResult>('move_messages', { ids, to }),
  copyMessages: (ids: number[], to: number) => invoke<ActionResult>('copy_messages', { ids, to }),
  deleteMessages: (ids: number[]) => invoke<ActionResult>('delete_messages', { ids }),
  serverDelete: (ids: number[]) => invoke<ActionResult>('server_delete', { ids }),
  purgeMessages: (ids: number[]) => invoke<ActionResult>('purge_messages', { ids }),
  listArchived: (offset: number, limit: number) => invoke<Page>('list_archived', { offset, limit }),
  listIds: (folderId: number | null, query: string | null, searchFolder: number | null) =>
    invoke<number[]>('list_ids', { folderId, query, searchFolder }),
  // 도구(9.4)
  toolPreview: (range: Range) => invoke<Preview>('tool_preview', { range }),
  toolRun: (range: Range, action: string, to: number | null) =>
    invoke<ToolStart>('tool_run', { range, action, to }),
  batchProgress: (batch: number) => invoke<Progress>('batch_progress', { batch }),
  cancelBatch: (batch: number) => invoke<number>('cancel_batch', { batch }),
  spacePreview: (percent: number) => invoke<SpacePreview>('space_preview', { percent }),
  spaceRun: (percent: number) => invoke<ToolStart>('space_run', { percent }),
  exportMail: (target: { ids?: number[]; range?: Range }, zip: boolean) =>
    invoke<number | null>('export_mail', { target, zip }),
  cancelExport: () => invoke<void>('cancel_export'),
  // 가져오기(7단계): 로컬 전용 메일로, 서버에는 올리지 않는다
  importPick: (folder: boolean) => invoke<string[]>('import_pick', { folder }),
  importStart: (paths: string[]) => invoke<void>('import_start', { paths }),
  importCancel: () => invoke<void>('import_cancel'),
  // 작성(6단계)
  composeNew: (kind: 'new' | 'reply' | 'reply_all' | 'forward', refId: number | null) =>
    invoke<number>('compose_new', { kind, refId }),
  composePopout: (draftId: number) => invoke<void>('compose_popout', { draftId }),
  composeInWindow: (draftId: number) => invoke<boolean>('compose_in_window', { draftId }),
  openDraft: (messageId: number) => invoke<number>('open_draft', { messageId }),
  composeLoad: (draftId: number) => invoke<DraftView>('compose_load', { draftId }),
  composeSave: (draftId: number, content: DraftSave, upload: boolean) =>
    invoke<void>('compose_save', { draftId, content, upload }),
  composeUpload: (draftId: number) => invoke<void>('compose_upload', { draftId }),
  composeServerSaved: (draftId: number) => invoke<boolean>('compose_server_saved', { draftId }),
  composeAddFiles: (draftId: number, paths: string[]) =>
    invoke<FileView[]>('compose_add_files', { draftId, paths }),
  composePickFiles: (draftId: number) => invoke<FileView[]>('compose_pick_files', { draftId }),
  composeRemoveFile: (draftId: number, name: string) =>
    invoke<FileView[]>('compose_remove_file', { draftId, name }),
  composeSend: (draftId: number, content: DraftSave) => invoke<void>('compose_send', { draftId, content }),
  composeDiscard: (draftId: number) => invoke<void>('compose_discard', { draftId }),
  folderMarkRead: (folderId: number) => invoke<ActionResult>('folder_mark_read', { folderId }),
  folderCreate: (parent: number | null, name: string) => invoke<void>('folder_create', { parent, name }),
  folderRename: (folderId: number, name: string) => invoke<void>('folder_rename', { folderId, name }),
  folderDelete: (folderId: number) => invoke<string>('folder_delete', { folderId }),
  folderMove: (folderId: number, to: number) => invoke<void>('folder_move', { folderId, to }),
  folderEmpty: (folderId: number) => invoke<void>('folder_empty', { folderId }),
  folderFavorite: (folderId: number, on: boolean) => invoke<void>('folder_favorite', { folderId, on }),
  activityView: () => invoke<ActivityView>('activity_view'),
  setOutlookUrl: (url: string) => invoke<string>('set_outlook_url', { url }),
  duplicatesFind: () => invoke<DupFound>('duplicates_find'),
  /** 묶음 제외·제외 취소(저장, 제외 취소할 때까지 남는다) */
  duplicatesExclude: (keys: string[], excluded: boolean) => invoke<void>('duplicates_exclude', { keys, excluded }),
  duplicatesPurge: (groups: { ids: number[]; drop: number[] }[]) =>
    invoke<ActionResult>('duplicates_purge', { groups }),
  templatesList: () => invoke<Template[]>('templates_list'),
  templatesSet: (list: Template[]) => invoke<void>('templates_set', { list }),
  contactsLocal: (query: string) => invoke<Recipient[]>('contacts_local', { query }),
  contactsServer: (query: string) => invoke<Recipient[]>('contacts_server', { query }),
  // 설정(8단계)
  getSettings: () => invoke<SettingsView>('get_settings'),
  setSettings: (settings: Settings) => invoke<Settings>('set_settings', { settings }),
  /** 글자 크기만 바꿔 저장하고 열린 창 모두에(Ctrl+휠). 맞춘 값. */
  setZoom: (zoom: number) => invoke<number>('set_zoom', { zoom }),
  cleanupPreview: (cleanup: Cleanup) => invoke<number | null>('cleanup_preview', { cleanup }),
  forgetRemoteImages: () => invoke<number>('forget_remote_images'),
  openLogs: () => invoke<void>('open_logs'),
  setLastFolder: (folderId: number | null) => invoke<void>('set_last_folder', { folderId }),
  setPaneWidths: (folders: number, list: number) => invoke<void>('set_pane_widths', { folders, list }),
  perfSinceStart: () => invoke<number>('perf_since_start'),
  perfWrite: (lines: string[], file: 'perf.log' | 'uitest.log' | 'imagecheck.log' | 'memtest.log' = 'perf.log') =>
    invoke<string>('perf_write', { lines, file }),
  perfExit: () => invoke<void>('perf_exit'),
  perfMemory: () => invoke<MemoryReport>('perf_memory'),
};

/** 명령 오류(문자열)를 사람이 읽을 문장으로. */
export function errorText(e: unknown): string {
  if (typeof e === 'string') return e;
  if (e instanceof Error) return e.message;
  return String(e);
}
