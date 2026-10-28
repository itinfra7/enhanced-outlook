<script lang="ts">
  // 새 메일 알림 창(사용자 요청 2026-10-21): 화면 오른쪽 아래, 몇 초 뒤 닫힌다(마우스를 올리면 기다린다).
  // 누르면 주 창에서 그 메일을 연다. 내용은 설정(새로운 메일 도착 / 보낸 사람 / 보낸 사람 + 제목), 안읽음 수는 늘.
  import { onMount, tick } from 'svelte';
  import { invoke } from '@tauri-apps/api/core';

  interface Item {
    id: number;
    folderId: number;
    sender: string;
    subject: string;
  }
  interface Payload {
    mode: 'off' | 'arrived' | 'sender' | 'senderSubject';
    unread: number;
    items: Item[];
    count: number;
    folderId: number;
  }

  const SHOW_MS = 8000;
  let p = $state<Payload | null>(null);
  let box: HTMLDivElement;
  let timer = 0;

  function arm() {
    clearTimeout(timer);
    timer = window.setTimeout(() => void invoke('notify_close'), SHOW_MS);
  }

  function open(item: Item | null) {
    if (!p) return;
    void invoke('notify_open', {
      folderId: p.folderId,
      messageId: item?.id ?? p.items[0]?.id ?? null,
    });
  }

  onMount(async () => {
    p = await invoke<Payload | null>('notify_payload');
    if (!p) return void invoke('notify_close');
    await tick();
    await invoke('notify_ready', { height: Math.ceil(box.getBoundingClientRect().height) });
    arm();
  });
</script>

<div
  class="box"
  bind:this={box}
  role="alert"
  onmouseenter={() => clearTimeout(timer)}
  onmouseleave={arm}
>
  {#if p}
    <div class="head">
      <button class="title" onclick={() => open(null)}>📬 새로운 메일 도착</button>
      <button class="close" aria-label="닫기" onclick={() => void invoke('notify_close')}>✕</button>
    </div>
    {#if p.mode !== 'arrived'}
      {#each p.items as it (it.id)}
        <button class="item" onclick={() => open(it)}>
          <span class="sender">{it.sender || '(보낸 사람 없음)'}</span>
          {#if p.mode === 'senderSubject'}<span class="subject">{it.subject || '(제목 없음)'}</span>{/if}
        </button>
      {/each}
      {#if p.count > p.items.length}<div class="more">외 {p.count - p.items.length}통</div>{/if}
    {/if}
    <button class="unread" onclick={() => open(null)}>읽지 않은 메일 {p.unread}통</button>
  {/if}
</div>

<style>
  :global(html),
  :global(body) {
    margin: 0;
    overflow: hidden;
    background: var(--bg);
  }

  .box {
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: 10px 12px 12px;
    border: 1px solid var(--border);
    box-sizing: border-box;
    font-size: 13px;
    color: var(--fg);
    user-select: none;
  }

  button {
    border: 0;
    background: none;
    color: inherit;
    font: inherit;
    text-align: left;
    cursor: pointer;
  }

  .head {
    display: flex;
    align-items: center;
    margin-bottom: 4px;
  }

  .title {
    flex: 1;
    padding: 0;
    font-weight: 700;
  }

  .close {
    padding: 0 2px;
    color: var(--fg-muted);
  }

  .item {
    display: flex;
    flex-direction: column;
    padding: 4px 6px;
    border-radius: 4px;
  }

  .item:hover,
  .unread:hover {
    background: var(--bg-hover);
  }

  .sender {
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .subject {
    color: var(--fg-muted);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .more {
    padding: 2px 6px;
    color: var(--fg-muted);
    font-size: 12px;
  }

  .unread {
    margin-top: 4px;
    padding: 4px 6px;
    border-radius: 4px;
    color: var(--accent-text);
    font-weight: 600;
  }
</style>
