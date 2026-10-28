<script lang="ts">
  // 설정(계획서 10장 8단계): 동기화 주기, 외부 이미지 정책. 파일은 EnhancedOutlook\settings.json(포터블 폴더 안).
  // 저장 위치를 바꾸는 설정은 두지 않는다(용량이 필요하면 포터블 폴더째 옮긴다).
  import { onMount } from 'svelte';
  import { api, errorText } from './api';
  import Modal from './Modal.svelte';
  import RemoteAllowDialog from './RemoteAllowDialog.svelte';
  import type { Cleanup, Settings, SettingsView } from './types';
  import { ZOOM_MAX, ZOOM_MIN, clampZoom } from './zoom';

  interface Props {
    onclose: () => void;
    onsaved: (message: string, saved: Settings) => void;
  }

  let { onclose, onsaved }: Props = $props();
  // 기능별 탭(0.7.1 사용자 요청: 설정이 너무 길다).
  type Tab = 'general' | 'view' | 'compose' | 'cleanup' | 'domino' | 'about';
  const TABS: [Tab, string][] = [
    ['general', '일반'],
    ['view', '화면'],
    ['compose', '메일 쓰기'],
    ['cleanup', '자동 정리'],
    ['domino', 'HCL Domino'],
    ['about', '정보'],
  ];
  let tab = $state<Tab>('general');
  let view = $state<SettingsView | null>(null);
  let draft = $state<Settings>({
    outlookUrl: '',
    inboxEverySecs: 10,
    othersEverySecs: 60,
    remoteImages: 'ask',
    cleanup: { mode: 'off', days: 30, percent: 90 },
    zoom: 100,
    domino: {
      enabled: false,
      portal: '',
      cleanup: { mode: 'off', days: 30, percent: 90 },
      unified: false,
    },
    notify: 'arrived',
    sheetPaste: 'ask',
    hiddenDomains: [],
  });
  /** 숨길 도메인 추가 칸 */
  let newDomain = $state('');

  function addDomain() {
    const d = newDomain.trim().replace(/^[@.]+/, '').toLowerCase();
    if (d && !draft.hiddenDomains.includes(d)) draft.hiddenDomains = [...draft.hiddenDomains, d];
    newDomain = '';
  }
  let error = $state('');
  let busy = $state(false);
  let allowOpen = $state(false);
  /** 지금 기준으로 서버에서 지울 받은 편지함(Outlook)·받은 메일(HCL Domino) 수(모르면 null) */
  let target = $state<number | null>(null);
  let bnTarget = $state<number | null>(null);

  onMount(async () => {
    try {
      view = await api.getSettings();
      const s = view.settings;
      draft = {
        ...s,
        cleanup: { ...s.cleanup },
        domino: { ...s.domino, cleanup: { ...s.domino.cleanup } },
        hiddenDomains: [...s.hiddenDomains],
      };
    } catch (e) {
      error = errorText(e);
    }
  });

  // 다시 세는 동안에도 앞 숫자를 그대로 둔다(줄이 사라졌다 나타나면 아래 단추가 움직인다).
  function watchTarget(c: Cleanup, count: (c: Cleanup) => Promise<number | null>, set: (n: number | null) => void) {
    if (!view || c.mode === 'off') {
      set(null);
      return;
    }
    const timer = setTimeout(() => {
      count(c).then(set, () => set(null));
    }, 300);
    return () => clearTimeout(timer);
  }

  $effect(() => watchTarget({ ...draft.cleanup }, api.cleanupPreview, (n) => (target = n)));
  $effect(() =>
    watchTarget(
      draft.domino.enabled ? { ...draft.domino.cleanup } : { mode: 'off', days: 0, percent: 0 },
      api.dominoCleanupPreview,
      (n) => (bnTarget = n),
    ),
  );

  function same(a: Cleanup, b: Cleanup): boolean {
    return a.mode === b.mode && a.days === b.days && a.percent === b.percent;
  }

  let cleanupChanged = $derived(view !== null && !same(draft.cleanup, view.settings.cleanup));
  let bnCleanupChanged = $derived(view !== null && !same(draft.domino.cleanup, view.settings.domino.cleanup));

  function span(secs: number): string {
    return secs < 60 ? `${secs}초` : `${secs / 60}분`;
  }

  let changed = $derived(
    view !== null &&
      (draft.outlookUrl.trim() !== view.settings.outlookUrl ||
        draft.domino.portal.trim() !== view.settings.domino.portal ||
        draft.hiddenDomains.join('\n') !== view.settings.hiddenDomains.join('\n') ||
        draft.inboxEverySecs !== view.settings.inboxEverySecs ||
        draft.othersEverySecs !== view.settings.othersEverySecs ||
        draft.remoteImages !== view.settings.remoteImages ||
        draft.zoom !== view.settings.zoom ||
        draft.domino.enabled !== view.settings.domino.enabled ||
        draft.domino.unified !== view.settings.domino.unified ||
        draft.notify !== view.settings.notify ||
        draft.sheetPaste !== view.settings.sheetPaste ||
        cleanupChanged ||
        bnCleanupChanged),
  );

  function clamp(c: Cleanup) {
    c.days = Math.min(3650, Math.max(1, Math.round(Number(c.days) || 30)));
    c.percent = Math.min(100, Math.max(1, Math.round(Number(c.percent) || 90)));
  }

  async function save() {
    clamp(draft.cleanup);
    clamp(draft.domino.cleanup);
    draft.zoom = clampZoom(draft.zoom);
    // 화면의 숫자는 다시 세는 중일 수 있어 저장할 때 한 번 더 센다.
    const c = draft.cleanup;
    const n = cleanupChanged && c.mode !== 'off' ? await api.cleanupPreview({ ...c }).catch(() => target) : null;
    if (n && !window.confirm(`받은 편지함 ${n}통을 서버에서 삭제합니다.`)) {
      return;
    }
    const b = draft.domino.cleanup;
    const bn =
      draft.domino.enabled && bnCleanupChanged && b.mode !== 'off'
        ? await api.dominoCleanupPreview({ ...b }).catch(() => bnTarget)
        : null;
    if (bn && !window.confirm(`HCL Domino 받은 메일 ${bn}통을 서버에서 삭제합니다.`)) {
      return;
    }
    if (view && view.settings.outlookUrl && draft.outlookUrl.trim() !== view.settings.outlookUrl &&
      !window.confirm('Outlook 웹메일 주소를 바꾸면 다시 로그인합니다.')) {
      return;
    }
    busy = true;
    error = '';
    try {
      const saved = await api.setSettings(draft);
      onsaved(
        saved.othersEverySecs !== draft.othersEverySecs
          ? `저장했습니다(나머지 폴더 ${span(saved.othersEverySecs)}로 맞춤)`
          : '저장했습니다',
        saved,
      );
      onclose();
    } catch (e) {
      error = errorText(e);
    } finally {
      busy = false;
    }
  }
