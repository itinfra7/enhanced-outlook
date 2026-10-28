<script lang="ts">
  // 앱 안 대화 창(가운데, 뒤 배경 어둡게). Esc로 닫는다.
  import type { Snippet } from 'svelte';

  interface Props {
    title: string;
    width?: number;
    onclose: () => void;
    children: Snippet;
    footer?: Snippet;
  }

  let { title, width = 420, onclose, children, footer }: Props = $props();
  let dialog: HTMLDivElement;

  $effect(() => {
    dialog?.focus();
  });

  function onkeydown(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.preventDefault();
      e.stopPropagation();
      onclose();
    }
  }
</script>

<div class="backdrop">
  <div
    class="dialog"
    style:width="{width}px"
    role="dialog"
    aria-modal="true"
    aria-label={title}
    tabindex="-1"
    bind:this={dialog}
    {onkeydown}
  >
    <header>
      <h2>{title}</h2>
      <button class="x" onclick={onclose} aria-label="닫기">✕</button>
    </header>
    <div class="body">{@render children()}</div>
    {#if footer}<footer>{@render footer()}</footer>{/if}
  </div>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    background: rgb(0 0 0 / 0.28);
    z-index: 30;
  }

  .dialog {
    max-width: calc(100vw - 32px);
    max-height: calc(100vh - 48px);
    display: flex;
    flex-direction: column;
    border-radius: 8px;
    background: var(--bg);
    box-shadow: 0 8px 32px rgb(0 0 0 / 0.25);
    outline: none;
  }

  header {
    display: flex;
    align-items: center;
    padding: 14px 16px 6px 20px;
  }

  h2 {
    flex: 1;
    margin: 0;
    font-size: 15px;
  }

  .x {
    border: 0;
    background: none;
    color: var(--fg-muted);
    font-size: 14px;
    cursor: pointer;
  }

  .body {
    padding: 6px 20px 12px;
    overflow: auto;
  }

  footer {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    padding: 10px 20px 16px;
  }

  :global(.btn) {
    height: 30px;
    padding: 0 14px;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--bg-pane);
    color: var(--fg);
    font: inherit;
    cursor: pointer;
  }

  :global(.btn.primary) {
    border-color: var(--accent);
    background: var(--accent);
    color: #fff;
  }

  :global(.btn.danger) {
    border-color: #c50f1f;
    background: #c50f1f;
    color: #fff;
  }

  :global(.btn:disabled) {
    opacity: 0.55;
    cursor: default;
  }
</style>
