<script lang="ts">
  // 작성(계획서 9.3, 0.7.0): 웹메일처럼 본문 영역(`inline`)에서 쓰고 "별도 창에서 편집"으로 창을 띄운다.
  // 로컬에 5초마다 저장하고 서버 임시 보관함에는 1분마다·닫을 때·Ctrl+S에 올린다. 보내기를 누르면 바로 닫히며
  // "보낼 편지함"에서 뒤로 보낸다(실패하면 주 창에 알림, 임시 보관함에서 다시 열 수 있다).
  import { onDestroy, onMount } from 'svelte';
  import { emit, listen } from '@tauri-apps/api/event';
  import { getCurrentWebview } from '@tauri-apps/api/webview';
  import { getCurrentWindow } from '@tauri-apps/api/window';
  import { api, errorText } from '../api';
  import { bytes, clock } from '../format';
  import type { DraftOptions, DraftSave, DraftView, FileView, Recipient } from '../types';
  import Editor from './Editor.svelte';
  import Icon from './Icon.svelte';
  import { fitInView, placeAt } from '../fit';
  import {
    cellOf,
    cleanHtml,
    clearSelected,
    deleteCol,
    deleteRow,
    insertCol,
    insertRow,
    installSelectionStyle,
    markSelected,
    rectCells,
    selectedCells,
    shade,
    tableOf,
  } from './quoteTables';
  import RecipientInput from './RecipientInput.svelte';
  import { zoomInFrame } from '../zoom';
  import { emailHtml } from './tables';
  import AddressBook from './AddressBook.svelte';
  import MessageOptions from './MessageOptions.svelte';
  import Templates from './Templates.svelte';

  interface Props {
    draftId: number;
    /** 주 창·메일 창의 본문 영역에서 쓴다(아니면 별도 작성 창). */
    inline?: boolean;
    /** 본문 영역에서 닫혔다(보냄·버림·별도 창으로 옮김). */
    onclosed?: () => void;
  }

  let { draftId, inline = false, onclosed }: Props = $props();

  let draft = $state<DraftView | null>(null);
  let loadError = $state('');
  let to = $state<Recipient[]>([]);
  let cc = $state<Recipient[]>([]);
  let bcc = $state<Recipient[]>([]);
  let showBcc = $state(false);
  let subject = $state('');
  let bodyHtml = $state('');
  let includeQuote = $state(true);
  /** 인용문(원문): 작성 창에서 바로 고친다(0.9.0 사용자 요청). 고친 적이 있으면 저장할 때 함께 저장한다. */
  let quoteFrame = $state<HTMLIFrameElement>();
  let quoteHtml: string | null = null;
  let quoteLoaded = false;
  let files = $state<FileView[]>([]);
  let options = $state<DraftOptions>({
    importance: 1,
    sensitivity: 0,
    readReceipt: false,
    deliveryReceipt: false,
    plainText: false,
  });
  let dirty = false;
  /** 서버 임시 보관함에 올릴 바뀐 내용이 있다(로컬에 저장한 뒤에도). */
  let unsent = false;
  let lastUpload = Date.now();
  let savedAt = $state<number | null>(null);
  let serverSaved = $state(false);
  let error = $state('');
  let busy = $state(false);
  let dropping = $state(false);
  let closing = false;
  let toInput = $state<RecipientInput>();
  let editor = $state<Editor>();
  /** 서식 도구 모음(웹메일의 "서식 옵션") */
  let toolbar = $state(true);
  let emojiOpen = $state(false);
  /** 웹메일 "기타 작업" 메뉴 */
  let moreOpen = $state(false);
  /** "보낸 사람" 칸(웹메일처럼 숨기기·표시, 이 PC에 기억) */
  let showFrom = $state(readShowFrom());
  let pickField = $state<'to' | 'cc' | 'bcc' | null>(null);
  let optionsOpen = $state(false);
  let templatesOpen = $state(false);
  /** 알림 줄(이름 확인·접근성 결과 등) */
  let info = $state('');
  let ccInput = $state<RecipientInput>();
  let bccInput = $state<RecipientInput>();

  function readShowFrom(): boolean {
    try {
      return localStorage.getItem('compose.showFrom') !== '0';
    } catch {
      return true;
    }
  }

  function setShowFrom(v: boolean) {
    showFrom = v;
    try {
      localStorage.setItem('compose.showFrom', v ? '1' : '0');
    } catch {
      // 기억하지 못해도 된다.
    }
  }

  const IMPORTANCE = ['낮음', '보통', '높음'];

  function setOptions(o: DraftOptions) {
    options = o;
    touch();
  }

  /** 이름 확인(웹메일과 같음): 입력하다 만 주소를 칩으로 만들고, 이름 없는 주소는 주소록·서버에서 이름을 찾는다. */
  async function checkNames() {
    toInput?.commitPending();
    ccInput?.commitPending();
    bccInput?.commitPending();
    await new Promise((r) => setTimeout(r, 0));
    const unknown: string[] = [];
    const resolve = async (list: Recipient[]) =>
      Promise.all(
        list.map(async (r) => {
          if (!/^[^\s@<>,;]+@[^\s@<>,;]+\.[^\s@<>,;]+$/.test(r.email)) {
            unknown.push(r.email);
            return r;
          }
          if (r.name) return r;
          const found = [
            ...(await api.contactsServer(r.email).catch(() => [] as Recipient[])),
            ...(await api.contactsLocal(r.email).catch(() => [] as Recipient[])),
          ].find((x) => x.email.toLowerCase() === r.email.toLowerCase() && x.name);
          return found ? { ...r, name: found.name } : r;
        }),
      );
    to = await resolve(to);
    cc = await resolve(cc);
    bcc = await resolve(bcc);
    touch();
    info = unknown.length ? `확인하지 못한 주소: ${unknown.join(', ')}` : '이름을 확인했습니다.';
  }

  /** 일반 텍스트로 전환·HTML로 전환. */
  function togglePlain() {
    if (!options.plainText) {
      if (!window.confirm('일반 텍스트로 바꾸면 서식과 그림이 없어집니다.')) return;
      editor?.toPlain();
    }
    setOptions({ ...options, plainText: !options.plainText });
  }

  function checkAccessibility() {
    const issues = editor?.accessibilityIssues() ?? [];
    info = issues.length ? `접근성 문제: ${issues.join(' · ')}` : '접근성 문제가 없습니다.';
  }

  /** 메뉴 항목을 누르면 메뉴를 닫고 실행한다. */
  function run(f: () => void) {
    moreOpen = false;
    f();
  }

  const EMOJI = [
    '😀', '😃', '😄', '😁', '😆', '😅', '😂', '🙂', '😊', '😇', '😉', '😍',
    '😘', '😋', '😎', '🤔', '😐', '😏', '😮', '😢', '😭', '😤', '😡', '😱',
    '👍', '👎', '👏', '🙏', '💪', '👌', '✌️', '🤝', '🎉', '🎂', '❤️', '💔',
    '⭐', '🔥', '💯', '✅', '❌', '⚠️', '📌', '📎', '📅', '⏰', '☕', '🍀',
  ];

  const win = getCurrentWindow();

  let total = $derived(
    bodyHtml.length +
      files.reduce((n, f) => n + f.size, 0) +
      (draft?.forwardAttachments ?? []).reduce((n, f) => n + (f.excluded ? 0 : f.size), 0),
  );
  let over = $derived(draft ? total > draft.maxBytes : false);
  let kindLabel = $derived(
    ({ new: '새 메일', reply: '회신', reply_all: '전체 회신', forward: '전달' } as const)[draft?.kind ?? 'new'],
  );

  function content(): DraftSave {
    const body = quoteLoaded ? quoteFrame?.contentDocument?.body : null;
    if (quoteHtml !== null && body) quoteHtml = cleanHtml(body);
    return { to, cc, bcc, subject, bodyHtml: emailHtml(bodyHtml), includeQuote, quoteHtml, options };
  }

  // ---- 인용문 칸의 표(1.0.3 사용자 지적: 회신·전달한 원문의 표가 표로 다뤄지지 않았다) ----
  type QuoteCell = HTMLTableCellElement;
  type QuoteItem = { label: string; run: () => void } | { sep: true };
  const QUOTE_COLORS = [
    '#000000', '#444444', '#666666', '#999999', '#cccccc', '#ffffff',
    '#c00000', '#ff0000', '#ffc000', '#ffff00', '#92d050', '#00b050',
    '#00b0f0', '#0070c0', '#002060', '#7030a0', '#ff00ff', '#8a5300',
  ];
  let quoteMenu = $state<{ x: number; y: number; items: QuoteItem[] } | null>(null);
  let quotePalette = $state<{ x: number; y: number; cells: QuoteCell[] } | null>(null);

  /** 인용문 표를 바꿨다: 저장할 내용으로 표시. */
  function quoteChanged() {
    const doc = quoteFrame?.contentDocument;
    if (!doc?.body) return;
    quoteHtml = cleanHtml(doc.body);
    touch();
  }

  /** 오른쪽 클릭한 칸의 메뉴(위쪽 편집기의 표 메뉴와 같은 항목, 병합·분할은 없음). */
  function quoteTableMenu(cell: QuoteCell, x: number, y: number) {
    const doc = cell.ownerDocument;
    const picked = selectedCells(doc);
    const cells = picked.includes(cell) ? picked : [cell];
    const act = (f: () => void) => () => {
      f();
      clearSelected(doc);
      quoteChanged();
    };
    quoteMenu = {
      x,
      y,
      items: [
        { label: '위에 행 삽입', run: act(() => insertRow(cell, false)) },
        { label: '아래에 행 삽입', run: act(() => insertRow(cell, true)) },
        { label: '왼쪽에 열 삽입', run: act(() => insertCol(cell, false)) },
        { label: '오른쪽에 열 삽입', run: act(() => insertCol(cell, true)) },
        { sep: true },
        { label: '행 삭제', run: act(() => deleteRow(cell)) },
        { label: '열 삭제', run: act(() => deleteCol(cell)) },
        { label: '셀 배경색', run: () => (quotePalette = { x, y, cells }) },
        { sep: true },
        { label: '표 삭제', run: act(() => tableOf(cell)?.remove()) },
      ],
    };
  }

  function quoteShade(color: string | null) {
    const p = quotePalette;
    quotePalette = null;
    if (!p) return;
    shade(p.cells, color);
    const doc = p.cells[0]?.ownerDocument;
    if (doc) clearSelected(doc);
    quoteChanged();
  }

  /** 인용문 칸 높이(px): 원문 높이. 짧으면 CSS가 창 아래까지 채운다. 작성 화면 전체가 한 번에 스크롤된다(웹메일처럼,
   *  1.0.5 사용자 지적: 320px로 고정돼 원문이 창 도중에 끝났다). */
  let quoteH = $state(0);

  function fitQuote(doc: Document) {
    const win = doc.defaultView;
    if (!doc.body || !win) return;
    // 가로 스크롤 막대가 있으면 그만큼 더(세로는 칸 높이를 맞추므로 숨긴다).
    const hbar = Math.max(0, win.innerHeight - doc.documentElement.clientHeight);
    const next = Math.ceil(doc.body.scrollHeight) + hbar;
    // 1~2px 차이로 계속 고치지 않게(원문 CSS가 body 높이를 칸에 맞추는 경우).
    if (Math.abs(next - quoteH) > 2) quoteH = next;
  }

  /** 인용문 문서가 열리면 고칠 수 있게 한다. */
  function onQuoteLoad(e: Event) {
    const frame = e.currentTarget as HTMLIFrameElement;
    const doc = frame.contentDocument;
    zoomInFrame(doc);
    if (!doc?.body) return;
    doc.body.contentEditable = 'true';
    doc.body.spellcheck = false;
    doc.body.style.outline = 'none';
    installSelectionStyle(doc);
    // 고치거나 그림이 늦게 들어오거나 창 폭이 바뀌어 원문 높이가 바뀌면 칸 높이를 다시 맞춘다.
    const Observer = (doc.defaultView as (Window & typeof globalThis) | null)?.ResizeObserver;
    if (Observer) {
      doc.documentElement.style.overflowY = 'hidden';
      new Observer(() => fitQuote(doc)).observe(doc.body);
    }
    fitQuote(doc);
    doc.addEventListener('input', () => {
      quoteHtml = cleanHtml(doc.body);
      touch();
    });
    // 표: 끌어서 여러 칸 고르기, 오른쪽 클릭 표 메뉴.
    let anchor: QuoteCell | null = null;
    doc.addEventListener('mousedown', (ev) => {
      quoteMenu = null;
      quotePalette = null;
      if (ev.button !== 0) return;
      clearSelected(doc);
      anchor = cellOf(ev.target as Node);
    });
    doc.addEventListener('mousemove', (ev) => {
      if (!anchor || (ev.buttons & 1) === 0) return;
      const c = cellOf(ev.target as Node);
      if (c && c !== anchor && tableOf(c) === tableOf(anchor)) {
        markSelected(doc, rectCells(anchor, c));
        doc.getSelection()?.removeAllRanges();
        ev.preventDefault();
      }
    });
    doc.addEventListener('mouseup', () => (anchor = null));
    doc.addEventListener('contextmenu', (ev) => {
      const cell = cellOf(ev.target as Node);
      if (!cell) return;
      ev.preventDefault();
      const r = frame.getBoundingClientRect();
      quoteTableMenu(cell, r.left + ev.clientX, r.top + ev.clientY);
    });
    // 인용문 안에서도 Ctrl+S는 저장, 새로 고침·찾기·인쇄는 막는다(본문과 같게).
    doc.addEventListener('keydown', (ev) => {
      const k = ev.key.toLowerCase();
      if ((ev.ctrlKey || ev.metaKey) && k === 's') {
        ev.preventDefault();
        saveNow();
      } else if (ev.key === 'F5' || ((ev.ctrlKey || ev.metaKey) && ['r', 'p', 'f'].includes(k))) {
        ev.preventDefault();
      }
    });
    quoteLoaded = true;
  }

  function touch() {
    dirty = true;
    unsent = true;
    serverSaved = false;
  }

  /** 로컬에 저장한다. `upload`면 서버 임시 보관함에도 올린다(바뀐 것이 있을 때). */
  async function save(upload = false) {
    if (!draft || closing) return;
    const wantUpload = upload && unsent;
    if (!dirty && !wantUpload) return;
    dirty = false;
    if (wantUpload) {
      unsent = false;
      lastUpload = Date.now();
    }
    try {
      await api.composeSave(draftId, content(), wantUpload);
      savedAt = Date.now();
    } catch (e) {
      dirty = true;
      if (wantUpload) unsent = true;
      error = `저장 실패: ${errorText(e)}`;
    }
  }

  /** 닫기 전: 남은 내용을 저장하고 임시 보관함에 올린다(기다리지 않아도 되게 한 번에). */
  function flush() {
    if (!draft || closing) return;
    closing = true;
    if (dirty || unsent) void api.composeSave(draftId, content(), true).catch(() => {});
  }

  async function addFiles(paths: string[]) {
    try {
      files = await api.composeAddFiles(draftId, paths);
      unsent = true;
      serverSaved = false;
    } catch (e) {
      error = errorText(e);
    }
  }

  async function pickFiles() {
    try {
      const before = files.length;
      files = await api.composePickFiles(draftId);
      if (files.length !== before) {
        unsent = true;
        serverSaved = false;
      }
    } catch (e) {
      error = errorText(e);
    }
  }

  /** 전달할 원본 첨부를 빼거나 되돌린다. */
  async function excludeOriginal(key: string, excluded: boolean) {
    if (!draft) return;
    try {
      draft.forwardAttachments = await api.composeExcludeOriginal(draftId, key, excluded);
      unsent = true;
      serverSaved = false;
    } catch (e) {
      error = errorText(e);
    }
  }

  async function removeFile(name: string) {
    try {
      files = await api.composeRemoveFile(draftId, name);
      unsent = true;
      serverSaved = false;
    } catch (e) {
      error = errorText(e);
    }
  }

  /** 본문 영역이면 화면이 닫고, 별도 창이면 창을 닫는다. */
  async function close() {
    closing = true;
    if (inline) onclosed?.();
    else await win.destroy();
  }

  /** 별도 창에서 편집(웹메일과 같은 이름): 저장하고 창으로 옮긴다. */
  async function popout() {
    try {
      dirty = true;
      await save(false);
      closing = true;
      await api.composePopout(draftId);
      onclosed?.();
    } catch (e) {
      closing = false;
      error = errorText(e);
    }
  }

  async function send() {
    error = '';
    if (!to.length && !cc.length && !bcc.length) {
      error = '받는 사람을 적어 주세요.';
      toInput?.focus();
      return;
    }
    if (!subject.trim() && !window.confirm('제목 없이 보낼까요?')) return;
    busy = true;
    try {
      await api.composeSend(draftId, content());
      await close();
    } catch (e) {
      error = errorText(e);
      busy = false;
    }
  }

  async function discard() {
    if (!window.confirm('이 메일을 삭제할까요?')) return;
    try {
      await api.composeDiscard(draftId);
      await close();
    } catch (e) {
      error = errorText(e);
    }
  }

  /** 초안 저장(Ctrl+S): 임시 보관함에 바로 올린다. */
  function saveNow() {
    dirty = true;
    unsent = true;
    void save(true);
  }

  function onkeydown(e: KeyboardEvent) {
    // 본문 영역에서는 작성 화면 안의 키만 받는다.
    if (inline && !root?.contains(e.target as Node)) return;
    if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 's') {
      e.preventDefault();
      saveNow();
    }
    // 브라우저 새로 고침·찾기·인쇄를 막는다(작성 중 내용을 잃지 않게).
    if (e.key === 'F5' || ((e.ctrlKey || e.metaKey) && ['r', 'p', 'f'].includes(e.key.toLowerCase()))) {
      e.preventDefault();
    }
  }

  let root = $state<HTMLElement>();
  /** 화면 자체 점검이 서식 도구를 보는 중(같은 요청이 겹쳐 와도 한 번만) */
  let selfTesting = false;

  // 별도 창이면 창 제목을 제목에 맞춘다.
  $effect(() => {
    if (inline) return;
    const t = subject.trim() ? `${subject.trim()} - ${kindLabel}` : `${kindLabel} - Enhanced Outlook`;
    void win.setTitle(t).catch(() => {});
  });

  // 본문 영역에서 다른 메일을 고르거나 다른 작성으로 바뀌면: 저장하고 임시 보관함에 올린다.
  onDestroy(() => flush());

  onMount(() => {
    let timer: ReturnType<typeof setInterval> | undefined;
    const unlisten: Promise<() => void>[] = [];
    (async () => {
      try {
        const d = await api.composeLoad(draftId);
        draft = d;
        to = d.to;
        cc = d.cc;
        bcc = d.bcc;
        showBcc = d.bcc.length > 0;
        subject = d.subject;
        bodyHtml = d.bodyHtml;
        includeQuote = d.includeQuote;
        quoteHtml = d.quoteHtml;
        files = d.attachments;
        options = d.options;
        serverSaved = d.serverSaved;
        // 아직 임시 보관함에 없는 새 메일은 처음 저장 때 올린다.
        unsent = !d.serverSaved;
        if (d.state === 2 && d.lastError) error = `발송 실패: ${d.lastError}`;
        setTimeout(() => (to.length ? editor?.focus() : toInput?.focus()), 50);
      } catch (e) {
        loadError = errorText(e);
      }
    })();
    // 로컬 저장 5초마다, 임시 보관함은 1분마다(바뀐 것이 있을 때).
    timer = setInterval(() => void save(Date.now() - lastUpload >= 60_000), 5000);
    unlisten.push(
      listen('drafts-changed', async () => {
        if (unsent || closing) return;
        serverSaved = await api.composeServerSaved(draftId).catch(() => serverSaved);
      }),
    );
    // 화면 자체 점검(`--ui-test`, 합성 저장소)이 이 창의 서식 도구를 확인할 때.
    unlisten.push(
      listen<{ label: string }>('ui-test-compose', async (e) => {
        // 편집기가 준비된 뒤에만 답한다(점검은 답이 올 때까지 다시 묻는다).
        if (e.payload.label !== win.label || !editor || selfTesting) return;
        selfTesting = true;
        const fails = await editor.selfTest();
        await emit('ui-test-compose-result', { label: win.label, fails });
      }),
    );
    if (!inline) {
      unlisten.push(
        win.onCloseRequested(async () => {
          if (!closing) {
            dirty = true;
            await save(true);
            closing = true;
          }
        }),
      );
    }
    unlisten.push(
      getCurrentWebview().onDragDropEvent((e) => {
        if (e.payload.type === 'over' || e.payload.type === 'enter') dropping = true;
        else if (e.payload.type === 'leave') dropping = false;
        else if (e.payload.type === 'drop') {
          dropping = false;
          void addFiles(e.payload.paths);
        }
      }),
    );
    return () => {
      clearInterval(timer);
      for (const u of unlisten) void u.then((f) => f());
    };
  });
