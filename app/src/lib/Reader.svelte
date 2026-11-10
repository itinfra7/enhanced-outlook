<script lang="ts">
  // 읽기 창(계획서 9.2). 본문은 샌드박스 iframe(스크립트 불가)의 srcdoc. 링크는 새 창 요청 →
  // 앱이 기본 브라우저로 넘긴다(allow-popups). 같은 출처 권한이 없어 본문은 앱에 접근할 수 없다.
  import { api } from './api';
  import { bytes, fullDate } from './format';
  import { measureAll } from './imgcheck';
  import { highlightDocument, splitHits } from './highlight';
  import type { MessageView } from './types';
  import { zoomInFrame } from './zoom';
  import { printMessage } from './print';

  interface Props {
    msg: MessageView | null;
    loading: boolean;
    onloaded: () => void;
    /** 'once' 이번만, 'sender' 이 보낸 사람 항상, 'domain' 이 도메인 항상 */
    onshowimages: (how: 'once' | 'sender' | 'domain') => void;
    onopenattachment: (index: number) => void;
    onsaveattachment: (index: number) => void;
    onsaveall: () => void;
    /** 로그인함(아니면 오프라인 사용: 회신·전달 단추를 숨긴다) */
    online: boolean;
    /** 검색 결과에서 연 메일이면 본문에서 칠할 검색어 */
    highlight?: string[];
    oncompose: (kind: 'reply' | 'reply_all' | 'forward' | 'copy') => void;
  }

  let {
    msg,
    loading,
    onloaded,
    onshowimages,
    onopenattachment,
    onsaveattachment,
    onsaveall,
    online,
    highlight = [],
    oncompose,
  }: Props = $props();

  const BLANK = '<!doctype html><html><head><meta charset="utf-8"></head><body></body></html>';

  let expanded = $state(false);
  let imgMenu = $state(false);

  $effect(() => {
    void msg?.id;
    expanded = false;
    imgMenu = false;
  });

  let senderDomain = $derived(msg?.sender.includes('@') ? msg.sender.split('@').pop() ?? '' : '');

  function pickImages(how: 'once' | 'sender' | 'domain') {
    imgMenu = false;
    onshowimages(how);
  }

  function count(list: string): number {
    return list ? list.split(';').filter((s) => s.trim()).length : 0;
  }

  let longHeader = $derived(msg ? count(msg.to) + count(msg.cc) + count(msg.bcc) > 6 : false);

  // 받지 못했거나 0폭으로 그려진 그림(서버 이름만)을 앱 로그에 한 번씩 남긴다. iframe load는 그림을 다 받거나
  // 실패한 뒤에 온다. broken: 지금 메일의 그런 그림 수(자체 점검이 data-broken으로 본다, 아직 모르면 -1).
  const reported = new Set<number>();
  let broken = $state(-1);
  // 읽음 표시 등으로 메일 정보만 새로 와도 본문 문자열이 같으면 다시 그리지 않으므로, 본문이 바뀔 때만 되돌린다.
  let doc = $derived(msg?.html ?? BLANK);
  $effect(() => {
    void doc;
    broken = -1;
  });
  function frameLoaded(e: Event) {
    onloaded();
    const id = msg?.id;
    if (id == null) return;
    try {
      const frameDoc = (e.currentTarget as HTMLIFrameElement).contentDocument;
      zoomInFrame(frameDoc);
      if (highlight.length) highlightDocument(frameDoc, highlight);
      const all = measureAll(frameDoc);
      const failed = all.filter((i) => i.state === 'failed').map((i) => i.source);
      const collapsed = all.filter((i) => i.state === 'collapsed').map((i) => i.source);
      broken = failed.length + collapsed.length;
      if (broken > 0 && !reported.has(id)) {
        reported.add(id);
        void api.reportBrokenImages(id, failed, collapsed);
      }
    } catch {
      // 진단일 뿐이다.
    }
  }
</script>

