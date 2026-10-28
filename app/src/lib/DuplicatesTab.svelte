<script lang="ts">
  // 도구 → 중복 메일(0.9.0 사용자 요청): 제목·본문·보낸 사람·받는 사람·참조가 같은 메일(받은 시각은 보지 않음).
  // 묶음마다 가장 이른 메일을 남기고 나머지를 완전 삭제한다. 묶음을 눌러 고르고 위의 "제외"를 누르면 그 묶음은
  // 지우지 않고 회색으로 흐리게 보인다(같은 단추가 "제외 취소"로 바뀐다, 1.0.3 사용자 요청). Ctrl·Shift+클릭으로 여러
  // 묶음을 함께 고른다. 제외는 저장해 두어 "제외 취소"할 때까지 다시 찾거나 앱을 다시 켜도 남는다(1.0.5 사용자 요청).
  // 한 묶음을 모두 지우지는 않는다.
  import { onMount } from 'svelte';
  import { listen } from '@tauri-apps/api/event';
  import { api, errorText } from './api';
  import ConfirmDialog from './ConfirmDialog.svelte';
  import { bytes } from './format';
  import type { DupFound, DupProgress, FolderView } from './types';

  interface Props {
    folders: FolderView[];
    /** 찾거나 지우는 중(도구의 다른 탭으로 못 가게) */
    onrunning: (on: boolean) => void;
  }

  let { folders, onrunning }: Props = $props();

  let found = $state<DupFound | null>(null);
  let prog = $state<DupProgress | null>(null);
  /** 제외한 묶음(열쇠). 배열이라 바꾸면 화면이 바로 따라간다. */
  let excluded = $state<string[]>([]);
  /** 고른 묶음들(열쇠). Ctrl+클릭은 더하거나 빼고, Shift+클릭은 마지막으로 누른 묶음부터 범위(1.0.5 사용자 요청). */
  let selected = $state<string[]>([]);
  /** Shift+클릭 범위의 시작(마지막으로 누른 묶음) */
  let anchor: string | null = null;
  let busy = $state(false);
  let error = $state('');
  let notice = $state('');
  let confirming = $state(false);

  let byId = $derived(new Map(folders.map((f) => [f.id, f])));
  let active = $derived((found?.groups ?? []).filter((g) => !excluded.includes(g.key)));
  let offCount = $derived((found?.groups.length ?? 0) - active.length);
  /** 고른 묶음이 모두 제외돼 있다(단추가 "제외 취소") */
  let selectedOff = $derived(selected.length > 0 && selected.every((k) => excluded.includes(k)));
  let dropCount = $derived(active.reduce((n, g) => n + g.mails.length - 1, 0));

  onMount(() => {
    const un = listen<DupProgress>('dupes-progress', (e) => {
      if (busy) prog = e.payload;
    });
    return () => void un.then((f) => f());
  });

  function folderLabel(id: number): string {
    const f = byId.get(id);
    if (!f) return '';
    const parent = f.parentId !== null ? byId.get(f.parentId) : undefined;
    return parent ? `${parent.name} / ${f.name}` : f.name;
  }

  function pad(n: number): string {
    return String(n).padStart(2, '0');
  }

  function stamp(ms: number): string {
    const d = new Date(ms);
    return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
  }

  function setBusy(on: boolean) {
    busy = on;
    onrunning(on);
  }

  async function find() {
    error = '';
    notice = '';
    prog = null;
    setBusy(true);
    try {
      found = await api.duplicatesFind();
      excluded = found.excluded;
      selected = [];
      anchor = null;
    } catch (e) {
      error = errorText(e);
    } finally {
      prog = null;
      setBusy(false);
    }
  }

  /** 묶음을 누름: 그냥 누르면 그 묶음만(다시 누르면 풀기), Ctrl은 더하거나 빼기, Shift는 범위. */
  function pickGroup(key: string, e: MouseEvent | KeyboardEvent) {
    const keys = (found?.groups ?? []).map((g) => g.key);
    if (e.shiftKey && anchor !== null && keys.includes(anchor)) {
      const [a, b] = [keys.indexOf(anchor), keys.indexOf(key)].sort((x, y) => x - y);
      const range = keys.slice(a, b + 1);
      // Ctrl+Shift는 고른 것에 범위를 더한다.
      selected = e.ctrlKey || e.metaKey ? [...new Set([...selected, ...range])] : range;
      return;
    }
    if (e.ctrlKey || e.metaKey) {
      selected = selected.includes(key) ? selected.filter((k) => k !== key) : [...selected, key];
    } else {
      selected = selected.length === 1 && selected[0] === key ? [] : [key];
    }
    anchor = key;
  }

  /** 고른 묶음들을 제외하거나(하나라도 제외가 아니면) 모두 제외 취소한다. 저장한 뒤 화면에 반영. */
  async function toggleSelected() {
    if (!selected.length) return;
    const off = !selectedOff;
    const keys = selected.filter((k) => excluded.includes(k) !== off);
    error = '';
    try {
      await api.duplicatesExclude(keys, off);
      excluded = off ? [...excluded, ...keys] : excluded.filter((k) => !keys.includes(k));
    } catch (e) {
      error = errorText(e);
    }
  }

  async function purge() {
    confirming = false;
    error = '';
    setBusy(true);
    try {
      const picked = active;
      const groups = picked.map((g) => ({ ids: g.mails.map((m) => m.id), drop: g.mails.slice(1).map((m) => m.id) }));
      const n = dropCount;
      const r = await api.duplicatesPurge(groups);
      if (found) {
        const gone = new Set(picked.map((g) => g.key));
        found = { ...found, groups: found.groups.filter((g) => !gone.has(g.key)) };
        selected = selected.filter((k) => !gone.has(k));
      }
      notice = `${n}통 완전 삭제` + (r.queued ? ` · 서버 반영 ${r.queued}건` : '') + (r.note ? ` ${r.note}` : '');
    } catch (e) {
      error = errorText(e);
    } finally {
      setBusy(false);
    }
  }
