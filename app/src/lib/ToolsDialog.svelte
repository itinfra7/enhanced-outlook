<script lang="ts">
  // 도구(계획서 9.4, 유저스크립트 기능 이식). 현재 폴더 기준, 시작·마지막 날짜(yyyymmdd) 또는 "전체".
  // 모든 일괄 작업은 대상 건수·용량을 먼저 보여 주고 확인을 받는다. 진행률·중지 지원.
  // 서버 작업은 로컬에 바로 반영되고 뒤에서 서버에 보낸다(로그인 전이면 로그인 뒤).
  import { onMount } from 'svelte';
  import { listen } from '@tauri-apps/api/event';
  import { api, errorText } from './api';
  import ConfirmDialog from './ConfirmDialog.svelte';
  import DuplicatesTab from './DuplicatesTab.svelte';
  import FolderPicker from './FolderPicker.svelte';
  import Modal from './Modal.svelte';
  import { bytes, fullDate } from './format';
  import type {
    ExportProgress,
    FolderView,
    ImportProgress,
    Preview,
    Progress,
    Range,
    SpacePreview,
    Status,
  } from './types';

  interface Props {
    folders: FolderView[];
    folder: FolderView | null;
    selection: number[];
    archivedCount: number;
    status: Status;
    initialTab?: Tab;
    onclose: () => void;
    onshowarchived: () => void;
  }

  type Tab = 'export' | 'import' | 'server' | 'purge' | 'move' | 'dupes' | 'archived' | 'space';

  let {
    folders,
    folder,
    selection,
    archivedCount,
    status,
    initialTab = 'export',
    onclose,
    onshowarchived,
  }: Props = $props();
  const TABS: [Tab, string][] = [
    ['export', '내보내기'],
    ['import', '가져오기'],
    ['server', '서버에서 삭제'],
    ['purge', '완전 삭제'],
    ['move', '이동·복사'],
    ['dupes', '중복 메일'],
    ['archived', '보관 메일'],
    ['space', '용량 확보'],
  ];

  // svelte-ignore state_referenced_locally
  let tab = $state<Tab>(initialTab);
  let from = $state('');
  let to = $state('');
  let all = $state(false);
  let useSelection = $state(false);
  let zip = $state(false);
  let moveMode = $state<'move' | 'copy'>('move');
  let target = $state<number | null>(null);
  let picking = $state(false);
  let percent = $state(80);
  let preview = $state<Preview | null>(null);
  let space = $state<SpacePreview | null>(null);
  let error = $state('');
  let notice = $state('');
  let busy = $state(false);
  let confirming = $state(false);
  let batch = $state<number | null>(null);
  let progress = $state<Progress | null>(null);
  let exportProg = $state<ExportProgress | null>(null);
  let importPaths = $state<string[]>([]);
  let importProg = $state<ImportProgress | null>(null);
  let timer: ReturnType<typeof setInterval> | undefined;
  /** 중복 메일을 찾거나 지우는 중 */
  let dupesRunning = $state(false);

  let targetName = $derived(folders.find((f) => f.id === target)?.name ?? '');
  let running = $derived(
    (batch !== null && (progress?.pending ?? 1) > 0) ||
      (exportProg !== null && !exportProg.finished) ||
      (importProg !== null && !importProg.finished),
  );

  $effect(() => {
    useSelection = selection.length > 0;
  });

  onMount(() => {
    const un = listen<ExportProgress>('export-progress', (e) => {
      exportProg = e.payload;
      if (e.payload.finished) {
        notice = e.payload.error
          ? e.payload.error
          : `${e.payload.done}통 내보냄${e.payload.skipped ? ` · 원문 없음 ${e.payload.skipped}통 건너뜀` : ''}: ${e.payload.path}`;
      }
    });
    const unImport = listen<ImportProgress>('import-progress', (e) => {
      importProg = e.payload;
      if (!e.payload.finished) return;
      const r = e.payload.report;
      if (r) {
        notice =
          (r.cancelled ? '중지함 · ' : '') +
          `${r.imported}통 가져옴` +
          (r.duplicates ? ` · 이미 있음 ${r.duplicates}통` : '') +
          (r.skipped ? ` · 읽지 못함 ${r.skipped}통` : '') +
          (r.needLogin ? ` · 로그인 뒤 가져올 것 ${r.needLogin}통` : '');
      }
      if (e.payload.error) error = e.payload.error;
    });
    return () => {
      clearInterval(timer);
      void un.then((f) => f());
      void unImport.then((f) => f());
    };
  });

  async function pickImport(folder: boolean) {
    error = '';
    notice = '';
    try {
      const picked = await api.importPick(folder);
      if (picked.length) {
        importPaths = picked;
        importProg = null;
      }
    } catch (e) {
      error = errorText(e);
    }
  }

  async function startImport() {
    error = '';
    notice = '';
    busy = true;
    try {
      importProg = {
        done: 0,
        total: 0,
        imported: 0,
        duplicates: 0,
        skipped: 0,
        current: '',
        indexing: false,
        finished: false,
        report: null,
        error: null,
      };
      await api.importStart(importPaths);
    } catch (e) {
      importProg = null;
      error = errorText(e);
    } finally {
      busy = false;
    }
  }

  function switchTab(t: Tab) {
    if (running || dupesRunning) return;
    tab = t;
    preview = null;
    space = null;
    error = '';
    notice = '';
    confirming = false;
    batch = null;
    progress = null;
    exportProg = null;
    importProg = null;
  }

  function range(): Range {
    return { folderId: folder?.id ?? 0, from: from.trim() || null, to: to.trim() || null, all };
  }

  async function check() {
    error = '';
    notice = '';
    preview = null;
    space = null;
    busy = true;
    try {
      if (tab === 'space') space = await api.spacePreview(percent);
      else if (tab === 'export' && useSelection)
        preview = { count: selection.length, bytes: 0, withoutEml: 0, onServer: 0 };
      else preview = await api.toolPreview(range());
    } catch (e) {
      error = errorText(e);
    } finally {
      busy = false;
    }
  }

  function watch(b: number | null) {
    batch = b;
    clearInterval(timer);
    if (b === null) return;
    const poll = async () => {
      try {
        progress = await api.batchProgress(b);
        if (progress.pending === 0) clearInterval(timer);
      } catch {
        clearInterval(timer);
      }
    };
    void poll();
    timer = setInterval(poll, 500);
  }

  async function run() {
    confirming = false;
    error = '';
    busy = true;
    try {
      if (tab === 'export') {
        const total = await api.exportMail(useSelection ? { ids: selection } : { range: range() }, zip);
        if (total !== null) exportProg = { done: 0, total, skipped: 0, finished: false, error: null, path: '' };
        return;
      }
      const start =
        tab === 'space'
          ? await api.spaceRun(percent)
          : await api.toolRun(
              range(),
              tab === 'server' ? 'server_delete' : tab === 'purge' ? 'purge' : moveMode,
              tab === 'move' ? target : null,
            );
      notice =
        `${start.count}통 처리함` +
        (start.queued ? ` · 서버 반영 ${start.queued}건` : '') +
        (start.note ? ` ${start.note}` : '');
      watch(start.batch);
    } catch (e) {
      error = errorText(e);
    } finally {
      busy = false;
    }
  }

  async function stop() {
    if (exportProg && !exportProg.finished) {
      await api.cancelExport();
      return;
    }
    if (importProg && !importProg.finished) {
      await api.importCancel();
      return;
    }
    if (batch === null) return;
    try {
      const n = await api.cancelBatch(batch);
      notice = `중지함. 서버에 보내지 않은 ${n}통은 되돌림`;
      progress = await api.batchProgress(batch);
      clearInterval(timer);
    } catch (e) {
      error = errorText(e);
    }
  }

  let confirmText = $derived.by(() => {
    const p = preview;
    const where = folder?.name ?? '';
    const span = all ? '전체' : `${from || '처음'} ~ ${to || '마지막'}`;
    if (tab === 'space' && space)
      return {
        title: '용량 확보',
        message: `오래된 메일 ${space.count}통(${bytes(space.bytes)})을 서버에서 지웁니다. 로컬 사본은 남습니다.`,
        detail: '웹메일 "삭제된 항목 복구"로 되살릴 수 있습니다.',
        label: '서버에서 삭제',
        danger: true,
        double: undefined,
      };
    if (!p) return null;
    if (tab === 'server')
      return {
        title: '서버에서 삭제',
        message: `[${where}] ${span} ${p.count - p.withoutEml}통을 서버에서 지웁니다. 로컬 사본은 남습니다.`,
        detail: p.withoutEml ? `원문 없는 ${p.withoutEml}통은 건너뜁니다.` : '',
        label: '서버에서 삭제',
        danger: true,
        double: undefined,
      };
    if (tab === 'purge')
      return {
        title: '완전 삭제',
        message: `[${where}] ${span} ${p.count}통(${bytes(p.bytes)})을 로컬과 서버에서 영구히 지웁니다.`,
        detail: '되돌릴 수 없습니다.',
        label: '완전 삭제',
        danger: true,
        double: '되돌릴 수 없음을 확인했습니다',
      };
    if (tab === 'move')
      return {
        title: moveMode === 'move' ? '이동' : '복사',
        message: `[${where}] ${span} ${p.count}통을 [${targetName}](으)로 ${moveMode === 'move' ? '옮깁니다' : '복사합니다'}.`,
        detail: '',
        label: moveMode === 'move' ? '이동' : '복사',
        danger: false,
        double: undefined,
      };
    return null;
  });
