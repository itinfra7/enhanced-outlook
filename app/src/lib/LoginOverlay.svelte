<script lang="ts">
  // 로그인 창(계획서 9.5). 마지막 상태 위에 띄운다. "오프라인 사용"(또는 Esc)으로 닫으면 로그인 없이
  // 로컬 메일만 쓴다(보내기·동기화 같은 온라인 기능은 로그인할 때까지 숨김).
  // 아이디·비밀번호는 저장하지 않는다: <form> 제출이 없어 WebView2 자동 완성에 남지 않고
  // (자동 완성 기능도 꺼 둠), 비밀번호 칸은 시도할 때마다 비운다.
  // HCL Domino 계정 연동(0.5.0)도 같은 창을 쓴다(제목·아이디 이름·닫기 단추만 다르다).
  interface Props {
    username: string;
    reason: string;
    onlogin: (username: string, password: string) => Promise<string | null>;
    onoffline: () => void;
    title?: string;
    userLabel?: string;
    closeLabel?: string;
  }

  let {
    username,
    reason,
    onlogin,
    onoffline,
    title = 'Outlook 로그인',
    userLabel = '아이디',
    closeLabel = '오프라인 사용',
  }: Props = $props();

  let user = $state('');
  let password = $state('');
  let busy = $state(false);
  let error = $state('');
  let userInput: HTMLInputElement;
  let passInput: HTMLInputElement;

  $effect(() => {
    user = username;
    // 아이디를 기억하고 있으면 비밀번호부터.
    setTimeout(() => (username ? passInput : userInput)?.focus(), 0);
  });

  async function submit() {
    if (busy) return;
    if (!user.trim()) {
      // 받침이 있으면 '을'(사번을), 없으면 '를'(아이디를).
      const c = userLabel.charCodeAt(userLabel.length - 1) - 0xac00;
      error = `${userLabel}${c >= 0 && c < 11172 && c % 28 !== 0 ? '을' : '를'} 입력하세요.`;
      userInput.focus();
      return;
    }
    if (!password) {
      error = '비밀번호를 입력하세요.';
      passInput.focus();
      return;
    }
    busy = true;
    error = '';
    const pw = password;
    password = '';
    const failure = await onlogin(user.trim(), pw);
    busy = false;
    if (failure) {
      error = failure;
      setTimeout(() => passInput?.focus(), 0);
    }
  }

  function onkeydown(e: KeyboardEvent) {
    if (e.isComposing || e.keyCode === 229) return;
    if (e.key === 'Enter') {
      e.preventDefault();
      void submit();
    } else if (e.key === 'Escape' && !busy) {
      e.preventDefault();
      onoffline();
    }
  }
</script>

<div class="backdrop">
  <div class="dialog" role="dialog" aria-modal="true" aria-labelledby="login-title" tabindex="-1" {onkeydown}>
    <h2 id="login-title">{title}</h2>
    {#if reason}<p class="reason">{reason}</p>{/if}
    <label>
      <span>{userLabel}</span>
      <input
        bind:this={userInput}
        bind:value={user}
        name="outlook-user-nosave"
        autocomplete="off"
        spellcheck="false"
        disabled={busy}
      />
    </label>
    <label>
      <span>비밀번호</span>
      <input
        bind:this={passInput}
        bind:value={password}
        type="password"
        name="outlook-pass-nosave"
        autocomplete="new-password"
        disabled={busy}
      />
    </label>
    {#if error}<p class="error" role="alert">{error}</p>{/if}
    <p class="note">{userLabel}·비밀번호는 저장하지 않습니다.</p>
    <div class="buttons">
      <button class="secondary" onclick={onoffline} disabled={busy}>{closeLabel}</button>
      <button class="primary" onclick={submit} disabled={busy}>{busy ? '연결 중…' : '로그인'}</button>
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

  .reason {
    margin: -4px 0 12px;
    font-size: 12px;
    color: #8a5300;
  }

  .error {
    margin: 0 0 8px;
    font-size: 12px;
    color: #c50f1f;
  }

  .note {
    margin: 0 0 12px;
    font-size: 11px;
    color: var(--fg-muted);
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
