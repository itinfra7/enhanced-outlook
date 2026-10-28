<script lang="ts">
  // 검색창. 한글 IME 조합 중에는 검색하지 않는다(조합이 끝나면 잠시 뒤 검색, Enter는 바로).
  // 조합 중 Enter(isComposing, keyCode 229)는 조합 확정용이라 무시한다.
  interface Props {
    value: string;
    scopeAll: boolean;
    folderName: string;
    onsearch: (q: string) => void;
    onclear: () => void;
    onleave: () => void;
  }

  let { value = $bindable(), scopeAll = $bindable(), folderName, onsearch, onclear, onleave }: Props =
    $props();

  let input: HTMLInputElement;
  let composing = false;
  let timer: ReturnType<typeof setTimeout> | undefined;

  function schedule(delay = 250) {
    clearTimeout(timer);
    // 그새 다음 글자 조합이 시작됐으면 건너뛴다(그 조합이 끝날 때 다시 잡는다).
    timer = setTimeout(() => {
      if (!composing) onsearch(value);
    }, delay);
  }

  function oninput(e: Event) {
    if (composing || (e as InputEvent).isComposing) return;
    if (!value.trim()) {
      clearTimeout(timer);
      onclear();
      return;
    }
    schedule();
  }

  function onkeydown(e: KeyboardEvent) {
    if (e.isComposing || e.keyCode === 229) return;
    if (e.key === 'Enter') {
      e.preventDefault();
      clearTimeout(timer);
      if (value.trim()) onsearch(value);
      else onclear();
    } else if (e.key === 'Escape') {
      e.preventDefault();
      clearTimeout(timer);
      value = '';
      onclear();
      onleave();
    } else if (e.key === 'ArrowDown') {
      e.preventDefault();
      onleave();
    }
  }

  function toggleScope() {
    scopeAll = !scopeAll;
    if (value.trim()) onsearch(value);
  }

  export function focus() {
    input.focus();
    input.select();
  }
</script>

<div class="box">
  <input
    bind:this={input}
    bind:value
    class="search"
    type="search"
    placeholder="검색" title="제목: 보낸이: 받는이: 참조: 숨은참조: 첨부: 기간:2025-01-01..2025-06-30"
    aria-label="메일 검색"
    autocomplete="off"
    spellcheck="false"
    {oninput}
    {onkeydown}
    oncompositionstart={() => (composing = true)}
    oncompositionend={() => {
      composing = false;
      if (value.trim()) schedule(150);
    }}
  />
  <button class="scope" onclick={toggleScope} title="검색 범위">
    {scopeAll ? '전체 폴더' : `현재 폴더${folderName ? `: ${folderName}` : ''}`}
  </button>
</div>

<style>
  .box {
    display: flex;
    flex: 1;
    max-width: 720px;
    gap: 6px;
  }

  .search {
    flex: 1;
    min-width: 0;
    height: 28px;
    padding: 0 10px;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--bg);
    color: var(--fg);
    font: inherit;
  }

  .search:focus {
    outline: 2px solid var(--accent);
    outline-offset: -1px;
  }

  .scope {
    flex: none;
    max-width: 200px;
    height: 28px;
    padding: 0 10px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--bg-pane);
    color: var(--fg);
    font: inherit;
    font-size: 12px;
    cursor: pointer;
  }
</style>
