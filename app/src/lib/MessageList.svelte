<script lang="ts">
  // 가상 목록: 보이는 행(+여유)만 DOM에 둔다. 행 높이 고정, 위치는 transform으로.
  import { listDate } from './format';
  import { splitHits } from './highlight';
  import { ROW_H, type ListModel } from './list.svelte';

  interface Props {
    model: ListModel;
    /** 현재 행 번호(-1: 없음, 키보드 이동 기준) */
    selected: number;
    /** 고른 메일 ID들(여러 개 선택) */
    selection: Set<number>;
    /** 보낸 편지함: 보낸 사람 대신 받는 사람 */
    showTo: boolean;
    empty: string;
    /** 검색 결과면 칠할 검색어(아니면 빈 목록) */
    highlight: string[];
    /** 통합 편지함: HCL Domino 메일에 'HCL Domino' 꼬리표 */
    unified?: boolean;
    onpick: (index: number, mods: { ctrl: boolean; shift: boolean }) => void;
    /** 두 번 눌러 새 창에서 열기 */
    onopen: (index: number) => void;
    oncontext: (index: number, x: number, y: number) => void;
    onkey: (e: KeyboardEvent) => void;
  }

  let {
    model,
    selected,
    selection,
    showTo,
    empty,
    highlight,
    unified = false,
    onpick,
    onopen,
    oncontext,
    onkey,
  }: Props = $props();

  const OVERSCAN = 8;
  let viewport: HTMLDivElement;
  let scrollTop = $state(0);
  let height = $state(600);
  let today = $state(new Date());

  let first = $derived(Math.max(0, Math.floor(scrollTop / ROW_H) - OVERSCAN));
  let last = $derived(Math.min(model.total - 1, Math.ceil((scrollTop + height) / ROW_H) + OVERSCAN));
  let indices = $derived.by(() => {
    const out: number[] = [];
    for (let i = first; i <= last; i++) out.push(i);
    return out;
  });

  $effect(() => {
    void model.version;
    model.ensure(first, last);
  });

  // 목록이 바뀌면 맨 위로.
  $effect(() => {
    void model.epoch;
    if (viewport) {
      viewport.scrollTop = 0;
      scrollTop = 0;
    }
  });

  $effect(() => {
    const ro = new ResizeObserver(() => (height = viewport.clientHeight));
    ro.observe(viewport);
    const timer = setInterval(() => (today = new Date()), 60_000);
    return () => {
      ro.disconnect();
      clearInterval(timer);
    };
  });

  function onscroll() {
    scrollTop = viewport.scrollTop;
  }

  function onclick(e: MouseEvent) {
    const el = (e.target as HTMLElement).closest<HTMLElement>('[data-index]');
    if (el) onpick(Number(el.dataset.index), { ctrl: e.ctrlKey || e.metaKey, shift: e.shiftKey });
  }

  function ondblclick(e: MouseEvent) {
    const el = (e.target as HTMLElement).closest<HTMLElement>('[data-index]');
    if (el && !e.shiftKey && !e.ctrlKey && !e.metaKey) onopen(Number(el.dataset.index));
  }

  /** Shift+클릭(구간 선택)이 브라우저의 글자 선택 넓히기로 이어지지 않게. 키보드 이동이 되게 초점은 준다. */
  function onmousedown(e: MouseEvent) {
    if (e.shiftKey && e.button === 0) {
      e.preventDefault();
      window.getSelection()?.removeAllRanges();
      viewport.focus();
    }
  }

  function oncontextmenu(e: MouseEvent) {
    const el = (e.target as HTMLElement).closest<HTMLElement>('[data-index]');
    e.preventDefault();
    if (el) oncontext(Number(el.dataset.index), e.clientX, e.clientY);
  }

  /** 행이 보이게 최소한만 스크롤한다. */
  export function scrollToIndex(i: number) {
    const top = i * ROW_H;
    if (top < viewport.scrollTop) viewport.scrollTop = top;
    else if (top + ROW_H > viewport.scrollTop + viewport.clientHeight)
      viewport.scrollTop = top + ROW_H - viewport.clientHeight;
  }

  /** 한 화면에 들어가는 행 수(PgUp/PgDn). */
  export function pageRows(): number {
    return Math.max(1, Math.floor(viewport.clientHeight / ROW_H) - 1);
  }

  export function setScrollTop(y: number) {
    viewport.scrollTop = y;
  }

  export function maxScroll(): number {
    return viewport.scrollHeight - viewport.clientHeight;
  }

  export function focus() {
    viewport.focus();
  }

  /** 지금 보이는 행 중 데이터가 아직 없는 행 수(측정용). */
  export function blankVisible(): number {
    const top = Math.floor(viewport.scrollTop / ROW_H);
    const bottom = Math.min(model.total - 1, Math.floor((viewport.scrollTop + viewport.clientHeight) / ROW_H));
    let n = 0;
    for (let i = top; i <= bottom; i++) if (!model.row(i)) n++;
    return n;
  }
</script>

