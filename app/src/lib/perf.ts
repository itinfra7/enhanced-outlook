// --perf-test 시나리오(계획서 4.1-4 목표 수치). 실제 화면 경로(명령 호출 → 그리기)를 그대로 재고
// 결과를 EnhancedOutlook\logs\perf.log에 쓴 뒤 앱을 끝낸다. 메일함은 바꾸지 않는다(읽기만).
import { tick } from 'svelte';
import { api } from './api';
import { bytes } from './format';
import { ROW_H } from './list.svelte';
import type { Bootstrap, FolderView, RowView } from './types';

export interface PerfCtx {
  boot: Bootstrap;
  /** 앱 스크립트 시작 시각(performance.now) */
  scriptStart: number;
  /** 화면 쪽 시작 단계(performance.now) */
  startMarks: [string, number][];
  /** 첫 그림 시각(performance.now)과 그때의 프로세스 시작 후 경과 */
  firstPaint: { at: number; sinceStart: number };
  folders: () => FolderView[];
  selectFolder: (id: number) => Promise<number>;
  /** 메일을 열고 본문 iframe이 다 그려질 때까지. Rust 처리, 명령 왕복, 본문 문서 로드 시간과 캐시 여부. */
  openAt: (index: number) => Promise<{
    ms: number;
    cached: boolean;
    ipc: number;
    load: number;
    inline: number;
    htmlBytes: number;
    /** 본문이 지정한 글꼴 이름(진단용, 내용은 담지 않음) */
    fonts: string;
  } | null>;
  search: (q: string) => Promise<number>;
  total: () => number;
  row: (i: number) => RowView | undefined;
  rowAt: (i: number) => Promise<RowView | undefined>;
  setScrollTop: (y: number) => void;
  maxScroll: () => number;
  /** 지금 화면에 보이는 행 중 아직 데이터가 없는 행 수 */
  blankVisible: () => number;
}

const TARGET = { firstPaint: 300, folder: 50, open: 100, searchP95: 100, searchMax: 1000, fps: 57 };

function frame(): Promise<number> {
  return new Promise((r) => requestAnimationFrame(r));
}

/** 다음 그림이 화면에 나간 직후. */
export function afterPaint(): Promise<void> {
  return new Promise((r) => requestAnimationFrame(() => setTimeout(r, 0)));
}

function stats(xs: number[]) {
  const s = [...xs].sort((a, b) => a - b);
  const at = (q: number) => s[Math.min(s.length - 1, Math.max(0, Math.ceil(q * s.length) - 1))] ?? 0;
  const mean = s.reduce((a, b) => a + b, 0) / Math.max(1, s.length);
  return { n: s.length, mean, p50: at(0.5), p95: at(0.95), max: s[s.length - 1] ?? 0 };
}

const f1 = (x: number) => x.toFixed(1);
const verdict = (ok: boolean) => (ok ? '통과' : '실패');

const WORDS = [
  '보고서', '회의', '일정', '검토', '요청', '결과', '예산', '프로젝트', '계약', '승인',
  '견적', '납품', '고객', '매출', '분석', '계획', '진행', '현황', '공유', '확인',
];
const NAMES = ['김민수', '이지영', '박서연', '최도윤', '정하준', '강지우', '조예린', '윤현우'];
const LATIN = ['minsu', 'jiyoung', 'kim', 'lee', 'park', 'report', 'meeting', 'budget'];

/** 질의 50종: 낱말, 두세 낱말, 이름, 주소 일부, 필드, 구문, 기간. */
function queries(): string[] {
  const out: string[] = [];
  for (let k = 0; k < 50; k++) {
    const w = (n: number) => WORDS[(k * 7 + n * 3) % WORDS.length];
    switch (k % 8) {
      case 0: out.push(w(0)); break;
      case 1: out.push(`${w(0)} ${w(1)}`); break;
      case 2: out.push(NAMES[k % NAMES.length]); break;
      case 3: out.push(LATIN[k % LATIN.length]); break;
      case 4: out.push(`제목:${w(2)}`); break;
      case 5: out.push(`보낸이:${LATIN[k % 3]}`); break;
      case 6: out.push(`"${w(0)} ${w(1)}"`); break;
      default: out.push(`${w(3)} 기간:2025-01-01..2025-06-30`);
    }
  }
  return out;
}

