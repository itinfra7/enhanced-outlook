<script lang="ts">
  // 폴더 트리(계획서 9.1): 깊이만큼 들여 쓰고 안읽음 수를 굵게.
  import type { FolderView } from './types';

  interface Props {
    folders: FolderView[];
    current: number | null;
    /** "보관 메일" 모아 보기 중 */
    archivedView: boolean;
    onselect: (id: number) => void;
    onarchived: () => void;
    /** 폴더 오른쪽 클릭(웹메일과 같은 폴더 메뉴) */
    oncontext?: (f: FolderView, x: number, y: number) => void;
  }

  let { folders, current, archivedView, onselect, onarchived, oncontext }: Props = $props();
  let favorites = $derived(folders.filter((f) => f.favorite));

  function context(e: MouseEvent, f: FolderView) {
    if (!oncontext || f.wellKnown === 'group') return;
    e.preventDefault();
    oncontext(f, e.clientX, e.clientY);
  }
  let archivedCount = $derived(folders.reduce((n, f) => n + f.archivedMail, 0));

  const ICONS: Record<string, string> = {
    unified: '📬',
    inbox: '📥',
    drafts: '📝',
    sentitems: '📤',
    deleteditems: '🗑',
    junkemail: '⚠',
    outbox: '⏳',
  };
</script>

<nav class="tree" aria-label="폴더">
  {#if favorites.length}
    <div class="group" style:padding-left="10px">즐겨찾기</div>
    {#each favorites as f (f.id)}
      <button
        class="item"
        class:sel={f.id === current}
        style:padding-left="24px"
        title={f.name}
        data-favorite={f.id}
        onclick={() => onselect(f.id)}
        oncontextmenu={(e) => context(e, f)}
      >
        <span class="icon" aria-hidden="true">{ICONS[f.wellKnown ?? ''] ?? (f.local ? '🗃' : '📁')}</span>
        <span class="name">{f.name}</span>
        {#if f.unread > 0}<span class="unread">{f.unread}</span>{/if}
      </button>
    {/each}
    <div class="sep"></div>
  {/if}
  {#each folders as f (f.id)}
    {#if f.wellKnown === 'group'}
      <!-- 'Outlook 메일' 묶음 제목(통합 편지함을 켰을 때): 고를 수 없다 -->
      <div class="group" style:padding-left="{10 + f.depth * 14}px">{f.name}</div>
    {:else}
    <button
      class="item"
      class:sel={f.id === current}
      style:padding-left="{10 + f.depth * 14}px"
      title={f.archived
        ? `${f.name} (서버에 없음)`
        : f.local
          ? `${f.name} · ${f.total}통 (가져온 폴더)`
          : `${f.name} · ${f.total}통${f.archivedMail ? ` · 보관 ${f.archivedMail}` : ''}`}
      data-folder={f.id}
      onclick={() => onselect(f.id)}
      oncontextmenu={(e) => context(e, f)}
    >
      <span class="icon" aria-hidden="true">{ICONS[f.wellKnown ?? ''] ?? (f.local ? '🗃' : '📁')}</span>
      <span class="name" class:gone={f.archived}>{f.name}</span>
      {#if f.unread > 0}<span class="unread">{f.unread}</span>{/if}
    </button>
    {/if}
  {:else}
    <p class="empty">폴더 없음</p>
  {/each}
  {#if folders.length > 0}
    <div class="sep"></div>
    <button
      class="item"
      class:sel={archivedView}
      style:padding-left="10px"
      title="서버에서 지워진 메일"
      onclick={onarchived}
    >
      <span class="icon" aria-hidden="true">🗄</span>
      <span class="name">보관 메일</span>
      {#if archivedCount > 0}<span class="count">{archivedCount}</span>{/if}
    </button>
  {/if}
</nav>

<style>
  .tree {
    padding: 4px 0;
  }

  .item {
    display: flex;
    align-items: center;
    gap: 6px;
    width: 100%;
    height: 28px;
    padding-right: 10px;
    border: 0;
    border-left: 3px solid transparent;
    background: none;
    color: var(--fg);
    font: inherit;
    text-align: left;
    cursor: pointer;
  }

  .item:hover {
    background: var(--bg-hover);
  }

  .item.sel {
    background: var(--bg-sel);
    border-left-color: var(--accent);
  }

  .icon {
    flex: none;
    width: 16px;
    font-size: 12px;
    text-align: center;
  }

  .name {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .name.gone {
    color: var(--fg-muted);
    font-style: italic;
  }

  .unread {
    flex: none;
    font-weight: 700;
    color: var(--accent-text);
  }

  .empty {
    margin: 0;
    padding: 8px 12px;
    color: var(--fg-muted);
  }

  .group {
    height: 26px;
    padding-top: 6px;
    font-size: 12px;
    font-weight: 600;
    color: var(--fg-muted);
  }

  .sep {
    height: 1px;
    margin: 6px 10px;
    background: var(--border);
  }

  .count {
    flex: none;
    color: var(--fg-muted);
  }
</style>