{#snippet marked(text: string)}
  {#if highlight.length}
    {#each splitHits(text, highlight) as p, k (k)}{#if p.hit}<mark>{p.t}</mark>{:else}{p.t}{/if}{/each}
  {:else}{text}{/if}
{/snippet}

<article class="reader" data-broken={broken}>
  {#if msg}
    <header class="head">
      <div class="replybar">
        {#if online}
          <button class="mini" title="회신 (Ctrl+R)" onclick={() => oncompose('reply')}>↩ 회신</button>
          <button class="mini" title="전체 회신 (Ctrl+Shift+R)" onclick={() => oncompose('reply_all')}>↩↩ 전체 회신</button>
          <button class="mini" title="전달 (Ctrl+F)" onclick={() => oncompose('forward')}>↪ 전달</button>
          <button
            class="mini"
            title="이 메일의 제목·본문·첨부로 새 메일 쓰기(받는 사람은 비움)"
            onclick={() => oncompose('copy')}>✎ 새로 작성</button
          >
        {/if}
        <button class="mini" title="인쇄 (Ctrl+P)" onclick={() => msg && printMessage(msg)}>인쇄</button>
      </div>
      <h1 class="subject">{@render marked(msg.subject || '(제목 없음)')}</h1>
      {#if msg.archivedAt}
        <div class="archived">서버에서 삭제됨 · {fullDate(msg.archivedAt)}</div>
      {/if}
      {#if msg.origin}
        <div class="origin" data-origin={msg.origin}>
          {msg.origin === 'html' ? 'HTML 백업에서 복원한 메일' : msg.origin.startsWith('domino') ? 'HCL Domino 메일' : '가져온 메일'}
        </div>
      {/if}
      <div class="meta" class:clamp={longHeader && !expanded}>
        <div><span class="k">보낸 사람</span><span class="v">{@render marked(msg.from)}</span></div>
        <div><span class="k">날짜</span><span class="v">{fullDate(msg.date)}</span></div>
        {#if msg.to}<div><span class="k">받는 사람</span><span class="v">{msg.to}</span></div>{/if}
        {#if msg.cc}<div><span class="k">참조</span><span class="v">{msg.cc}</span></div>{/if}
        {#if msg.bcc}<div><span class="k">숨은 참조</span><span class="v">{msg.bcc}</span></div>{/if}
        {#if msg.replyTo && msg.replyTo !== msg.from}
          <div><span class="k">회신 주소</span><span class="v">{msg.replyTo}</span></div>
        {/if}
      </div>
      {#if longHeader}
        <button class="link" onclick={() => (expanded = !expanded)}>
          {expanded ? '받는 사람 접기' : '받는 사람 모두 보기'}
        </button>
      {/if}
      {#if msg.attachments.length > 0}
        <div class="atts">
          {#each msg.attachments as a (a.index)}
            <span class="att" title="{a.name} ({a.mime})">
              <span class="name">{a.name}</span>
              <span class="size">{bytes(a.size)}</span>
              <button class="mini" onclick={() => onopenattachment(a.index)}>열기</button>
              <button class="mini" onclick={() => onsaveattachment(a.index)}>저장</button>
            </span>
          {/each}
          {#if msg.attachments.length > 1}
            <button class="mini all" onclick={onsaveall}>모두 저장</button>
          {/if}
        </div>
      {/if}
      {#if msg.hasRemoteImages && !msg.remoteAllowed}
        <div class="remote">
          외부 이미지 차단됨
          <span class="split">
            <button class="mini" onclick={() => pickImages('once')}>이미지 표시</button>
            {#if msg.sender}
              <button class="mini caret" aria-label="항상 허용" aria-expanded={imgMenu} onclick={() => (imgMenu = !imgMenu)}
                >▾</button
              >
              {#if imgMenu}
                <span class="menu" role="menu">
                  <button role="menuitem" onclick={() => pickImages('sender')}>{msg.sender} 항상 허용</button>
                  {#if senderDomain}
                    <button role="menuitem" onclick={() => pickImages('domain')}>@{senderDomain} 항상 허용</button>
                  {/if}
                </span>
              {/if}
            {/if}
          </span>
        </div>
      {/if}
    </header>
  {:else}
    <div class="none">{loading ? '여는 중…' : '읽을 메일을 선택하세요'}</div>
  {/if}
  <!-- 메일이 없을 때도 빈 문서로 만들어 둔다: 첫 메일을 열 때 프레임 준비 비용이 들지 않게.
       allow-same-origin: 본문을 앱과 같은 렌더러에서 그려 글꼴 캐시를 같이 쓴다(4단계 측정: 실메일 문서 로드
       평균 52→35ms). 스크립트는 sandbox(allow-scripts 없음)·정화·CSP로 막혀 있어 본문이 앱에 접근할 수 없다.
       경고: allow-scripts를 함께 넣으면 샌드박스가 무의미해진다. 절대 둘을 같이 쓰지 말 것. -->
  <iframe
    class="body"
    class:hidden={!msg}
    title="메일 본문"
    sandbox="allow-same-origin allow-popups allow-popups-to-escape-sandbox"
    referrerpolicy="no-referrer"
    srcdoc={doc}
    onload={frameLoaded}
  ></iframe>
</article>

<style>
  .reader {
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0;
  }

  .head {
    flex: none;
    padding: 14px 18px 10px;
    border-bottom: 1px solid var(--border);
    background: var(--bg);
  }

  .replybar {
    display: flex;
    gap: 6px;
    margin-bottom: 8px;
  }

  .head mark {
    padding: 0;
    background: #ffe066;
    color: inherit;
  }

  .subject {
    margin: 0 0 8px;
    font-size: 18px;
    font-weight: 600;
    line-height: 1.35;
    word-break: break-word;
  }

  .origin {
    margin: 0 0 8px;
    padding: 4px 8px;
    font-size: 12px;
    border-radius: 4px;
    color: #0e5a8a;
    background: #e3f1fb;
  }

  .archived {
    margin: 0 0 8px;
    padding: 4px 8px;
    font-size: 12px;
    border-radius: 4px;
    color: #8a5300;
    background: #fff4ce;
  }

  .meta {
    display: grid;
    gap: 2px;
    font-size: 12.5px;
  }

  .meta.clamp {
    max-height: 4.6em;
    overflow: hidden;
  }

  .meta > div {
    display: flex;
    gap: 8px;
  }

  .k {
    flex: none;
    width: 64px;
    color: var(--fg-muted);
  }

  .v {
    min-width: 0;
    word-break: break-all;
  }

  .link {
    margin-top: 2px;
    padding: 0;
    border: 0;
    background: none;
    color: var(--accent);
    font: inherit;
    font-size: 12px;
    cursor: pointer;
  }

  .atts {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    margin-top: 8px;
  }

  .att {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    max-width: 100%;
    padding: 3px 4px 3px 8px;
    border: 1px solid var(--border);
    border-radius: 4px;
    font-size: 12px;
  }

  .att .name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    max-width: 260px;
  }

  .att .size {
    color: var(--fg-muted);
  }

  .mini {
    padding: 1px 7px;
    border: 1px solid var(--border);
    border-radius: 3px;
    background: var(--bg-pane);
    color: var(--fg);
    font: inherit;
    font-size: 12px;
    cursor: pointer;
  }

  .mini:hover {
    background: var(--bg-hover);
  }

  .remote {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-top: 8px;
    padding: 4px 8px;
    font-size: 12px;
    border-radius: 4px;
    background: var(--bg-bar);
  }

  .split {
    position: relative;
    display: inline-flex;
  }

  .split .mini:first-child {
    border-radius: 3px 0 0 3px;
  }

  .caret {
    margin-left: -1px;
    padding: 1px 5px;
    border-radius: 0 3px 3px 0;
  }

  .menu {
    position: absolute;
    top: calc(100% + 2px);
    left: 0;
    z-index: 5;
    display: flex;
    flex-direction: column;
    min-width: 100%;
    padding: 4px 0;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--bg);
    box-shadow: 0 4px 14px rgb(0 0 0 / 0.15);
  }

  .menu button {
    padding: 5px 12px;
    border: 0;
    background: none;
    color: var(--fg);
    font: inherit;
    font-size: 12px;
    text-align: left;
    white-space: nowrap;
    cursor: pointer;
  }

  .menu button:hover {
    background: var(--bg-hover);
  }

  .body {
    flex: 1;
    width: 100%;
    min-height: 0;
    border: 0;
    background: #fff;
  }

  .body.hidden {
    flex: none;
    height: 0;
    visibility: hidden;
  }

  .none {
    display: flex;
    flex: 1;
    align-items: center;
    justify-content: center;
    color: var(--fg-muted);
  }
</style>
