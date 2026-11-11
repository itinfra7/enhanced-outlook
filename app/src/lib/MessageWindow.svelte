<script lang="ts">
  // 메일 한 통을 새 창에서 본다(목록에서 두 번 누르기·오른쪽 클릭 "새 창에서 열기", 웹메일과 같게).
  // 읽기 창과 같은 화면(회신·전달·첨부·외부 이미지)을 쓴다. 창 이름 "message-{번호}".
  // 회신·전달은 웹메일처럼 이 창 안에서 쓰고, 보내거나 닫으면 메일로 돌아온다(0.7.0).
  import { onMount } from 'svelte';
  import { listen } from '@tauri-apps/api/event';
  import { getCurrentWindow } from '@tauri-apps/api/window';
  import { api, errorText } from './api';
  import Compose from './compose/Compose.svelte';
  import { printMessage } from './print';
  import Reader from './Reader.svelte';
  import type { MessagesChanged, MessageView, Status } from './types';

  interface Props {
    messageId: number;
  }

  let { messageId }: Props = $props();
  let msg = $state<MessageView | null>(null);
  let online = $state(false);
  /** 이 창에서 쓰는 회신·전달(작성 중 메일 번호). */
  let composeId = $state<number | null>(null);
  let note = $state('');
  let noteTimer: ReturnType<typeof setTimeout> | undefined;
  const win = getCurrentWindow();

  function toast(text: string) {
    note = text;
    clearTimeout(noteTimer);
    noteTimer = setTimeout(() => (note = ''), 4000);
  }

  async function load(allowRemote = false) {
    try {
      msg = (await api.openMessage(messageId, allowRemote)).msg;
      void win.setTitle(msg.subject || '(제목 없음)').catch(() => {});
    } catch (e) {
      toast(errorText(e));
    }
  }

  async function showImages(how: 'once' | 'sender' | 'domain') {
    if (!msg) return;
    if (how !== 'once') {
      try {
        await api.allowRemote(how, how === 'domain' ? (msg.sender.split('@').pop() ?? '') : msg.sender);
      } catch (e) {
        toast(errorText(e));
      }
    }
    await load(true);
  }

  async function run(f: () => Promise<unknown>) {
    try {
      await f();
    } catch (e) {
      toast(errorText(e));
    }
  }

  function onkeydown(e: KeyboardEvent) {
    if (composeId != null) return; // 작성 화면이 받는다
    if (e.key === 'Escape') void win.close();
    // 인쇄는 메일만(머리글 포함).
    if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'p') {
      e.preventDefault();
      if (msg) printMessage(msg);
      return;
    }
    // 브라우저 새로 고침·찾기를 막는다.
    if (e.key === 'F5' || ((e.ctrlKey || e.metaKey) && ['r', 'f'].includes(e.key.toLowerCase()))) {
      e.preventDefault();
    }
  }

  onMount(() => {
    void load();
    void api.appStatus().then((s) => (online = s.loggedIn), () => {});
    const un = [
      listen<Status>('status', (e) => (online = e.payload.loggedIn)),
      // 원문이 늦게 왔거나 읽음 등이 바뀌면 다시 그린다.
      listen<MessagesChanged>('messages-changed', (e) => {
        if (e.payload.ids.length === 0 || e.payload.ids.includes(messageId)) void load(msg?.remoteAllowed ?? false);
      }),
    ];
    return () => {
      for (const u of un) void u.then((f) => f());
    };
  });
</script>

<svelte:window {onkeydown} />

<div class="wrap">
  {#if composeId != null}
    {#key composeId}
      <Compose draftId={composeId} inline onclosed={() => (composeId = null)} />
    {/key}
  {:else}
  <Reader
    {msg}
    loading={msg === null}
    onloaded={() => {}}
    onshowimages={(how) => void showImages(how)}
    onopenattachment={(i) => void run(() => api.openAttachment(messageId, i))}
    onsaveattachment={(i) =>
      void run(async () => {
        const path = await api.saveAttachment(messageId, i);
        if (path) toast(`저장했습니다: ${path}`);
      })}
    onsaveall={() =>
      void run(async () => {
        const n = await api.saveAllAttachments(messageId);
        if (n != null) toast(`첨부 ${n}개를 저장했습니다`);
      })}
    {online}
    onlinkedchanged={() => void load(msg?.remoteAllowed ?? false)}
    oncompose={(kind) =>
      void run(async () => {
        composeId = await api.composeNew(kind, messageId);
      })}
  />
  {/if}
  {#if note}<div class="toast" role="status">{note}</div>{/if}
</div>

<style>
  .wrap {
    position: relative;
    height: 100%;
    background: var(--bg);
  }

  .toast {
    position: absolute;
    left: 50%;
    bottom: 16px;
    transform: translateX(-50%);
    padding: 8px 14px;
    border-radius: 6px;
    background: #323130;
    color: #fff;
    font-size: 13px;
  }
</style>
