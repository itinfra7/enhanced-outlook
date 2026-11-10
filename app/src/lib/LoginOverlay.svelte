<script lang="ts">
  // 로그인 창(계획서 9.5). 마지막 상태 위에 띄운다. "오프라인 사용"(또는 Esc)으로 닫으면 로그인 없이
  // 로컬 메일만 쓴다(보내기·동기화 같은 온라인 기능은 로그인할 때까지 숨김).
  // 비밀번호는 저장하지 않는다: <form> 제출이 없어 WebView2 자동 완성에 남지 않고
  // (자동 완성 기능도 꺼 둠), 비밀번호 칸은 시도할 때마다 비운다.
  // 아이디는 로그인하면 앱이 기억한다(1.1.0, state.json). 아이디 칸의 ✕(onforget)로 지우면 다시 로그인할 때까지 비운다.
  // HCL Domino 계정 연동(0.5.0)도 같은 창을 쓴다(제목·아이디 이름·닫기 단추만 다르다).
  interface Props {
    username: string;
    reason: string;
    onlogin: (username: string, password: string) => Promise<string | null>;
    onoffline: () => void;
    onforget?: () => void;
    title?: string;
    userLabel?: string;
    closeLabel?: string;
  }

  let {
    username,
    reason,
    onlogin,
    onoffline,
    onforget,
    title = 'Outlook 로그인',
    userLabel = '아이디',
    closeLabel = '오프라인 사용',
  }: Props = $props();

  /** 받침이 있으면 `a`(사번을·사번은), 없으면 `b`(아이디를·아이디는). */
  function josa(word: string, a: string, b: string): string {
    const c = word.charCodeAt(word.length - 1) - 0xac00;
    return c >= 0 && c < 11172 && c % 28 !== 0 ? a : b;
  }

  function forget() {
    user = '';
    onforget?.();
    userInput?.focus();
  }

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
      error = `${userLabel}${josa(userLabel, '을', '를')} 입력하세요.`;
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
      <span class="userbox">
        <input
          bind:this={userInput}
          bind:value={user}
          name="outlook-user-nosave"
          autocomplete="off"
          spellcheck="false"
          disabled={busy}
        />
        {#if user && !busy}
          <button
            type="button"
            class="clear"
            title="{userLabel} 지우기(다시 로그인할 때까지 기억하지 않음)"
            aria-label="{userLabel} 지우기"
            onclick={forget}>✕</button
          >
        {/if}
      </span>
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
    <p class="note">비밀번호는 저장하지 않습니다. {userLabel}{josa(userLabel, '은', '는')} 로그인하면 기억합니다.</p>
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

  .userbox {
    position: relative;
    display: grid;
    font-size: inherit;
    color: inherit;
  }

  .userbox input {
    padding-right: 30px;
  }

  label .userbox .clear {
    position: absolute;
    top: 3px;
    right: 3px;
    width: 24px;
    height: 24px;
    padding: 0;
    border: none;
    border-radius: 4px;
    background: transparent;
    color: var(--fg-muted);
    font-size: 12px;
    line-height: 24px;
  }

  label .userbox .clear:hover {
    background: var(--bg-hover);
    color: var(--fg);
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
