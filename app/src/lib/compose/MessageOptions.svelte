<script lang="ts">
  // 메시지 옵션(웹메일 "기타 작업 → 메시지 옵션 표시", 0.7.0): 우편물 종류, 배달 확인 요청, 읽음 확인 요청.
  // S/MIME 암호화·디지털 서명은 웹메일에서도 꺼져 있어 넣지 않았다.
  import Modal from '../Modal.svelte';
  import type { DraftOptions } from '../types';

  interface Props {
    options: DraftOptions;
    onsave: (o: DraftOptions) => void;
    onclose: () => void;
  }

  let { options, onsave, onclose }: Props = $props();
  // svelte-ignore state_referenced_locally
  let draft = $state({ ...options });

  const SENSITIVITY = ['보통', '개인', '비공개', '비밀 우편'];
</script>

<Modal title="메시지 옵션" width={300} {onclose}>
  <label class="col">
    우편물 종류
    <select bind:value={draft.sensitivity} aria-label="우편물 종류">
      {#each SENSITIVITY as s, i (i)}<option value={i}>{s}</option>{/each}
    </select>
  </label>
  <label class="row"><input type="checkbox" bind:checked={draft.deliveryReceipt} /> 배달 확인 요청</label>
  <label class="row"><input type="checkbox" bind:checked={draft.readReceipt} /> 읽음 확인 요청</label>
  {#snippet footer()}
    <button
      class="btn primary"
      onclick={() => {
        onsave({ ...draft });
        onclose();
      }}>확인</button
    >
    <button class="btn" onclick={onclose}>취소</button>
  {/snippet}
</Modal>

<style>
  .col {
    display: flex;
    flex-direction: column;
    gap: 4px;
    margin-bottom: 8px;
    font-size: 13px;
  }

  select {
    height: 28px;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--bg);
    color: var(--fg);
    font: inherit;
  }

  .row {
    display: flex;
    align-items: center;
    gap: 6px;
    margin: 6px 0;
    font-size: 13px;
  }
</style>
