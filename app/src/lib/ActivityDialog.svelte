<script lang="ts">
  // 작업 현황(0.7.1 사용자 요청): 상태 표시줄의 "동기화 중"·"서버 반영 대기"를 누르면 연다.
  // 지금 하는 일은 상태(status)로, 대기·실패 작업은 1초마다 다시 읽어 한 줄씩 보인다.
  import { onMount } from 'svelte';
  import { api, errorText } from './api';
  import { clock, listDate } from './format';
  import Modal from './Modal.svelte';
  import type { ActivityView, OpLine, Status } from './types';

  interface Props {
    status: Status;
    /** HCL Domino 계정 연동을 켜 둠 */
    domino: boolean;
    onclose: () => void;
  }

  let { status, domino, onclose }: Props = $props();
  let view = $state<ActivityView | null>(null);
  let error = $state('');

  onMount(() => {
    let stopped = false;
    const load = async () => {
      try {
        view = await api.activityView();
        error = '';
      } catch (e) {
        error = errorText(e);
      }
    };
    void load();
    const timer = setInterval(() => {
      if (!stopped) void load();
    }, 1000);
    return () => {
      stopped = true;
      clearInterval(timer);
    };
  });

  let bn = $derived(status.domino);

  /** 지금 하는 일: [무엇, 상태, 바쁨] */
  let now = $derived.by(() => {
    const out: [string, string, boolean][] = [];
    const outlook = status.connecting
      ? ['연결 중', true]
      : status.loginRequired
        ? ['로그인 필요', false]
        : !status.loggedIn
          ? ['오프라인', false]
          : status.syncing
            ? [status.emlTotal > 0 ? `원문 받는 중 ${status.emlDone}/${status.emlTotal}` : '폴더·메일 목록 확인 중', true]
            : [status.lastSync ? `쉬는 중 · ${clock(status.lastSync)} 확인` : '쉬는 중', false];
    out.push(['Outlook 메일 동기화', outlook[0] as string, outlook[1] as boolean]);
    const pending = view?.outlookTotal ?? status.pendingOps;
    if (pending > 0)
      out.push(['Outlook 서버 반영', status.loggedIn ? `보내는 중 ${pending}건` : `로그인하면 보냄 ${pending}건`, status.loggedIn]);
    if (domino) {
      const b = bn.connecting
        ? ['연결 중', true]
        : !bn.loggedIn
          ? [bn.loginRequired ? '로그인 필요' : '로그인 전', false]
          : bn.syncing
            ? [bn.total > 0 ? `가져오는 중 ${bn.done}/${bn.total}` : '메일함 확인 중', true]
            : [bn.lastSync ? `쉬는 중 · ${clock(bn.lastSync)} 확인` : '쉬는 중', false];
      out.push(['HCL Domino 확인', b[0] as string, b[1] as boolean]);
      const bp = view?.dominoTotal ?? bn.pending;
      if (bp > 0) out.push(['HCL Domino 웹메일 반영', bn.loggedIn ? `보내는 중 ${bp}건` : `로그인하면 보냄 ${bp}건`, bn.loggedIn]);
    }
    if (view && view.indexPending > 0) out.push(['검색 색인', `남은 메일 ${view.indexPending.toLocaleString()}통`, true]);
    if (view?.importing) out.push(['가져오기', '진행 중', true]);
    return out;
  });

  function when(o: OpLine): string {
    const parts = [listDate(o.createdAt)];
    if (o.attempts > 0) parts.push(`시도 ${o.attempts}회`);
    if (o.nextAt) parts.push(`다음 ${clock(o.nextAt)}`);
    return parts.join(' · ');
  }
</script>

