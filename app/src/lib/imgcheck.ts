// 본문 그림 상태 재기: 읽기 창 진단(받지 못한 그림·0폭 그림 로그)과 `--image-check` 점검이 같이 쓴다.
// 주소·경로는 다루지 않고 종류와 서버 이름만 돌려준다.
import { invoke } from '@tauri-apps/api/core';
import { api, errorText } from './api';

export type ImgState =
  | 'ok' // 보임
  | 'nosrc' // 주소 없음(원문에 없는 cid, 차단한 외부 그림)
  | 'pixel' // 2px 이하 그림(추적 픽셀·간격용)
  | 'hidden' // 메일 CSS가 숨김(display:none 등)
  | 'failed' // 받지 못함·그림이 아님
  | 'pending' // 아직 받는 중
  | 'authored' // 메일이 2px 이하로 크기를 정함(width="1" 등)
  | 'collapsed'; // 받았는데 앱의 폭 상한 때문에 0폭·0높이로 그려짐

export interface ImgInfo {
  state: ImgState;
  /** 'inline'(본문 그림), 'data', 외부면 서버 이름 */
  source: string;
}

function source(img: HTMLImageElement): string {
  const src = img.getAttribute('src') ?? '';
  if (/^http:\/\/app-cid\.localhost/i.test(src)) return 'inline';
  if (/^data:/i.test(src)) return 'data';
  try {
    return new URL(img.src).hostname || '?';
  } catch {
    return '?';
  }
}

export function measure(img: HTMLImageElement): ImgInfo {
  const src = img.getAttribute('src') ?? '';
  const s = src ? source(img) : '';
  if (!src) return { state: 'nosrc', source: s };
  if (!img.complete) return { state: 'pending', source: s };
  if (img.naturalWidth === 0) return { state: 'failed', source: s };
  if (img.naturalWidth <= 2 && img.naturalHeight <= 2) return { state: 'pixel', source: s };
  // 그려지지 않는 상자(조상 중 display:none) 또는 숨김.
  const view = img.ownerDocument.defaultView;
  if (img.getClientRects().length === 0 || view?.getComputedStyle(img).visibility === 'hidden') {
    return { state: 'hidden', source: s };
  }
  const r = img.getBoundingClientRect();
  if (r.width < 2 || r.height < 2) {
    // 앱의 폭 상한을 빼고 다시 재 본다: 그래도 작으면 메일이 그렇게 정한 크기(width="1", .0104in 등)다.
    const prev = img.style.getPropertyValue('max-width');
    const prio = img.style.getPropertyPriority('max-width');
    img.style.setProperty('max-width', 'none', 'important');
    const r2 = img.getBoundingClientRect();
    img.style.setProperty('max-width', prev, prio);
    if (!prev) img.style.removeProperty('max-width');
    if (r2.width < 2 || r2.height < 2) return { state: 'authored', source: s };
    return { state: 'collapsed', source: s };
  }
  return { state: 'ok', source: s };
}

export function measureAll(doc: Document | null | undefined): ImgInfo[] {
  return Array.from(doc?.images ?? []).map(measure);
}

interface CheckItem {
  id: number;
  date: number;
  folder: string;
  origin: string | null;
}

const LABEL: Record<ImgState, string> = {
  ok: '보임',
  nosrc: '주소 없음(원문에 없는 그림·차단한 외부 그림)',
  pixel: '2px 이하(추적 픽셀·간격용)',
  hidden: '메일 CSS가 숨김',
  failed: '받지 못함',
  pending: '15초 안에 못 받음',
  authored: '메일이 2px 이하로 크기를 정함',
  collapsed: '받았는데 앱 규칙 때문에 0폭',
};

function when(d: number): string {
  const t = new Date(d < 1e11 ? d * 1000 : d);
  const p = (n: number) => String(n).padStart(2, '0');
  return `${t.getFullYear()}-${p(t.getMonth() + 1)}-${p(t.getDate())} ${p(t.getHours())}:${p(t.getMinutes())}`;
}

/** `--image-check`: 그림이 있는 메일을 보이지 않는 읽기 창에 하나씩 그려 그림 상태를 센다(읽기만).
 *  결과는 EnhancedOutlook\logs\imagecheck.log에 메일 번호·날짜·폴더·서버 이름만 남긴다. */