</script>

<!-- 받은 편지함 자동 정리 기준(Outlook·HCL Domino 같은 모양) -->
{#snippet cleanupRows(c: Cleanup, count: number | null, group: string)}
  <!-- 조각 인자에는 bind:group을 쓸 수 없어 checked·onchange로 -->
  <label class="row radio">
    <input type="radio" name={group} checked={c.mode === 'off'} onchange={() => (c.mode = 'off')} />
    끔
  </label>
  <label class="row radio">
    <input type="radio" name={group} checked={c.mode === 'age'} onchange={() => (c.mode = 'age')} />
    <input
      class="num"
      type="number"
      min="1"
      max="3650"
      bind:value={c.days}
      onfocus={() => (c.mode = 'age')}
    />일 지난 메일 서버에서 삭제
  </label>
  <label class="row radio">
    <input type="radio" name={group} checked={c.mode === 'quota'} onchange={() => (c.mode = 'quota')} />
    용량
    <input
      class="num"
      type="number"
      min="1"
      max="100"
      bind:value={c.percent}
      onfocus={() => (c.mode = 'quota')}
    />% 이상이면 오래된 메일부터 서버에서 삭제
  </label>
  {#if c.mode !== 'off'}<p class="info target" class:unknown={count === null}>대상 {count ?? 0}통</p>{/if}
{/snippet}

<Modal title="설정" width={500} {onclose}>
  {#if view}
    <nav class="tabs">
      {#each TABS as [t, label] (t)}
        <button class:on={tab === t} onclick={() => (tab = t)} data-settings-tab={t}>{label}</button>
      {/each}
    </nav>
    <div class="tab-body">
      {#if tab === 'general'}
        <section>
          <h3>Outlook 웹메일 주소</h3>
          <input class="text" bind:value={draft.outlookUrl} placeholder="mail.example.com" spellcheck="false" data-outlook-url />
        </section>

        <section>
          <h3>동기화 주기</h3>
          <label class="row">
            받은 편지함
            <select bind:value={draft.inboxEverySecs}>
              {#each view.inboxChoices as s (s)}<option value={s}>{span(s)}</option>{/each}
            </select>
          </label>
          <label class="row">
            나머지 폴더
            <select bind:value={draft.othersEverySecs}>
              {#each view.othersChoices as s (s)}<option value={s}>{span(s)}</option>{/each}
            </select>
          </label>
        </section>

        <section>
          <h3>새 메일 알림</h3>
          <label class="row">
            알림 내용
            <select bind:value={draft.notify} data-notify>
              <option value="off">끔</option>
              <option value="arrived">새로운 메일 도착</option>
              <option value="sender">보낸 사람</option>
              <option value="senderSubject">보낸 사람 + 제목</option>
            </select>
          </label>
        </section>
      {:else if tab === 'view'}
        <section>
          <h3>글자 크기</h3>
          <label class="row">
            화면 확대
            <span class="zoom">
              <input
                type="number"
                min={ZOOM_MIN}
                max={ZOOM_MAX}
                step="1"
                bind:value={draft.zoom}
                onblur={() => (draft.zoom = clampZoom(draft.zoom))}
                data-zoom
              />%
            </span>
          </label>
        </section>

        <section>
          <h3>외부 이미지</h3>
          <label class="row radio">
            <input type="radio" bind:group={draft.remoteImages} value="ask" />
            차단(허용 목록만 표시)
          </label>
          <label class="row radio">
            <input type="radio" bind:group={draft.remoteImages} value="always" />
            항상 표시
          </label>
          <div class="row">
            허용 목록 {view.remoteSenders}개
            <button class="btn small" onclick={() => (allowOpen = true)}>관리</button>
          </div>
        </section>
      {:else if tab === 'compose'}
        <section>
          <h3>붙여넣기</h3>
          <label class="row">
            스프레드시트 붙여넣기
            <select bind:value={draft.sheetPaste} data-sheet-paste-setting>
              <option value="ask">매번 묻기</option>
              <option value="table">표로</option>
              <option value="image">그림으로</option>
            </select>
          </label>
        </section>

        <section data-hidden-domains>
          <h3>자동 완성에서 숨길 도메인</h3>
          {#each draft.hiddenDomains as d (d)}
            <div class="row domain">
              {d}
              <button class="btn small" onclick={() => (draft.hiddenDomains = draft.hiddenDomains.filter((x) => x !== d))}>삭제</button>
            </div>
          {/each}
          <div class="row domain">
            <input
              class="text"
              bind:value={newDomain}
              placeholder="send.example.com"
              spellcheck="false"
              onkeydown={(e) => {
                if (e.key === 'Enter' && !e.isComposing) {
                  e.preventDefault();
                  addDomain();
                }
              }}
            />
            <button class="btn small" disabled={!newDomain.trim()} onclick={addDomain}>추가</button>
          </div>
        </section>
      {:else if tab === 'cleanup'}
        <section>
          <h3>받은 편지함 자동 정리</h3>
          {@render cleanupRows(draft.cleanup, target, 'outlook-cleanup')}
        </section>
      {:else if tab === 'domino'}
        <section data-domino-settings>
          <h3>HCL Domino 계정 연동</h3>
          <label class="row radio">
            <input type="checkbox" bind:checked={draft.domino.enabled} />
            사용
          </label>
          <label class="row portal">
            포털 주소
            <input class="text" bind:value={draft.domino.portal} placeholder="portal.example.com" spellcheck="false" data-domino-portal />
          </label>
          {#if draft.domino.enabled}
            <label class="row radio" data-unified>
              <input type="checkbox" bind:checked={draft.domino.unified} />
              통합 편지함
            </label>
            <h4>받은 메일 자동 정리</h4>
            {@render cleanupRows(draft.domino.cleanup, bnTarget, 'domino-cleanup')}
          {/if}
        </section>
      {:else}
        <section>
          <h3>정보</h3>
          <p class="info">Enhanced Outlook {view.version}</p>
          <p class="info">만든 사람: itinfra7 (https://github.com/itinfra7) · AGPL-3.0</p>
          <p class="info" title={view.root}>데이터: {view.root}</p>
          <button class="btn small" onclick={() => void api.openLogs().catch((e) => (error = errorText(e)))}>로그 폴더 열기</button>
        </section>
      {/if}
    </div>
  {/if}
  {#if error}<p class="error">{error}</p>{/if}

  {#snippet footer()}
    <button class="btn primary" disabled={busy || !changed} onclick={save}>저장</button>
    <button class="btn" onclick={onclose}>닫기</button>
  {/snippet}
</Modal>

{#if allowOpen}
  <RemoteAllowDialog
    onclose={() => (allowOpen = false)}
    onchange={(n) => {
      if (view) view = { ...view, remoteSenders: n };
    }}
  />
{/if}

<style>
  .tabs {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
    margin-bottom: 12px;
    border-bottom: 1px solid var(--border);
  }

  .tabs button {
    padding: 6px 10px;
    border: 0;
    border-bottom: 2px solid transparent;
    background: none;
    color: var(--fg-muted);
    font: inherit;
    cursor: pointer;
  }

  .tabs button.on {
    color: var(--accent);
    border-bottom-color: var(--accent);
    font-weight: 600;
  }

  /* 탭마다 높이가 달라 창이 출렁이지 않게 */
  .tab-body {
    min-height: 260px;
  }

  section {
    margin: 0 0 14px;
  }

  h3 {
    margin: 0 0 6px;
    font-size: 13px;
  }

  h4 {
    margin: 8px 0 4px;
    font-size: 12.5px;
    font-weight: 600;
  }

  .row {
    display: flex;
    align-items: center;
    gap: 8px;
    margin: 6px 0;
    font-size: 13px;
  }

  .row select {
    margin-left: auto;
    height: 28px;
    padding: 0 6px;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--bg);
    color: var(--fg);
    font: inherit;
  }

  .zoom {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    margin-left: auto;
  }

  .zoom input {
    width: 64px;
    height: 28px;
    padding: 0 6px;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--bg);
    color: var(--fg);
    font: inherit;
    text-align: right;
  }

  input.text {
    width: 100%;
    height: 28px;
    padding: 0 8px;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--bg);
    color: var(--fg);
    font: inherit;
    box-sizing: border-box;
  }

  .row.portal input.text {
    flex: 1;
    width: auto;
  }

  .row.domain {
    margin: 4px 0;
  }

  .row.domain input.text {
    flex: 1;
  }

  .row.radio {
    align-items: flex-start;
  }

  .row.radio input {
    margin-top: 3px;
  }

  .row.radio {
    gap: 6px;
  }

  .row input.num {
    width: 56px;
    height: 24px;
    margin-top: -2px;
    padding: 0 4px;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--bg);
    color: var(--fg);
    font: inherit;
    text-align: right;
  }

  .info {
    margin: 4px 0;
    font-size: 12px;
    overflow-wrap: anywhere;
  }

  /* 용량을 몰라 셀 수 없을 때도 줄 높이는 남긴다 */
  .target.unknown {
    visibility: hidden;
  }

  :global(.btn.small) {
    height: 26px;
    padding: 0 10px;
    font-size: 12px;
  }

  .row :global(.btn.small) {
    margin-left: auto;
  }

  .error {
    color: #c50f1f;
    font-size: 12.5px;
  }
</style>
