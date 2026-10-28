<script lang="ts">
  // 오른쪽 클릭 메뉴. 바깥을 누르거나 Esc면 닫힌다.
  import type { MenuItem } from './types';

  interface Props {
    x: number;
    y: number;
    items: MenuItem[];
    onclose: () => void;
  }

  let { x, y, items, onclose }: Props = $props();
  let menu: HTMLDivElement;
  let pos = $state({ left: 0, top: 0 });

  $effect(() => {
    // 화면 밖으로 나가지 않게.
    const r = menu.getBoundingClientRect();
    pos = {
      left: Math.min(x, window.innerWidth - r.width - 4),
      top: Math.min(y, window.innerHeight - r.height - 4),
    };
    menu.focus();
  });

  function onkeydown(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.preventDefault();
      onclose();
    }
  }
</script>

<div class="catcher" role="presentation" onmousedown={onclose} oncontextmenu={(e) => e.preventDefault()}></div>
<div
  class="menu"
  role="menu"
  tabindex="-1"
  bind:this={menu}
  style:left="{pos.left}px"
  style:top="{pos.top}px"
  {onkeydown}
>
  {#each items as it, i (i)}
    {#if it.separator}
      <div class="sep" role="separator"></div>
    {:else}
      <button
        class="item"
        class:danger={it.danger}
        role="menuitem"
        disabled={it.disabled}
        onclick={() => {
          onclose();
          it.run?.();
        }}
      >
        <span>{it.label}</span>{#if it.shortcut}<span class="sc">{it.shortcut}</span>{/if}
      </button>
    {/if}
  {/each}
</div>

<style>
  .catcher {
    position: fixed;
    inset: 0;
    z-index: 40;
  }

  .menu {
    position: fixed;
    z-index: 41;
    min-width: 200px;
    padding: 4px 0;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--bg);
    box-shadow: 0 6px 24px rgb(0 0 0 / 0.18);
    outline: none;
  }

  .item {
    display: flex;
    justify-content: space-between;
    gap: 16px;
    width: 100%;
    padding: 6px 14px;
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

  .item.danger {
    color: #c50f1f;
  }

  .sc {
    color: var(--fg-muted);
    font-size: 12px;
  }

  .sep {
    height: 1px;
    margin: 4px 0;
    background: var(--border);
  }
</style>