function stamp(d = new Date()): string {
  const p = (n: number) => String(n).padStart(2, '0');
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
}

export async function runPerf(ctx: PerfCtx): Promise<void> {
  const lines: string[] = [];
  const fails: string[] = [];
  // scripts\perf.ps1(성능 회귀 점검)가 읽는 값. 단위 ms·fps.
  const result: Record<string, number> = {};
  const log = (s: string) => {
    lines.push(s);
    console.log(s);
  };
  const folders = ctx.folders();
  const totalMail = folders.reduce((a, f) => a + f.total, 0);
  log(`==== 성능 측정 ${stamp()} ====`);
  log(
    `데이터: ${ctx.boot.root} · 메일 ${totalMail}통(폴더 ${folders.length}개) · 색인 ${ctx.boot.indexed}통 · ` +
      `창 ${window.innerWidth}×${window.innerHeight} · 화면 배율 ${window.devicePixelRatio}`,
  );

  // 1. 첫 화면
  const fp = ctx.firstPaint;
  const webviewReady = fp.sinceStart - fp.at;
  const ok1 = fp.sinceStart <= TARGET.firstPaint;
  result.firstPaint = fp.sinceStart;
  log(
    `[첫 화면] 프로세스 시작→폴더 트리·목록 첫 그림 ${f1(fp.sinceStart)}ms ` +
      `(문서 시작 전 ${f1(webviewReady)}ms + 화면 스크립트 시작 ${f1(ctx.scriptStart)}ms + 첫 그림까지 ${f1(fp.at - ctx.scriptStart)}ms) · ` +
      `기준 ${TARGET.firstPaint}ms → ${verdict(ok1)}`,
  );
  log(
    `    Rust 쪽(프로세스 시작 후): ${ctx.boot.marks.map(([n, ms]) => `${n} ${f1(ms)}ms`).join(' · ')} · ` +
      `화면 문서 시작 ${f1(webviewReady)}ms`,
  );
  const nav = performance.getEntriesByType('navigation')[0] as PerformanceNavigationTiming | undefined;
  const res = (performance.getEntriesByType('resource') as PerformanceResourceTiming[])
    .filter((r) => /\.(js|css)$/.test(r.name))
    .map((r) => `${r.name.split('/').pop()?.replace(/-[\w]+\./, '.')} ${f1(r.startTime)}→${f1(r.responseEnd)}`);
  log(
    `    화면 쪽(문서 시작 후): 문서 응답 ${f1(nav?.responseEnd ?? 0)}ms · ${res.join(' · ')} · ` +
      `스크립트 시작 ${f1(ctx.scriptStart)}ms · ` +
      `${ctx.startMarks.map(([n, t]) => `${n} ${f1(t)}ms`).join(' · ')} (Rust 폴더 트리·건수·첫 페이지 ${f1(ctx.boot.ms)}ms) · ` +
      `첫 그림 ${f1(fp.at)}ms`,
  );
  if (!ok1) fails.push('첫 화면');

  // 2. 폴더 전환: 메일이 많은 폴더 4개를 돌아가며 24회
  const big = [...folders].sort((a, b) => b.total - a.total).slice(0, 4);
  const switchMs: number[] = [];
  const switchRust: number[] = [];
  for (let k = 0; k < 24 && big.length > 1; k++) {
    const f = big[k % big.length];
    const t0 = performance.now();
    switchRust.push(await ctx.selectFolder(f.id));
    await tick();
    await afterPaint();
    switchMs.push(performance.now() - t0);
  }
  if (switchMs.length) {
    const s = stats(switchMs);
    const ok = s.p95 <= TARGET.folder;
    result.folderP95 = s.p95;
    log(
      `[폴더 전환] ${s.n}회(${big.map((f) => `${f.name} ${f.total}통`).join(', ')}) 평균 ${f1(s.mean)}ms · ` +
        `p95 ${f1(s.p95)}ms · 최대 ${f1(s.max)}ms (Rust 평균 ${f1(stats(switchRust).mean)}ms) · 기준 ${TARGET.folder}ms → ${verdict(ok)}`,
    );
    if (!ok) fails.push('폴더 전환');
  }

  // 3. 메일 열기: 가장 큰 폴더에서
  const biggest = big[0];
  if (biggest) {
    await ctx.selectFolder(biggest.id);
    await tick();
    const n = ctx.total();
    // (a) 처음 여는 메일: 목록 곳곳(원문 있는 것), 캐시 없음
    const coldMs: number[] = [];
    const coldRust: number[] = [];
    const coldIpc: number[] = [];
    const coldLoad: number[] = [];
    const coldInfo: string[] = [];
    const sizes: number[] = [];
    for (let k = 0, tries = 0; coldMs.length < 30 && tries < 400 && n > 0; tries++, k++) {
      const i = Math.floor(((k * 7919) % 997) / 997 * Math.min(n, 5000));
      const r = await ctx.rowAt(i);
      if (!r?.hasEml) continue;
      // 사람이 보이는 행을 누르는 상황: 행을 먼저 화면에 보이게 해 두고(재지 않음) 열기만 잰다.
      ctx.setScrollTop(Math.max(0, i * ROW_H - 3 * ROW_H));
      await afterPaint();
      await afterPaint();
      const t0 = performance.now();
      const o = await ctx.openAt(i);
      await afterPaint();
      if (!o || o.cached) continue;
      coldMs.push(performance.now() - t0);
      coldRust.push(o.ms);
      coldIpc.push(o.ipc);
      coldLoad.push(o.load);
      coldInfo.push(`HTML ${bytes(o.htmlBytes)}, 인라인 이미지 ${o.inline}, 글꼴 ${o.fonts || '지정 없음'}`);
      sizes.push(r.size);
    }
    if (coldMs.length) {
      const s = stats(coldMs);
      // 계획서 4.1-4 "메일 열기 100ms(평균 크기)": 평균으로 판정하고, p95는 따로 밝힌다.
      const ok = s.mean <= TARGET.open;
      const load = stats(coldLoad);
      result.openMean = s.mean;
      result.openP95 = s.p95;
      log(
        `[메일 열기] 처음 여는 메일 ${s.n}통 평균 ${f1(s.mean)}ms · p50 ${f1(s.p50)}ms · p95 ${f1(s.p95)}ms · 최대 ${f1(s.max)}ms ` +
          `(평균 크기 ${bytes(stats(sizes).mean)}; Rust 평균 ${f1(stats(coldRust).mean)}ms, 명령 왕복 평균 ${f1(stats(coldIpc).mean)}ms, ` +
          `본문 문서 로드 평균 ${f1(load.mean)}ms·p95 ${f1(load.p95)}ms, 나머지는 레이아웃·그림) · 기준 평균 ${TARGET.open}ms → ${verdict(ok)}` +
          (s.p95 > TARGET.open ? ` (p95는 ${TARGET.open}ms 초과: 아래 큰 HTML 메일)` : ''),
      );
      const slow = coldMs
        .map((ms, k) => ({ ms, k }))
        .sort((a, b) => b.ms - a.ms)
        .slice(0, 5)
        .map(
          ({ ms, k }) =>
            `${k + 1}번째 ${f1(ms)}ms(왕복 ${f1(coldIpc[k])}, 문서 ${f1(coldLoad[k])}, 원문 ${bytes(sizes[k])}, ${coldInfo[k]})`,
        );
      log(`    느린 순: ${slow.join(' · ')}`);
      if (!ok) fails.push('메일 열기');
    } else {
      log('[메일 열기] 원문 있는 메일이 없어 건너뜀');
    }
    // (b) ↓로 다음 메일(선읽기 캐시)
    const seqMs: number[] = [];
    let hits = 0;
    const start = 100;
    for (let i = start; i < start + 30 && i < n; i++) {
      const t0 = performance.now();
      const o = await ctx.openAt(i);
      await afterPaint();
      if (!o) continue;
      seqMs.push(performance.now() - t0);
      if (o.cached) hits++;
      // 사람이 ↓를 누르는 간격(선읽기가 끝날 시간)
      await new Promise((r) => setTimeout(r, 60));
    }
    if (seqMs.length) {
      const s = stats(seqMs);
      log(
        `[메일 열기] ↓로 다음 메일 ${s.n}통 평균 ${f1(s.mean)}ms · p95 ${f1(s.p95)}ms · 최대 ${f1(s.max)}ms ` +
          `(선읽기 캐시 적중 ${hits}/${s.n})`,
      );
    }
  }

  // 4. 검색: 질의 50종, 전체 폴더
  const qs = queries();
  const searchMs: number[] = [];
  const searchRust: number[] = [];
  let firstQuery = 0;
  let hitsTotal = 0;
  for (const [k, q] of qs.entries()) {
    const t0 = performance.now();
    searchRust.push(await ctx.search(q));
    await tick();
    await afterPaint();
    const ms = performance.now() - t0;
    if (k === 0) firstQuery = ms;
    searchMs.push(ms);
    hitsTotal += ctx.total();
  }
  {
    const s = stats(searchMs);
    const ok = s.p95 <= TARGET.searchP95 && s.max <= TARGET.searchMax;
    result.searchP95 = s.p95;
    result.searchMax = s.max;
    log(
      `[검색] 질의 ${s.n}종(전체 폴더) p50 ${f1(s.p50)}ms · p95 ${f1(s.p95)}ms · 최대 ${f1(s.max)}ms · ` +
        `첫 질의 ${f1(firstQuery)}ms (Rust 평균 ${f1(stats(searchRust).mean)}ms, 평균 결과 ${Math.round(hitsTotal / qs.length)}통) · ` +
        `기준 p95 ${TARGET.searchP95}ms·최대 ${TARGET.searchMax}ms → ${verdict(ok)}`,
    );
    if (!ok) fails.push('검색');
  }

  // 5. 스크롤: 가장 큰 폴더
  if (biggest) {
    await ctx.selectFolder(biggest.id);
    await tick();
    await afterPaint();
    const rows = ctx.total();
    // (a) 연속 스크롤: 프레임마다 120px(휠을 빠르게 굴리는 정도)
    const smooth = await scrollRun(ctx, 300, (k) => k * 120);
    const ok = smooth.fps >= TARGET.fps && smooth.janky <= smooth.frames * 0.01;
    result.scrollFps = smooth.fps;
    result.scrollJankyPct = (smooth.janky / smooth.frames) * 100;
    log(
      `[스크롤] ${rows}행 연속 스크롤 ${smooth.frames}프레임 평균 ${f1(smooth.fps)}fps · ` +
        `33ms 넘는 프레임 ${smooth.janky} · 최장 ${f1(smooth.worst)}ms · 빈 행이 보인 프레임 ${smooth.blankFrames} · ` +
        `기준 60fps(평균 ${TARGET.fps}fps 이상, 33ms 초과 1% 이하) → ${verdict(ok)}`,
    );
    if (!ok) fails.push('스크롤');
    // (b) 스크롤 막대 끌기: 프레임마다 임의 위치(매번 새 페이지를 받아야 함)
    const max = ctx.maxScroll();
    const drag = await scrollRun(ctx, 120, (k) => ((k * 7919) % 1000) / 1000 * max);
    log(
      `[스크롤] 스크롤 막대로 임의 위치 이동 ${drag.frames}프레임 평균 ${f1(drag.fps)}fps · ` +
        `33ms 넘는 프레임 ${drag.janky} · 최장 ${f1(drag.worst)}ms · 빈 행이 보인 프레임 ${drag.blankFrames}(받는 중 자리 표시)`,
    );
    ctx.setScrollTop(0);
  }

  log(fails.length ? `판정: 기준 초과 — ${fails.join(', ')}` : '판정: 모든 항목 기준 이내');
  log(`결과 JSON: ${JSON.stringify(result, (_k, v: unknown) => (typeof v === 'number' ? Math.round(v * 10) / 10 : v))}`);
  log('');
  try {
    const path = await api.perfWrite(lines);
    console.log(`기록: ${path}`);
  } finally {
    await api.perfExit();
  }
}

async function scrollRun(ctx: PerfCtx, frames: number, pos: (k: number) => number) {
  const max = ctx.maxScroll();
  let last = await frame();
  const t0 = last;
  const gaps: number[] = [];
  let blankFrames = 0;
  for (let k = 1; k <= frames; k++) {
    ctx.setScrollTop(Math.min(max, pos(k)));
    const now = await frame();
    gaps.push(now - last);
    last = now;
    if (ctx.blankVisible() > 0) blankFrames++;
  }
  const elapsed = last - t0;
  return {
    frames,
    fps: (frames / elapsed) * 1000,
    janky: gaps.filter((g) => g > 33.4).length,
    worst: Math.max(...gaps),
    blankFrames,
  };
}
