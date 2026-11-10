<script lang="ts">
  // 기본 화면(계획서 9.1): 폴더 트리 / 가상 목록 / 읽기 창, 상단 검색창, 하단 상태 표시줄.
  // 시작하면 로그인 전에 마지막 상태를 바로 그리고 그 위에 로그인 창을 띄운다(4.1-3, 9.5).
  // 5단계: 여러 개 선택, 읽음·플래그·이동·복사·삭제·완전 삭제(로컬에 바로 반영, 서버는 뒤에서), 도구(9.4).
  import { onMount, tick } from 'svelte';
  import { SvelteSet } from 'svelte/reactivity';
  import { listen } from '@tauri-apps/api/event';
  import { api, errorText } from './lib/api';
  import { ListModel } from './lib/list.svelte';
  import ConfirmDialog from './lib/ConfirmDialog.svelte';
  import ContextMenu from './lib/ContextMenu.svelte';
  import Compose from './lib/compose/Compose.svelte';
  import FolderPicker from './lib/FolderPicker.svelte';
  import FolderTree from './lib/FolderTree.svelte';
  import LoginOverlay from './lib/LoginOverlay.svelte';
  import ServerSetup from './lib/ServerSetup.svelte';
  import MessageList from './lib/MessageList.svelte';
  import Reader from './lib/Reader.svelte';
  import SearchBox from './lib/SearchBox.svelte';
  import SettingsDialog from './lib/SettingsDialog.svelte';
  import StatusBar from './lib/StatusBar.svelte';
  import ActivityDialog from './lib/ActivityDialog.svelte';
  import ToolsDialog from './lib/ToolsDialog.svelte';
  import { afterPaint, runPerf } from './lib/perf';
  import { runUiTest } from './lib/uitest';
  import { runMem } from './lib/memtest';
  import { runImageCheck } from './lib/imgcheck';
  import { searchTerms } from './lib/highlight';
  import { printMessage } from './lib/print';
  import type {
    ActionResult,
    Bootstrap,
    FolderView,
    ListSort,
    MenuItem,
    MessageView,
    MessagesChanged,
    OpFailed,
    Page,
    SendResult,
    SortKey,
    Status,
  } from './lib/types';

  const scriptStart = performance.now();

  let folders = $state<FolderView[]>([]);
  let currentFolder = $state<number | null>(null);
  let mode = $state<'folder' | 'search' | 'archived'>('folder');
  let query = $state('');
  let scopeAll = $state(false);
  let searchInfo = $state('');
  const model = new ListModel();
  /** 현재 행(키보드 이동 기준) */
  let selected = $state(-1);
  let selectedId = -1;
  /** 고른 메일들(여러 개 선택) */
  const sel = new SvelteSet<number>();
  let anchor = -1;
  let msg = $state<MessageView | null>(null);
  let opening = $state(false);
  let status = $state<Status>({
    loggedIn: false,
    connecting: false,
    syncing: false,
    loginRequired: false,
    lastSync: null,
    lastError: null,
    emlDone: 0,
    emlTotal: 0,
    quotaUsed: null,
    quotaLimit: null,
    pendingOps: 0,
    domino: {
      loggedIn: false,
      connecting: false,
      syncing: false,
      loginRequired: false,
      username: null,
      lastSync: null,
      lastError: null,
      done: 0,
      total: 0,
      quotaUsed: null,
      quotaLimit: null,
      pending: 0,
    },
  });
  let username = $state('');
  /** 로그인 창에 채울 아이디(지난번에 로그인한 것, ✕로 지운다) */
  let rememberedUser = $state('');
  let rememberedDominoUser = $state('');
  let loginOpen = $state(false);
  /** Outlook 웹메일 주소(비면 처음 실행: 로그인 창 대신 주소부터 묻는다) */
  let outlookUrl = $state('');
  let serverSetupOpen = $state(false);
  let loginTitle = $derived(outlookUrl ? `${outlookUrl.replace(/^https?:\/\//, '')} 로그인` : 'Outlook 로그인');

  /** Outlook 로그인 창을 연다. 주소를 아직 모르면 주소부터. */
  function openOutlookLogin() {
    if (outlookUrl) loginOpen = true;
    else serverSetupOpen = true;
  }
  let loginReason = $state('');
  // HCL Domino 계정 연동(0.5.0): 켜 두었으면 Outlook에 로그인한 뒤 HCL Domino 로그인 창을 띄운다.
  let dominoEnabled = $state(false);
  let bnLoginOpen = $state(false);
  let bnReason = $state('');
  let toastText = $state('');
  let list = $state<MessageList>();
  let searchBox = $state<SearchBox>();
  let perfMode = false;

  let menu = $state<{ x: number; y: number; items: MenuItem[] } | null>(null);
  let picker = $state<'move' | 'copy' | null>(null);
  let confirm = $state<{
    title: string;
    message: string;
    detail?: string;
    label: string;
    double?: string;
    run: () => Promise<void>;
  } | null>(null);
  let tools = $state<{
    tab: 'export' | 'import' | 'server' | 'purge' | 'move' | 'dupes' | 'archived' | 'space';
    /** 폴더 오른쪽 클릭에서 열었으면 그 폴더 */
    folder?: FolderView;
  } | null>(null);
  let settingsOpen = $state(false);
  let activityOpen = $state(false);

  let openSeq = 0;
  let loadWaiter: (() => void) | null = null;
  let toastTimer: ReturnType<typeof setTimeout> | undefined;
  let foldersTimer: ReturnType<typeof setTimeout> | undefined;
  let refreshTimer: ReturnType<typeof setTimeout> | undefined;
  let sawEml = false;

  let folder = $derived(folders.find((f) => f.id === currentFolder) ?? null);
  let archivedCount = $derived(folders.reduce((n, f) => n + f.archivedMail, 0));
  // ---- 창 나누기: 폴더 목록·메일 목록 폭을 경계를 끌어 바꾼다(두 번 누르면 처음 폭) ----
  const FOLDERS_W = 220;
  const LIST_W = 380;
  let foldersW = $state(FOLDERS_W);
  let listW = $state(LIST_W);
  let dragging = $state<'' | 'folders' | 'list'>('');
  /** 실제로 움직인 뒤에만 덮개를 깐다(그냥 두 번 누르기는 경계가 받아야 처음 폭으로 돌아간다) */
  let cover = $state(false);

  // 마우스 붙잡기(pointer capture)와 읽기 창 iframe의 pointer-events 끄기를 쓰지 않는다: 끈 뒤 본문 휠 스크롤이
  // 먹통이 됐다(사용자 지적 2026-10-21, 글자를 끌어야 풀림). 끄는 동안만 화면 전체 덮개가 마우스를 받는다.
  function startDrag(e: PointerEvent, which: 'folders' | 'list') {
    if (e.button !== 0) return;
    e.preventDefault();
    const [x0, f0, l0] = [e.clientX, foldersW, listW];
    dragging = which;
    const up = () => {
      window.removeEventListener('pointermove', move);
      window.removeEventListener('pointerup', up);
      window.removeEventListener('pointercancel', up);
      window.removeEventListener('blur', up);
      dragging = '';
      cover = false;
      if (!perfMode) void api.setPaneWidths(foldersW, listW);
    };
    const move = (ev: PointerEvent) => {
      // 창 밖에서 단추를 뗐다.
      if (ev.buttons === 0) return up();
      const dx = ev.clientX - x0;
      if (dx !== 0) cover = true;
      if (which === 'folders') foldersW = Math.round(Math.min(480, Math.max(140, f0 + dx)));
      else listW = Math.round(Math.min(Math.max(260, window.innerWidth - foldersW - 320), Math.max(260, l0 + dx)));
    };
    window.addEventListener('pointermove', move);
    window.addEventListener('pointerup', up);
    window.addEventListener('pointercancel', up);
    window.addEventListener('blur', up);
  }

  function resetWidth(which: 'folders' | 'list') {
    if (which === 'folders') foldersW = FOLDERS_W;
    else listW = LIST_W;
    if (!perfMode) void api.setPaneWidths(foldersW, listW);
  }

  let showTo = $derived(
    mode === 'folder' && ['sentitems', 'drafts', 'outbox'].includes(folder?.wellKnown ?? ''),
  );
  let info = $derived(
    (mode === 'search'
      ? searchInfo
      : mode === 'archived'
        ? `보관 메일 · ${model.total}통`
        : folder
          ? `${folder.name} · ${model.total}통${folder.unread ? ` · 안읽음 ${folder.unread}` : ''}` +
            (folder.archivedMail ? ` · 보관 ${folder.archivedMail}` : '')
          : '') + (sel.size > 1 ? ` · ${sel.size}통 선택` : ''),
  );

  function toast(text: string, ms = 3200) {
    toastText = text;
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => (toastText = ''), ms);
  }

  /** 로그인한 뒤 받은 편지함을 연다(처음 설치라 폴더가 아직 없으면 첫 동기화로 생길 때). */
  let wantInbox = false;

  /** 시작 폴더: 통합 편지함(켰으면), 아니면 받은 편지함 */
  function homeFolder(): FolderView | undefined {
    return folders.find((f) => f.wellKnown === 'unified') ?? folders.find((f) => f.wellKnown === 'inbox');
  }

  async function openInbox(): Promise<boolean> {
    const inbox = homeFolder();
    if (!inbox) return false;
    wantInbox = false;
    if (mode === 'search') query = '';
    await selectFolder(inbox.id);
    return true;
  }

  async function reloadFolders() {
    try {
      folders = await api.folders();
      if (wantInbox && (await openInbox())) return;
      if (currentFolder != null && !folders.some((f) => f.id === currentFolder)) {
        const inbox = homeFolder() ?? folders.find((f) => f.id >= 0);
        if (inbox) await selectFolder(inbox.id);
      } else if (currentFolder == null && folders.length) {
        const inbox = homeFolder() ?? folders.find((f) => f.id >= 0);
        if (inbox) await selectFolder(inbox.id);
      }
    } catch (e) {
      toast(errorText(e));
    }
  }

  function scheduleFolders() {
    clearTimeout(foldersTimer);
    foldersTimer = setTimeout(() => void reloadFolders(), 400);
  }

  function scheduleRefresh() {
    clearTimeout(refreshTimer);
    refreshTimer = setTimeout(() => model.refresh(), 250);
  }

  function clearSelection() {
    selected = -1;
    selectedId = -1;
    sel.clear();
    anchor = -1;
    msg = null;
  }

  /** 폴더 전환. Rust 처리 시간(ms)을 돌려준다(측정용). */
  async function selectFolder(id: number, first?: Page): Promise<number> {
    currentFolder = id;
    mode = 'folder';
    // 다음 실행(비정상 종료 뒤 포함)은 이 폴더에서 시작한다.
    if (!perfMode) void api.setLastFolder(id);
    query = '';
    const s = { ...sort };
    const isDefault = s.key === 'date' && !s.reverse;
    const ok = await model.reset(
      `f:${id}:${s.key}:${s.reverse}`,
      (o, l) => api.listMessages(id, o, l, isDefault ? null : s),
      isDefault ? first : undefined,
    );
    if (ok) clearSelection();
    return model.lastMs;
  }

  // ---- 목록 정렬(웹메일과 같은 기준 + 첨부·안읽음) ----
  let sort = $state<ListSort>({ key: 'date', reverse: false });
  const SORTS: { key: SortKey; label: string; dir: [string, string] }[] = [
    { key: 'date', label: '날짜', dir: ['최신순', '오래된순'] },
    { key: 'from', label: '보낸 사람', dir: ['가나다순', '역순'] },
    { key: 'to', label: '받는 사람', dir: ['가나다순', '역순'] },
    { key: 'subject', label: '제목', dir: ['가나다순', '역순'] },
    { key: 'size', label: '크기', dir: ['큰 것부터', '작은 것부터'] },
    { key: 'importance', label: '중요도', dir: ['높음부터', '낮음부터'] },
    { key: 'flag', label: '플래그', dir: ['플래그 먼저', '플래그 나중'] },
    { key: 'attachments', label: '첨부 파일', dir: ['첨부 먼저', '첨부 나중'] },
    { key: 'unread', label: '안읽음', dir: ['안읽음 먼저', '안읽음 나중'] },
  ];
  let sortInfo = $derived(SORTS.find((s) => s.key === sort.key) ?? SORTS[0]);

  async function setSort(next: ListSort) {
    sort = next;
    if (!perfMode) void api.setListSort(next);
    if (mode === 'folder' && currentFolder != null) await selectFolder(currentFolder);
  }

  /** 보관 메일 모아 보기(계획서 9.4). */
  async function showArchived() {
    mode = 'archived';
    query = '';
    const ok = await model.reset('archived', (o, l) => api.listArchived(o, l));
    if (ok) clearSelection();
  }

  async function runSearch(q: string): Promise<number> {
    const text = q.trim();
    if (!text) {
      await clearSearch();
      return 0;
    }
    const folderId = scopeAll ? null : currentFolder;
    const t0 = performance.now();
    try {
      const ok = await model.reset(`s:${folderId}:${text}`, (o, l) => api.search(text, folderId, o, l));
      if (!ok) return 0;
      mode = 'search';
      searchedFor = text;
      clearSelection();
      const where = folderId == null ? '전체 폴더' : (folder?.name ?? '');
      searchInfo = `검색 "${text}" (${where}) · ${model.total}통 · ${(performance.now() - t0).toFixed(0)}ms`;
    } catch (e) {
      searchInfo = `검색어를 해석할 수 없습니다: ${errorText(e)}`;
    }
    return model.lastMs;
  }

  async function clearSearch() {
    if (mode === 'search' && currentFolder != null) await selectFolder(currentFolder);
  }

  /** 마지막으로 검색한 말(검색 결과 목록·본문에서 칠한다). */
  let searchedFor = $state('');
  let terms = $derived(mode === 'search' ? searchTerms(searchedFor) : []);

  function prefetchAround(index: number) {
    const ids = [model.row(index + 1), model.row(index - 1)]
      .filter((r) => r?.hasEml)
      .map((r) => r!.id);
    if (ids.length) void api.prefetch(ids);
  }

  /** i번째 메일을 열고 본문 그림이 끝날 때까지 기다린다. 열면 바로 읽음(사용자 결정 2026-10-16). */
  async function openAt(index: number, allowRemote = false) {
    if (index < 0 || index >= model.total) return null;
    selected = index;
    list?.scrollToIndex(index);
    const row = await model.rowAt(index);
    if (!row) return null;
    selectedId = row.id;
    const seq = ++openSeq;
    // 임시 보관함의 메일은 웹메일처럼 작성 화면으로 연다(로그인 전이라 못 열면 읽기 창으로).
    if (isDraftRow(row) && !perfMode) {
      try {
        const id = await api.openDraft(row.id);
        if (seq !== openSeq) return null;
        msg = null;
        await showCompose(id);
        return null;
      } catch (e) {
        if (seq !== openSeq) return null;
        toast(errorText(e));
      }
    }
    composeId = null;
    opening = true;
    try {
      const t0 = performance.now();
      const o = await api.openMessage(row.id, allowRemote);
      if (seq !== openSeq) return null;
      const t1 = performance.now();
      const same = msg?.html === o.msg.html;
      const loaded = new Promise<void>((r) => (loadWaiter = r));
      msg = o.msg;
      if (!perfMode && !row.read) void api.markRead([row.id], true).catch(() => {});
      if (!same) await Promise.race([loaded, new Promise((r) => setTimeout(r, 3000))]);
      prefetchAround(index);
      return {
        ms: o.ms,
        cached: o.cached,
        ipc: t1 - t0,
        load: performance.now() - t1,
        inline: o.msg.inlineImages,
        htmlBytes: o.msg.html.length,
        fonts: fontNames(o.msg.html),
      };
    } catch (e) {
      if (seq === openSeq) toast(errorText(e));
      return null;
    } finally {
      if (seq === openSeq) opening = false;
    }
  }

  /** 본문이 지정한 글꼴 이름들(측정 로그 진단용). 앱 기본 글꼴 지정은 뺀다. */
  function fontNames(html: string): string {
    const names = new Set<string>();
    for (const m of html.matchAll(/font-family\s*:\s*([^;"}<]+)|face=["']?([^"'>]+)/gi)) {
      for (const n of (m[1] ?? m[2] ?? '').split(',')) {
        const name = n.trim().replace(/^['"]|['"]$/g, '').replace(/&quot;/g, '');
        if (name && name !== 'inherit') names.add(name);
      }
    }
    names.delete('Malgun Gothic');
    names.delete('Segoe UI');
    names.delete('sans-serif');
    return [...names].slice(0, 6).join('/');
  }

  // ---- 선택 ----

  /** 한 통만 고르고 연다. */
  async function openMail(folderId: number, messageId: number | null) {
    const target = folders.some((f) => f.id === folderId) ? folderId : homeFolder()?.id;
    if (target == null) return;
    if (mode === 'search') query = '';
    await selectFolder(target);
    if (messageId == null) return;
    const i = model.indexOfId(messageId);
    if (i >= 0) await selectOnly(i);
    else void api.openMessageWindow(messageId);
  }

  async function selectOnly(index: number) {
    const row = await model.rowAt(index);
    if (!row) return;
    sel.clear();
    sel.add(row.id);
    anchor = index;
    await openAt(index);
  }

  /** anchor부터 index까지 고른다(Shift). */
  async function selectRange(from: number, to: number) {
    const [a, b] = from <= to ? [from, to] : [to, from];
    const ids: number[] = [];
    for (let i = a; i <= b; i++) {
      const r = await model.rowAt(i);
      if (r) ids.push(r.id);
    }
    sel.clear();
    for (const id of ids) sel.add(id);
    selected = to;
    list?.scrollToIndex(to);
  }

  async function pick(index: number, mods: { ctrl: boolean; shift: boolean }) {
    if (mods.shift && anchor >= 0) {
      await selectRange(anchor, index);
      if (sel.size === 1) await openAt(index);
      return;
    }
    if (mods.ctrl) {
      const row = await model.rowAt(index);
      if (!row) return;
      if (sel.has(row.id)) sel.delete(row.id);
      else sel.add(row.id);
      anchor = index;
      selected = index;
      if (sel.size === 1) {
        const only = [...sel][0];
        const i = model.indexOfId(only);
        if (i >= 0) await openAt(i);
      }
      return;
    }
    await selectOnly(index);
  }

  async function selectAll() {
    try {
      const ids =
        mode === 'search'
          ? await api.listIds(null, query.trim(), scopeAll ? null : currentFolder)
          : mode === 'archived'
            ? await api.listIds(null, null, null)
            : await api.listIds(currentFolder, null, null);
      sel.clear();
      for (const id of ids) sel.add(id);
      toast(`${ids.length}통 선택`);
    } catch (e) {
      toast(errorText(e));
    }
  }

  /** 조작 대상: 고른 메일들(없으면 현재 행). */
  function targets(): number[] {
    if (sel.size > 0) return [...sel];
    return selectedId >= 0 ? [selectedId] : [];
  }

  // ---- 조작(로컬에 바로 반영, 서버는 뒤에서) ----

  function report(r: ActionResult, done: string) {
    if (r.note) toast(r.note);
    else if (r.changed > 0 && targets().length > 1) toast(`${r.changed}통 ${done}`);
  }

  /** 목록에서 빠질 수 있는 조작 뒤: 같은 자리의 다음 메일을 고른다(Outlook처럼). */
  async function afterRemoval(prevIndex: number, prevIds: number[]) {
    const { index, row } = await model.reloadAt(prevIndex);
    scheduleFolders();
    if (!row) {
      clearSelection();
      return;
    }
    if (prevIds.includes(row.id)) return; // 그대로 남아 있음(예: 검색 결과)
    sel.clear();
    sel.add(row.id);
    anchor = index;
    await openAt(index);
  }

  async function act(
    run: (ids: number[]) => Promise<ActionResult>,
    done: string,
    removes: boolean,
  ) {
    const ids = targets();
    if (!ids.length) return;
    const prev = selected;
    try {
      const r = await run(ids);
      report(r, done);
      if (removes) await afterRemoval(prev, ids);
    } catch (e) {
      toast(errorText(e));
    }
  }

  const markRead = (read: boolean) =>
    act((ids) => api.markRead(ids, read), read ? '읽음으로 표시' : '안읽음으로 표시', false);

  function toggleFlag() {
    const ids = targets();
    const r = ids.length ? model.row(model.indexOfId(ids[0])) : undefined;
    const flag = r && r.flag === 1 ? 0 : 1;
    void act((ids) => api.setFlag(ids, flag), flag ? '플래그 표시' : '플래그 지움', false);
  }

  const markFlag = (flag: number) => act((ids) => api.setFlag(ids, flag), '플래그 바꿈', false);

  /** 고정·해제(웹메일의 "고정"과 같다): 고른 메일 중 첫 메일이 고정이면 풀고, 아니면 고정한다. */
  function togglePin() {
    const ids = targets();
    const r = ids.length ? model.row(model.indexOfId(ids[0])) : undefined;
    void markPinned(!r?.pinned);
  }

  const markPinned = (pinned: boolean) =>
    act((ids) => api.setPinned(ids, pinned), pinned ? '고정함' : '고정 해제', false);

  function deleteSelected() {
    const ids = targets();
    if (!ids.length) return;
    const inTrash = mode === 'folder' && folder?.wellKnown === 'deleteditems';
    if (inTrash) {
      confirm = {
        title: '서버에서 삭제',
        message: `${ids.length}통을 서버에서 지웁니다. 로컬 사본은 남습니다.`,
        detail: '웹메일 "삭제된 항목 복구"로 되살릴 수 있습니다.',
        label: '서버에서 삭제',
        run: () => act((ids) => api.deleteMessages(ids), '서버에서 삭제', false),
      };
      return;
    }
    return act((ids) => api.deleteMessages(ids), '지운 편지함으로 옮김', mode !== 'search');
  }

  function serverDeleteSelected() {
    const ids = targets();
    if (!ids.length) return;
    confirm = {
      title: '서버에서만 삭제',
      message: `${ids.length}통을 서버에서 지웁니다. 로컬 사본은 남습니다.`,
      label: '서버에서 삭제',
      run: () => act((ids) => api.serverDelete(ids), '서버에서 삭제', false),
    };
  }

  function purgeSelected() {
    const ids = targets();
    if (!ids.length) return;
    confirm = {
      title: '완전 삭제',
      message: `${ids.length}통을 로컬과 서버에서 영구히 지웁니다.`,
      detail: '되돌릴 수 없습니다.',
      label: '완전 삭제',
      double: '되돌릴 수 없음을 확인했습니다',
      run: () => act((ids) => api.purgeMessages(ids), '완전 삭제', true),
    };
  }

  function pickTarget(id: number) {
    const kind = picker;
    picker = null;
    if (kind === 'move') void act((ids) => api.moveMessages(ids, id), '옮김', mode === 'folder');
    else if (kind === 'copy') void act((ids) => api.copyMessages(ids, id), '복사함', false);
  }

  async function oncontext(index: number, x: number, y: number) {
    const row = await model.rowAt(index);
    if (!row) return;
    if (!sel.has(row.id)) await selectOnly(index);
    const n = targets().length;
    const inTrash = mode === 'folder' && folder?.wellKnown === 'deleteditems';
    menu = {
      x,
      y,
      items: [
        { label: '새 창에서 열기', run: () => void openInWindow(index) },
        ...(n === 1 && msg?.id === row.id ? [{ label: '인쇄', shortcut: 'Ctrl+P', run: () => msg && printMessage(msg) }] : []),
        { separator: true, label: '' },
        { label: '읽음으로 표시', shortcut: 'Ctrl+Q', run: () => void markRead(true) },
        { label: '안읽음으로 표시', shortcut: 'Ctrl+U', run: () => void markRead(false) },
        { label: '플래그 표시', shortcut: 'Insert', run: () => void markFlag(1) },
        { label: '완료로 표시', run: () => void markFlag(2) },
        { label: '플래그 지우기', run: () => void markFlag(0) },
        row.pinned
          ? { label: '고정 해제', run: () => void markPinned(false) }
          : { label: '고정(맨 위에 두기)', run: () => void markPinned(true) },
        { separator: true, label: '' },
        { label: '이동…', shortcut: 'Ctrl+Shift+V', run: () => (picker = 'move') },
        { label: '복사…', run: () => (picker = 'copy') },
        { label: 'EML로 내보내기…', run: () => (tools = { tab: 'export' }) },
        { separator: true, label: '' },
        { label: inTrash ? '서버에서 삭제(로컬 보관)' : '삭제', shortcut: 'Del', run: deleteSelected },
        ...(inTrash ? [] : [{ label: '서버에서만 삭제(로컬 보관)', run: serverDeleteSelected }]),
        { label: `완전 삭제${n > 1 ? ` (${n}통)` : ''}`, shortcut: 'Shift+Del', danger: true, run: purgeSelected },
      ],
    };
  }

  /** i번째 메일을 새 창에서 연다(두 번 누르기·오른쪽 클릭 "새 창에서 열기", 웹메일과 같게). */
  // ---- 폴더 오른쪽 클릭(0.7.0, 웹메일 폴더 메뉴 기준) ----

  /** 폴더를 옮길 곳 고르기 중인 폴더 */
  let folderMoving = $state<FolderView | null>(null);

  async function folderRun(f: () => Promise<unknown>, done?: string) {
    try {
      const r = await f();
      if (typeof r === 'string') toast(r);
      else if (done) toast(done);
      scheduleFolders();
    } catch (e) {
      toast(errorText(e));
    }
  }

  function onFolderContext(f: FolderView, x: number, y: number) {
    const server = !f.local && !f.domino && !f.archived && f.id >= 0;
    const local = f.local && !f.domino;
    const fixed = f.wellKnown != null;
    // 모두 읽음은 어느 폴더에나(HCL Domino는 웹메일에도 반영, 통합 편지함은 들어 있는 메일함마다).
    const items: MenuItem[] = [
      {
        label: '모두 읽은 상태로 표시',
        disabled: f.unread === 0,
        run: () => void folderRun(() => api.folderMarkRead(f.id), '모두 읽은 상태로 표시했습니다'),
      },
    ];
    if (f.id < 0) {
      menu = { x, y, items };
      return;
    }
    if (server || local) {
      items.push(
        { separator: true, label: '' },
        {
          label: '새 하위 폴더 만들기',
          disabled: server && !status.loggedIn,
          run: () => {
            const name = window.prompt('새 폴더 이름', '');
            if (name?.trim()) void folderRun(() => api.folderCreate(f.id, name), '폴더를 만들었습니다');
          },
        },
        {
          label: '이름 바꾸기',
          disabled: fixed || (server && !status.loggedIn),
          run: () => {
            const name = window.prompt('폴더 이름', f.name);
            if (name?.trim() && name.trim() !== f.name) void folderRun(() => api.folderRename(f.id, name));
          },
        },
        {
          label: '삭제',
          danger: true,
          disabled: fixed || (server && !status.loggedIn),
          run: () =>
            (confirm = {
              title: '폴더 삭제',
              message: server
                ? `'${f.name}'을(를) 지운 편지함으로 옮깁니다.`
                : `'${f.name}'과(와) 안의 메일 ${f.total}통을 앱에서 지웁니다.`,
              label: '삭제',
              run: () => folderRun(() => api.folderDelete(f.id)),
            }),
        },
      );
    }
    if (server) {
      items.push(
        {
          label: '폴더 비우기',
          danger: true,
          disabled: !status.loggedIn || f.total === 0,
          run: () =>
            (confirm = {
              title: '폴더 비우기',
              message:
                f.wellKnown === 'deleteditems'
                  ? `지운 편지함의 메일 ${f.total}통을 서버에서 지웁니다. 로컬 사본은 남습니다.`
                  : `'${f.name}'의 메일 ${f.total}통을 지운 편지함으로 옮깁니다.`,
              label: '비우기',
              run: () => folderRun(() => api.folderEmpty(f.id), '폴더를 비웠습니다'),
            }),
        },
        {
          label: '이동…',
          disabled: fixed || !status.loggedIn,
          run: () => (folderMoving = f),
        },
      );
    }
    items.push(
      { separator: true, label: '' },
      {
        label: f.favorite ? '즐겨찾기에서 제거' : '즐겨찾기에 추가',
        run: () => void folderRun(() => api.folderFavorite(f.id, !f.favorite)),
      },
      { label: '내보내기…', run: () => (tools = { tab: 'export', folder: f }) },
    );
    menu = { x, y, items };
  }

  async function openInWindow(index: number) {
    const row = await model.rowAt(index);
    if (!row) return;
    try {
      // 임시 보관함에서 쓰던 메일은 별도 작성 창으로.
      if (isDraftRow(row)) {
        const id = await api.openDraft(row.id);
        if (composeId === id) composeId = null;
        await api.composePopout(id);
        return;
      }
      await api.openMessageWindow(row.id);
    } catch (e) {
      toast(errorText(e));
    }
  }

  // ---- 작성(6단계, 0.7.0: 웹메일처럼 본문 영역에서 쓰고 "별도 창에서 편집"으로 창을 띄운다) ----

  /** 본문 영역에서 쓰는 메일(작성 중 메일 번호). */
  let composeId = $state<number | null>(null);

  /** 임시 보관함의 메일(앱이나 웹메일에서 쓰던 것). */
  function isDraftRow(row: { folderId: number; draft: number | null }): boolean {
    return row.draft != null || folders.find((f) => f.id === row.folderId)?.wellKnown === 'drafts';
  }

  /** 작성 화면을 본문 영역에 연다(이미 별도 창에 열려 있으면 그 창을 앞으로). */
  async function showCompose(id: number) {
    if (await api.composeInWindow(id).catch(() => false)) {
      await api.composePopout(id);
      return;
    }
    composeId = id;
  }

  /** 새 메일·회신·전달. 회신·전달은 지금 연 메일(없으면 현재 행)로. */
  async function compose(kind: 'new' | 'reply' | 'reply_all' | 'forward' | 'copy') {
    // 오프라인 사용(로그인 전)에는 보내기 같은 온라인 기능을 쓰지 않는다.
    if (!status.loggedIn) {
      toast('로그인하면 쓸 수 있습니다');
      return;
    }
    const ref = kind === 'new' ? null : (msg?.id ?? (selectedId >= 0 ? selectedId : null));
    if (kind !== 'new' && ref === null) {
      toast('메일을 먼저 고르세요');
      return;
    }
    try {
      await showCompose(await api.composeNew(kind, ref));
    } catch (e) {
      toast(errorText(e));
    }
  }

  // ---- 읽기 창 ----

  async function reopenCurrent() {
    if (!msg) return;
    const id = msg.id;
    try {
      const o = await api.openMessage(id, msg.remoteAllowed);
      if (msg?.id === id) msg = o.msg;
    } catch {
      // 지워졌으면 그대로 둔다.
    }
  }

  function onReaderLoaded() {
    loadWaiter?.();
    loadWaiter = null;
  }

  async function showImages(how: 'once' | 'sender' | 'domain') {
    if (!msg) return;
    if (how !== 'once') {
      try {
        const v = await api.allowRemote(how, how === 'domain' ? (msg.sender.split('@').pop() ?? '') : msg.sender);
        toast(`${how === 'domain' ? '@' : ''}${v} 이미지 항상 허용`);
      } catch (e) {
        toast(errorText(e));
      }
    }
    try {
      const o = await api.openMessage(msg.id, true);
      msg = o.msg;
    } catch (e) {
      toast(errorText(e));
    }
  }

  async function openAttachment(index: number) {
    if (!msg) return;
    try {
      await api.openAttachment(msg.id, index);
    } catch (e) {
      toast(errorText(e));
    }
  }

  async function saveAttachment(index: number) {
    if (!msg) return;
    try {
      const path = await api.saveAttachment(msg.id, index);
      if (path) toast(`저장했습니다: ${path}`);
    } catch (e) {
      toast(errorText(e));
    }
  }

  async function saveAll() {
    if (!msg) return;
    try {
      const n = await api.saveAllAttachments(msg.id);
      if (n != null) toast(`첨부 ${n}개를 저장했습니다`);
    } catch (e) {
      toast(errorText(e));
    }
  }

  // ---- 로그인 ----

  async function doLogin(u: string, p: string): Promise<string | null> {
    try {
      await api.login(u, p);
      username = u;
      rememberedUser = u;
      loginOpen = false;
      loginReason = '';
      // 로그인하면 받은 편지함에서(동기화가 바로 돌아 최신으로 채워진다).
      wantInbox = true;
      void openInbox();
      if (dominoEnabled && !status.domino.loggedIn) bnLoginOpen = true;
      return null;
    } catch (e) {
      return errorText(e);
    }
  }

  async function doDominoLogin(u: string, p: string): Promise<string | null> {
    try {
      await api.dominoLogin(u, p);
      rememberedDominoUser = u;
      bnLoginOpen = false;
      bnReason = '';
      return null;
    } catch (e) {
      return errorText(e);
    }
  }

  async function doLogout() {
    await api.logout();
    toast('로그아웃했습니다');
  }

  async function syncNow() {
    try {
      await api.syncNow();
      if (status.domino.loggedIn) await api.dominoSyncNow();
    } catch (e) {
      toast(errorText(e));
    }
  }

  // ---- 키보드 ----

  async function move(to: number | 'home' | 'end', extend = false) {
    const n = model.total;
    if (!n) return;
    let i: number;
    if (to === 'home') i = 0;
    else if (to === 'end') i = n - 1;
    else i = selected < 0 ? 0 : Math.min(n - 1, Math.max(0, selected + to));
    if (extend) {
      if (anchor < 0) anchor = Math.max(0, selected);
      await selectRange(anchor, i);
      if (sel.size === 1) await openAt(i);
      return;
    }
    await selectOnly(i);
  }

  function onListKey(e: KeyboardEvent) {
    if (e.ctrlKey || e.altKey || e.metaKey) return;
    const ext = e.shiftKey;
    switch (e.key) {
      case 'ArrowDown':
        e.preventDefault();
        void move(1, ext);
        break;
      case 'ArrowUp':
        e.preventDefault();
        void move(-1, ext);
        break;
      case 'Home':
        e.preventDefault();
        void move('home', ext);
        break;
      case 'End':
        e.preventDefault();
        void move('end', ext);
        break;
      case 'PageDown':
        e.preventDefault();
        void move(list?.pageRows() ?? 10, ext);
        break;
      case 'PageUp':
        e.preventDefault();
        void move(-(list?.pageRows() ?? 10), ext);
        break;
      case 'Enter':
        e.preventDefault();
        if (selected >= 0) void openAt(selected);
        break;
    }
  }

  /** 웹메일·Outlook과 같은 단축키. 브라우저 기본 동작(새로 고침·찾기·인쇄)은 막는다. */
  function onGlobalKey(e: KeyboardEvent) {
    if (e.isComposing || e.keyCode === 229) return;
    // 본문 영역의 작성 화면 안의 키는 작성 화면이 받는다(Del·Ctrl+R 등이 목록에 가지 않게).
    if ((e.target as HTMLElement | null)?.closest?.('.compose')) return;
    const k = e.key.toLowerCase();
    const ctrl = e.ctrlKey || e.metaKey;
    const busy =
      loginOpen || bnLoginOpen || serverSetupOpen || confirm !== null || picker !== null || tools !== null || menu !== null;
    if ((ctrl && k === 'e') || e.key === 'F3') {
      e.preventDefault();
      if (!busy) searchBox?.focus();
      return;
    }
    if (e.key === 'F5') {
      e.preventDefault();
      // 오프라인 사용 중이면 로그인 창을 연다.
      if (status.loggedIn) void syncNow();
      else if (!busy) openOutlookLogin();
      return;
    }
    if (ctrl && k === 'r') {
      e.preventDefault();
      if (!busy) void compose(e.shiftKey ? 'reply_all' : 'reply');
      return;
    }
    if (ctrl && k === 'f') {
      e.preventDefault();
      if (!busy) void compose('forward');
      return;
    }
    if (ctrl && k === 'n') {
      e.preventDefault();
      if (!busy) void compose('new');
      return;
    }
    if (ctrl && k === 'p') {
      e.preventDefault();
      if (!busy && msg) printMessage(msg);
      return;
    }
    if (ctrl && (k === 'g' || k === 'h' || k === 'j')) {
      e.preventDefault();
      return;
    }
    const t = e.target as HTMLElement;
    if (busy || t instanceof HTMLInputElement || t instanceof HTMLTextAreaElement) return;
    if (ctrl && k === 'q') {
      e.preventDefault();
      void markRead(true);
      return;
    }
    if (ctrl && k === 'u') {
      e.preventDefault();
      void markRead(false);
      return;
    }
    if (ctrl && k === 'a') {
      e.preventDefault();
      void selectAll();
      return;
    }
    if (ctrl && e.shiftKey && k === 'v') {
      e.preventDefault();
      if (targets().length) picker = 'move';
      return;
    }
    if (e.key === 'Insert') {
      e.preventDefault();
      toggleFlag();
      return;
    }
    if (e.key === 'Delete') {
      e.preventDefault();
      if (e.shiftKey) purgeSelected();
      else deleteSelected();
      return;
    }
    if (t === document.body) onListKey(e);
  }

  // ---- 이벤트 ----

  function onMessagesChanged(p: MessagesChanged) {
    scheduleFolders();
    // 통합 편지함은 여러 폴더를 함께 보여 준다.
    if (mode !== 'folder' || p.folderId === currentFolder || (currentFolder ?? 0) < 0) scheduleRefresh();
    if (msg && p.ids.includes(msg.id)) void reopenCurrent();
  }

  function onStatus(s: Status) {
    const wasSyncing = status.syncing;
    if (s.emlTotal > 0) sawEml = true;
    status = s;
    if (wasSyncing && !s.syncing && sawEml) {
      sawEml = false;
      scheduleRefresh();
      scheduleFolders();
      if (msg && !msg.hasEml) void reopenCurrent();
    }
  }

  // 새로 고친 뒤에도 같은 메일을 선택한 채로(새 메일이 위에 들어오면 위치가 바뀐다).
  $effect(() => {
    void model.version;
    if (selectedId < 0 || selected < 0) return;
    const r = model.row(selected);
    if (r && r.id !== selectedId) {
      const i = model.indexOfId(selectedId);
      if (i >= 0) selected = i;
    }
  });

  async function start() {
    let boot: Bootstrap;
    const marks: [string, number][] = [];
    try {
      boot = await api.bootstrap();
    } catch (e) {
      toast(errorText(e));
      return;
    }
    marks.push(['시작 명령 응답', performance.now()]);
    perfMode = boot.perfTest;
    if (boot.paneWidths) [foldersW, listW] = boot.paneWidths;
    if (boot.listSort) sort = boot.listSort;
    folders = boot.folders;
    status = boot.status;
    username = boot.username ?? '';
    rememberedUser = boot.rememberedUser ?? '';
    rememberedDominoUser = boot.rememberedDominoUser ?? '';
    dominoEnabled = boot.dominoEnabled;
    outlookUrl = boot.outlookUrl;
    if (boot.first) await selectFolder(boot.first.folderId, boot.first.page);
    await tick();
    marks.push(['목록 반영', performance.now()]);
    await afterPaint();
    const at = performance.now();
    const sinceStart = await api.perfSinceStart();
    if (boot.perfTest) {
      scopeAll = true;
      if (boot.memTest) {
        await runMem({
          folders: () => folders,
          selectFolder,
          openAt: (i) => openAt(i),
          search: (q) => runSearch(q),
          total: () => model.total,
          openedId: () => msg?.id ?? null,
          setScrollTop: (y) => list?.setScrollTop(y),
          maxScroll: () => list?.maxScroll() ?? 0,
          compose: async (kind) => showCompose(await api.composeNew(kind, kind === 'new' ? null : (msg?.id ?? null))),
          openSettings: (open) => (settingsOpen = open),
          openTools: (open) => (tools = open ? { tab: 'dupes' } : null),
        });
        return;
      }
      await runPerf({
        boot,
        scriptStart,
        startMarks: marks,
        firstPaint: { at, sinceStart },
        folders: () => folders,
        selectFolder,
        openAt: (i) => openAt(i),
        search: (q) => runSearch(q),
        total: () => model.total,
        row: (i) => model.row(i),
        rowAt: (i) => model.rowAt(i),
        setScrollTop: (y) => list?.setScrollTop(y),
        maxScroll: () => list?.maxScroll() ?? 0,
        blankVisible: () => list?.blankVisible() ?? 0,
      });
      return;
    }
    if (boot.imageCheck) {
      await runImageCheck();
      return;
    }
    if (boot.uiTest) {
      perfMode = false;
      await runUiTest({
        folders: () => folders,
        reloadFolders,
        selectFolder: (id) => selectFolder(id),
        showArchived,
        selectOnly,
        pick,
        selectAll,
        selection: () => [...sel],
        markRead: (read) => markRead(read),
        setFlag: (flag) => markFlag(flag),
        moveTo: (id) => act((ids) => api.moveMessages(ids, id), '옮김', true),
        copyTo: (id) => act((ids) => api.copyMessages(ids, id), '복사함', false),
        deleteSelected,
        purgeSelected,
        confirmOpen: () => confirm !== null,
        confirmRun: async () => {
          const run = confirm?.run;
          confirm = null;
          await run?.();
        },
        rowAt: (i) => model.rowAt(i),
        total: () => model.total,
        status: () => status,
        openedId: () => msg?.id ?? null,
        openLogin: () => (loginOpen = true),
        openSettings: (open) => (settingsOpen = open),
        openTools: (tab) => (tools = tab ? { tab } : null),
        // 점검은 로그인 없이 돈다: 로그인 확인만 건너뛰고 같은 길로 연다.
        compose: async (kind) =>
          showCompose(
            await api.composeNew(kind, kind === 'new' ? null : (msg?.id ?? (selectedId >= 0 ? selectedId : null))),
          ),
        composeId: () => composeId,
        search: (q, all) => {
          if (all !== undefined) scopeAll = all;
          return runSearch(q);
        },
      });
      return;
    }
    if (boot.crashedBefore) {
      toast(
        '지난번에 비정상 종료됐습니다. 데이터는 그대로입니다.',
        9000,
      );
    }
    if (!status.loggedIn) {
      loginReason = status.loginRequired ? '다시 로그인하세요.' : '';
      openOutlookLogin();
    } else if (dominoEnabled && !status.domino.loggedIn) {
      bnLoginOpen = true;
    }
  }

  onMount(() => {
    const unlisteners = [
      listen<Status>('status', (e) => onStatus(e.payload)),
      listen('folders-changed', () => scheduleFolders()),
      listen<MessagesChanged>('messages-changed', (e) => onMessagesChanged(e.payload)),
      listen<SendResult>('send-result', (e) =>
        toast(
          e.payload.error
            ? `발송 실패: ${e.payload.error} (임시 보관함에 남음)`
            : '메일을 보냈습니다',
        ),
      ),
      listen<OpFailed>('op-failed', (e) =>
        toast(`${e.payload.what} 실패, 되돌림: ${e.payload.error}`),
      ),
      listen('login-required', () => {
        loginReason = '다시 로그인하세요.';
        openOutlookLogin();
      }),
      listen('domino-login-required', () => {
        bnReason = '다시 로그인하세요.';
        bnLoginOpen = true;
      }),
      // 새 메일 알림을 눌렀다: 그 메일함(통합 편지함)에서 그 메일을 연다.
      listen<{ folderId: number; messageId: number | null }>('open-mail', (e) =>
        void openMail(e.payload.folderId, e.payload.messageId),
      ),
    ];
    void start();
    return () => {
      for (const u of unlisteners) void u.then((f) => f());
    };
  });
</script>

<svelte:window onkeydown={onGlobalKey} />

<div
  class="shell"
  class:dragging={dragging !== ''}
  style:grid-template-columns="{foldersW}px min({listW}px, calc(100vw - {foldersW}px - 300px)) minmax(0, 1fr)"
>
  <div
    class="split"
    class:on={dragging === 'folders'}
    style:left="{foldersW - 3}px"
    role="separator"
    aria-orientation="vertical"
    aria-label="폴더 목록 폭"
    onpointerdown={(e) => startDrag(e, 'folders')}
    ondblclick={() => resetWidth('folders')}
  ></div>
  <div
    class="split"
    class:on={dragging === 'list'}
    style:left="calc(min({foldersW + listW}px, 100vw - 300px) - 3px)"
    role="separator"
    aria-orientation="vertical"
    aria-label="메일 목록 폭"
    onpointerdown={(e) => startDrag(e, 'list')}
    ondblclick={() => resetWidth('list')}
  ></div>
  {#if cover}<div class="drag-cover"></div>{/if}
  <header class="topbar">
    <span class="brand">Enhanced Outlook</span>
    <SearchBox
      bind:this={searchBox}
      bind:value={query}
      bind:scopeAll
      folderName={folder?.name ?? ''}
      onsearch={(q) => void runSearch(q)}
      onclear={() => void clearSearch()}
      onleave={() => list?.focus()}
    />
    <span class="account">
      {#if status.loggedIn}
        <button class="plain primary" onclick={() => void compose('new')} title="새 메일 (Ctrl+N)">새 메일</button>
      {/if}
      <button class="plain" onclick={() => (tools = { tab: 'export' })}>도구</button>
      <button class="plain" onclick={() => (settingsOpen = true)}>설정</button>
      {#if status.loggedIn}
        <span class="user">{username}</span>
        <button class="plain" onclick={doLogout}>로그아웃</button>
      {:else}
        <button class="plain" onclick={openOutlookLogin}>로그인</button>
      {/if}
    </span>
  </header>

  <aside class="pane folders">
    <FolderTree
      {folders}
      current={mode === 'folder' ? currentFolder : null}
      archivedView={mode === 'archived'}
      onselect={(id) => void selectFolder(id)}
      onarchived={() => void showArchived()}
      oncontext={onFolderContext}
    />
  </aside>

  <section class="pane list" aria-label="메일 목록">
    <div class="actions" role="toolbar" aria-label="메일 조작">
      <button title="읽음으로 표시 (Ctrl+Q)" onclick={() => void markRead(true)}>읽음</button>
      <button title="안읽음으로 표시 (Ctrl+U)" onclick={() => void markRead(false)}>안읽음</button>
      <button title="플래그 (Insert)" onclick={toggleFlag}>⚑</button>
      <button title="고정·고정 해제(목록 맨 위에 두기)" onclick={togglePin}>📌</button>
      <button title="이동 (Ctrl+Shift+V)" onclick={() => targets().length && (picker = 'move')}>이동</button>
      <button
        title={mode === 'folder' && folder?.wellKnown === 'deleteditems' ? '서버에서 삭제, 로컬 보관 (Del)' : '삭제 (Del)'}
        onclick={deleteSelected}>삭제</button
      >
      <button class="danger" title="로컬과 서버에서 영구히 삭제 (Shift+Del)" onclick={purgeSelected}>완전 삭제</button>
      {#if mode === 'folder'}
        <span class="sort">
          <select
            aria-label="정렬 기준"
            title="정렬 기준"
            value={sort.key}
            onchange={(e) => void setSort({ key: (e.target as HTMLSelectElement).value as SortKey, reverse: false })}
          >
            {#each SORTS as s (s.key)}<option value={s.key}>{s.label}</option>{/each}
          </select>
          <button title="정렬 방향" onclick={() => void setSort({ ...sort, reverse: !sort.reverse })}
            >{sortInfo.dir[sort.reverse ? 1 : 0]}</button
          >
        </span>
      {/if}
    </div>
    <div class="listbody">
      <MessageList
        bind:this={list}
        {model}
        {selected}
        selection={sel}
        {showTo}
        empty={mode === 'search' ? '검색 결과 없음' : mode === 'archived' ? '보관 메일 없음' : '메일 없음'}
        highlight={terms}
        unified={mode === 'folder' && currentFolder === -1}
        onpick={(i, m) => void pick(i, m)}
        onopen={(i) => void openInWindow(i)}
        oncontext={(i, x, y) => void oncontext(i, x, y)}
        onkey={onListKey}
      />
    </div>
  </section>

  <main class="pane reader">
    {#if sel.size > 1}
      <div class="multi">
        <p class="big">{sel.size}통 선택</p>
        <div class="multi-actions">
          <button class="btn" onclick={() => void markRead(true)}>읽음</button>
          <button class="btn" onclick={() => void markRead(false)}>안읽음</button>
          <button class="btn" onclick={() => (picker = 'move')}>이동</button>
          <button class="btn" onclick={() => (picker = 'copy')}>복사</button>
          <button class="btn" onclick={() => (tools = { tab: 'export' })}>내보내기</button>
          <button class="btn" onclick={deleteSelected}>삭제</button>
          <button class="btn danger" onclick={purgeSelected}>완전 삭제</button>
        </div>
      </div>
    {/if}
    <div class="readerwrap" class:hidden={sel.size > 1}>
      {#if composeId != null}
        {#key composeId}
          <Compose draftId={composeId} inline onclosed={() => (composeId = null)} />
        {/key}
      {:else}
      <Reader
        {msg}
        loading={opening}
        onloaded={onReaderLoaded}
        onshowimages={(how) => void showImages(how)}
        onopenattachment={(i) => void openAttachment(i)}
        onsaveattachment={(i) => void saveAttachment(i)}
        onsaveall={() => void saveAll()}
        online={status.loggedIn}
        highlight={terms}
        oncompose={(kind) => void compose(kind)}
      />
      {/if}
    </div>
  </main>

  <div class="status">
    <StatusBar
      {status}
      {info}
      domino={dominoEnabled}
      onlogin={openOutlookLogin}
      onsync={() => void syncNow()}
      ondominologin={() => (bnLoginOpen = true)}
      ondominosync={() => void api.dominoSyncNow().catch((e) => toast(errorText(e)))}
      onactivity={() => (activityOpen = true)}
    />
  </div>
</div>

{#if activityOpen}
  <ActivityDialog {status} domino={dominoEnabled} onclose={() => (activityOpen = false)} />
{/if}

{#if toastText}
  <div class="toast" role="status">{toastText}</div>
{/if}

{#if menu}
  <ContextMenu x={menu.x} y={menu.y} items={menu.items} onclose={() => (menu = null)} />
{/if}

{#if folderMoving}
  {@const moving = folderMoving}
  <FolderPicker
    title={`'${moving.name}'을(를) 옮길 곳`}
    folders={folders.filter((x) => !x.local && !x.domino && !x.archived && x.id !== moving.id)}
    exclude={moving.id}
    onpick={(to) => {
      folderMoving = null;
      void folderRun(() => api.folderMove(moving.id, to), '폴더를 옮겼습니다');
    }}
    oncancel={() => (folderMoving = null)}
  />
{/if}

{#if picker}
  <FolderPicker
    title={picker === 'move' ? `${targets().length}통을 옮길 폴더` : `${targets().length}통을 복사할 폴더`}
    {folders}
    exclude={mode === 'folder' ? currentFolder : null}
    onpick={pickTarget}
    oncancel={() => (picker = null)}
  />
{/if}

{#if confirm}
  <ConfirmDialog
    title={confirm.title}
    message={confirm.message}
    detail={confirm.detail}
    confirmLabel={confirm.label}
    danger
    double={confirm.double}
    onconfirm={() => {
      const run = confirm?.run;
      confirm = null;
      void run?.();
    }}
    oncancel={() => (confirm = null)}
  />
{/if}

{#if tools}
  <ToolsDialog
    {folders}
    folder={tools.folder ?? (mode === 'folder' ? folder : null)}
    selection={[...sel]}
    {archivedCount}
    {status}
    initialTab={tools.tab}
    onclose={() => (tools = null)}
    onshowarchived={() => void showArchived()}
  />
{/if}

{#if settingsOpen}
  <SettingsDialog
    onclose={() => (settingsOpen = false)}
    onsaved={(m, saved) => {
      toast(m);
      // 외부 이미지 정책이 바뀌었을 수 있다: 열린 메일을 다시 그린다.
      if (msg) void reopenCurrent();
      outlookUrl = saved.outlookUrl;
      const on = saved.domino.enabled && saved.domino.portal !== '';
      const turnedOn = on && !dominoEnabled;
      dominoEnabled = on;
      if (turnedOn && !status.domino.loggedIn) bnLoginOpen = true;
      // 통합 편지함을 켰으면 폴더 목록을 다시 받은 뒤 통합 편지함으로.
      if (saved.domino.enabled && saved.domino.unified && !folders.some((f) => f.id === -1)) wantInbox = true;
      scheduleFolders();
    }}
  />
{/if}

{#if serverSetupOpen}
  <ServerSetup
    onsaved={(url) => {
      outlookUrl = url;
      serverSetupOpen = false;
      loginReason = '';
      loginOpen = true;
    }}
    onoffline={() => (serverSetupOpen = false)}
  />
{:else if loginOpen}
  <LoginOverlay
    username={username || rememberedUser}
    reason={loginReason}
    title={loginTitle}
    onlogin={doLogin}
    onoffline={() => (loginOpen = false)}
    onforget={() => {
      username = '';
      rememberedUser = '';
      void api.forgetUser(false);
    }}
  />
{:else if bnLoginOpen}
  <LoginOverlay
    username={status.domino.username || rememberedDominoUser}
    reason={bnReason}
    onforget={() => {
      rememberedDominoUser = '';
      void api.forgetUser(true);
    }}
    onlogin={doDominoLogin}
    onoffline={() => (bnLoginOpen = false)}
    title="HCL Domino 로그인"
    closeLabel="HCL Domino 오프라인"
  />
{/if}

<style>
  .shell {
    position: relative;
    display: grid;
    /* 열 폭은 style(끌어 바꾼 폭)로. */
    grid-template-rows: 44px 1fr 26px;
    grid-template-areas:
      'top top top'
      'folders list reader'
      'status status status';
    height: 100%;
  }

  /* 창 경계(끌어서 폭 조절) */
  .split {
    position: absolute;
    top: 44px;
    bottom: 26px;
    z-index: 6;
    width: 6px;
    cursor: col-resize;
    touch-action: none;
  }

  .split:hover,
  .split.on {
    background: rgb(15 108 189 / 0.22);
  }

  .shell.dragging {
    cursor: col-resize;
    user-select: none;
  }

  /* 끄는 동안 읽기 창 iframe 위까지 덮어 마우스를 받는다 */
  .drag-cover {
    position: fixed;
    inset: 0;
    z-index: 50;
    cursor: col-resize;
  }

  .topbar {
    grid-area: top;
    display: flex;
    align-items: center;
    gap: 16px;
    padding: 0 12px;
    background: var(--bg-bar);
    border-bottom: 1px solid var(--border);
  }

  .brand {
    flex: none;
    width: 196px;
    font-weight: 700;
    color: var(--accent);
  }

  .account {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-left: auto;
    font-size: 12px;
  }

  .user {
    color: var(--fg-muted);
  }

  .plain.primary {
    border-color: var(--accent);
    background: var(--accent);
    color: #fff;
  }

  .plain {
    height: 26px;
    padding: 0 10px;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--bg);
    color: var(--fg);
    font: inherit;
    cursor: pointer;
  }

  .pane {
    min-width: 0;
    min-height: 0;
    overflow: hidden;
  }

  .folders {
    grid-area: folders;
    overflow-y: auto;
    background: var(--bg-pane);
    border-right: 1px solid var(--border);
  }

  .list {
    grid-area: list;
    display: flex;
    flex-direction: column;
    border-right: 1px solid var(--border);
  }

  .actions {
    flex: none;
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 2px;
    padding: 4px 6px;
    border-bottom: 1px solid var(--border);
    background: var(--bg);
  }

  .actions button {
    height: 26px;
    padding: 0 8px;
    border: 0;
    border-radius: 4px;
    background: none;
    color: var(--fg);
    font: inherit;
    font-size: 12.5px;
    cursor: pointer;
  }

  .actions button:hover {
    background: var(--bg-hover);
  }

  .actions .danger {
    margin-left: auto;
    color: #c50f1f;
  }

  .sort {
    display: inline-flex;
    align-items: center;
    gap: 2px;
  }

  .sort select {
    height: 26px;
    padding: 0 4px;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--bg);
    color: var(--fg);
    font: inherit;
    font-size: 12.5px;
  }

  .listbody {
    flex: 1;
    min-height: 0;
  }

  .reader {
    grid-area: reader;
    position: relative;
  }

  .readerwrap {
    height: 100%;
  }

  .readerwrap.hidden {
    visibility: hidden;
  }

  .multi {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 12px;
    z-index: 1;
  }

  .multi .big {
    margin: 0;
    font-size: 16px;
    font-weight: 600;
  }

  .multi-actions {
    display: flex;
    flex-wrap: wrap;
    justify-content: center;
    gap: 6px;
  }

  .status {
    grid-area: status;
  }

  .toast {
    position: fixed;
    left: 50%;
    bottom: 40px;
    transform: translateX(-50%);
    max-width: 70%;
    padding: 8px 14px;
    border-radius: 6px;
    font-size: 12.5px;
    color: #fff;
    background: rgb(32 32 32 / 0.92);
    box-shadow: 0 4px 16px rgb(0 0 0 / 0.2);
    z-index: 50;
  }
</style>