{#snippet ops(list: OpLine[], total: number)}
  <ul class="ops">
    {#each list as o, i (i)}
      <li>
        <div class="line">
          <span class="what">{o.what}</span>
          <span class="subj" title={o.subject ?? ''}>{o.subject || (o.subject === null ? '' : '(제목 없음)')}</span>
          <span class="when">{when(o)}</span>
        </div>
        {#if o.detail || o.error}
          <div class="sub">
            {#if o.detail}<span>{o.detail}</span>{/if}
            {#if o.error}<span class="err" title={o.error}>{o.error}</span>{/if}
          </div>
        {/if}
      </li>
    {/each}
  </ul>
  {#if total > list.length}<p class="more">외 {(total - list.length).toLocaleString()}건</p>{/if}
{/snippet}

<Modal title="작업 현황" width={640} {onclose}>
  <section data-activity-now>
    <h3>지금 하는 일</h3>
    <ul class="now">
      {#each now as [what, state, busy] (what)}
        <li><span class="dot" class:busy></span><span class="what">{what}</span><span>{state}</span></li>
      {/each}
    </ul>
    {#if status.lastError}<p class="err">Outlook: {status.lastError}</p>{/if}
    {#if domino && bn.lastError}<p class="err">HCL Domino: {bn.lastError}</p>{/if}
  </section>

  {#if view}
    {#if view.outlookTotal > 0}
      <section data-activity-outlook={view.outlookTotal}>
        <h3>Outlook 서버 반영 대기 {view.outlookTotal.toLocaleString()}건</h3>
        {#if view.outlookKinds.length > 1}
          <p class="kinds">{view.outlookKinds.map((k) => `${k.what} ${k.count.toLocaleString()}`).join(' · ')}</p>
        {/if}
        {@render ops(view.outlook, view.outlookTotal)}
      </section>
    {/if}
    {#if view.dominoTotal > 0}
      <section data-activity-domino={view.dominoTotal}>
        <h3>HCL Domino 웹메일 반영 대기 {view.dominoTotal.toLocaleString()}건</h3>
        {@render ops(view.domino, view.dominoTotal)}
      </section>
    {/if}
    {#if view.failed.length > 0}
      <section>
        <h3>최근 7일 실패한 작업</h3>
        <ul class="ops">
          {#each view.failed as o, i (i)}
            <li>
              <div class="line">
                <span class="what">{o.source} · {o.what}</span>
                <span class="subj" title={o.subject ?? ''}>{o.subject ?? ''}</span>
                <span class="when">{listDate(o.createdAt)}</span>
              </div>
              {#if o.detail || o.error}
                <div class="sub">
                  {#if o.detail}<span>{o.detail}</span>{/if}
                  {#if o.error}<span class="err" title={o.error}>{o.error}</span>{/if}
                </div>
              {/if}
            </li>
          {/each}
        </ul>
      </section>
    {/if}
    {#if view.outlookTotal === 0 && view.dominoTotal === 0}
      <p class="empty">서버에 보낼 작업이 없습니다.</p>
    {/if}
  {/if}
  {#if error}<p class="err">{error}</p>{/if}

  {#snippet footer()}
    <button class="btn" onclick={onclose}>닫기</button>
  {/snippet}
</Modal>

<style>
  section + section {
    margin-top: 14px;
  }

  h3 {
    margin: 0 0 6px;
    font-size: 13px;
  }

  .now,
  .ops {
    margin: 0;
    padding: 0;
    list-style: none;
    font-size: 12.5px;
  }

  .now li {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 2px 0;
  }

  .now .what {
    width: 140px;
    flex: none;
    color: var(--fg-muted);
  }

  .dot {
    flex: none;
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: #8a8a8a;
  }

  .dot.busy {
    background: #0f6cbd;
  }

  .ops {
    max-height: 240px;
    overflow-y: auto;
    border: 1px solid var(--border-soft);
    border-radius: 4px;
  }

  .ops li {
    padding: 4px 8px;
    border-bottom: 1px solid var(--border-soft);
  }

  .line,
  .sub {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
  }

  .ops .what {
    flex: none;
    font-weight: 600;
  }

  .subj {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }

  .when,
  .sub {
    flex: none;
    color: var(--fg-muted);
    font-size: 11.5px;
  }

  .sub .err {
    min-width: 0;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }

  .kinds,
  .more,
  .empty {
    margin: 0 0 6px;
    color: var(--fg-muted);
    font-size: 12px;
  }

  .more {
    margin: 4px 0 0;
  }

  .err {
    margin: 4px 0 0;
    color: #c50f1f;
    font-size: 12px;
  }
</style>