export async function runImageCheck(): Promise<void> {
  const t0 = performance.now();
  const lines: string[] = [`==== 본문 그림 점검 ${when(Date.now())} ====`];
  try {
    const items = await invoke<CheckItem[]>('image_check_list');
    lines.push(`그림 참조가 있는 메일 ${items.length}통 (목록 ${((performance.now() - t0) / 1000).toFixed(0)}초)`);
    const frame = document.createElement('iframe');
    // 읽기 창과 같은 샌드박스(스크립트 없음). 화면 밖, 읽기 창쯤 되는 폭.
    frame.setAttribute('sandbox', 'allow-same-origin');
    frame.setAttribute('referrerpolicy', 'no-referrer');
    frame.style.cssText = 'position:fixed;left:-20000px;top:0;width:700px;height:700px;border:0';
    document.body.appendChild(frame);
    const load = (html: string) =>
      new Promise<boolean>((resolve) => {
        const timer = setTimeout(() => resolve(false), 15000);
        frame.onload = () => {
          clearTimeout(timer);
          resolve(true);
        };
        frame.srcdoc = html;
      });
    const imgs = new Map<ImgState, Map<string, number>>();
    const mails = new Map<ImgState, number>();
    const examples = new Map<ImgState, string[]>();
    /** `상태|출처` → 예(메일 3통까지) */
    const bySource = new Map<string, string[]>();
    let oldCollapsedMails = 0;
    let oldCollapsedImgs = 0;
    const oldExamples: string[] = [];
    let blockedMails = 0;
    let errors = 0;
    for (const [k, it] of items.entries()) {
      let msg;
      try {
        msg = (await api.openMessage(it.id, false)).msg;
      } catch {
        errors++;
        continue;
      }
      if (msg.hasRemoteImages && !msg.remoteAllowed) blockedMails++;
      await load(`${msg.html}<!--${it.id}-->`);
      await new Promise((r) => requestAnimationFrame(() => r(null)));
      const doc = frame.contentDocument;
      const all = measureAll(doc);
      const tag = `#${it.id} ${when(it.date)} ${it.folder}${it.origin ? `(${it.origin})` : ''}`;
      const seen = new Set<ImgState>();
      for (const i of all) {
        const m = imgs.get(i.state) ?? new Map<string, number>();
        m.set(i.source, (m.get(i.source) ?? 0) + 1);
        imgs.set(i.state, m);
        const key = `${i.state}|${i.source}`;
        const ex = bySource.get(key) ?? [];
        if (ex.length < 3 && !ex.includes(tag)) ex.push(tag);
        bySource.set(key, ex);
        if (!seen.has(i.state)) {
          seen.add(i.state);
          mails.set(i.state, (mails.get(i.state) ?? 0) + 1);
          const ex = examples.get(i.state) ?? [];
          if (ex.length < 8) ex.push(tag);
          examples.set(i.state, ex);
        }
      }
      // 예전 규칙(max-width:100%)이었다면 0폭이 됐을 그림.
      if (doc && all.some((i) => i.state === 'ok')) {
        const before = Array.from(doc.images).map((img) => measure(img).state);
        const style = doc.createElement('style');
        style.textContent = 'img{max-width:100% !important}';
        doc.head?.appendChild(style);
        const after = Array.from(doc.images).map((img) => measure(img).state);
        style.remove();
        const n = after.filter((s, j) => s === 'collapsed' && before[j] === 'ok').length;
        if (n > 0) {
          oldCollapsedMails++;
          oldCollapsedImgs += n;
          if (oldExamples.length < 8) oldExamples.push(tag);
        }
      }
      if (k % 500 === 499) console.log(`그림 점검 ${k + 1}/${items.length}`);
    }
    frame.remove();
    lines.push(`걸린 시간 ${((performance.now() - t0) / 1000).toFixed(0)}초, 열지 못한 메일 ${errors}`);
    lines.push(`외부 그림을 막아 둔 메일(허용 목록 밖) ${blockedMails}통`);
    for (const s of Object.keys(LABEL) as ImgState[]) {
      const m = imgs.get(s);
      if (!m) continue;
      const total = [...m.values()].reduce((a, b) => a + b, 0);
      const by = [...m.entries()]
        .sort((a, b) => b[1] - a[1])
        .slice(0, 10)
        .map(([h, n]) => `${h || '-'} ${n}`)
        .join(', ');
      lines.push(`[${LABEL[s]}] 그림 ${total}개 · 메일 ${mails.get(s) ?? 0}통 · ${by}`);
      if (s !== 'ok') lines.push(`    예: ${(examples.get(s) ?? []).join(' · ')}`);
      if (s === 'failed' || s === 'pending' || s === 'collapsed') {
        for (const [h] of [...m.entries()].sort((a, b) => b[1] - a[1]).slice(0, 12)) {
          lines.push(`    ${h || '-'}: ${(bySource.get(`${s}|${h}`) ?? []).join(' · ')}`);
        }
      }
    }
    lines.push(
      `[예전 규칙(max-width:100%)이면 0폭] 그림 ${oldCollapsedImgs}개 · 메일 ${oldCollapsedMails}통`,
      `    예: ${oldExamples.join(' · ')}`,
    );
  } catch (e) {
    lines.push(`오류: ${errorText(e)}`);
  }
  lines.push('');
  try {
    await api.perfWrite(lines, 'imagecheck.log');
  } finally {
    await api.perfExit();
  }
}
