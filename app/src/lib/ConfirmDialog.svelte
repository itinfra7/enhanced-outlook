<script lang="ts">
  // 확인 창. `double`이면 이중 확인(되돌릴 수 없는 완전 삭제 등): 확인란을 체크해야 실행 단추가 켜진다.
  import Modal from './Modal.svelte';

  interface Props {
    title: string;
    message: string;
    detail?: string;
    confirmLabel: string;
    danger?: boolean;
    double?: string;
    onconfirm: () => void;
    oncancel: () => void;
  }

  let { title, message, detail, confirmLabel, danger = false, double, onconfirm, oncancel }: Props =
    $props();
  let agreed = $state(false);
</script>

<Modal {title} onclose={oncancel}>
  <p class="msg">{message}</p>
  {#if detail}<p class="detail">{detail}</p>{/if}
  {#if double}
    <label class="agree"><input type="checkbox" bind:checked={agreed} /> {double}</label>
  {/if}
  {#snippet footer()}
    <button class="btn" onclick={oncancel}>취소</button>
    <button
      class="btn"
      class:danger
      class:primary={!danger}
      disabled={!!double && !agreed}
      onclick={onconfirm}>{confirmLabel}</button
    >
  {/snippet}
</Modal>

<style>
  .msg {
    margin: 4px 0 8px;
    line-height: 1.5;
    white-space: pre-line;
  }

  .detail {
    margin: 0 0 8px;
    font-size: 12px;
    color: var(--fg-muted);
    white-space: pre-line;
  }

  .agree {
    display: flex;
    align-items: center;
    gap: 6px;
    margin-top: 10px;
    font-size: 12.5px;
    color: #c50f1f;
  }
</style>
