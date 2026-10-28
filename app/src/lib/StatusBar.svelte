<script lang="ts">
  // 상태 표시줄(계획서 9.1, 9.5): 연결·동기화 상태, 목록 정보, 메일함 용량 막대.
  import { bytes, clock } from './format';
  import type { Status } from './types';

  interface Props {
    status: Status;
    info: string;
    /** HCL Domino 계정 연동을 켜 둠 */
    domino: boolean;
    onlogin: () => void;
    onsync: () => void;
    ondominologin: () => void;
    ondominosync: () => void;
    /** 작업 현황 창 열기(동기화 중·반영 대기를 누름) */
    onactivity: () => void;
  }

  let { status, info, domino, onlogin, onsync, ondominologin, ondominosync, onactivity }: Props = $props();

  let ratio = $derived(
    status.quotaUsed != null && status.quotaLimit ? Math.min(1, status.quotaUsed / status.quotaLimit) : null,
  );
  let bn = $derived(status.domino);
  let bnRatio = $derived(
    bn.quotaUsed != null && bn.quotaLimit ? Math.min(1, bn.quotaUsed / bn.quotaLimit) : null,
  );
</script>

<footer class="bar">
  <span class="state">
    {#if status.connecting}
      <span class="dot busy"></span><button class="act" onclick={onactivity} title="작업 현황" data-activity-open>연결 중…</button>
    {:else if status.loginRequired}
      <span class="dot warn"></span><button class="link" onclick={onlogin}>로그인 필요</button>
    {:else if !status.loggedIn}
      <span class="dot off"></span><button class="act" onclick={onactivity} title="작업 현황" data-activity-open>오프라인</button>
      · <button class="link" onclick={onlogin}>로그인</button>
    {:else if status.syncing}
      <span class="dot busy"></span><button class="act" onclick={onactivity} title="작업 현황" data-activity-open
        >동기화 중{status.emlTotal > 0 ? ` · 원문 ${status.emlDone}/${status.emlTotal}` : '…'}</button
      >
    {:else}
      <span class="dot ok"></span><button class="act" onclick={onactivity} title="작업 현황" data-activity-open
        >연결됨{status.lastSync ? ` · ${clock(status.lastSync)} 확인` : ''}</button
      >
      <button class="link" onclick={onsync} title="지금 동기화 (F5)">↻</button>
    {/if}
    {#if status.pendingOps > 0}
      <button
        class="act pending"
        onclick={onactivity}
        title={status.loggedIn ? '서버에 보내는 중' : '로그인하면 반영'}
        >· 서버 반영 대기 {status.pendingOps}건</button
      >
    {/if}
    {#if status.lastError}
      <span class="err" title={status.lastError}>⚠ {status.lastError}</span>
    {/if}
    {#if domino}
      <span class="bn" data-domino>
        · HCL Domino
        {#if bn.connecting}
          연결 중…
        {:else if !bn.loggedIn}
          <button class="link" onclick={ondominologin}>{bn.loginRequired ? '로그인 필요' : '로그인'}</button>
        {:else if bn.syncing}
          <button class="act" onclick={onactivity} title="작업 현황">{bn.total > 0 ? `${bn.done}/${bn.total}` : '확인 중…'}</button>
        {:else}
          <button class="act" onclick={onactivity} title="작업 현황">{bn.lastSync ? `${clock(bn.lastSync)} 확인` : '연결됨'}</button>
          <button class="link" onclick={ondominosync} title="HCL Domino 지금 확인">↻</button>
        {/if}
        {#if bn.pending > 0}<button class="act pending" onclick={onactivity} title="작업 현황">반영 대기 {bn.pending}건</button>{/if}
        {#if bn.lastError}<span class="err" title={bn.lastError}>⚠ {bn.lastError}</span>{/if}
      </span>
    {/if}
  </span>
  <span class="info">{info}</span>
  {#if domino && bnRatio != null && bn.quotaUsed != null && bn.quotaLimit}
    <span class="quota" title="HCL Domino 메일함 사용량">
      HCL Domino
      <span class="meter"><span class="fill" class:high={bnRatio > 0.9} style:width="{bnRatio * 100}%"></span></span>
      {bytes(bn.quotaUsed)} / {bytes(bn.quotaLimit)}
    </span>
  {/if}
  <span class="quota" title="메일함 사용량">
    {#if ratio != null && status.quotaUsed != null && status.quotaLimit}
      <span class="meter"><span class="fill" class:high={ratio > 0.9} style:width="{ratio * 100}%"></span></span>
      {bytes(status.quotaUsed)} / {bytes(status.quotaLimit)}
    {:else}
      메일함 용량 —
    {/if}
  </span>
</footer>

<style>
  .bar {
    display: flex;
    align-items: center;
    gap: 16px;
    height: 100%;
    padding: 0 12px;
    font-size: 12px;
    color: var(--fg-muted);
    background: var(--bg-bar);
    border-top: 1px solid var(--border);
    white-space: nowrap;
  }

  .state {
    display: flex;
    align-items: center;
    gap: 4px;
    min-width: 0;
    overflow: hidden;
  }

  .info {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    text-align: center;
  }

  .dot {
    display: inline-block;
    width: 8px;
    height: 8px;
    margin-right: 4px;
    border-radius: 50%;
  }

  .dot.ok {
    background: #107c10;
  }

  .dot.busy {
    background: #0f6cbd;
  }

  .dot.warn {
    background: #c50f1f;
  }

  .dot.off {
    background: #8a8a8a;
  }

  .pending {
    margin-left: 4px;
    color: #8a5300;
  }

  .bn {
    display: flex;
    align-items: center;
    gap: 4px;
    margin-left: 4px;
  }

  .err {
    margin-left: 8px;
    max-width: 360px;
    overflow: hidden;
    text-overflow: ellipsis;
    color: #c50f1f;
  }

  .act {
    padding: 0;
    border: 0;
    background: none;
    color: inherit;
    font: inherit;
    cursor: pointer;
  }

  .act:hover {
    text-decoration: underline;
  }

  .act.pending {
    color: #8a5300;
  }

  .link {
    padding: 0 2px;
    border: 0;
    background: none;
    color: var(--accent);
    font: inherit;
    cursor: pointer;
  }

  .quota {
    display: flex;
    align-items: center;
    gap: 6px;
  }

  .meter {
    display: inline-block;
    width: 90px;
    height: 6px;
    border-radius: 3px;
    background: var(--border);
    overflow: hidden;
  }

  .fill {
    display: block;
    height: 100%;
    background: var(--accent);
  }

  .fill.high {
    background: #c50f1f;
  }
</style>
