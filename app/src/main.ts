import { mount } from 'svelte';
import { getCurrentWindow } from '@tauri-apps/api/window';
import './app.css';
import App from './App.svelte';
import Compose from './lib/compose/Compose.svelte';
import MessageWindow from './lib/MessageWindow.svelte';
import Notify from './lib/Notify.svelte';
import { installZoom } from './lib/zoom';

const target = document.getElementById('app');
if (!target) throw new Error('#app 요소가 없습니다');

// 같은 화면 묶음을 창 이름으로 나눠 쓴다: "main" = 주 창, "compose-{번호}" = 작성 창(계획서 9.3 별도 창),
// "message-{번호}" = 메일 한 통을 새 창에서 보기, "notify-{번호}" = 새 메일 알림 창.
const label = getCurrentWindow().label;
const compose = /^compose-(\d+)$/.exec(label);
const message = /^message-(\d+)$/.exec(label);

// 글자 크기: Ctrl+휠·Ctrl+0(알림 창 빼고).
if (!/^notify-\d+$/.test(label)) void installZoom();

export default compose
  ? mount(Compose, { target, props: { draftId: Number(compose[1]) } })
  : message
    ? mount(MessageWindow, { target, props: { messageId: Number(message[1]) } })
    : /^notify-\d+$/.test(label)
      ? mount(Notify, { target })
      : mount(App, { target });