</script>

<Modal title="도구" width={tab === 'dupes' ? 900 : 560} {onclose}>
  <nav class="tabs">
    {#each TABS as [t, label] (t)}
      <button class:on={tab === t} disabled={(running || dupesRunning) && tab !== t} onclick={() => switchTab(t)}>{label}</button>
    {/each}
  </nav>

  {#if tab === 'dupes'}
    <p class="desc">제목·본문·보낸 사람·받는 사람·참조가 같은 메일입니다. 묶음마다 가장 이른 메일만 남깁니다.</p>
    <DuplicatesTab {folders} onrunning={(on) => (dupesRunning = on)} />
  {:else if tab === 'archived'}
    <p class="desc">서버에서 지워져 로컬에만 남은 메일입니다.</p>
    <p class="big">보관 메일 {archivedCount}통</p>
    <button class="btn primary" onclick={() => { onshowarchived(); onclose(); }}>보관 메일 보기</button>
  {:else if tab === 'space'}
    <p class="desc">오래된 메일부터 서버에서 지웁니다. 로컬 사본은 남습니다.</p>
    {#if status.quotaUsed != null && status.quotaLimit}
      <p>현재 {bytes(status.quotaUsed)} / {bytes(status.quotaLimit)} ({Math.round((status.quotaUsed / status.quotaLimit) * 100)}%)</p>
    {:else}
      <p class="warn">로그인한 뒤 쓸 수 있습니다.</p>
    {/if}
    <label class="row">목표 사용률 <input type="number" min="1" max="100" bind:value={percent} class="num" /> % 이하</label>
  {:else if tab === 'import'}
    <p class="desc">EML·ZIP·HTML·PST·IMS 백업을 로컬로 가져옵니다. 서버에는 올리지 않습니다.</p>
    <p class="desc">HCL Domino: .idf 파일이 있는 폴더(없으면 메일함 폴더들이 든 폴더), PST 파일, HCL Traveler 백업(.ims)을 고르세요. "HCL Domino 메일" 아래에 들어갑니다.</p>
    <div class="row">
      <button class="btn" disabled={running} onclick={() => pickImport(true)}>폴더 고르기</button>
      <button class="btn" disabled={running} onclick={() => pickImport(false)}>파일 고르기</button>
    </div>
    {#if importPaths.length}
      <ul class="paths">
        {#each importPaths as p (p)}<li title={p}>{p}</li>{/each}
      </ul>
    {/if}
    {#if importProg && !importProg.finished}
      <div class="prog">
        <div class="meter">
          <div class="fill" style:width="{importProg.total ? (importProg.done / importProg.total) * 100 : 0}%"></div>
        </div>
        {#if importProg.indexing}검색 색인 중…
        {:else}가져오는 중 {importProg.done}/{importProg.total} · 가져옴 {importProg.imported} · 이미 있음 {importProg.duplicates}{importProg.skipped
            ? ` · 읽지 못함 ${importProg.skipped}`
            : ''}{/if}
      </div>
      {#if importProg.current && !importProg.indexing}<p class="current" title={importProg.current}>{importProg.current}</p>{/if}
    {/if}
    {#if importProg?.report?.errors.length}
      <details class="errors">
        <summary>읽지 못한 파일 {importProg.report.skipped}개{importProg.report.skipped > importProg.report.errors.length
            ? `(앞 ${importProg.report.errors.length}개)`
            : ''}</summary>
        <ul>
          {#each importProg.report.errors as e, i (i)}<li>{e}</li>{/each}
        </ul>
      </details>
    {/if}
  {:else}
    <p class="desc">
      {#if tab === 'export'}EML로 저장합니다.
      {:else if tab === 'server'}서버에서만 지웁니다. 로컬 사본은 남습니다.
      {:else if tab === 'purge'}로컬과 서버에서 영구히 지웁니다.
      {:else}기간 안의 메일을 다른 폴더로 옮기거나 복사합니다.{/if}
    </p>
    {#if tab === 'export' && selection.length > 0}
      <label class="row"><input type="checkbox" bind:checked={useSelection} /> 목록에서 고른 메일 {selection.length}통만</label>
    {/if}
    {#if !(tab === 'export' && useSelection)}
      <p class="where">대상 폴더: <b>{folder?.name ?? '(폴더를 고르세요)'}</b></p>
      <div class="row">
        <input class="date" placeholder="시작 yyyymmdd" bind:value={from} disabled={all} maxlength="10" />
        ~
        <input class="date" placeholder="마지막 yyyymmdd" bind:value={to} disabled={all} maxlength="10" />
        <label><input type="checkbox" bind:checked={all} /> 전체</label>
      </div>
    {/if}
    {#if tab === 'export'}
      <div class="row">
        <label><input type="radio" bind:group={zip} value={false} /> 폴더로</label>
        <label><input type="radio" bind:group={zip} value={true} /> ZIP 파일로</label>
      </div>
    {/if}
    {#if tab === 'move'}
      <div class="row">
        <label><input type="radio" bind:group={moveMode} value="move" /> 이동</label>
        <label><input type="radio" bind:group={moveMode} value="copy" /> 복사</label>
        <button class="btn" onclick={() => (picking = true)}>{targetName || '대상 폴더 고르기'}</button>
      </div>
    {/if}
  {/if}

  {#if preview && tab !== 'space'}
    <p class="preview">
      대상 {preview.count}통{preview.bytes ? ` · ${bytes(preview.bytes)}` : ''}
      {#if preview.withoutEml}· 원문 없음 {preview.withoutEml}통{/if}
      {#if tab !== 'export' && preview.onServer}· 서버에 있음 {preview.onServer}통{/if}
    </p>
  {/if}
  {#if space}
    <p class="preview">
      {#if space.need === 0}이미 목표 이하입니다.
      {:else}줄일 양 {bytes(space.need)} → 오래된 메일 {space.count}통 · {bytes(space.bytes)}
        {#if space.oldest && space.newest}({fullDate(space.oldest).slice(0, 10)} ~ {fullDate(space.newest).slice(0, 10)}){/if}
        {#if space.withoutEml}· 원문 없어 건너뜀 {space.withoutEml}통{/if}
      {/if}
    </p>
  {/if}

  {#if progress && batch !== null}
    {@const total = progress.pending + progress.done + progress.abandoned}
    <div class="prog">
      <div class="meter"><div class="fill" style:width="{total ? ((progress.done + progress.abandoned) / total) * 100 : 0}%"></div></div>
      서버 반영 {progress.done}/{total}{progress.abandoned ? ` · 실패·중지 ${progress.abandoned}` : ''}
      {#if progress.pending > 0 && !status.loggedIn}<span class="warn">· 로그인하면 반영</span>{/if}
    </div>
  {/if}
  {#if exportProg && !exportProg.finished}
    <div class="prog">
      <div class="meter"><div class="fill" style:width="{exportProg.total ? (exportProg.done / exportProg.total) * 100 : 0}%"></div></div>
      내보내는 중 {exportProg.done}/{exportProg.total}
    </div>
  {/if}
  {#if notice}<p class="notice">{notice}</p>{/if}
  {#if error}<p class="error">{error}</p>{/if}

  {#snippet footer()}
    {#if running}
      <button class="btn" onclick={stop}>중지</button>
    {/if}
    {#if tab === 'import'}
      <button class="btn primary" disabled={busy || running || importPaths.length === 0} onclick={startImport}>가져오기</button>
    {:else if tab !== 'archived' && tab !== 'dupes'}
      <button class="btn" disabled={busy || running || (tab !== 'space' && !folder && !useSelection)} onclick={check}>대상 확인</button>
      <button
        class="btn primary"
        disabled={busy ||
          running ||
          (tab === 'space' ? !space || space.count === 0 : !preview || preview.count === 0) ||
          (tab === 'move' && target === null)}
        onclick={() => (tab === 'export' ? run() : (confirming = true))}
      >
        {tab === 'export' ? '내보내기' : '실행'}
      </button>
    {/if}
    <button class="btn" onclick={onclose}>닫기</button>
  {/snippet}
</Modal>

{#if picking}
  <FolderPicker
    title={moveMode === 'move' ? '옮길 폴더' : '복사할 폴더'}
    {folders}
    exclude={folder?.id ?? null}
    onpick={(id) => {
      target = id;
      picking = false;
    }}
    oncancel={() => (picking = false)}
  />
{/if}

{#if confirming && confirmText}
  <ConfirmDialog
    title={confirmText.title}
    message={confirmText.message}
    detail={confirmText.detail}
    confirmLabel={confirmText.label}
    danger={confirmText.danger}
    double={confirmText.double}
    onconfirm={run}
    oncancel={() => (confirming = false)}
  />
{/if}

<style>
  .tabs {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
    margin-bottom: 12px;
    border-bottom: 1px solid var(--border);
  }

  .tabs button {
    padding: 6px 10px;
    border: 0;
    border-bottom: 2px solid transparent;
    background: none;
    color: var(--fg-muted);
    font: inherit;
    cursor: pointer;
  }

  .tabs button.on {
    color: var(--accent);
    border-bottom-color: var(--accent);
    font-weight: 600;
  }

  .desc {
    margin: 0 0 10px;
    font-size: 12.5px;
    color: var(--fg-muted);
  }

  .row {
    display: flex;
    align-items: center;
    gap: 8px;
    margin: 8px 0;
  }

  .where {
    margin: 6px 0;
  }

  .date,
  .num {
    height: 28px;
    padding: 0 8px;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--bg);
    color: var(--fg);
    font: inherit;
  }

  .date {
    width: 130px;
  }

  .num {
    width: 70px;
  }

  .big {
    font-size: 16px;
    font-weight: 600;
  }

  .preview {
    margin: 12px 0 4px;
    padding: 8px 10px;
    border-radius: 4px;
    background: var(--bg-bar);
  }

  .prog {
    display: flex;
    align-items: center;
    gap: 8px;
    margin: 10px 0 0;
    font-size: 12.5px;
  }

  .meter {
    flex: none;
    width: 160px;
    height: 6px;
    border-radius: 3px;
    background: var(--border);
    overflow: hidden;
  }

  .fill {
    height: 100%;
    background: var(--accent);
  }

  .notice {
    margin: 10px 0 0;
    font-size: 12.5px;
  }

  .paths {
    margin: 4px 0;
    padding-left: 18px;
    font-size: 12px;
    max-height: 90px;
    overflow-y: auto;
  }

  .paths li,
  .current {
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }

  .current {
    margin: 4px 0 0;
    font-size: 11.5px;
    color: var(--fg-muted);
  }

  .errors {
    margin: 8px 0 0;
    font-size: 12px;
  }

  .errors ul {
    margin: 4px 0 0;
    padding-left: 18px;
    max-height: 120px;
    overflow-y: auto;
    color: var(--fg-muted);
  }

  .error,
  .warn {
    color: #c50f1f;
    font-size: 12.5px;
  }
</style>
