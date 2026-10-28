<script lang="ts">
  // 주소록에서 고르기(웹메일의 "받는 사람"·"참조"·"숨은 참조" 단추, 0.7.0): 로컬 주소록과 서버(FindPeople)에서
  // 찾아 받는 사람·참조·숨은 참조에 넣는다.
  import { api } from '../api';
  import Modal from '../Modal.svelte';
  import type { Recipient } from '../types';

  type Field = 'to' | 'cc' | 'bcc';

  interface Props {
    /** 처음 넣을 칸 */
    field: Field;
    onadd: (field: Field, r: Recipient) => void;
    onclose: () => void;
  }

  let { field, onadd, onclose }: Props = $props();
  let query = $state('');
  let results = $state<Recipient[]>([]);
  let searching = $state(false);
  let added = $state<string[]>([]);
  let seq = 0;
  let timer: ReturnType<typeof setTimeout> | undefined;

  const LABEL: Record<Field, string> = { to: '받는 사람', cc: '참조', bcc: '숨은 참조' };

  function search() {
    clearTimeout(timer);
    const q = query.trim();
    if (!q) {
      results = [];
      return;
    }
    const mine = ++seq;
    timer = setTimeout(async () => {
      searching = true;
      const [local, server] = await Promise.all([
        api.contactsLocal(q).catch(() => [] as Recipient[]),
        api.contactsServer(q).catch(() => [] as Recipient[]),
      ]);
      if (mine !== seq) return;
      const merged: Recipient[] = [];
      for (const r of [...server, ...local]) {
        if (!merged.some((m) => m.email.toLowerCase() === r.email.toLowerCase())) merged.push(r);
      }
      results = merged.slice(0, 40);
      searching = false;
    }, 250);
  }

  function add(f: Field, r: Recipient) {
    onadd(f, r);
    added = [...added, `${LABEL[f]}: ${r.name || r.email}`];
  }
</script>

<Modal title="주소록" width={560} {onclose}>
  <input
    class="q"
    placeholder="이름 또는 주소"
    aria-label="이름 또는 주소"
    bind:value={query}
    oninput={search}
    spellcheck="false"
    autocomplete="off"
  />
  <ul class="list" aria-label="찾은 사람">
    {#each results as r (r.email)}
      <li>
        <span class="who"><span class="n">{r.name || r.email}</span>{#if r.name}<span class="e">{r.email}</span>{/if}</span>
        {#each ['to', 'cc', 'bcc'] as const as f (f)}
          <button class="add" class:main={f === field} onclick={() => add(f, r)}>{LABEL[f]}</button>
        {/each}
      </li>
    {:else}
      <li class="none">{query.trim() ? (searching ? '찾는 중…' : '없음') : ''}</li>
    {/each}
  </ul>
  {#if added.length}<p class="added">{added.join(' · ')}</p>{/if}
  {#snippet footer()}
    <button class="btn primary" onclick={onclose}>닫기</button>
  {/snippet}
</Modal>

<style>
  .q {
    width: 100%;
    height: 30px;
    padding: 0 8px;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--bg);
    color: var(--fg);
    font: inherit;
  }

  .list {
    height: 300px;
    margin: 8px 0 0;
    padding: 0;
    overflow-y: auto;
    list-style: none;
    border: 1px solid var(--border-soft);
    border-radius: 4px;
  }

  li {
    display: flex;
    align-items: center;
    gap: 4px;
    padding: 4px 8px;
    border-bottom: 1px solid var(--border-soft);
  }

  li.none {
    border: 0;
    color: var(--fg-muted);
  }

  .who {
    flex: 1;
    min-width: 0;
    display: flex;
    gap: 8px;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }

  .e {
    color: var(--fg-muted);
    font-size: 12px;
  }

  .add {
    flex: none;
    height: 24px;
    padding: 0 8px;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--bg);
    color: var(--fg);
    font: inherit;
    font-size: 12px;
    cursor: pointer;
  }

  .add.main {
    border-color: var(--accent);
    color: var(--accent);
  }

  .added {
    margin: 8px 0 0;
    color: var(--fg-muted);
    font-size: 12px;
  }
</style>