</script>

<div class="bar">
  <button class="btn" disabled={busy} onclick={find} data-dupes-find>{found ? '다시 찾기' : '찾기'}</button>
  <button class="btn" disabled={busy || !selected.length} onclick={toggleSelected} data-dupes-exclude>
    {selectedOff ? '제외 취소' : '제외'}{selected.length > 1 ? ` (${selected.length}묶음)` : ''}
  </button>
  <button class="btn danger" disabled={busy || dropCount === 0} onclick={() => (confirming = true)} data-dupes-purge>
    완전 삭제{dropCount ? ` (${dropCount}통)` : ''}
  </button>
  {#if found}
    <span class="sum">
      메일 {found.scanned}통 · 중복 {found.groups.length}묶음{offCount ? ` · 제외 ${offCount}묶음` : ''}{found.withoutBody
        ? ` · 원문 없어 비교 못함 ${found.withoutBody}통`
        : ''}
    </span>
  {/if}
</div>

{#if busy && prog}
  <div class="prog">
    <div class="meter"><div class="fill" style:width="{prog.total ? (prog.done / prog.total) * 100 : 0}%"></div></div>
    {prog.stage === 'read' ? '메일 목록 읽는 중' : '본문 비교 중'} {prog.done}/{prog.total}
  </div>
{:else if busy && !found}
  <div class="prog">찾는 중…</div>
{/if}

{#if found && found.groups.length}
  <div class="list" role="listbox" aria-label="중복 메일 묶음">
    {#each found.groups as g (g.key)}
      {@const off = excluded.includes(g.key)}
      <div
        class="group"
        class:off
        class:sel={selected.includes(g.key)}
        role="option"
        aria-selected={selected.includes(g.key)}
        tabindex="-1"
        data-dupes-group={g.key}
        onmousedown={(e) => {
          // Shift+클릭으로 글자가 칠해지지 않게.
          if (e.shiftKey) e.preventDefault();
        }}
        onclick={(e) => pickGroup(g.key, e)}
        onkeydown={(e) => {
          if (e.key === 'Enter' || e.key === ' ') {
            e.preventDefault();
            pickGroup(g.key, e);
          }
        }}
      >
        {#each g.mails as m, i (m.id)}
          <div class="row">
            <span class="tag" class:keep={i === 0} class:drop={i > 0 && !off}>{off ? '제외' : i === 0 ? '남김' : '삭제'}</span>
            <span class="date">{stamp(m.date)}</span>
            <span class="folder" title={folderLabel(m.folderId)}>{folderLabel(m.folderId)}</span>
            <span class="from" title={m.from}>{m.from}</span>
            <span class="subject" title={m.subject}>{m.subject || '(제목 없음)'}</span>
            <span class="size">{m.size ? bytes(m.size) : ''}</span>
          </div>
        {/each}
      </div>
    {/each}
  </div>
{:else if found}
  <p class="empty">중복 메일이 없습니다.</p>
{/if}

{#if notice}<p class="notice">{notice}</p>{/if}
{#if error}<p class="error">{error}</p>{/if}

{#if confirming}
  <ConfirmDialog
    title="완전 삭제"
    message={`중복 메일 ${dropCount}통을 로컬과 서버에서 영구히 지웁니다. 묶음마다 가장 이른 메일은 남깁니다.`}
    detail="되돌릴 수 없습니다."
    confirmLabel="완전 삭제"
    danger={true}
    double="되돌릴 수 없음을 확인했습니다"
    onconfirm={purge}
    oncancel={() => (confirming = false)}
  />
{/if}

<style>
  .bar {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-bottom: 8px;
  }

  .sum {
    font-size: 12.5px;
    color: var(--fg-muted);
  }

  .prog {
    display: flex;
    align-items: center;
    gap: 8px;
    margin: 6px 0;
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

  .list {
    max-height: calc(100vh - 300px);
    min-height: 120px;
    overflow-y: auto;
    border: 1px solid var(--border);
    border-radius: 4px;
  }

  .group {
    padding: 2px 0;
    border-bottom: 1px solid var(--border);
    cursor: pointer;
  }

  .group:last-child {
    border-bottom: 0;
  }

  /* 고른 묶음 */
  .group.sel {
    background: var(--bg-sel-focus);
    box-shadow: inset 3px 0 0 var(--accent);
  }

  /* 제외한 묶음: 회색으로 흐리게(지우지 않음) */
  .group.off {
    background: #d6d6d6;
    color: #6b6b6b;
    opacity: 0.55;
  }

  .group.off.sel {
    box-shadow: inset 3px 0 0 #6b6b6b;
  }

  .row {
    display: grid;
    grid-template-columns: 40px 140px 150px 120px 1fr 56px;
    align-items: center;
    gap: 8px;
    padding: 3px 8px;
    font-size: 12.5px;
    cursor: default;
  }

  .group:not(.sel):not(.off):hover {
    background: var(--bg-hover);
  }

  .row > span {
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }

  .tag {
    font-size: 11.5px;
    color: var(--fg-muted);
  }

  .tag.keep {
    color: var(--accent);
    font-weight: 600;
  }

  .tag.drop {
    color: #c50f1f;
  }

  .date,
  .size {
    font-variant-numeric: tabular-nums;
  }

  .size {
    text-align: right;
    color: var(--fg-muted);
  }

  .folder,
  .from {
    color: var(--fg-muted);
  }

  .empty,
  .notice {
    margin: 10px 0 0;
    font-size: 12.5px;
  }

  .error {
    margin: 10px 0 0;
    color: #c50f1f;
    font-size: 12.5px;
  }
</style>