{#snippet marked(text: string)}
  {#if highlight.length}
    {#each splitHits(text, highlight) as p, k (k)}{#if p.hit}<mark>{p.t}</mark>{:else}{p.t}{/if}{/each}
  {:else}{text}{/if}
{/snippet}

<div
  class="viewport"
  bind:this={viewport}
  {onscroll}
  {onmousedown}
  {onclick}
  {ondblclick}
  {oncontextmenu}
  onkeydown={onkey}
  aria-multiselectable="true"
  tabindex="0"
  role="listbox"
  aria-label="메일 목록"
>
  {#if model.total === 0}
    <p class="empty">{empty}</p>
  {/if}
  <div class="spacer" style:height="{model.total * ROW_H}px">
    {#each indices as i (i)}
      {@const r = model.row(i)}
      {@const picked = r !== undefined && selection.has(r.id)}
      <div
        class="row"
        class:sel={picked}
        class:cur={i === selected}
        class:unread={r !== undefined && !r.read}
        class:flagged={r !== undefined && r.flag === 1}
        style:transform="translateY({i * ROW_H}px)"
        data-index={i}
        role="option"
        tabindex="-1"
        aria-selected={picked}
      >
        {#if r}
          <div class="l1">
            <span class="who"
              >{@render marked((showTo ? r.to : r.from) || (showTo ? '(받는 사람 없음)' : '(보낸 사람 없음)'))}</span
            >
            {#if r.pinned}<span class="pin" title="고정">📌</span>{/if}
            <span class="date">{listDate(r.date, today)}</span>
          </div>
          <div class="l2">
            {#if r.importance === 2}<span class="imp" title="중요도 높음">!</span>{/if}
            <span class="subj">{@render marked(r.subject || '(제목 없음)')}</span>
            {#if r.attach}<span class="ic" title="첨부 있음">📎</span>{/if}
            {#if r.flag === 1}<span class="ic flag" title="플래그">⚑</span>{/if}
            {#if r.flag === 2}<span class="ic done" title="완료">✓</span>{/if}
            {#if r.archived}<span class="tag" title="서버에서 삭제됨">보관</span>{/if}
            {#if r.origin === 'domino-sync'}
              <!-- 연동으로 받은 메일은 가져온 메일이 아니다: 통합 편지함에서만 출처를 붙인다 -->
              {#if unified}<span class="tag domino">HCL Domino</span>{/if}
            {:else if r.origin}<span
                class="tag imported"
                title={r.origin === 'html'
                  ? 'HTML 백업에서 복원'
                  : r.origin.startsWith('domino')
                    ? 'HCL Domino 메일'
                    : '가져온 메일'}
              >가져옴</span
              >{/if}
            {#if !r.hasEml}<span class="tag pending" title="원문을 아직 받지 않음">…</span>{/if}
          </div>
        {:else}
          <div class="ph"></div>
          <div class="ph short"></div>
        {/if}
      </div>
    {/each}
  </div>
</div>

<style>
  .viewport {
    position: relative;
    height: 100%;
    overflow-y: auto;
    overflow-x: hidden;
    outline: none;
    contain: strict;
    /* 목록 글자는 선택하지 않는다(Shift·Ctrl 클릭, 끌기가 글자 선택으로 보이지 않게). */
    user-select: none;
  }

  .spacer {
    position: relative;
  }

  .empty {
    position: absolute;
    margin: 0;
    padding: 12px;
    color: var(--fg-muted);
  }

  .row {
    position: absolute;
    left: 0;
    right: 0;
    top: 0;
    height: 50px;
    padding: 6px 10px 6px 12px;
    border-bottom: 1px solid var(--border-soft);
    border-left: 3px solid transparent;
    cursor: default;
    contain: strict;
    will-change: transform;
  }

  .row mark {
    padding: 0;
    background: #ffe066;
    color: inherit;
  }

  .row:hover {
    background: var(--bg-hover);
  }

  /* 플래그 메일(웹메일처럼 옅은 노랑). 선택 색이 이긴다(아래). */
  .row.flagged {
    background: var(--bg-flag);
  }

  .row.flagged:hover {
    background: var(--bg-flag-hover);
  }

  .row.sel,
  .row.sel:hover {
    background: var(--bg-sel);
  }

  .viewport:focus-within .row.sel {
    background: var(--bg-sel-focus);
  }

  .viewport:focus-within .row.cur {
    outline: 1px dotted var(--accent);
    outline-offset: -2px;
  }

  .row.unread {
    border-left-color: var(--accent);
  }

  .row.unread .who,
  .row.unread .subj {
    font-weight: 700;
  }

  .row.unread .subj {
    color: var(--accent-text);
  }

  .l1,
  .l2 {
    display: flex;
    align-items: center;
    gap: 6px;
    height: 18px;
    line-height: 18px;
    white-space: nowrap;
  }

  .l2 {
    margin-top: 2px;
  }

  .who,
  .subj {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .date {
    flex: none;
    font-size: 12px;
    color: var(--fg-muted);
  }

  .pin {
    flex: none;
    font-size: 11px;
  }

  .subj {
    color: var(--fg-muted);
  }

  .imp {
    color: #c50f1f;
    font-weight: 700;
  }

  .ic {
    flex: none;
    font-size: 12px;
    color: var(--fg-muted);
  }

  .flag {
    color: #c50f1f;
  }

  .done {
    color: #107c10;
  }

  .tag {
    flex: none;
    padding: 0 5px;
    font-size: 11px;
    line-height: 16px;
    border-radius: 3px;
    color: var(--fg-muted);
    background: var(--bg-bar);
  }

  .tag.pending {
    padding: 0 3px;
  }

  .tag.imported {
    color: #0e5a8a;
    background: #e3f1fb;
  }

  .tag.domino {
    color: #8a3b00;
    background: #fdeee2;
  }

  .ph {
    height: 12px;
    margin: 3px 0 6px;
    width: 70%;
    border-radius: 3px;
    background: var(--bg-bar);
  }

  .ph.short {
    width: 45%;
  }
</style>
