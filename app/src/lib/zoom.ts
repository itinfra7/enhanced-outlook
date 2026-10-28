// 글자 크기(화면 확대, 0.7.0 사용자 요청): 웹 브라우저처럼 Ctrl+휠로 10%씩, Ctrl+0은 100%.
// 범위는 설정과 같은 80~150%이고 바로 저장되며 열린 창 모두에 적용된다(주 창·작성 창·메일 창).
// 메일 본문(iframe) 안에서 굴려도 되도록 iframe 문서에도 같은 처리기를 붙인다.
import { listen } from '@tauri-apps/api/event';
import { api } from './api';

export const ZOOM_MIN = 80;
export const ZOOM_MAX = 150;

let current = 100;
let pending: ReturnType<typeof setTimeout> | undefined;
let badge: HTMLDivElement | null = null;
let badgeTimer: ReturnType<typeof setTimeout> | undefined;

export function clampZoom(z: number): number {
  if (!Number.isFinite(z)) return 100;
  return Math.min(ZOOM_MAX, Math.max(ZOOM_MIN, Math.round(z)));
}

/** 10% 눈금으로 한 칸(95%에서 올리면 100%). */
function step(z: number, up: boolean): number {
  return clampZoom(up ? Math.floor(z / 10) * 10 + 10 : Math.ceil(z / 10) * 10 - 10);
}

function show(z: number) {
  if (!badge) {
    badge = document.createElement('div');
    badge.className = 'zoom-badge';
    badge.setAttribute('role', 'status');
    document.body.appendChild(badge);
  }
  badge.textContent = `${z}%`;
  badge.hidden = false;
  clearTimeout(badgeTimer);
  badgeTimer = setTimeout(() => badge && (badge.hidden = true), 1200);
}

function apply(next: number) {
  if (next === current) return;
  current = next;
  show(next);
  // 굴리는 동안 여러 번 오므로 마지막 값만 저장·적용한다.
  clearTimeout(pending);
  pending = setTimeout(() => void api.setZoom(next).catch(() => {}), 120);
}

function onWheel(e: WheelEvent) {
  if (!e.ctrlKey || e.deltaY === 0) return;
  e.preventDefault();
  apply(step(current, e.deltaY < 0));
}

function onKey(e: KeyboardEvent) {
  if (!(e.ctrlKey || e.metaKey) || e.altKey) return;
  if (e.key === '0') apply(100);
  else if (e.key === '=' || e.key === '+') apply(step(current, true));
  else if (e.key === '-' || e.key === '_') apply(step(current, false));
  else return;
  e.preventDefault();
}

/** 이 창에 Ctrl+휠·Ctrl+0/+/- 처리기를 단다(창마다 한 번). */
export async function installZoom() {
  window.addEventListener('wheel', onWheel, { passive: false });
  window.addEventListener('keydown', onKey, true);
  try {
    current = clampZoom((await api.getSettings()).settings.zoom);
  } catch {
    // 기본값으로
  }
  // 다른 창·설정에서 바꿨다.
  await listen<number>('zoom-changed', (e) => (current = clampZoom(e.payload)));
}

/** 메일 본문 등 iframe 문서에도 같은 처리기를 단다(load 때마다). */
export function zoomInFrame(doc: Document | null) {
  if (!doc) return;
  doc.addEventListener('wheel', onWheel, { passive: false });
  doc.addEventListener('keydown', onKey, true);
}
