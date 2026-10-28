<script lang="ts">
  // 이동·복사 대상 폴더 고르기. 서버에서 사라진 폴더와 지금 폴더는 고를 수 없다.
  import Modal from './Modal.svelte';
  import type { FolderView } from './types';

  interface Props {
    title: string;
    folders: FolderView[];
    exclude: number | null;
    onpick: (id: number) => void;
    oncancel: () => void;
  }

  let { title, folders, exclude, onpick, oncancel }: Props = $props();
  let filter = $state('');
  // 통합 편지함·'Outlook 메일' 묶음 제목(화면에만 있는 줄, 번호 < 0)은 대상이 아니다.
  let shown = $derived(
    folders.filter(
      (f) => f.id >= 0 && (!filter.trim() || f.name.toLowerCase().includes(filter.trim().toLowerCase())),
    ),
  );
</script>

<Modal {title} width={360} onclose={oncancel}>
  <input class="filter" placeholder="폴더 이름으로 찾기" bind:value={filter} autocomplete="off" />
  <div class="list">
    {#each shown as f (f.id)}
      <button
        class="item"
        style:padding-left="{10 + (filter ? 0 : f.depth) * 14}px"
        disabled={f.archived || f.id === exclude}
        title={f.archived ? '서버에서 사라진 폴더' : f.name}
        onclick={() => onpick(f.id)}>{f.name}</button
      >
    {/each}
  </div>
</Modal>

<style>
  .filter {
    width: 100%;
    height: 28px;
    margin-bottom: 8px;
    padding: 0 8px;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--bg);
    color: var(--fg);
    font: inherit;
  }

  .list {
    max-height: 50vh;
    overflow-y: auto;
    border: 1px solid var(--border);
    border-radius: 4px;
  }

  .item {
    display: block;
    width: 100%;
    height: 28px;
    padding-right: 10px;
    border: 0;
    background: none;
    color: var(--fg);
    font: inherit;
    text-align: left;
    cursor: pointer;
  }

  .item:hover:not(:disabled) {
    background: var(--bg-hover);
  }

  .item:disabled {
    color: var(--fg-muted);
    cursor: default;
  }
</style>
