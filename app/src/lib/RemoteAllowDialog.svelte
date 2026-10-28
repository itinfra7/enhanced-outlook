<script lang="ts">
  // 외부 이미지 허용 목록: 보낸 사람 주소·도메인(하위 도메인 포함). 보기·추가·삭제.
  import { onMount } from 'svelte';
  import { api, errorText } from './api';
  import Modal from './Modal.svelte';

  interface Props {
    onclose: () => void;
    /** 목록이 바뀜(설정 창의 개수 갱신용) */
    onchange: (count: number) => void;
  }

  let { onclose, onchange }: Props = $props();
  let senders = $state<string[]>([]);
  let domains = $state<string[]>([]);
  let input = $state('');
  let error = $state('');

  async function load() {
    try {
      const l = await api.remoteAllowlist();
      senders = l.senders;
      domains = l.domains;
      onchange(senders.length + domains.length);
    } catch (e) {
      error = errorText(e);
    }
  }

  onMount(load);

  // "a@b.com"은 보낸 사람, "b.com"·"@b.com"은 도메인.
  async function add() {
    const v = input.trim();
    if (!v) return;
    error = '';
    const kind = /^[^@\s]+@/.test(v) ? 'sender' : 'domain';
    try {
      await api.allowRemote(kind, v);
      input = '';
      await load();
    } catch (e) {
      error = errorText(e);
    }
  }

  async function remove(kind: 'sender' | 'domain', v: string) {
    error = '';
    try {
      await api.removeRemoteAllow(kind, v);
      await load();
    } catch (e) {
      error = errorText(e);
    }
  }

  async function clearAll() {
    if (!window.confirm('허용 목록을 모두 지울까요?')) return;
    try {
      await api.forgetRemoteImages();
      await load();
    } catch (e) {
      error = errorText(e);
    }
  }
</script>

<Modal title="외부 이미지 허용 목록" width={440} {onclose}>
  <form class="add" onsubmit={(e) => { e.preventDefault(); void add(); }}>
    <input bind:value={input} placeholder="name@example.com 또는 example.com" spellcheck="false" />
    <button class="btn" type="submit" disabled={!input.trim()}>추가</button>
  </form>
  {#if senders.length + domains.length === 0}
    <p class="empty">없음</p>
  {:else}
    <ul>
      {#each domains as d (d)}
        <li><span>@{d}</span><button aria-label="@{d} 삭제" onclick={() => remove('domain', d)}>✕</button></li>
      {/each}
      {#each senders as s (s)}
        <li><span>{s}</span><button aria-label="{s} 삭제" onclick={() => remove('sender', s)}>✕</button></li>
      {/each}
    </ul>
  {/if}
  {#if error}<p class="error">{error}</p>{/if}

  {#snippet footer()}
    <button class="btn" disabled={senders.length + domains.length === 0} onclick={clearAll}>모두 지우기</button>
    <button class="btn" onclick={onclose}>닫기</button>
  {/snippet}
</Modal>

<style>
  .add {
    display: flex;
    gap: 6px;
    margin-bottom: 10px;
  }

  .add input {
    flex: 1;
    height: 28px;
    padding: 0 8px;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--bg);
    color: var(--fg);
    font: inherit;
  }

  ul {
    margin: 0;
    padding: 0;
    list-style: none;
    max-height: 280px;
    overflow-y: auto;
    border: 1px solid var(--border);
    border-radius: 4px;
  }

  li {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 4px 8px;
    font-size: 13px;
  }

  li + li {
    border-top: 1px solid var(--border);
  }

  li span {
    flex: 1;
    min-width: 0;
    overflow-wrap: anywhere;
  }

  li button {
    border: 0;
    background: none;
    color: var(--fg-muted);
    cursor: pointer;
  }

  li button:hover {
    color: #c50f1f;
  }

  .empty {
    color: var(--fg-muted);
    font-size: 13px;
  }

  .error {
    color: #c50f1f;
    font-size: 12.5px;
  }
</style>
