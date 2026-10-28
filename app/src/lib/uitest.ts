// --ui-test(합성 저장소에서만): 5단계 조작을 실제 화면 함수로 해 보고 결과를 EnhancedOutlook\logs\uitest.log에 쓴 뒤
// 앱을 끝낸다. 로그인하지 않으므로 서버 작업은 대기열에만 쌓인다(오프라인 조작 = 즉시 로컬 반영 확인).
import { invoke } from '@tauri-apps/api/core';
import { emit, listen } from '@tauri-apps/api/event';
import { api, errorText } from './api';
import { measure } from './imgcheck';
import { printDocument } from './print';
import type { FolderView, ImportProgress, RowView, Status } from './types';

export interface UiCtx {
  folders: () => FolderView[];
  reloadFolders: () => Promise<void>;
  selectFolder: (id: number) => Promise<number>;
  showArchived: () => Promise<void>;
  selectOnly: (i: number) => Promise<void>;
  pick: (i: number, m: { ctrl: boolean; shift: boolean }) => Promise<void>;
  selectAll: () => Promise<void>;
  selection: () => number[];
  markRead: (read: boolean) => Promise<void>;
  setFlag: (flag: number) => Promise<void>;
  moveTo: (folderId: number) => Promise<void>;
  copyTo: (folderId: number) => Promise<void>;
  deleteSelected: () => Promise<void> | void;
  purgeSelected: () => void;
  confirmOpen: () => boolean;
  confirmRun: () => Promise<void>;
  rowAt: (i: number) => Promise<RowView | undefined>;
  total: () => number;
  status: () => Status;
  openedId: () => number | null;
  openLogin: () => void;
  /** 설정 창 열기·닫기 */
  openSettings: (open: boolean) => void;
  /** 도구 창을 그 탭으로 열기(null이면 닫기) */
  openTools: (tab: 'dupes' | null) => void;
  /** 새 메일·회신 쓰기(주 창의 단추·단축키와 같음) */
  compose: (kind: 'new' | 'reply') => Promise<void>;
  /** 본문 영역에서 쓰는 메일(작성 중 메일 번호) */
  composeId: () => number | null;
  /** `all`이면 전체 폴더 범위로(주지 않으면 지금 범위 그대로) */
  search: (q: string, all?: boolean) => Promise<number>;
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

function stamp(d = new Date()): string {
  const p = (n: number) => String(n).padStart(2, '0');
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
}

export async function runUiTest(ctx: UiCtx): Promise<void> {
  const lines: string[] = [`==== 화면 자체 점검(5~8단계) ${stamp()} ====`];
  let fails = 0;
  const check = (name: string, ok: boolean, detail = '') => {
    if (!ok) fails++;
    lines.push(`[${ok ? '통과' : '실패'}] ${name}${detail ? ` — ${detail}` : ''}`);
  };
  const step = async (name: string, f: () => Promise<void>) => {
    try {
      await f();
    } catch (e) {
      check(name, false, `예외: ${errorText(e)}`);
    }
  };
  const folder = (name: string) => {
    const f = ctx.folders().find((x) => x.name === name);
    if (!f) throw new Error(`폴더 없음: ${name}`);
    return f;
  };
  const total = async (name: string) => {
    await ctx.reloadFolders();
    return folder(name).total;
  };
  const rows = async (folderId: number, offset: number, n: number) =>
    (await api.listMessages(folderId, offset, n)).rows;
  /** 가져오기를 시작하고 끝날 때까지 기다린다. */
  const runImport = async (paths: string[]) => {
    const finished = new Promise<ImportProgress>((resolve) => {
      const un = listen<ImportProgress>('import-progress', (e) => {
        if (e.payload.finished) {
          void un.then((f) => f());
          resolve(e.payload);
        }
      });
    });
    await api.importStart(paths);
    return finished;
  };

  const pending0 = ctx.status().pendingOps;
  const inbox = folder('받은 편지함');

  await step('열면 읽음', async () => {
    await ctx.selectFolder(inbox.id);
    // 같은 합성 저장소에서 여러 번 돌리면 앞쪽 메일이 다 읽음이 되므로 넓게 찾는다.
    let idx = -1;
    for (let off = 0; off < 5000 && idx < 0; off += 1000) {
      const page = await rows(inbox.id, off, 1000);
      const k = page.findIndex((r) => !r.read);
      if (k >= 0) idx = off + k;
      if (page.length < 1000) break;
    }
    if (idx < 0) throw new Error('안읽은 메일이 없음');
    const id = (await ctx.rowAt(idx))!.id;
    await ctx.selectOnly(idx);
    await sleep(400);
    const r = (await rows(inbox.id, idx, 1))[0];
    check('열면 읽음', r.id === id && r.read, `메일 #${id} 읽음=${r.read}`);
  });

  let picked: number[] = [];
  await step('여러 개 선택', async () => {
    await ctx.selectOnly(0);
    await ctx.pick(1, { ctrl: true, shift: false });
    await ctx.pick(2, { ctrl: true, shift: false });
    const ctrl = ctx.selection().length;
    await ctx.pick(5, { ctrl: false, shift: true });
    picked = ctx.selection();
    // Shift+클릭이 글자 선택으로 번지지 않는다: 목록 글자는 선택 불가, Shift 누른 mousedown은 기본 동작을 막는다.
    const listEl = document.querySelector<HTMLElement>('[role="listbox"][aria-label="메일 목록"]');
    const rowEl = listEl?.querySelector<HTMLElement>('[data-index="5"]');
    const down = new MouseEvent('mousedown', { bubbles: true, cancelable: true, shiftKey: true, button: 0 });
    rowEl?.dispatchEvent(down);
    const noSelect = !!listEl && getComputedStyle(listEl).userSelect === 'none';
    check(
      '여러 개 선택(Shift 구간 선택은 글자 선택 없이)',
      ctrl === 3 && picked.length === 4 && down.defaultPrevented && noSelect,
      `Ctrl 3통 → ${ctrl}, Shift 2~5행 → ${picked.length}, Shift 누름 글자 선택 막음 ${down.defaultPrevented}, 목록 글자 선택 불가 ${noSelect}`,
    );
  });

  await step('안읽음·플래그', async () => {
    await ctx.markRead(false);
    await ctx.setFlag(1);
    await sleep(300);
    const r = await rows(inbox.id, 2, 4);
    check(
      '안읽음·플래그(로컬 즉시)',
      r.every((x) => !x.read && x.flag === 1),
      r.map((x) => `#${x.id} 읽음=${x.read} 플래그=${x.flag}`).join(', '),
    );
  });

  await step('이동', async () => {
    const proj = folder('프로젝트');
    const [inBefore, pBefore] = [await total('받은 편지함'), proj.total];
    await ctx.moveTo(proj.id);
    await sleep(300);
    const [inAfter, pAfter] = [await total('받은 편지함'), await total('프로젝트')];
    const next = ctx.openedId();
    check(
      '이동(목록에서 빠지고 다음 메일 선택)',
      inAfter === inBefore - 4 && pAfter === pBefore + 4 && next !== null && !picked.includes(next),
      `받은 편지함 ${inBefore}→${inAfter}, 프로젝트 ${pBefore}→${pAfter}, 다음 열린 메일 #${next}`,
    );
  });

  let deletedId = -1;
  await step('삭제(지운 편지함으로)', async () => {
    const trash = folder('지운 편지함');
    const tBefore = trash.total;
    await ctx.selectOnly(0);
    deletedId = ctx.selection()[0];
    await ctx.deleteSelected();
    await sleep(300);
    const tAfter = await total('지운 편지함');
    const inTrash = (await rows(trash.id, 0, 50)).some((r) => r.id === deletedId);
    check('삭제(일반 편지함 → 지운 편지함)', tAfter === tBefore + 1 && inTrash, `지운 편지함 ${tBefore}→${tAfter}, #${deletedId}`);
  });

  await step('지운 편지함에서 삭제(서버만, 로컬 보관)', async () => {
    const trash = folder('지운 편지함');
    await ctx.selectFolder(trash.id);
    const list = await rows(trash.id, 0, 50);
    const idx = list.findIndex((r) => r.id === deletedId);
    await ctx.selectOnly(idx);
    const r0 = ctx.deleteSelected();
    if (r0) await r0;
    const asked = ctx.confirmOpen();
    await ctx.confirmRun();
    await sleep(300);
    const r = (await rows(trash.id, 0, 50)).find((x) => x.id === deletedId);
    check('지운 편지함 삭제(확인 창 → 로컬 보관)', asked && !!r && r.archived, `확인 창=${asked}, 보관=${r?.archived}`);
  });

  await step('보관 메일 모아 보기·완전 삭제', async () => {
    await ctx.showArchived();
    const before = ctx.total();
    const ids = await api.listIds(null, null, null);
    const idx = ids.indexOf(deletedId);
    await ctx.selectOnly(idx);
    ctx.purgeSelected();
    const asked = ctx.confirmOpen();
    await ctx.confirmRun();
    await sleep(300);
    const after = (await api.listArchived(0, 1)).total;
    const gone = !(await api.listIds(null, null, null)).includes(deletedId);
    check(
      '보관 메일 보기 → 완전 삭제(이중 확인)',
      idx >= 0 && asked && after === before - 1 && gone,
      `보관 ${before}→${after}, #${deletedId} 사라짐=${gone}`,
    );
  });

  await step('복사', async () => {
    const meeting = folder('회의');
    const before = meeting.total;
    await ctx.selectFolder(inbox.id);
    await ctx.selectOnly(0);
    await ctx.copyTo(meeting.id);
    await sleep(300);
    const after = await total('회의');
    check('복사(사본 생김)', after === before + 1, `회의 ${before}→${after}`);
  });

  await step('전체 선택', async () => {
    const fault = folder('장애');
    await ctx.selectFolder(fault.id);
    await ctx.selectAll();
    check('전체 선택(Ctrl+A)', ctx.selection().length === fault.total, `${ctx.selection().length}/${fault.total}`);
  });

  await step('도구: 중복 메일 찾기(지우지 않음)', async () => {
    const t = performance.now();
    const found = await api.duplicatesFind();
    const ms = Math.round(performance.now() - t);
    // 묶음마다 첫 메일이 가장 이르다. 한 묶음을 모두 지우려 하면 거절한다.
    const earliest = found.groups.every(
      (g) => g.mails.length > 1 && g.mails.every((m) => m.date >= g.mails[0].date),
    );
    const g = found.groups[0]?.mails;
    const refused = g
      ? await api
          .duplicatesPurge([{ ids: g.map((m) => m.id), drop: g.map((m) => m.id) }])
          .then(() => '', errorText)
      : '한 통은 남겨야';
    check(
      '도구: 중복 메일 찾기(묶음마다 가장 이른 메일 남김, 모두 지우기 거절)',
      earliest && refused.includes('한 통은 남겨야'),
      `${found.scanned}통 중 ${found.groups.length}묶음, 원문 없음 ${found.withoutBody}, ${ms}ms, 거절 "${refused}"`,
    );
  });

  /** 조건이 맞을 때까지(최대 ms) 기다린다. */
  const waitFor = async (f: () => boolean, ms: number) => {
    for (let t = 0; t < ms && !f(); t += 100) await sleep(100);
    return f();
  };
  const q = <T extends Element>(s: string) => document.querySelector<T>(s);

  await step('화면: 중복 메일 묶음 고르기(Ctrl·Shift)·제외·저장·제외 취소', async () => {
    // Ctrl·Shift를 보려면 묶음이 셋 이상 있어야 한다: 받은 편지함 메일 몇 통을 다른 폴더로 복사해 중복을 만든다.
    let pre = await api.duplicatesFind();
    if (pre.groups.length < 3) {
      const mails = (await rows(inbox.id, 1, 40)).filter((r) => r.hasEml).slice(0, 3);
      await api.copyMessages(
        mails.map((r) => r.id),
        folder('회의').id,
      );
      await sleep(300);
      pre = await api.duplicatesFind();
    }
    // 앞 점검이 남긴 제외(합성 저장소에 저장돼 있다)를 먼저 푼다.
    if (pre.excluded.length) await api.duplicatesExclude(pre.excluded, false);
    ctx.openTools('dupes');
    await waitFor(() => !!q('[data-dupes-find]'), 5000);
    const findBtn = () => q<HTMLButtonElement>('[data-dupes-find]');
    const find = async () => {
      findBtn()?.click();
      await sleep(300);
      return waitFor(() => !!q('[data-dupes-group]') && !findBtn()?.disabled, 120_000);
    };
    const listed = await find();
    const btn = () => q<HTMLButtonElement>('[data-dupes-exclude]');
    const label = () => btn()?.textContent?.trim() ?? '';
    const purge = () => q('[data-dupes-purge]')?.textContent?.trim() ?? '';
    const groups = () => [...document.querySelectorAll<HTMLElement>('[data-dupes-group]')];
    const click = (i: number, init: MouseEventInit = {}) =>
      groups()[i]?.dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true, ...init }));
    const marked = (cls: string) =>
      groups()
        .map((g, i) => (g.classList.contains(cls) ? i : -1))
        .filter((i) => i >= 0)
        .join(',');
    const n = groups().length;
    const purgeBefore = purge();
    const disabledBefore = !!btn()?.disabled;
    // 하나: 고르기 → 제외(회색) → 제외 취소.
    click(0);
    await sleep(150);
    const one = marked('sel') === '0' && label() === '제외' && !btn()?.disabled;
    btn()?.click();
    await sleep(400);
    const g0 = groups()[0];
    const grey = !!g0 && g0.classList.contains('off') && Number(getComputedStyle(g0).opacity) < 1;
    const excludedLabel = label();
    const purgeExcluded = purge();
    btn()?.click();
    await sleep(400);
    const back = marked('off') === '' && label() === '제외' && purge() === purgeBefore;
    // 여럿: 둘째 → 첫째 → Shift+셋째(0~2) → Ctrl+둘째 빼기(0·2) → 제외 → 다시 찾아도 제외(저장) → 제외 취소 →
    // 다시 찾으면 풀려 있음.
    click(1);
    click(0);
    click(2, { shiftKey: true });
    await sleep(150);
    const range = marked('sel');
    click(1, { ctrlKey: true });
    await sleep(150);
    const picked = marked('sel');
    const multiLabel = label();
    btn()?.click();
    await sleep(400);
    const offTwo = marked('off');
    await find();
    const kept = marked('off');
    click(0, { ctrlKey: true });
    click(2, { ctrlKey: true });
    await sleep(150);
    const undoLabel = label();
    btn()?.click();
    await sleep(400);
    await find();
    const cleared = marked('off');
    ctx.openTools(null);
    check(
      '화면: 중복 메일 묶음 고르기(Ctrl·Shift)·제외(회색)·저장·제외 취소',
      listed && n >= 3 && disabledBefore && one && grey && excludedLabel === '제외 취소' &&
        purgeExcluded !== purgeBefore && back && range === '0,1,2' && picked === '0,2' &&
        multiLabel === '제외 (2묶음)' && offTwo === '0,2' && kept === '0,2' &&
        undoLabel === '제외 취소 (2묶음)' && cleared === '',
      `묶음 ${n}, 고르기 전 단추 꺼짐 ${disabledBefore}, 하나 고름 ${one}, 제외 회색 ${grey}, 단추 "${excludedLabel}", ` +
        `완전 삭제 "${purgeBefore}"→"${purgeExcluded}", 취소 ${back}, Shift 범위 [${range}], Ctrl 빼기 [${picked}] "${multiLabel}", ` +
        `제외 [${offTwo}], 다시 찾아도 [${kept}], "${undoLabel}", 취소 뒤 다시 찾음 [${cleared}]`,
    );
  });

  await step('도구: 이동 → 중지(되돌림)', async () => {
    const report = folder('보고');
    const misc = folder('기타');
    const range = { folderId: report.id, from: '20250101', to: '20251231', all: false };
    const p = await api.toolPreview(range);
    const [rBefore, mBefore] = [report.total, misc.total];
    const start = await api.toolRun(range, 'move', misc.id);
    await sleep(300);
    const [rMid, mMid] = [await total('보고'), await total('기타')];
    const prog = start.batch !== null ? await api.batchProgress(start.batch) : null;
    const undone = start.batch !== null ? await api.cancelBatch(start.batch) : 0;
    await sleep(300);
    const [rAfter, mAfter] = [await total('보고'), await total('기타')];
    check(
      '도구 이동·진행률·중지',
      p.count > 0 &&
        start.queued === p.count &&
        rMid === rBefore - p.count &&
        mMid === mBefore + p.count &&
        prog?.pending === p.count &&
        undone === p.count &&
        rAfter === rBefore &&
        mAfter === mBefore,
      `대상 ${p.count}통, 보고 ${rBefore}→${rMid}→${rAfter}, 기타 ${mBefore}→${mMid}→${mAfter}, 대기 ${prog?.pending}, 되돌림 ${undone}`,
    );
  });

  await step('내보내기', async () => {
    // 최근 메일(원문 있음) 20통 + 원문 없는 오래된 메일 2통.
    const recent = (await rows(inbox.id, 0, 20)).map((r) => r.id);
    const old = (await rows(inbox.id, 90000, 2)).map((r) => r.id);
    const ids = [...recent, ...old];
    const [d1, s1] = await invoke<[number, number]>('ui_test_export', { ids, zip: false });
    const [d2, s2] = await invoke<[number, number]>('ui_test_export', { ids, zip: true });
    check(
      '내보내기(폴더·ZIP, 원문 없는 메일은 건너뜀)',
      d1 === 20 && s1 === 2 && d2 === 20 && s2 === 2,
      `폴더 ${d1}통(건너뜀 ${s1}), ZIP ${d2}통(건너뜀 ${s2}) → EnhancedOutlook\\dev\\uitest-export`,
    );
  });

  await step('작성: 회신 미리 채움·저장·첨부·검사·보낼 편지함', async () => {
    const first = (await rows(inbox.id, 0, 1))[0];
    const draftId = await api.composeNew('reply', first.id);
    const d = await api.composeLoad(draftId);
    const prefilled =
      d.kind === 'reply' && d.to.length === 1 && d.subject.startsWith('RE: ') && d.includeQuote && !!d.quoteDoc;
    // 저장
    const content = {
      to: d.to,
      cc: [],
      bcc: [],
      subject: d.subject,
      bodyHtml: '<p>점검 회신</p>',
      includeQuote: true,
      // 작성 창에서 고친 인용문(0.9.0): 다시 열면 고친 것이 보인다.
      quoteHtml: '<p>점검 고친 원문</p>',
      options: d.options,
    };
    await api.composeSave(draftId, content, false);
    const reloaded = await api.composeLoad(draftId);
    const saved = reloaded.bodyHtml === '<p>점검 회신</p>';
    const quoteKept = reloaded.quoteHtml === content.quoteHtml && !!reloaded.quoteDoc?.includes('점검 고친 원문');
    // 첨부(앞에서 내보낸 파일 하나)
    const exported = await invoke<string[]>('ui_test_export_files');
    const files = await api.composeAddFiles(draftId, exported.slice(0, 1));
    const removed = await api.composeRemoveFile(draftId, files[0].name);
    const again = await api.composeAddFiles(draftId, exported.slice(0, 1));
    // 검사: 받는 사람 없음, 잘못된 주소
    const noTo = await api.composeSend(draftId, { ...content, to: [] }).then(() => '', errorText);
    const badTo = await api
      .composeSend(draftId, { ...content, to: [{ name: '', email: 'not-an-address' }] })
      .then(() => '', errorText);
    const before = ctx.status().pendingOps;
    await api.composeSend(draftId, content);
    await sleep(300);
    const queued = await api.composeLoad(draftId);
    const reopen = await api.composePopout(draftId).then(() => '', errorText);
    // 임시 보관함에 "보내는 중"으로 보인다.
    const drafts = ctx.folders().find((f) => f.wellKnown === 'drafts');
    const row = drafts ? (await rows(drafts.id, 0, 5000)).find((r) => r.draft === 1) : undefined;
    check(
      '작성: 회신 미리 채움·저장·첨부·검사·보낼 편지함(로그인 전이라 대기)',
      prefilled &&
        saved &&
        quoteKept &&
        files.length === 1 &&
        removed.length === 0 &&
        again.length === 1 &&
        noTo.includes('받는 사람') &&
        badTo.includes('주소 형식') &&
        queued.state === 1 &&
        ctx.status().pendingOps === before + 1 &&
        reopen.includes('보내는 중') &&
        row?.subject === d.subject,
      `받는 사람 ${d.to.map((r) => r.email).join(',')}, 제목 "${d.subject.slice(0, 12)}…", 인용 ${!!d.quoteDoc}, 저장 ${saved}, 고친 인용문 ${quoteKept}, 첨부 ${files.length}→${removed.length}→${again.length}, 검사 "${noTo}"/"${badTo}", 상태 ${queued.state}, 다시 열기 "${reopen}", 임시 보관함 "보내는 중" ${!!row}`,
    );
  });

  await step('작성: 새 메일 버리기', async () => {
    const draftId = await api.composeNew('new', null);
    await api.composeDiscard(draftId);
    const gone = await api.composeLoad(draftId).then(
      () => false,
      () => true,
    );
    check('작성: 새 메일 버리기', gone, `#${draftId} 사라짐=${gone}`);
  });

  /** 본문 영역의 작성 화면을 "삭제"로 닫는다(확인 창은 점검에서만 바로 예). */
  const discardInline = async () => {
    const orig = window.confirm;
    window.confirm = () => true;
    try {
      [...document.querySelectorAll<HTMLButtonElement>('.compose.inline button.btn')]
        .find((b) => b.textContent?.trim() === '삭제')
        ?.click();
      await waitFor(() => !q('.compose.inline'), 4000);
    } finally {
      window.confirm = orig;
    }
  };

  await step('화면: 받는 사람 자동 완성(↑↓ 강조·Tab으로 넣기·창 닫힘)·이름 옆 주소', async () => {
    await ctx.compose('new');
    await waitFor(() => !!q('.compose.inline input[aria-label="받는 사람"]'), 4000);
    let query = '';
    for (const c of ['a', 'e', 'i', 'o', 'n', 'k', 's', 'j']) {
      if ((await api.contactsLocal(c)).filter((r) => r.name).length >= 3) {
        query = c;
        break;
      }
    }
    const input = q<HTMLInputElement>('.compose.inline input[aria-label="받는 사람"]');
    if (!input || !query) throw new Error(`받는 사람 칸 ${!!input}, 검색어 "${query}"`);
    input.focus();
    input.value = query;
    input.dispatchEvent(new Event('input', { bubbles: true }));
    const shown = await waitFor(() => document.querySelectorAll('.compose.inline .suggest li').length >= 3, 3000);
    const buttons = () => [...document.querySelectorAll<HTMLElement>('.compose.inline .suggest button')];
    const on = () => buttons().findIndex((b) => b.classList.contains('on'));
    const key = (k: string) =>
      input.dispatchEvent(new KeyboardEvent('keydown', { key: k, code: k, bubbles: true, cancelable: true }));
    const first = on();
    key('ArrowDown');
    await sleep(100);
    const down = on();
    const b = buttons();
    const bg = (i: number) => (b[i] ? getComputedStyle(b[i]).backgroundColor : '');
    const visible = down >= 0 && bg(down) !== bg(down === 0 ? 1 : 0);
    key('ArrowDown');
    await sleep(100);
    key('ArrowUp');
    await sleep(100);
    const up = on();
    const want = b[up]?.querySelector('.e')?.textContent?.trim() ?? '';
    key('Tab');
    await sleep(700);
    const closed = !q('.compose.inline .suggest');
    const chips = [...document.querySelectorAll<HTMLElement>('.compose.inline .chip')];
    const chip = chips[chips.length - 1];
    const cn = chip?.querySelector<HTMLElement>('.cn');
    const ce = chip?.querySelector<HTMLElement>('.ce');
    const added = !!want && ce?.textContent?.trim() === want;
    const smaller = !!cn && !!ce && parseFloat(getComputedStyle(ce).fontSize) < parseFloat(getComputedStyle(cn).fontSize);
    await discardInline();
    check(
      '화면: 받는 사람 자동 완성(↑↓ 강조·Tab으로 넣기·창 닫힘)·이름 옆 작은 주소',
      shown && first === 0 && down === 1 && visible && up === 1 && added && closed && smaller,
      `제안 ${buttons().length || 'x'}("${query}"), 강조 ${first}→↓${down}→↓↑${up}, 색 구분 ${visible}, 넣은 주소 ${added}("${want}"), 창 닫힘 ${closed}, 주소 작게 ${smaller}`,
    );
  });

  // 한글 조합 중에 누른 ↓·Tab(1.0.4 사용자 지적: 이름을 치고 처음 누른 ↓가 두 칸 움직였다). WebView2는 조합 중
  // 키(key "Process")를 보내고, 조합이 끝난 뒤 같은 키를 한 번 더 보낸다. 그 순서를 그대로 흉내 낸다.
  await step('화면: 받는 사람 자동 완성(한글 조합 중 ↓·Tab은 한 번만)', async () => {
    await ctx.compose('new');
    await waitFor(() => !!q('.compose.inline input[aria-label="받는 사람"]'), 4000);
    let query = '';
    for (const c of ['a', 'e', 'i', 'o', 'n', 'k', 's', 'j']) {
      if ((await api.contactsLocal(c)).filter((r) => r.name).length >= 4) {
        query = c;
        break;
      }
    }
    const input = q<HTMLInputElement>('.compose.inline input[aria-label="받는 사람"]');
    if (!input || !query) throw new Error(`받는 사람 칸 ${!!input}, 검색어 "${query}"`);
    const buttons = () => [...document.querySelectorAll<HTMLElement>('.compose.inline .suggest button')];
    const on = () => buttons().findIndex((b) => b.classList.contains('on'));
    const chips = () => document.querySelectorAll('.compose.inline .chip').length;
    const comp = (type: string) => input.dispatchEvent(new CompositionEvent(type, { bubbles: true, data: query }));
    const key = (k: string, composing: boolean) =>
      input.dispatchEvent(
        new KeyboardEvent('keydown', {
          key: composing ? 'Process' : k,
          code: k,
          isComposing: composing,
          bubbles: true,
          cancelable: true,
        }),
      );
    input.focus();
    comp('compositionstart');
    input.value = query;
    input.dispatchEvent(new Event('input', { bubbles: true }));
    const shown = await waitFor(() => buttons().length >= 4, 3000);
    const first = on();
    // 조합 중 ↓ → 조합 끝 → 같은 ↓가 다시 옴: 한 칸.
    key('ArrowDown', true);
    comp('compositionend');
    key('ArrowDown', false);
    await sleep(300);
    const once = on();
    // 다시 보내지 않는 입력기: 조합이 끝나고 잠시 뒤 한 칸, 늦게 온 같은 키는 버린다.
    comp('compositionstart');
    key('ArrowDown', true);
    comp('compositionend');
    await sleep(250);
    const fallback = on();
    key('ArrowDown', false);
    await sleep(100);
    const late = on();
    // 조합 중 Tab → 조합 끝 → 같은 Tab: 한 사람만 넣고 다음 칸으로 넘어가지 않는다(Tab의 기본 동작을 막음.
    // 점검이 보내는 키는 원래 포커스를 옮기지 않아서 막았는지로 본다).
    const before = chips();
    comp('compositionstart');
    key('Tab', true);
    comp('compositionend');
    const tabHeld = !key('Tab', false);
    await sleep(700);
    const added = chips() - before;
    const stayed = tabHeld && input.value === '';
    await discardInline();
    check(
      '화면: 받는 사람 자동 완성(한글 조합 중 ↓·Tab은 한 번만)',
      shown && first === 0 && once === 1 && fallback === 2 && late === 2 && added === 1 && stayed,
      `제안 ${buttons().length || 'x'}("${query}"), 강조 ${first}→조합 중 ↓ ${once}→다시 오지 않는 ↓ ${fallback}→늦게 온 ↓ ${late}, ` +
        `조합 중 Tab으로 넣은 수 ${added}, 다음 칸으로 안 넘어감 ${tabHeld}, 칸 비움 ${input.value === ''}`,
    );
  });

  await step('화면: 회신 원문 고치기(작성 창에서 바로)·저장', async () => {
    await ctx.selectFolder(inbox.id);
    let idx = -1;
    for (let i = 0; i < 50 && idx < 0; i++) if ((await ctx.rowAt(i))?.hasEml) idx = i;
    if (idx < 0) throw new Error('원문 있는 메일 없음');
    await ctx.selectOnly(idx);
    await ctx.compose('reply');
    const frame = () => q<HTMLIFrameElement>('.compose.inline iframe[title="인용할 원문"]');
    const editable = await waitFor(() => !!frame()?.contentDocument?.body?.isContentEditable, 5000);
    const id = ctx.composeId();
    const doc = frame()?.contentDocument;
    if (!doc || id === null) throw new Error('인용문 칸 없음');
    doc.body.insertAdjacentHTML('afterbegin', '<p>점검 원문 고침</p>');
    doc.body.dispatchEvent(new Event('input', { bubbles: true }));
    // 인용문 안에서 Ctrl+S(저장).
    doc.dispatchEvent(new KeyboardEvent('keydown', { key: 's', code: 'KeyS', ctrlKey: true, bubbles: true, cancelable: true }));
    let saved = false;
    for (let i = 0; i < 30 && !saved; i++) {
      await sleep(200);
      saved = !!(await api.composeLoad(id)).quoteHtml?.includes('점검 원문 고침');
    }
    // 원문 칸(1.0.5 사용자 지적: 320px로 고정돼 창 도중에 끝났다): 창 아래까지 채우고, 원문이 길어지면 그만큼 늘어나
    // 칸 안에서 따로 스크롤되지 않고 작성 화면 전체가 스크롤된다.
    const box = q<HTMLElement>('.compose.inline .body');
    const fr = frame();
    const reach = !!box && !!fr && fr.getBoundingClientRect().bottom >= box.getBoundingClientRect().bottom - 2;
    doc.body.insertAdjacentHTML('beforeend', `<div id="long">${'<p>점검 긴 원문</p>'.repeat(80)}</div>`);
    await sleep(400);
    const grown =
      !!box && !!fr && fr.clientHeight >= doc.body.scrollHeight - 2 && box.scrollHeight > box.clientHeight + 100;
    doc.getElementById('long')?.remove();
    await sleep(400);
    const shrunk = !!box && !!fr && fr.getBoundingClientRect().bottom >= box.getBoundingClientRect().bottom - 2 &&
      fr.clientHeight < doc.body.scrollHeight + box.clientHeight;
    check(
      '화면: 회신 원문 칸이 창 아래까지·원문 길이만큼 늘고 줆(작성 화면 하나로 스크롤)',
      reach && grown && shrunk,
      `창 아래까지 ${reach}, 길어지면 늘어남 ${grown}, 줄면 다시 맞춤 ${shrunk}`,
    );
    // 원문의 표(1.0.3): 끌어서 두 칸 고르기 → 오른쪽 클릭 "셀 배경색" → 행 삽입 → 열 삭제.
    doc.body.insertAdjacentHTML(
      'beforeend',
      '<table id="qt" border="1"><tr><td>가</td><td>나</td></tr><tr><td>다</td><td>라</td></tr></table>',
    );
    const table = () => doc.getElementById('qt') as HTMLTableElement | null;
    const td = (r: number, c: number) => table()?.rows[r]?.cells[c] ?? null;
    const mouse = (el: Element | null, type: string, extra: MouseEventInit = {}) =>
      el?.dispatchEvent(new MouseEvent(type, { bubbles: true, cancelable: true, button: 0, buttons: 1, ...extra }));
    mouse(td(0, 0), 'mousedown');
    mouse(td(1, 1), 'mousemove');
    mouse(td(1, 1), 'mouseup', { buttons: 0 });
    const picked = doc.querySelectorAll('.eo-cell-sel').length;
    const menuAt = async (el: Element | null, item: string) => {
      mouse(el, 'contextmenu', { button: 2, buttons: 0, clientX: 5, clientY: 5 });
      await sleep(100);
      [...document.querySelectorAll<HTMLButtonElement>('[data-quote-table-menu] button')]
        .find((b) => b.textContent?.trim() === item)
        ?.click();
      await sleep(100);
    };
    await menuAt(td(0, 0), '셀 배경색');
    const palette = !!q('[data-quote-palette]');
    q<HTMLButtonElement>('[data-quote-palette] .swatch[title="#ffc000"]')?.click();
    await sleep(100);
    const colored = [td(0, 0), td(0, 1), td(1, 0), td(1, 1)].filter((c) =>
      /background-color:\s*#ffc000/i.test(c?.getAttribute('style') ?? ''),
    ).length;
    await menuAt(td(0, 0), '아래에 행 삽입');
    const rows3 = table()?.rows.length === 3;
    await menuAt(td(0, 0), '열 삭제');
    const cols1 = [...(table()?.rows ?? [])].every((r) => r.cells.length === 1);
    doc.dispatchEvent(new KeyboardEvent('keydown', { key: 's', code: 'KeyS', ctrlKey: true, bubbles: true, cancelable: true }));
    let tableSaved = false;
    for (let i = 0; i < 30 && !tableSaved; i++) {
      await sleep(200);
      const h = (await api.composeLoad(id)).quoteHtml ?? '';
      tableSaved = h.includes('id="qt"') && !h.includes('eo-cell-sel') && /background-color:\s*#ffc000/i.test(h);
    }
    await discardInline();
    check('화면: 회신 원문 고치기(작성 창에서 바로)·저장', editable && saved, `고칠 수 있음 ${editable}, 저장 ${saved}`);
    check(
      '화면: 원문 표(끌어서 여러 칸·오른쪽 클릭 셀 배경색·행 삽입·열 삭제·저장)',
      picked === 4 && palette && colored === 4 && rows3 && cols1 && tableSaved,
      `고른 칸 ${picked}, 색 고르기 ${palette}, 칠한 칸 ${colored}, 행 3개 ${rows3}, 열 1개 ${cols1}, 저장 ${tableSaved}`,
    );
  });

  await step('작성: 본문 영역에서 쓰기·임시 보관함에서 다시 열기·별도 창에서 편집', async () => {
    await ctx.compose('new');
    for (let i = 0; i < 40 && !q('.compose.inline'); i++) await sleep(100);
    const id = ctx.composeId();
    const inline = !!q('.compose.inline') && id !== null;
    // 임시 보관함에 행이 생기고, 그 행을 고르면 같은 메일이 본문 영역에 열린다.
    const drafts = ctx.folders().find((f) => f.wellKnown === 'drafts');
    if (!drafts || id === null) throw new Error('임시 보관함 또는 작성 화면 없음');
    await ctx.selectFolder(drafts.id);
    let index = -1;
    for (let i = 0; i < ctx.total() && index < 0; i++) {
      const r = await ctx.rowAt(i);
      if (r?.draft === 0) index = i;
    }
    if (index >= 0) await ctx.selectOnly(index);
    for (let i = 0; i < 40 && ctx.composeId() !== id; i++) await sleep(100);
    const reopened = ctx.composeId() === id;
    // 웹메일 "기타 작업" 메뉴와 같은 항목, 받는 사람 단추(주소록), 보낸 사람 칸.
    q<HTMLButtonElement>('.compose.inline [aria-label="기타 작업"]')?.click();
    await sleep(100);
    const items = [...document.querySelectorAll<HTMLElement>('.compose.inline .more [role^="menuitem"]')].map(
      (b) => b.textContent?.trim() ?? '',
    );
    const wantItems = ['초안 저장', '보낸 사람', '이름 확인', '높음', '보통', '낮음', '일반 텍스트로 전환', '메시지 옵션 표시', '접근성 문제 확인', '내 템플릿'];
    const menuOk = wantItems.every((w) => items.some((x) => x.includes(w)));
    q<HTMLButtonElement>('.compose.inline [aria-label="기타 작업"]')?.click();
    const pickers = document.querySelectorAll('.compose.inline .label.pick').length;
    const fromShown = !!q('.compose.inline [data-from]');
    // 별도 창에서 편집: 창이 뜨고 본문 영역은 닫힌다.
    q<HTMLButtonElement>('.compose.inline [aria-label="별도 창에서 편집"]')?.click();
    let popped = false;
    for (let i = 0; i < 40 && !popped; i++) {
      await sleep(150);
      popped = (await invoke<string[]>('ui_test_window_labels')).includes(`compose-${id}`);
    }
    // 별도 창이 뜬 뒤 본문 영역이 닫히기까지 잠깐 걸린다(2026-10-23 한 번 너무 빨리 읽어 실패).
    let closedInline = false;
    for (let i = 0; i < 30 && !closedInline; i++) {
      closedInline = ctx.composeId() === null && !q('.compose.inline');
      if (!closedInline) await sleep(100);
    }
    await invoke('ui_test_close_window', { label: `compose-${id}` });
    await api.composeDiscard(id);
    await ctx.selectFolder(inbox.id);
    check(
      '작성: 본문 영역에서 쓰기·임시 보관함에서 다시 열기·기타 작업 메뉴·주소록 단추·별도 창에서 편집',
      inline && index >= 0 && reopened && menuOk && pickers >= 2 && fromShown && popped && closedInline,
      `본문 영역 ${inline}, 임시 보관함 행 ${index >= 0}, 다시 열림 ${reopened}, 메뉴 ${menuOk ? '모두' : items.join('/')}, 주소록 단추 ${pickers}, 보낸 사람 ${fromShown}, 별도 창 ${popped}, 본문 영역 닫힘 ${closedInline}`,
    );
  });

  await step('폴더 오른쪽 클릭 메뉴·즐겨찾기·모두 읽음(로컬 폴더)', async () => {
    const q = <T extends Element>(s: string) => document.querySelector<T>(s);
    // 받은 편지함(서버 폴더): 웹메일과 같은 메뉴.
    const btn = q<HTMLElement>(`.tree [data-folder="${inbox.id}"]`);
    btn?.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true, clientX: 40, clientY: 80 }));
    await sleep(100);
    const labels = [...document.querySelectorAll<HTMLButtonElement>('.menu .item')].map((b) => ({
      t: b.textContent?.trim() ?? '',
      off: b.disabled,
    }));
    const want = ['모두 읽은 상태로 표시', '새 하위 폴더 만들기', '이름 바꾸기', '삭제', '폴더 비우기', '이동', '즐겨찾기에 추가', '내보내기'];
    const menuOk = want.every((w) => labels.some((l) => l.t.startsWith(w)));
    const fixedOff = labels.find((l) => l.t === '이름 바꾸기')?.off === true; // 받은 편지함은 이름 바꾸기 없음
    // 즐겨찾기에 추가 → 맨 위에 보인다 → 제거.
    [...document.querySelectorAll<HTMLButtonElement>('.menu .item')].find((b) => b.textContent?.startsWith('즐겨찾기에 추가'))?.click();
    for (let i = 0; i < 30 && !q(`.tree [data-favorite="${inbox.id}"]`); i++) await sleep(100);
    const fav = !!q(`.tree [data-favorite="${inbox.id}"]`);
    await api.folderFavorite(inbox.id, false);
    // 로컬 폴더 만들기·이름 바꾸기·모두 읽음·지우기(서버와 상관없음).
    await api.folderCreate(null, '점검 로컬 폴더');
    await ctx.reloadFolders();
    const local = ctx.folders().find((f) => f.name === '점검 로컬 폴더');
    let renamed = false;
    let removed = false;
    if (local) {
      await api.folderRename(local.id, '점검 로컬 폴더2');
      await ctx.reloadFolders();
      renamed = ctx.folders().some((f) => f.id === local.id && f.name === '점검 로컬 폴더2');
      await api.folderDelete(local.id);
      await ctx.reloadFolders();
      removed = !ctx.folders().some((f) => f.id === local.id);
    }
    // 서버 폴더 이름 바꾸기는 로그인 전이라 막힌다(서버에 바로 요청하는 기능).
    const offline = await api.folderRename(inbox.id + 0, '바꿈').then(() => '', errorText);
    check(
      '폴더 오른쪽 클릭 메뉴·즐겨찾기·로컬 폴더 만들기·이름 바꾸기·지우기',
      menuOk && fixedOff && fav && renamed && removed && offline !== '',
      `메뉴 ${menuOk ? '모두' : labels.map((l) => l.t).join('/')}, 받은 편지함 이름 바꾸기 막힘 ${fixedOff}, 즐겨찾기 ${fav}, 로컬 이름 바꾸기 ${renamed}, 지우기 ${removed}, 서버 폴더 "${offline}"`,
    );
  });

  await step('메일 인쇄 문서', async () => {
    // 인쇄 창은 사람이 닫아야 해서 띄우지 않고, 보낼 문서(머리글 + 본문)와 읽기 창 단추만 본다.
    await ctx.selectOnly(0);
    const id = ctx.openedId();
    if (id === null) throw new Error('연 메일 없음');
    const m = (await api.openMessage(id, false)).msg;
    const doc = printDocument(m);
    const button = [...document.querySelectorAll<HTMLButtonElement>('.reader .replybar button')].some(
      (b) => b.textContent?.trim() === '인쇄',
    );
    check(
      '메일 인쇄 문서(제목·보낸 사람·날짜 머리글 + 본문, 읽기 창 인쇄 단추)',
      button && doc.includes('보낸 사람') && doc.includes('날짜') && doc.indexOf('보낸 사람') < doc.length - m.html.length + 200,
      `단추 ${button}, 문서 ${doc.length}자`,
    );
  });

  await step('작성: 서식 도구', async () => {
    // 작성 창(별도 WebView2 창) 안에서 웹메일과 같은 도구가 모두 있는지, 각 기능이 보낼 HTML에 남는지 본다.
    const draftId = await api.composeNew('new', null);
    await api.composePopout(draftId);
    const label = `compose-${draftId}`;
    const result = new Promise<string[] | null>((resolve) => {
      const timer = setTimeout(() => resolve(null), 15000);
      const un = listen<{ label: string; fails: string[] }>('ui-test-compose-result', (e) => {
        if (e.payload.label !== label) return;
        clearTimeout(timer);
        void un.then((f) => f());
        resolve(e.payload.fails);
      });
    });
    // 창이 뜨고 편집기가 준비될 때까지 몇 번 묻는다.
    let fails: string[] | null = null;
    const ask = setInterval(() => void emit('ui-test-compose', { label }), 700);
    fails = await result;
    clearInterval(ask);
    await api.composeDiscard(draftId);
    await invoke('ui_test_close_window', { label });
    check(
      '작성: 서식 도구(웹메일과 같은 단추, 글꼴·크기·강조·색·목록·들여쓰기·맞춤·링크·첨자·방향·표·그림·실행 취소·서식 제거)',
      fails !== null && fails.length === 0,
      fails === null ? '작성 창 응답 없음' : fails.length ? `실패: ${fails.join(', ')}` : '모두 됨',
    );
  });

  await step('용량 확보(로그인 전)', async () => {
    try {
      await api.spacePreview(80);
      check('용량 확보는 로그인 전에 막힘', false, '오류가 나지 않음');
    } catch (e) {
      check('용량 확보는 로그인 전에 막힘', errorText(e).includes('용량'), errorText(e));
    }
  });

  await step('검색', async () => {
    await ctx.search('보고서');
    check('조작 뒤 검색', ctx.total() > 0, `${ctx.total()}통`);
  });

  await step('가져오기', async () => {
    // 앞에서 내보낸 EML ZIP(20통, 모두 이미 있음) + 유저스크립트 형식 HTML 백업 견본(4통).
    const sample = await invoke<string>('ui_test_sample_backup');
    const exported = sample.replace(/uitest-html-backup_01\.zip$/, 'uitest-export.zip');
    const [inBefore, projBefore] = [await total('받은 편지함'), await total('프로젝트')];
    // 같은 합성 저장소에서 다시 돌리면 견본도 이미 있다.
    const again = ctx.folders().some((f) => f.name === '옛날 편지함');
    const p = await runImport([exported, sample]);
    const r = p.report;
    await sleep(600);
    const [inAfter, projAfter] = [await total('받은 편지함'), await total('프로젝트')];
    const old = ctx.folders().find((f) => f.name === '옛날 편지함');
    const [newMail, dups, created, inAdd, projAdd] = again
      ? [0, 24, '', 0, 0]
      : [4, 20, '옛날 편지함', 2, 1];
    check(
      `가져오기(EML ZIP 중복 건너뜀, HTML 백업은 같은 이름 폴더에 합치고 없는 폴더는 로컬로${again ? ', 다시 실행이라 모두 중복' : ''})`,
      !!r &&
        r.duplicates === dups &&
        r.imported === newMail &&
        r.restoredHtml === newMail &&
        r.skipped === 0 &&
        r.createdFolders.join(',') === created &&
        inAfter === inBefore + inAdd &&
        projAfter === projBefore + projAdd &&
        !!old?.local &&
        old.total === 1,
      `중복 ${r?.duplicates}, 가져옴 ${r?.imported}(복원 ${r?.restoredHtml}), 건너뜀 ${r?.skipped}, 새 폴더 [${r?.createdFolders}], 받은 편지함 ${inBefore}→${inAfter}, 프로젝트 ${projBefore}→${projAfter}, 옛날 편지함 로컬=${old?.local} ${old?.total}통${p.error ? `, 오류 ${p.error}` : ''}`,
    );

    // 검색되고, 목록·읽기 창에 가져온 메일로 표시된다(실제 화면).
    await ctx.search('백업복원견본', true);
    const found = ctx.total();
    let idx = -1;
    for (let i = 0; i < found; i++) {
      if ((await ctx.rowAt(i))?.subject.includes('분기 결산 (최종)')) idx = i;
    }
    const row = idx >= 0 ? await ctx.rowAt(idx) : undefined;
    if (idx >= 0) await ctx.selectOnly(idx);
    await sleep(500);
    const tag = document.querySelector('.row.cur .tag.imported')?.textContent?.trim();
    const banner = document.querySelector('[data-origin="html"]')?.textContent ?? '';
    const opened = row ? (await api.openMessage(row.id, false)).msg : null;
    check(
      '가져온 메일 검색·표시(목록 "가져옴", 읽기 창 "HTML 백업에서 복원됨", 본문 이미지·첨부)',
      found === 4 &&
        row?.origin === 'html' &&
        tag === '가져옴' &&
        banner.includes('HTML 백업에서 복원') &&
        opened?.inlineImages === 1 &&
        opened.attachments.some((a) => a.name === '결산 (최종).xlsx'),
      `검색 ${found}통, 출처=${row?.origin}, 표시 "${tag}", 안내 ${banner.includes('HTML 백업에서 복원')}, 인라인 이미지 ${opened?.inlineImages}, 첨부 [${opened?.attachments.map((a) => a.name)}]`,
    );
  });

  await step('HCL Domino 가져오기', async () => {
    // HCL Domino 백업 형식 견본 폴더(4통: 받은 메일 2, 외부 메일 1, 보낸 메일 1).
    const dir = await invoke<string>('ui_test_sample_domino');
    const again = ctx.folders().some((f) => f.name === 'HCL Domino 메일');
    const p = await runImport([dir]);
    const r = p.report;
    await sleep(600);
    await ctx.reloadFolders();
    const top = ctx.folders().find((f) => f.name === 'HCL Domino 메일' && f.parentId === null);
    const child = (name: string) => ctx.folders().find((f) => f.name === name && f.parentId === top?.id);
    const [inbox2, ext, sent] = [child('받은 메일'), child('외부 메일'), child('보낸 메일')];
    check(
      `HCL Domino 가져오기(HCL Domino 메일 아래 메일함 구조, 기간 폴더는 뺌${again ? ', 다시 실행이라 모두 중복' : ''})`,
      !!r &&
        (again
          ? r.imported === 0 && r.duplicates === 4
          : r.imported === 4 && r.restoredDomino === 4 && r.extraFiles === 1 && r.missingFiles === 0) &&
        r.skipped === 0 &&
        !!top?.local &&
        inbox2?.total === 2 &&
        ext?.total === 1 &&
        sent?.total === 1,
      `가져옴 ${r?.imported}(HCL Domino ${r?.restoredDomino}), 중복 ${r?.duplicates}, 건너뜀 ${r?.skipped}, 남은 파일 첨부 ${r?.extraFiles}, 폴더 [${r?.perFolder.map(([f, n]) => `${f} ${n}`).join(', ')}], 트리 받은 메일 ${inbox2?.total}·외부 메일 ${ext?.total}·보낸 메일 ${sent?.total}${p.error ? `, 오류 ${p.error}` : ''}`,
    );

    await ctx.search('HCL Domino견본', true);
    const found = ctx.total();
    let idx = -1;
    for (let i = 0; i < found; i++) {
      if ((await ctx.rowAt(i))?.subject.includes('분기 결산 (최종)')) idx = i;
    }
    const row = idx >= 0 ? await ctx.rowAt(idx) : undefined;
    if (idx >= 0) await ctx.selectOnly(idx);
    await sleep(500);
    const banner = document.querySelector('[data-origin="domino"]')?.textContent ?? '';
    const opened = row ? (await api.openMessage(row.id, false)).msg : null;
    check(
      'HCL Domino 메일 검색·표시(읽기 창 안내, 본문 그림·첨부, 원래 보낸 사람)',
      found === 4 &&
        row?.origin === 'domino' &&
        banner.includes('HCL Domino 메일') &&
        opened?.inlineImages === 1 &&
        opened.attachments.some((a) => a.name === '결산 (최종).xlsx') &&
        opened.from.includes('minsu.kim@example.com'),
      `검색 ${found}통, 출처=${row?.origin}, 안내 ${banner.includes('HCL Domino 메일')}, 인라인 이미지 ${opened?.inlineImages}, 첨부 [${opened?.attachments.map((a) => a.name)}]`,
    );
  });

  await step('설정', async () => {
    // 외부 이미지가 있는 합성 메일(5의 배수 번째)을 찾아 정책 전후로 열어 본다. 끝나면 원래 설정으로 되돌린다.
    const before = await api.getSettings();
    const list = await rows(inbox.id, 0, 60);
    let remoteId = -1;
    for (const r of list) {
      if (!r.hasEml) continue;
      const m = (await api.openMessage(r.id, false)).msg;
      if (m.hasRemoteImages && !m.remoteAllowed) {
        remoteId = r.id;
        break;
      }
    }
    const saved = await api.setSettings({
      ...before.settings,
      inboxEverySecs: 300,
      othersEverySecs: 60,
      remoteImages: 'always',
    });
    const after = await api.getSettings();
    const shown = remoteId >= 0 ? (await api.openMessage(remoteId, false)).msg.remoteAllowed : false;
    // 읽기 창에 열면 받지 못한 외부 그림(합성 메일의 가짜 서버)을 세어 로그에 남긴다.
    let broken = -1;
    const at = list.findIndex((r) => r.id === remoteId);
    if (at >= 0) {
      await ctx.selectFolder(inbox.id);
      await ctx.selectOnly(at);
      for (let i = 0; i < 100 && broken < 0; i++) {
        await sleep(200);
        broken = Number(document.querySelector('article.reader')?.getAttribute('data-broken') ?? -1);
      }
    }
    check('읽기 창: 받지 못한 외부 그림 진단(로그에 서버 이름만)', broken > 0, `#${remoteId} 받지 못함 ${broken}개`);
    await api.setSettings(before.settings);
    const back = remoteId >= 0 ? (await api.openMessage(remoteId, false)).msg.remoteAllowed : true;
    check(
      '설정 저장·적용(나머지 폴더 주기는 받은 편지함 이상, 외부 이미지 늘 표시 → 되돌림)',
      saved.othersEverySecs === 300 &&
        after.settings.remoteImages === 'always' &&
        after.root.endsWith('EnhancedOutlook') &&
        remoteId >= 0 &&
        shown &&
        !back,
      `저장 ${saved.inboxEverySecs}/${saved.othersEverySecs}초, 외부 이미지 메일 #${remoteId} 늘 표시=${shown}, 되돌린 뒤=${back}, 데이터 위치 ${after.root}`,
    );

    // 허용 목록: 도메인 허용 → 그 도메인 메일은 표시, 빼면 다시 차단. 보낸 사람 허용도 같은 식.
    if (remoteId >= 0) {
      const sender = (await api.openMessage(remoteId, false)).msg.sender;
      const domain = sender.split('@').pop() ?? '';
      const listBefore = await api.remoteAllowlist();
      const added = await api.allowRemote('domain', `@${domain.toUpperCase()}`);
      const byDomain = (await api.openMessage(remoteId, false)).msg.remoteAllowed;
      const listed = (await api.remoteAllowlist()).domains.includes(domain);
      await api.removeRemoteAllow('domain', domain);
      const afterRemove = (await api.openMessage(remoteId, false)).msg.remoteAllowed;
      await api.allowRemote('sender', sender);
      const bySender = (await api.openMessage(remoteId, false)).msg.remoteAllowed;
      await api.removeRemoteAllow('sender', sender);
      const bad = await api.allowRemote('domain', 'not a domain').then(() => '', errorText);
      const listAfter = await api.remoteAllowlist();
      check(
        '외부 이미지 허용 목록(도메인·보낸 사람 추가·삭제, 형식 검사)',
        added === domain &&
          byDomain &&
          listed &&
          !afterRemove &&
          bySender &&
          bad.includes('도메인 형식') &&
          listAfter.senders.length === listBefore.senders.length &&
          listAfter.domains.length === listBefore.domains.length,
        `도메인 @${domain} 허용=${byDomain}·목록=${listed}·뺀 뒤=${afterRemove}, 보낸 사람 허용=${bySender}, 잘못된 값 "${bad}"`,
      );
    }
  });

  await step('설정 화면: 자동 정리 대상 줄·HCL Domino 연동', async () => {
    // 일수를 빠르게 바꿔도 '대상 n통' 줄은 그 자리에 남는다(사라졌다 나타나면 아래 단추가 움직였다, 사용자 지적 2026-10-21).
    // HCL Domino 연동은 켜면 받은 메일 자동 정리가 보이고, 끄면 숨는다. 저장하지 않고 닫는다.
    ctx.openSettings(true);
    const q = <T extends Element>(s: string) => document.querySelector<T>(s);
    // 설정은 기능별 탭(0.7.1): 탭 6개, 자동 정리 탭으로.
    for (let i = 0; i < 50 && !q('[data-settings-tab]'); i++) await sleep(100);
    const tabs = document.querySelectorAll('[data-settings-tab]').length;
    q<HTMLButtonElement>('[data-settings-tab="cleanup"]')?.click();
    await sleep(100);
    const radios = [...document.querySelectorAll<HTMLInputElement>('input[name="outlook-cleanup"]')];
    radios[1]?.click();
    await sleep(500);
    const days = radios[1]?.closest('label')?.querySelector<HTMLInputElement>('input.num');
    const line0 = q<HTMLElement>('section .target');
    const top0 = line0?.getBoundingClientRect().top ?? -1;
    let steady = line0 !== null;
    for (let i = 0; i < 8; i++) {
      days?.stepUp();
      days?.dispatchEvent(new Event('input', { bubbles: true }));
      await sleep(60);
      const line = q<HTMLElement>('section .target');
      if (!line || Math.abs(line.getBoundingClientRect().top - top0) > 0.5) steady = false;
    }
    await sleep(500);
    const counted = q('section .target')?.textContent ?? '';
    q<HTMLButtonElement>('[data-settings-tab="domino"]')?.click();
    await sleep(100);
    const bnBox = q<HTMLInputElement>('[data-domino-settings] input[type="checkbox"]');
    const off0 = bnBox ? !bnBox.checked && !q('input[name="domino-cleanup"]') : false;
    bnBox?.click();
    await sleep(200);
    const shown = document.querySelectorAll('input[name="domino-cleanup"]').length === 3;
    bnBox?.click();
    await sleep(200);
    const hidden = !q('input[name="domino-cleanup"]');
    q<HTMLButtonElement>('[data-settings-tab="compose"]')?.click();
    await sleep(100);
    const paste = !!q('[data-sheet-paste-setting]');
    ctx.openSettings(false);
    check(
      '설정 화면: 탭, 자동 정리 대상 줄이 움직이지 않음, HCL Domino 연동 켜고 끄기, 붙여넣기 설정',
      tabs === 6 && steady && /대상 \d+통/.test(counted) && off0 && shown && hidden && paste,
      `탭 ${tabs}개, 대상 줄 고정 ${steady}("${counted.trim()}"), HCL Domino 처음 끔 ${off0}, 켜면 정리 기준 ${shown}, 끄면 숨김 ${hidden}, 붙여넣기 설정 ${paste}`,
    );
  });

  await step('작업 현황 창(상태 표시줄 누르기)', async () => {
    const q = <T extends Element>(s: string) => document.querySelector<T>(s);
    q<HTMLButtonElement>('[data-activity-open]')?.click();
    for (let i = 0; i < 30 && !q('[data-activity-now]'); i++) await sleep(100);
    const opened = !!q('[data-activity-now]');
    const now = q('[data-activity-now]')?.textContent ?? '';
    const pending = ctx.status().pendingOps;
    for (let i = 0; i < 30 && pending > 0 && !q('[data-activity-outlook]'); i++) await sleep(100);
    const listed = pending === 0 || Number(q('[data-activity-outlook]')?.getAttribute('data-activity-outlook')) === pending;
    const lines = document.querySelectorAll('[data-activity-outlook] .ops li').length;
    q<HTMLElement>('[role="dialog"][aria-label="작업 현황"]')?.dispatchEvent(
      new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }),
    );
    await sleep(100);
    const closed = !q('[data-activity-now]');
    check(
      '작업 현황 창: 지금 하는 일·서버 반영 대기 목록·Esc로 닫기',
      opened && now.includes('Outlook 메일 동기화') && listed && (pending === 0 || lines > 0) && closed,
      `열림 ${opened}, 대기 ${pending}건 목록 ${listed}(${lines}줄), 닫힘 ${closed}`,
    );
  });

  await step('목록 정렬', async () => {
    // 크기 큰 것부터·작은 것부터, 보낸 사람 가나다순. 전체 수는 정렬과 상관없이 같다.
    const big = await api.listMessages(inbox.id, 0, 30, { key: 'size', reverse: false });
    const small = await api.listMessages(inbox.id, 0, 30, { key: 'size', reverse: true });
    const from = await api.listMessages(inbox.id, 0, 30, { key: 'from', reverse: false });
    const plain = await api.listMessages(inbox.id, 0, 30);
    const desc = big.rows.every((r, i) => i === 0 || big.rows[i - 1].size >= r.size);
    const asc = small.rows.every((r, i) => i === 0 || small.rows[i - 1].size <= r.size);
    const names = from.rows.map((r) => r.from);
    check(
      '목록 정렬(크기 큰 것부터·작은 것부터, 보낸 사람, 전체 수 같음)',
      desc && asc && big.rows.length === 30 && from.rows.length === 30 && big.total === plain.total && small.rows[0].size <= big.rows[0].size,
      `큰 것 ${big.rows[0]?.size}… 작은 것 ${small.rows[0]?.size}…, 보낸 사람 ${names.slice(0, 2).join('·')}, 전체 ${big.total}/${plain.total}`,
    );
  });

  await step('검색어 하이라이트', async () => {
    await ctx.search('보고서', true);
    await sleep(300);
    const marks = document.querySelectorAll('[role="listbox"][aria-label="메일 목록"] mark');
    const hit = Array.from(marks).some((m) => (m.textContent ?? '').includes('보고서'));
    await ctx.selectOnly(0);
    let body = 0;
    for (let i = 0; i < 25 && body === 0; i++) {
      await sleep(200);
      body = document.querySelector<HTMLIFrameElement>('article.reader iframe')?.contentDocument?.querySelectorAll('mark').length ?? 0;
      body += document.querySelectorAll('article.reader .head mark').length;
    }
    check('검색어 하이라이트(검색 결과 목록·읽기 창)', marks.length > 0 && hit && body > 0, `목록 ${marks.length}곳, 읽기 창 ${body}곳`);
    await ctx.selectFolder(inbox.id);
  });

  await step('새 창에서 열기', async () => {
    const first = (await rows(inbox.id, 0, 1))[0];
    await api.openMessageWindow(first.id);
    await sleep(1500);
    const label = `message-${first.id}`;
    const opened = (await invoke<string[]>('ui_test_window_labels')).includes(label);
    await invoke('ui_test_close_window', { label });
    check('메일 새 창에서 열기', opened, `${label} 열림=${opened}`);
  });

  await step('자동 정리', async () => {
    // 받은 편지함 자동 정리: 기준에 맞는 메일을 서버에서만 지운다(로컬은 그 자리에 "서버에서 삭제됨"으로 남는다).
    // 합성 저장소는 최근 메일에만 원문이 있고(날짜가 오늘보다 뒤일 수도 있다), 원문 없는 메일은 지우지 않는다.
    // 그래서 정리는 원문 있는 가장 오래된 메일보다 사흘 뒤를 가정해(1일 기준) 그 메일부터 3통만 해 본다
    // (같은 저장소에서 여러 번 돌려도 남게).
    const before = await api.getSettings();
    const total0 = await total('받은 편지함');
    const page = await rows(inbox.id, 0, 5000);
    const withEml = page.filter((r) => r.hasEml && !r.archived);
    const newestEml = withEml[0];
    if (!newestEml) throw new Error('원문 있는 메일이 없음');
    const days = 1;
    const cleanup = { mode: 'age' as const, days, percent: 90 };
    const preview = await api.cleanupPreview(cleanup);
    const saved = await api.setSettings({ ...before.settings, cleanup });
    const off = await api.cleanupPreview({ ...cleanup, mode: 'off' });
    const laterDays = Math.max(3, Math.ceil((newestEml.date - Date.now()) / 86_400_000) + 3);
    const [picked, queued] = await invoke<[number[], number]>('ui_test_cleanup_run', { cleanup, laterDays, max: 3 });
    await sleep(300);
    const total1 = await total('받은 편지함');
    // 지운 메일은 로컬에 그대로 열리고 "서버에서 삭제됨"(보관)으로 표시된다.
    const kept = await Promise.all(picked.map(async (id) => !!(await api.openMessage(id, false)).msg.archivedAt));
    const quotaUnknown = await api.cleanupPreview({ ...cleanup, mode: 'quota' });
    await api.setSettings(before.settings);
    const back = (await api.getSettings()).settings.cleanup.mode;
    check(
      '받은 편지함 자동 정리(서버에서만 삭제, 로컬은 남음, 설정 저장·되돌림)',
      saved.cleanup.mode === 'age' &&
        saved.cleanup.days === days &&
        preview !== null &&
        picked.length === 3 &&
        queued === picked.length &&
        total1 === total0 &&
        kept.every(Boolean) &&
        off === 0 &&
        quotaUnknown === null &&
        back === before.settings.cleanup.mode,
      `${days}일 기준 대상 ${preview}통, 정리 ${picked.length}통·서버 작업 ${queued}건, 받은 편지함 ${total0}→${total1}통, 지운 메일 로컬에 남고 서버에서 삭제됨 ${kept.filter(Boolean).length}/${picked.length}, 용량 기준(용량 모름)=${quotaUnknown}`,
    );
  });

  await step('오프라인 사용', async () => {
    // 로그인하지 않은 점검이라 지금이 오프라인 사용 상태다. 로그인 창의 "오프라인 사용"으로 닫고,
    // 온라인 기능(새 메일·회신·전달)이 숨겨졌는지 본다.
    await ctx.selectFolder(inbox.id);
    await ctx.selectOnly(0);
    await sleep(300);
    const reader = !!document.querySelector('article.reader .subject');
    ctx.openLogin();
    await sleep(200);
    const btn = Array.from(document.querySelectorAll<HTMLButtonElement>('[aria-labelledby="login-title"] button')).find(
      (b) => b.textContent?.trim() === '오프라인 사용',
    );
    btn?.click();
    await sleep(200);
    const closed = !document.querySelector('[aria-labelledby="login-title"]');
    const newMail = Array.from(document.querySelectorAll('.topbar button')).some((b) => b.textContent?.includes('새 메일'));
    // 회신 줄에는 인쇄 단추가 늘 있다(0.7.0). 회신·전달 단추만 숨는지 본다.
    const reply = [...document.querySelectorAll<HTMLButtonElement>('.replybar button')].some((b) =>
      /회신|전달/.test(b.textContent ?? ''),
    );
    check(
      '오프라인 사용(로그인 창 단추로 닫힘, 새 메일·회신·전달 숨김, 읽기는 됨)',
      !!btn && closed && !newMail && !reply && reader,
      `단추 ${!!btn}, 닫힘 ${closed}, 새 메일 ${newMail}, 회신 ${reply}, 읽기 창 ${reader}`,
    );
  });

  await step('표 칸 안 그림', async () => {
    // Jira 알림 바닥글처럼 옆 칸이 width="100%"인 표의 로고. 앱 본문 문서(CSS 포함)에 넣어 그려 본다.
    const list = await rows(inbox.id, 0, 30);
    const withEml = list.find((r) => r.hasEml);
    if (!withEml) throw new Error('원문 있는 메일이 없음');
    const base = (await api.openMessage(withEml.id, false)).msg.html;
    const canvas = document.createElement('canvas');
    canvas.width = 191;
    canvas.height = 24;
    const g = canvas.getContext('2d');
    if (g) {
      g.fillStyle = '#0052cc';
      g.fillRect(0, 0, 191, 24);
    }
    const footer =
      '<table width="100%"><tbody><tr><td width="100%" style="font-size:12px">이 메일은 자동으로 보냈습니다.</td>' +
      '<td valign="top" style="padding-left:20px"><table><tbody><tr><td style="padding-top:3px">' +
      `<img src="${canvas.toDataURL('image/png')}" width="191" height="24"></td></tr></tbody></table></td></tr></tbody></table>`;
    const frame = document.createElement('iframe');
    frame.setAttribute('sandbox', 'allow-same-origin');
    frame.style.cssText = 'position:fixed;left:-20000px;top:0;width:700px;height:400px;border:0';
    document.body.appendChild(frame);
    await new Promise<void>((resolve) => {
      frame.onload = () => resolve();
      frame.srcdoc = base.replace(/<body>[\s\S]*<\/body>/, `<body>${footer}</body>`);
    });
    const doc = frame.contentDocument!;
    const img = doc.images[0];
    const now = measure(img);
    const width = Math.round(img.getBoundingClientRect().width);
    const style = doc.createElement('style');
    style.textContent = 'img{max-width:100% !important}';
    doc.head.appendChild(style);
    const old = measure(img).state;
    frame.remove();
    check('표 칸 안 그림이 0폭이 되지 않음(Jira 바닥글 로고)', now.state === 'ok' && width >= 150, `폭 ${width}px, 예전 규칙이면 ${old}`);
  });

  await step('통합 편지함·Outlook 메일 묶음', async () => {
    // 견본 HCL Domino 백업 폴더를 웹메일 메일함으로 짝짓고 통합 편지함을 켠다(끝나면 설정 되돌림).
    const before = await api.getSettings();
    await invoke('ui_test_domino_boxes');
    await api.setSettings({
      ...before.settings,
      // 켜려면 포털 주소가 있어야 한다(로그인하지 않으니 예시 주소).
      domino: { ...before.settings.domino, enabled: true, unified: true, portal: 'portal.example.com' },
    });
    await ctx.reloadFolders();
    const fs = ctx.folders();
    const top = fs.find((f) => f.name === 'HCL Domino 메일' && f.parentId === null);
    const child = (name: string) => fs.find((f) => f.name === name && f.parentId === top?.id);
    const outlookInbox = fs.find((f) => f.wellKnown === 'inbox');
    const expect = (outlookInbox?.total ?? 0) + (child('받은 메일')?.total ?? 0) + (child('외부 메일')?.total ?? 0);
    const order = fs[0]?.id === -1 && fs[1]?.id === -2 && outlookInbox?.parentId === -2 && outlookInbox.depth === 1;
    const bnAfter = fs.indexOf(top!) > fs.indexOf(outlookInbox!);
    await ctx.selectFolder(-1);
    const shown = ctx.total();
    const page = await api.listMessages(-1, 0, 200);
    const newestFirst = page.rows.every((r, i) => i === 0 || page.rows[i - 1].date >= r.date);
    const fromBn = page.total > 0 && (await api.listIds(-1, null, null)).length === page.total;
    await api.setSettings(before.settings);
    await ctx.reloadFolders();
    const back = !ctx.folders().some((f) => f.id < 0);
    await ctx.selectFolder(inbox.id);
    check(
      '통합 편지함(맨 위, Outlook 받은 편지함 + HCL Domino 받은 메일·외부 메일, 최신순)·Outlook 메일 묶음·끄면 원래대로',
      order && bnAfter && shown === expect && page.total === expect && newestFirst && fromBn && back,
      `순서 ${order}, HCL Domino는 Outlook 아래 ${bnAfter}, 통합 ${shown}/${expect}통, 최신순 ${newestFirst}, 전체 선택 ${fromBn}, 끄면 사라짐 ${back}`,
    );
  });

  await step('같은 계정 안에서만 옮김', async () => {
    const first = (await rows(inbox.id, 0, 1))[0];
    const bnInbox = ctx.folders().find((f) => f.name === '받은 메일' && f.depth === 1);
    const moved = bnInbox
      ? await api.moveMessages([first.id], bnInbox.id).then(() => '옮겨짐', errorText)
      : '폴더 없음';
    const copied = bnInbox
      ? await api.copyMessages([first.id], bnInbox.id).then(() => '복사됨', errorText)
      : '폴더 없음';
    check(
      'Outlook 메일을 HCL Domino 메일함으로 옮기기·복사 막힘',
      moved.includes('같은 계정 안에서만') && copied.includes('같은 계정 안에서만'),
      `이동 "${moved}", 복사 "${copied}"`,
    );
  });

  await step('HCL Domino 메일 회신: 보내는 계정 안내', async () => {
    await ctx.search('HCL Domino견본', true);
    const row = await ctx.rowAt(0);
    if (!row) throw new Error('HCL Domino 견본 메일 없음');
    const draftId = await api.composeNew('reply', row.id);
    const d = await api.composeLoad(draftId);
    await api.composeDiscard(draftId);
    await ctx.selectFolder(inbox.id);
    check('HCL Domino 메일 회신에 보내는 계정 안내 줄', !!d.senderNote?.startsWith('보내는 계정은 '), `"${d.senderNote}"`);
  });

  await step('새 메일 알림 창', async () => {
    const id = await invoke<number>('ui_test_notify');
    let label = '';
    for (let i = 0; i < 40 && !label; i++) {
      await sleep(150);
      label = (await invoke<string[]>('ui_test_window_labels')).find((l) => l.startsWith('notify-')) ?? '';
    }
    // 알림을 누른 것처럼: 주 창에서 그 메일이 열린다.
    await invoke('notify_open', { folderId: inbox.id, messageId: id });
    let opened = -1;
    for (let i = 0; i < 30 && opened !== id; i++) {
      await sleep(100);
      opened = ctx.openedId() ?? -1;
    }
    await sleep(300);
    const closed = !(await invoke<string[]>('ui_test_window_labels')).some((l) => l.startsWith('notify-'));
    check('새 메일 알림 창(뜨고, 누르면 주 창에서 그 메일, 알림은 닫힘)', !!label && opened === id && closed, `창 ${label || '없음'}, 연 메일 #${opened}/#${id}, 닫힘 ${closed}`);
  });

  await step('경계 끌기 덮개', async () => {
    // 끄는 동안만 화면 덮개(본문 iframe 위), 놓으면 사라지고 iframe의 마우스 입력은 건드리지 않는다.
    const split = document.querySelectorAll<HTMLElement>('.split')[1];
    const r = split.getBoundingClientRect();
    const opt = (x: number, buttons: number) => ({ bubbles: true, clientX: x, clientY: r.top + 50, button: 0, buttons, pointerId: 1 });
    split.dispatchEvent(new PointerEvent('pointerdown', opt(r.left + 2, 1)));
    window.dispatchEvent(new PointerEvent('pointermove', opt(r.left + 30, 1)));
    await sleep(50);
    const during = !!document.querySelector('.drag-cover');
    window.dispatchEvent(new PointerEvent('pointerup', opt(r.left + 30, 0)));
    await sleep(50);
    const after = !document.querySelector('.drag-cover');
    const frame = document.querySelector<HTMLIFrameElement>('article.reader iframe');
    const iframeOk = !frame || getComputedStyle(frame).pointerEvents !== 'none';
    split.dispatchEvent(new MouseEvent('dblclick', { bubbles: true }));
    check('경계 끌기: 끄는 동안만 덮개, 놓으면 사라짐, 본문 iframe 마우스 입력 그대로', during && after && iframeOk, `끄는 중 ${during}, 놓은 뒤 없음 ${after}, iframe ${iframeOk}`);
  });

  await sleep(300);
  const pending = ctx.status().pendingOps;
  check('서버 반영 대기 수 표시', pending > pending0, `${pending0} → ${pending}건(로그인하면 서버로)`);

  lines.push(fails ? `판정: 실패 ${fails}건` : '판정: 모두 통과', '');
  try {
    await api.perfWrite(lines, 'uitest.log');
  } finally {
    await api.perfExit();
  }
}