</script>

<svelte:window
  {onkeydown}
  onmousedown={(e) => {
    if (!(e.target as HTMLElement | null)?.closest?.('.qmenu, .qpal')) {
      quoteMenu = null;
      quotePalette = null;
    }
  }}
/>

{#if loadError}
  <div class="load-error">
    <p>{loadError}</p>
    <button class="btn" onclick={() => void close()}>닫기</button>
  </div>
{:else if draft}
  <div class="compose" class:dropping class:inline bind:this={root}>
    <header class="top">
      <button class="btn primary" disabled={busy || over} onclick={send}>
        {busy ? '보내는 중…' : '보내기'}
      </button>
      <button class="btn icon" title="첨부" aria-label="첨부" onclick={pickFiles}><Icon name="attach" /></button>
      <button class="btn icon" title="인라인 그림 삽입" aria-label="인라인 그림 삽입" onclick={() => editor?.pickImage()}><Icon name="image" /></button>
      <span class="emoji-wrap">
        <button class="btn icon" title="이모지" aria-label="이모지" aria-expanded={emojiOpen} onclick={() => (emojiOpen = !emojiOpen)}><Icon name="emoji" /></button>
        {#if emojiOpen}
          <div class="emoji" role="menu" use:fitInView>
            {#each EMOJI as e (e)}
              <button
                aria-label={e}
                onmousedown={(ev) => ev.preventDefault()}
                onclick={() => {
                  editor?.insertText(e);
                  emojiOpen = false;
                }}>{e}</button
              >
            {/each}
          </div>
        {/if}
      </span>
      <button class="btn icon" class:on={toolbar} title="서식 옵션" aria-label="서식 옵션" aria-pressed={toolbar} disabled={options.plainText} onclick={() => (toolbar = !toolbar)}><Icon name="formatOptions" /></button>
      <span class="emoji-wrap">
        <button class="btn icon" title="기타 작업" aria-label="기타 작업" aria-expanded={moreOpen} onclick={() => (moreOpen = !moreOpen)}>…</button>
        {#if moreOpen}
          <div class="more" role="menu" use:fitInView>
            <button role="menuitem" onclick={() => run(saveNow)}>초안 저장<span class="k">Ctrl+S</span></button>
            <button role="menuitem" onclick={() => run(() => setShowFrom(!showFrom))}>{showFrom ? '보낸 사람 숨기기' : '보낸 사람 표시'}</button>
            <button role="menuitem" onclick={() => run(() => void checkNames())}>이름 확인</button>
            <div class="group" role="group" aria-label="중요도 설정">
              <span>중요도</span>
              {#each [2, 1, 0] as v (v)}
                <button role="menuitemradio" aria-checked={options.importance === v} class:on={options.importance === v} onclick={() => run(() => setOptions({ ...options, importance: v }))}>{IMPORTANCE[v]}</button>
              {/each}
            </div>
            <button role="menuitem" onclick={() => run(togglePlain)}>{options.plainText ? 'HTML로 전환' : '일반 텍스트로 전환'}</button>
            <button role="menuitem" onclick={() => run(() => (optionsOpen = true))}>메시지 옵션 표시…</button>
            <button role="menuitem" onclick={() => run(checkAccessibility)}>접근성 문제 확인</button>
            {#if draft.signature}
              <button role="menuitem" onclick={() => run(() => editor?.insertHtml(draft?.signature ?? ''))}>서명 삽입</button>
            {/if}
            <button role="menuitem" onclick={() => run(() => (templatesOpen = true))}>내 템플릿…</button>
          </div>
        {/if}
      </span>
      <button class="btn" onclick={discard}>삭제</button>
      {#if options.importance !== 1}
        <span class="imp" class:high={options.importance === 2}>중요도 {IMPORTANCE[options.importance]}</span>
      {/if}
      <span class="grow"></span>
      <span class="size" class:over title="본문·첨부 합계 / 한도">{bytes(total)} / {bytes(draft.maxBytes)}</span>
      <span class="saved">{serverSaved ? '임시 보관함에 저장됨' : savedAt ? `${clock(savedAt)} 저장됨` : ''}</span>
      {#if inline}
        <button class="btn icon" title="별도 창에서 편집" aria-label="별도 창에서 편집" onclick={() => void popout()}><Icon name="popout" /></button>
      {/if}
    </header>

    {#if error}<div class="error" role="alert">{error}</div>{/if}
    {#if info}
      <div class="note info" role="status">
        <span>{info}</span><button aria-label="닫기" onclick={() => (info = '')}>✕</button>
      </div>
    {/if}
    {#if draft.senderNote}
      <div class="note" data-sender-note>{draft.senderNote}</div>
    {/if}
    {#if draft.localOriginal}
      <div class="note">원본이 서버에 없어 새 메일로 보냅니다.</div>
    {/if}

    <div class="fields">
      {#if showFrom}
        <div class="from"><span class="label">보낸 사람</span><span data-from>{draft.from ?? ''}</span></div>
      {/if}
      <RecipientInput bind:this={toInput} label="받는 사람" list={to} onlabel={() => (pickField = 'to')} onchange={(l) => { to = l; touch(); }} />
      <RecipientInput bind:this={ccInput} label="참조" list={cc} onlabel={() => (pickField = 'cc')} onchange={(l) => { cc = l; touch(); }} />
      {#if showBcc}
        <RecipientInput bind:this={bccInput} label="숨은 참조" list={bcc} onlabel={() => (pickField = 'bcc')} onchange={(l) => { bcc = l; touch(); }} />
      {:else}
        <button class="link" onclick={() => (showBcc = true)}>숨은 참조 추가</button>
      {/if}
      <label class="subject">
        <span class="label">제목</span>
        <input bind:value={subject} oninput={touch} spellcheck="false" aria-label="제목" />
      </label>
    </div>

    {#if files.length || draft.forwardAttachments.length}
      <div class="files">
        {#each files as f (f.name)}
          <span class="file" title={f.name}>
            <span class="fname">{f.name}</span><span class="fsize">{bytes(f.size)}</span>
            <button aria-label="{f.name} 빼기" onclick={() => removeFile(f.name)}>✕</button>
          </span>
        {/each}
        {#each draft.forwardAttachments as f (f.key)}
          <span
            class="file fwd"
            class:excluded={f.excluded}
            title={f.excluded ? '원본 첨부(빼서 전달하지 않음)' : '원본 첨부(함께 전달)'}
          >
            <span class="fname">{f.name}</span><span class="fsize"
              >{bytes(f.size)} · {f.excluded ? '뺌' : '원본'}</span
            >
            {#if f.excluded}
              <button
                aria-label="{f.name} 다시 넣기"
                title="다시 넣기"
                onclick={() => excludeOriginal(f.key, false)}>↺</button
              >
            {:else}
              <button aria-label="{f.name} 빼기" title="빼기" onclick={() => excludeOriginal(f.key, true)}>✕</button>
            {/if}
          </span>
        {/each}
      </div>
    {/if}

    <div class="body">
      <!-- 원문이 보이면 원문 칸이, 아니면 편집기가 창 아래까지 채운다. -->
      <div class="edbox" class:fill={!draft.quoteDoc || !includeQuote}>
        <Editor bind:this={editor} html={bodyHtml} {toolbar} plain={options.plainText} onchange={(h) => { bodyHtml = h; touch(); }} />
      </div>
      {#if draft.quoteDoc}
        <div class="quote" class:open={includeQuote}>
          <label class="qhead">
            <input type="checkbox" bind:checked={includeQuote} onchange={touch} />
            원문 포함
          </label>
          <!-- 원문 포함을 껐다 켜도 고친 것이 남게 숨기기만 한다. -->
          <iframe
            bind:this={quoteFrame}
            title="인용할 원문"
            sandbox="allow-same-origin"
            referrerpolicy="no-referrer"
            srcdoc={draft.quoteDoc}
            hidden={!includeQuote}
            style:min-height={quoteH ? `${quoteH}px` : null}
            onload={onQuoteLoad}
          ></iframe>
        </div>
      {/if}
    </div>
    {#if dropping}<div class="dropzone">여기에 놓으면 첨부합니다</div>{/if}
  </div>
  {#if quoteMenu}
    <div class="qmenu" role="menu" aria-label="원문 표" use:placeAt={{ x: quoteMenu.x, y: quoteMenu.y }} data-quote-table-menu>
      {#each quoteMenu.items as item, i (i)}
        {#if 'sep' in item}<div class="qsep"></div>{:else}
          <button
            role="menuitem"
            onmousedown={(e) => e.preventDefault()}
            onclick={() => {
              quoteMenu = null;
              item.run();
            }}>{item.label}</button
          >
        {/if}
      {/each}
    </div>
  {/if}
  {#if quotePalette}
    <div class="qpal" role="menu" aria-label="셀 배경색" use:placeAt={{ x: quotePalette.x, y: quotePalette.y }} data-quote-palette>
      {#each QUOTE_COLORS as c (c)}<button class="swatch" style:background={c} title={c} aria-label={c} onmousedown={(e) => e.preventDefault()} onclick={() => quoteShade(c)}></button>{/each}
      <button class="wide" onmousedown={(e) => e.preventDefault()} onclick={() => quoteShade(null)}>없음</button>
    </div>
  {/if}
  {#if pickField}
    <AddressBook
      field={pickField}
      onadd={(f, r) => {
        const add = (l: Recipient[]) => (l.some((x) => x.email.toLowerCase() === r.email.toLowerCase()) ? l : [...l, r]);
        if (f === 'to') to = add(to);
        else if (f === 'cc') cc = add(cc);
        else {
          bcc = add(bcc);
          showBcc = true;
        }
        touch();
      }}
      onclose={() => (pickField = null)}
    />
  {/if}
  {#if optionsOpen}
    <MessageOptions {options} onsave={setOptions} onclose={() => (optionsOpen = false)} />
  {/if}
  {#if templatesOpen}
    <Templates
      current={() => editor?.selectionHtml() ?? ''}
      oninsert={(h) => editor?.insertHtml(h)}
      onclose={() => (templatesOpen = false)}
    />
  {/if}
{:else}
  <div class="load-error"><p>불러오는 중…</p></div>
{/if}

<style>
  .compose {
    position: relative;
    display: flex;
    flex-direction: column;
    height: 100%;
    background: var(--bg);
  }

  .top {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 8px 12px;
    border-bottom: 1px solid var(--border);
    background: var(--bg-bar);
  }

  .grow {
    flex: 1;
  }

  .size {
    font-size: 12px;
    color: var(--fg-muted);
  }

  .size.over {
    color: #c50f1f;
    font-weight: 600;
  }

  .saved {
    min-width: 90px;
    font-size: 12px;
    color: var(--fg-muted);
    text-align: right;
  }

  .error,
  .note {
    margin: 0;
    padding: 6px 14px;
    font-size: 12.5px;
  }

  .error {
    color: #c50f1f;
    background: #fde7e9;
  }

  .note {
    color: #8a5300;
    background: #fff4ce;
  }

  .fields {
    padding: 4px 14px 0;
  }

  .link {
    margin: 2px 0 2px 80px;
    padding: 0;
    border: 0;
    background: none;
    color: var(--accent);
    font: inherit;
    font-size: 12px;
    cursor: pointer;
  }

  .subject {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 4px 0;
    border-bottom: 1px solid var(--border-soft);
  }

  .label {
    flex: none;
    width: 72px;
    color: var(--fg-muted);
    font-size: 12.5px;
  }

  .subject input {
    flex: 1;
    height: 28px;
    border: 0;
    outline: none;
    background: none;
    color: var(--fg);
    font: inherit;
    font-size: 14px;
  }

  .files {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    padding: 8px 14px;
    border-bottom: 1px solid var(--border-soft);
  }

  .file {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    max-width: 300px;
    padding: 3px 4px 3px 8px;
    border: 1px solid var(--border);
    border-radius: 4px;
    font-size: 12px;
  }

  .file.fwd {
    background: var(--bg-pane);
  }

  .file.fwd.excluded {
    border-style: dashed;
    opacity: 0.6;
  }

  .file.fwd.excluded .fname {
    text-decoration: line-through;
  }

  .fname {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .fsize {
    flex: none;
    color: var(--fg-muted);
  }

  .file button {
    border: 0;
    background: none;
    color: var(--fg-muted);
    cursor: pointer;
  }

  .body {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    overflow-y: auto;
  }

  /* 편집기는 내용만큼(작성 화면 전체가 스크롤되므로 줄어들어 따로 스크롤되지 않게), 원문이 없으면 아래까지 채운다. */
  .edbox {
    flex: none;
  }

  .edbox.fill {
    flex: 1 0 auto;
    display: flex;
    flex-direction: column;
  }

  .edbox.fill > :global(.wrap) {
    flex: 1 0 auto;
  }

  .quote {
    flex: none;
    border-top: 1px dashed var(--border);
  }

  .quote.open {
    flex: 1 0 auto;
    display: flex;
    flex-direction: column;
  }

  .qhead {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 6px 14px;
    font-size: 12px;
    color: var(--fg-muted);
  }

  /* 높이는 원문 높이(min-height, fitQuote), 짧으면 창 아래까지. */
  .quote iframe {
    display: block;
    flex: 1 0 auto;
    width: 100%;
    height: 120px;
    border: 0;
    background: #fff;
  }

  .quote iframe[hidden] {
    display: none;
  }

  /* 인용문 표 메뉴·셀 배경색(위쪽 편집기의 오른쪽 클릭 메뉴와 같은 모양) */
  .qmenu,
  .qpal {
    position: fixed;
    z-index: 50;
    padding: 4px 0;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--bg);
    box-shadow: 0 6px 20px rgb(0 0 0 / 0.18);
  }

  .qmenu {
    display: flex;
    flex-direction: column;
    min-width: 180px;
  }

  .qmenu button {
    padding: 5px 14px;
    border: 0;
    background: none;
    color: var(--fg);
    font: inherit;
    font-size: 12.5px;
    text-align: left;
    white-space: nowrap;
    cursor: pointer;
  }

  .qmenu button:hover {
    background: var(--bg-sel-focus);
  }

  .qsep {
    height: 1px;
    margin: 4px 0;
    background: var(--border);
  }

  .qpal {
    display: grid;
    grid-template-columns: repeat(6, 20px);
    gap: 4px;
    padding: 6px;
  }

  .qpal .swatch {
    width: 20px;
    height: 20px;
    padding: 0;
    border: 1px solid rgb(0 0 0 / 0.2);
    border-radius: 3px;
    cursor: pointer;
  }

  .qpal .wide {
    grid-column: 1 / -1;
    height: 24px;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--bg-pane);
    color: var(--fg);
    font: inherit;
    font-size: 12px;
    cursor: pointer;
  }

  .dropzone {
    position: absolute;
    inset: 8px;
    display: flex;
    align-items: center;
    justify-content: center;
    border: 2px dashed var(--accent);
    border-radius: 8px;
    background: rgb(15 108 189 / 0.08);
    color: var(--accent);
    font-size: 15px;
    pointer-events: none;
  }

  .load-error {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 12px;
    height: 100%;
    color: var(--fg-muted);
  }

  .btn {
    height: 30px;
    padding: 0 14px;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--bg);
    color: var(--fg);
    font: inherit;
    cursor: pointer;
  }

  .btn.primary {
    border-color: var(--accent);
    background: var(--accent);
    color: #fff;
  }

  .btn.icon {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 34px;
    padding: 0;
  }

  .btn.on {
    background: var(--bg-sel-focus);
  }

  .emoji-wrap {
    position: relative;
    display: inline-flex;
  }

  /* "기타 작업" 메뉴 */
  .more {
    position: absolute;
    top: calc(100% + 4px);
    left: 0;
    z-index: 30;
    display: flex;
    flex-direction: column;
    min-width: 210px;
    padding: 4px 0;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--bg);
    box-shadow: 0 4px 14px rgb(0 0 0 / 0.16);
  }

  .more > button {
    display: flex;
    justify-content: space-between;
    padding: 6px 14px;
    border: 0;
    background: none;
    color: var(--fg);
    font: inherit;
    text-align: left;
    cursor: pointer;
  }

  .more > button:hover,
  .more .group button:hover {
    background: var(--bg-hover);
  }

  .more .k {
    color: var(--fg-muted);
    font-size: 12px;
  }

  .more .group {
    display: flex;
    align-items: center;
    gap: 2px;
    padding: 2px 14px;
  }

  .more .group span {
    flex: 1;
  }

  .more .group button {
    padding: 3px 8px;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: none;
    color: var(--fg);
    font: inherit;
    font-size: 12px;
    cursor: pointer;
  }

  .more .group button.on {
    border-color: var(--accent);
    background: var(--bg-sel-focus);
  }

  .imp {
    font-size: 12px;
    color: var(--fg-muted);
  }

  .imp.high {
    color: #c50f1f;
    font-weight: 600;
  }

  .from {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 0;
    border-bottom: 1px solid var(--border-soft);
    font-size: 13px;
  }

  .note.info {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .note.info span {
    flex: 1;
  }

  .note.info button {
    border: 0;
    background: none;
    color: inherit;
    cursor: pointer;
  }

  .emoji {
    position: absolute;
    top: calc(100% + 4px);
    left: 0;
    z-index: 30;
    display: grid;
    grid-template-columns: repeat(8, 30px);
    gap: 2px;
    padding: 6px;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--bg);
    box-shadow: 0 4px 14px rgb(0 0 0 / 0.16);
  }

  .emoji button {
    width: 30px;
    height: 30px;
    padding: 0;
    border: 0;
    border-radius: 4px;
    background: none;
    font-size: 18px;
    cursor: pointer;
  }

  .emoji button:hover {
    background: var(--bg-hover);
  }

  .btn:disabled {
    opacity: 0.55;
    cursor: default;
  }
</style>
