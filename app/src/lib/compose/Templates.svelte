<script lang="ts">
  // 내 템플릿(웹메일 작성 화면의 "내 템플릿"과 같은 기능, 0.7.0): 자주 쓰는 문구·그림을 저장해 두고 넣는다.
  // 새 템플릿은 작성 화면에서 고른 부분(없으면 본문 전체)으로 만든다.
  import { onMount } from 'svelte';
  import { api, errorText } from '../api';
  import Modal from '../Modal.svelte';
  import type { Template } from '../types';

  interface Props {
    /** 새 템플릿에 쓸 내용(고른 부분 또는 본문) */
    current: () => string;
    oninsert: (html: string) => void;
    onclose: () => void;
  }

  let { current, oninsert, onclose }: Props = $props();
  let list = $state<Template[]>([]);
  let name = $state('');
  let error = $state('');

  async function store(next: Template[]) {
    try {
      await api.templatesSet(next);
      list = next;
      error = '';
    } catch (e) {
      error = errorText(e);
    }
  }

  async function addNew() {
    const n = name.trim();
    const html = current();
    if (!n) {
      error = '이름을 적어 주세요.';
      return;
    }
    if (!html.replace(/<[^>]*>/g, '').trim() && !/<img/i.test(html)) {
      error = '넣을 내용을 작성 화면에 쓰거나 골라 주세요.';
      return;
    }
    await store([...list.filter((t) => t.name !== n), { name: n, html }]);
    name = '';
  }

  onMount(async () => {
    list = await api.templatesList().catch(() => []);
  });
</script>

<Modal title="내 템플릿" width={480} {onclose}>
  <ul class="list">
    {#each list as t (t.name)}
      <li>
        <button class="name" title="본문에 넣기" onclick={() => { oninsert(t.html); onclose(); }}>{t.name}</button>
        <button class="del" aria-label="{t.name} 지우기" onclick={() => void store(list.filter((x) => x !== t))}>✕</button>
      </li>
    {:else}
      <li class="none">없음</li>
    {/each}
  </ul>
  <div class="new">
    <input bind:value={name} placeholder="새 템플릿 이름" aria-label="새 템플릿 이름" />
    <button class="btn" onclick={() => void addNew()}>고른 내용으로 저장</button>
  </div>
  {#if error}<p class="error">{error}</p>{/if}
  {#snippet footer()}
    <button class="btn primary" onclick={onclose}>닫기</button>
  {/snippet}
</Modal>

<style>
  .list {
    max-height: 260px;
    margin: 0;
    padding: 0;
    overflow-y: auto;
    list-style: none;
    border: 1px solid var(--border-soft);
    border-radius: 4px;
  }

  li {
    display: flex;
    align-items: center;
    border-bottom: 1px solid var(--border-soft);
  }

  li.none {
    padding: 8px;
    border: 0;
    color: var(--fg-muted);
  }

  .name {
    flex: 1;
    padding: 7px 10px;
    border: 0;
    background: none;
    color: var(--fg);
    font: inherit;
    text-align: left;
    cursor: pointer;
  }

  .name:hover {
    background: var(--bg-hover);
  }

  .del {
    padding: 0 10px;
    border: 0;
    background: none;
    color: var(--fg-muted);
    cursor: pointer;
  }

  .new {
    display: flex;
    gap: 6px;
    margin-top: 10px;
  }

  .new input {
    flex: 1;
    height: 30px;
    padding: 0 8px;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--bg);
    color: var(--fg);
    font: inherit;
  }

  .error {
    margin: 6px 0 0;
    color: #c50f1f;
    font-size: 12.5px;
  }
</style>
