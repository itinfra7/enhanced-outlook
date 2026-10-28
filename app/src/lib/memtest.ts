// --mem-test(1.0.3 메모리 누수 점검): 같은 조작(폴더 전환·메일 열기·검색·스크롤·작성 열고 버리기·설정·도구·
// 메일 창)을 여러 번 되풀이하며 회차마다 메모리를 잰다. 처음 몇 회차에 캐시가 찬 뒤에도 회차마다 계속 늘면 누수다.
// 결과는 EnhancedOutlook\logs\memtest.log에 쓰고 앱을 끝낸다. 합성 저장소에서 돌린다(작성은 만들고 바로 버린다).
import { tick } from 'svelte';
import { invoke } from '@tauri-apps/api/core';
import { api } from './api';
import { afterPaint } from './perf';
import type { FolderView, MemoryReport } from './types';

export interface MemCtx {
  folders: () => FolderView[];
  selectFolder: (id: number) => Promise<number>;
  openAt: (index: number) => Promise<unknown>;
  /** 빈 문자열이면 검색을 끝낸다. */
  search: (q: string) => Promise<number>;
  total: () => number;
  openedId: () => number | null;
  setScrollTop: (y: number) => void;
  maxScroll: () => number;
  /** 본문 영역에 작성 화면을 연다(회신은 지금 연 메일로). */
  compose: (kind: 'new' | 'reply') => Promise<void>;
  openSettings: (open: boolean) => void;
  openTools: (open: boolean) => void;
}

const ROUNDS = 12;
/** 이 회차까지는 캐시가 차는 시간으로 보고 기울기 계산에서 뺀다. */
const WARMUP = 3;
const QUERIES = ['보고서', '회의 일정', '김민수', 'report', '제목:예산', '"프로젝트 진행"', 'kim', '계약 기간:2025-01-01..2025-06-30'];

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
const MB = 1024 * 1024;
const mb = (b: number) => (b / MB).toFixed(1);

async function waitFor(f: () => boolean | Promise<boolean>, ms: number): Promise<boolean> {
  for (const end = Date.now() + ms; Date.now() < end; ) {
    if (await f()) return true;
    await sleep(100);
  }
  return false;
}

interface Sample {
  mem: MemoryReport;
  heap: number;
  nodes: number;
}

function jsHeap(): number {
  return (performance as unknown as { memory?: { usedJSHeapSize: number } }).memory?.usedJSHeapSize ?? 0;
}

/** 주 문서와 같은 출처 iframe 문서(본문·인용문)의 요소 수. */
function domNodes(): number {
  let n = document.getElementsByTagName('*').length;
  for (const f of document.querySelectorAll('iframe')) {
    try {
      n += f.contentDocument?.getElementsByTagName('*').length ?? 0;
    } catch {
      // 다른 출처
    }
  }
  return n;
}

/** 조작이 끝난 뒤 잠깐 두고(가능하면 GC를 부르고) 잰다. */
async function sample(): Promise<Sample> {
  await sleep(1500);
  (window as unknown as { gc?: () => void }).gc?.();
  await sleep(500);
  return { mem: await api.perfMemory(), heap: jsHeap(), nodes: domNodes() };
}

/** 최소제곱 기울기(회차당 증가). */
function slope(ys: number[]): number {
  const n = ys.length;
  if (n < 2) return 0;
  const mx = (n - 1) / 2;
  const my = ys.reduce((a, b) => a + b, 0) / n;
  let num = 0;
  let den = 0;
  ys.forEach((y, x) => {
    num += (x - mx) * (y - my);
    den += (x - mx) ** 2;
  });
  return num / den;
}

function line(name: string, s: Sample): string {
  const { main, webview, webviewCount } = s.mem;
  return (
    `[${name}] 본체 전용 ${mb(main.private)}MB(작업 집합 ${mb(main.workingSet)}) · ` +
    `WebView2 ${webviewCount}개 전용 ${mb(webview.private)}MB(작업 집합 ${mb(webview.workingSet)}) · ` +
    `합계 전용 ${mb(main.private + webview.private)}MB · JS 힙 ${mb(s.heap)}MB · 요소 ${s.nodes}`
  );
}

