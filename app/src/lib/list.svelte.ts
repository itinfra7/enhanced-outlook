// 가상 목록의 데이터: 200행 단위 페이지(계획서 4.1-5)를 필요할 때 받아 둔다.
// 목록이 바뀌면(폴더 전환·검색) 세대 번호를 올려 늦게 온 이전 응답을 버린다.
import type { Page, RowView } from './types';

export const PAGE = 200;
export const ROW_H = 50;
/** 보이는 범위에서 이만큼 떨어진 페이지는 버린다(10만 행을 다 들고 있지 않게). */
const KEEP_PAGES = 12;

export type Fetcher = (offset: number, limit: number) => Promise<Page>;

export class ListModel {
  total = $state(0);
  /** 행 데이터가 바뀔 때마다 올린다(화면이 다시 읽는다). */
  version = $state(0);
  /** 목록 자체가 바뀔 때마다 올린다(맨 위로). */
  epoch = $state(0);
  key = $state('');
  /** 마지막 첫 페이지의 Rust 처리 시간 */
  lastMs = $state(0);

  private fetcher: Fetcher = async () => ({ total: 0, offset: 0, rows: [], ms: 0 });
  private pages = new Map<number, RowView[]>();
  /** 새로 고칠 때 새 페이지가 올 때까지 보여 줄 이전 내용 */
  private stale = new Map<number, RowView[]>();
  private pending = new Map<number, Promise<void>>();
  private gen = 0;

  /** 다른 목록으로 바꾼다. 첫 페이지가 오면 한 번에 바꾼다(그동안 이전 목록이 보인다). */
  async reset(key: string, fetcher: Fetcher, first?: Page): Promise<boolean> {
    const gen = ++this.gen;
    const page = first ?? (await fetcher(0, PAGE));
    if (gen !== this.gen) return false;
    this.fetcher = fetcher;
    this.pages = new Map([[0, page.rows]]);
    this.stale = new Map();
    this.pending = new Map();
    this.key = key;
    this.total = page.total;
    this.lastMs = page.ms;
    this.epoch++;
    this.version++;
    return true;
  }

  /** 같은 목록을 다시 받는다(동기화로 행이 바뀜). 받는 동안 이전 내용을 보여 준다. */
  refresh(): void {
    this.gen++;
    for (const [p, rows] of this.pages) this.stale.set(p, rows);
    this.pages = new Map();
    this.pending = new Map();
    this.version++;
  }

  row(i: number): RowView | undefined {
    void this.version;
    const p = Math.floor(i / PAGE);
    return (this.pages.get(p) ?? this.stale.get(p))?.[i - p * PAGE];
  }

  /** 보이는 범위의 페이지를 받아 둔다. */
  ensure(first: number, last: number): void {
    if (this.total === 0) return;
    const lo = Math.max(0, Math.floor(first / PAGE));
    const hi = Math.min(Math.floor((this.total - 1) / PAGE), Math.floor(Math.max(first, last) / PAGE));
    for (let p = lo; p <= hi; p++) {
      if (!this.pages.has(p) && !this.pending.has(p)) void this.load(p);
    }
    for (const p of this.pages.keys()) {
      if (p < lo - KEEP_PAGES || p > hi + KEEP_PAGES) this.pages.delete(p);
    }
    for (const p of this.stale.keys()) {
      if (p < lo - KEEP_PAGES || p > hi + KEEP_PAGES) this.stale.delete(p);
    }
  }

  private load(p: number): Promise<void> {
    const existing = this.pending.get(p);
    if (existing) return existing;
    const gen = this.gen;
    const task = this.fetcher(p * PAGE, PAGE)
      .then((page) => {
        if (gen !== this.gen) return;
        this.pages.set(p, page.rows);
        this.stale.delete(p);
        if (page.total !== this.total) this.total = page.total;
        this.version++;
      })
      .catch(() => {})
      .finally(() => {
        if (gen === this.gen) this.pending.delete(p);
      });
    this.pending.set(p, task);
    return task;
  }

  /** i번째 행(없으면 받아서). */
  async rowAt(i: number): Promise<RowView | undefined> {
    if (i < 0 || i >= this.total) return undefined;
    const p = Math.floor(i / PAGE);
    if (!this.pages.has(p)) await this.load(p);
    return this.row(i);
  }

  /** 같은 목록을 다시 받고 i번째(목록이 줄었으면 마지막) 행을 돌려준다. 조작 뒤 다음 메일 고르기용. */
  async reloadAt(i: number): Promise<{ index: number; row: RowView | undefined }> {
    this.refresh();
    await this.load(Math.floor(Math.max(0, i) / PAGE));
    const index = Math.min(Math.max(0, i), this.total - 1);
    if (index < 0) return { index: -1, row: undefined };
    return { index, row: await this.rowAt(index) };
  }

  /** 받아 둔 페이지에서 메일 ID의 위치. */
  indexOfId(id: number): number {
    for (const [p, rows] of this.pages) {
      const k = rows.findIndex((r) => r.id === id);
      if (k >= 0) return p * PAGE + k;
    }
    return -1;
  }
}
