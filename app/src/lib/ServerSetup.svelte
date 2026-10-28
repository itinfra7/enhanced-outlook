<script lang="ts">
  // 처음 실행: Outlook 웹메일 주소를 묻는다(소스에 기본 주소는 없다). 저장하면 설정에 남아 다음부터 묻지 않는다
  // (설정 → 일반에서 바꾼다). "오프라인 사용"(Esc)으로 닫으면 로컬 메일만 쓴다.
  import { api, errorText } from './api';

  interface Props {
    onsaved: (url: string) => void;
    onoffline: () => void;
  }

  let { onsaved, onoffline }: Props = $props();

  let url = $state('');
  let busy = $state(false);
  let error = $state('');
  let input: HTMLInputElement;

  $effect(() => {
    setTimeout(() => input?.focus(), 0);
  });

  async function save() {
    if (busy) return;
    if (!url.trim()) {
      error = '주소를 입력하세요.';
      input.focus();
      return;
    }
    busy = true;
    error = '';
    try {
      onsaved(await api.setOutlookUrl(url));
    } catch (e) {
      error = errorText(e);
      input.focus();
    } finally {
      busy = false;
    }
  }

  function onkeydown(e: KeyboardEvent) {
    if (e.isComposing || e.keyCode === 229) return;
    if (e.key === 'Enter') {
      e.preventDefault();
      void save();
    } else if (e.key === 'Escape' && !busy) {
      e.preventDefault();
      onoffline();
    }
  }
</script>

<div class="backdrop">
  <div class="dialog" role="dialog" aria-modal="true" aria-labelledby="server-title" tabindex="-1" {onkeydown} data-server-setup>
    <h2 id="server-title">Outlook 웹메일 주소</h2>
    <label>
      <span>주소</span>
      <input
        bind:this={input}
        bind:value={url}
        placeholder="mail.example.com"
        autocomplete="off"
        spellcheck="false"
        disabled={busy}
      />
    </label>
    {#if error}<p class="error" role="alert">{error}</p>{/if}
    <div class="buttons">
      <button class="secondary" onclick={onoffline} disabled={busy}>오프라인 사용</button>
      <button class="primary" onclick={save} disabled={busy}>다음</button>
    </div>
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
    z-index: 10;
  }

  .dialog {
    width: 340px;
    padding: 20px 22px 16px;
    border-radius: 8px;
    background: var(--bg);
    box-shadow: 0 8px 32px rgb(0 0 0 / 0.25);
    outline: none;
  }

  h2 {
    margin: 0 0 12px;
    font-size: 16px;
  }

  label {
    display: grid;
    gap: 4px;
    margin-bottom: 10px;
  }

  label span {
    font-size: 12px;
    color: var(--fg-muted);
  }

  input {
    height: 30px;
    padding: 0 8px;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--bg);
    color: var(--fg);
    font: inherit;
  }

  input:focus {
    outline: 2px solid var(--accent);
    outline-offset: -1px;
  }

  .error {
    margin: 0 0 8px;
    font-size: 12px;
    color: #c50f1f;
  }

  .buttons {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
  }

  button {
    height: 30px;
    padding: 0 14px;
    border-radius: 4px;
    font: inherit;
    cursor: pointer;
  }

  .primary {
    border: 1px solid var(--accent);
    background: var(--accent);
    color: #fff;
  }

  .secondary {
    border: 1px solid var(--border);
    background: var(--bg-pane);
    color: var(--fg);
  }

  button:disabled {
    opacity: 0.6;
    cursor: default;
  }
</style>