async function switchFolders(ctx: MemCtx, big: FolderView[], times: number): Promise<void> {
  for (let i = 0; i < times; i++) {
    await ctx.selectFolder(big[i % big.length].id);
    await tick();
    await afterPaint();
  }
}

/** 가장 큰 폴더에서 `from`번째부터 `count`통을 차례로 연다(선읽기 포함). */
async function openMails(ctx: MemCtx, big: FolderView[], from: number, count: number): Promise<void> {
  await ctx.selectFolder(big[0].id);
  await tick();
  const n = Math.min(ctx.total(), 3000);
  for (let i = 0; i < count && n > 0; i++) {
    await ctx.openAt((from + i) % n);
    await afterPaint();
  }
}

async function searches(ctx: MemCtx, times: number): Promise<void> {
  for (let i = 0; i < times; i++) {
    await ctx.search(QUERIES[i % QUERIES.length]);
    await tick();
    await afterPaint();
  }
  await ctx.search('');
  await tick();
}

/** 회신(인용문 칸)과 새 메일을 열었다가 "삭제"로 버린다. */
async function composeAndDiscard(ctx: MemCtx): Promise<void> {
  await ctx.openAt(0);
  await afterPaint();
  for (const kind of ['reply', 'new'] as const) {
    await ctx.compose(kind);
    if (!(await waitFor(() => !!document.querySelector('.compose.inline'), 4000))) continue;
    if (kind === 'reply') {
      await waitFor(
        () =>
          !!document.querySelector<HTMLIFrameElement>('.compose.inline iframe[title="인용할 원문"]')?.contentDocument
            ?.body?.isContentEditable,
        4000,
      );
    }
    await sleep(300);
    const orig = window.confirm;
    window.confirm = () => true;
    try {
      [...document.querySelectorAll<HTMLButtonElement>('.compose.inline button.btn')]
        .find((b) => b.textContent?.trim() === '삭제')
        ?.click();
      await waitFor(() => !document.querySelector('.compose.inline'), 4000);
    } finally {
      window.confirm = orig;
    }
  }
}

/** 메일 창(별도 WebView2 창)을 열었다가 닫는다. */
async function messageWindow(ctx: MemCtx): Promise<void> {
  await ctx.openAt(1);
  const id = ctx.openedId();
  if (id === null) return;
  const label = `message-${id}`;
  await api.openMessageWindow(id);
  await waitFor(async () => (await invoke<string[]>('ui_test_window_labels')).includes(label), 5000);
  await sleep(1500);
  await invoke('ui_test_close_window', { label });
  await waitFor(async () => !(await invoke<string[]>('ui_test_window_labels')).includes(label), 5000);
}

async function round(ctx: MemCtx, k: number, big: FolderView[]): Promise<void> {
  await switchFolders(ctx, big, 8);
  // 회차마다 다른 메일 30통
  await openMails(ctx, big, k * 30, 30);
  await searches(ctx, QUERIES.length);
  // 스크롤: 임의 위치로 60번
  const max = ctx.maxScroll();
  for (let i = 0; i < 60; i++) {
    ctx.setScrollTop((((k * 977 + i * 7919) % 1000) / 1000) * max);
    await afterPaint();
  }
  ctx.setScrollTop(0);
  await composeAndDiscard(ctx);
  // 설정·도구 창
  ctx.openSettings(true);
  await tick();
  await sleep(300);
  ctx.openSettings(false);
  ctx.openTools(true);
  await tick();
  await sleep(300);
  ctx.openTools(false);
  await tick();
  await messageWindow(ctx);
}

export async function runMem(ctx: MemCtx): Promise<void> {
  const lines: string[] = [`==== 메모리 점검 ${new Date().toLocaleString('ko-KR')} ====`];
  const log = (s: string) => {
    lines.push(s);
    console.log(s);
  };
  const gcNote = typeof (window as unknown as { gc?: unknown }).gc === 'function' ? 'GC 호출함' : 'GC 호출 없음';
  log(`조작 한 회: 폴더 전환 8 · 메일 열기 30 · 검색 ${QUERIES.length} · 스크롤 60 · 작성 열고 버리기 2 · 설정·도구 · 메일 창 1 (${gcNote})`);
  const samples: Sample[] = [];
  try {
    const start = await sample();
    log(line('시작', start));
    const big = [...ctx.folders()]
      .filter((f) => f.id >= 0)
      .sort((a, b) => b.total - a.total)
      .slice(0, 4);
    for (let k = 0; k < ROUNDS && big.length; k++) {
      await round(ctx, k, big);
      const s = await sample();
      samples.push(s);
      log(line(`${k + 1}회`, s));
    }
    const tail = samples.slice(WARMUP);
    const per = (f: (s: Sample) => number) => slope(tail.map(f));
    const mainSlope = per((s) => s.mem.main.private);
    const webSlope = per((s) => s.mem.webview.private);
    const nodeSlope = per((s) => s.nodes);
    // JS 힙은 GC 때마다 크게 오르내려(GC를 부를 수 없을 때) 앞뒤 절반의 최저값(GC 직후)을 비교한다.
    const half = Math.floor(tail.length / 2);
    const lowest = (xs: Sample[]) => Math.min(...xs.map((s) => s.heap));
    const heapRise = lowest(tail.slice(half)) - lowest(tail.slice(0, half));
    log(
      `[회차당 증가(${WARMUP + 1}~${ROUNDS}회)] 본체 ${mb(mainSlope)}MB · WebView2 ${mb(webSlope)}MB · ` +
        `요소 ${nodeSlope.toFixed(1)} · JS 힙 최저값 앞뒤 차 ${mb(heapRise)}MB`,
    );
    const grows: string[] = [];
    if (mainSlope > 2 * MB) grows.push('본체');
    if (webSlope > 4 * MB) grows.push('WebView2');
    if (heapRise > 2 * MB) grows.push('JS 힙');
    if (nodeSlope > 20) grows.push('요소');
    log(grows.length ? `판정: 계속 늘어남 — ${grows.join(', ')}` : '판정: 되풀이해도 늘지 않음');
    // 조작별로 나눠 같은 조작만 많이 되풀이한 뒤의 증가(어디서 느는지).
    const phases: [string, () => Promise<void>][] = [
      ['메일 창 열고 닫기 8번', async () => {
        for (let i = 0; i < 8; i++) await messageWindow(ctx);
      }],
      // 많이 열수록 계속 느는지(누수) 어느 선에서 멈추는지(캐시). 1.0.3 측정: 300통씩 +38·+7·+9·-2MB(캐시).
      ['메일 열기 300통', () => openMails(ctx, big, ROUNDS * 30, 300)],
      ['메일 열기 300통 더', () => openMails(ctx, big, ROUNDS * 30 + 300, 300)],
      ['검색 80번', () => searches(ctx, 80)],
      ['작성 열고 버리기 8번', async () => {
        for (let i = 0; i < 4; i++) await composeAndDiscard(ctx);
      }],
      ['폴더 전환 80번', () => switchFolders(ctx, big, 80)],
    ];
    let before = await sample();
    for (const [name, run] of phases) {
      await run();
      const after = await sample();
      log(
        `[${name}] 본체 ${mb(after.mem.main.private - before.mem.main.private)}MB · ` +
          `WebView2 ${mb(after.mem.webview.private - before.mem.webview.private)}MB · ` +
          `JS 힙 ${mb(after.heap - before.heap)}MB · 요소 ${after.nodes - before.nodes}`,
      );
      before = after;
    }
    // 최소화한 것처럼(메모리 목표만 낮춤), 알림 영역으로 숨긴 것처럼(창의 X: 그리기도 멈춤).
    for (const [mode, name] of [
      ['low', '최소화'],
      ['hidden', '알림 영역 숨김'],
    ] as const) {
      log(line(`${name} 전`, await sample()));
      try {
        await invoke('perf_background', { mode });
        await sleep(6000);
        log(line(`${name} 6초 뒤`, await sample()));
      } finally {
        await invoke('perf_background', { mode: 'normal' }).catch(() => {});
      }
      await sleep(1500);
      log(line(`${name} 뒤 다시 보임`, await sample()));
    }
  } catch (e) {
    log(`판정: 점검 중 오류 — ${e instanceof Error ? e.message : String(e)}`);
  }
  log('');
  try {
    await api.perfWrite(lines, 'memtest.log');
  } finally {
    await api.perfExit();
  }
}
