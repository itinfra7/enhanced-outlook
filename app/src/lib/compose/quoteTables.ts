// 인용문 칸(원문을 그대로 고치는 contentEditable 문서)의 표 편집(1.0.3 사용자 지적: 회신·전달한 원문의 표가 표로
// 다뤄지지 않았다). 위쪽 편집기(TipTap)와 같은 일을 DOM으로 한다: 행·열 넣기·지우기, 표 지우기, 여러 칸 고르기,
// 칸 배경색. rowspan·colspan은 칸 격자로 따져 맞춘다.

/** 고른 칸 표시(문서에 넣은 스타일, 저장할 때 뺀다). */
export const SEL_CLASS = 'eo-cell-sel';
export const SEL_STYLE_ID = 'eo-cell-sel-style';

type Cell = HTMLTableCellElement;

/** 표의 칸 격자: grid[행][열] = 그 자리를 덮는 칸. */
function gridOf(table: HTMLTableElement): Cell[][] {
  const grid: Cell[][] = [];
  [...table.rows].forEach((row, r) => {
    grid[r] ??= [];
    let c = 0;
    for (const cell of [...row.cells]) {
      while (grid[r][c]) c++;
      const rs = Math.max(1, cell.rowSpan);
      const cs = Math.max(1, cell.colSpan);
      for (let i = 0; i < rs; i++) {
        grid[r + i] ??= [];
        for (let j = 0; j < cs; j++) grid[r + i][c + j] = cell;
      }
      c += cs;
    }
  });
  return grid;
}

/** 칸의 격자 위치(왼쪽 위). */
function posOf(grid: Cell[][], cell: Cell): [number, number] {
  for (let r = 0; r < grid.length; r++) {
    const c = grid[r]?.indexOf(cell) ?? -1;
    if (c >= 0) return [r, c];
  }
  return [-1, -1];
}

function width(grid: Cell[][]): number {
  return Math.max(0, ...grid.map((row) => row.length));
}

export function tableOf(cell: Element): HTMLTableElement | null {
  return cell.closest('table');
}

export function cellOf(node: Node | null): Cell | null {
  // instanceof Element는 쓰지 않는다: 인용문 칸은 다른 문서(iframe)라 그 문서의 요소는 주 창의 Element가 아니다
  // (칸의 빈 곳을 누르면 칸을 못 찾았다).
  const el = node?.nodeType === 1 ? (node as Element) : node?.parentElement;
  return (el?.closest('td, th') as Cell | null) ?? null;
}

/** 새 칸: 옆 칸의 style(선·여백·글꼴)을 따라 하되 배경은 뺀다. */
function blankLike(model: Cell | undefined, doc: Document): Cell {
  const td = doc.createElement(model?.tagName === 'TH' ? 'th' : 'td') as Cell;
  const style = (model?.getAttribute('style') ?? '')
    .split(';')
    .filter((s) => s.trim() && !/^\s*(background|width)/i.test(s))
    .join(';');
  if (style) td.setAttribute('style', style);
  td.innerHTML = '<br>';
  return td;
}

/** 칸이 있는 행 위·아래에 빈 행. */
export function insertRow(cell: Cell, below: boolean): void {
  const table = tableOf(cell);
  if (!table) return;
  const grid = gridOf(table);
  const [r0] = posOf(grid, cell);
  const r = below ? r0 + Math.max(1, cell.rowSpan) - 1 : r0;
  const ref = table.rows[r];
  if (!ref) return;
  const tr = ref.ownerDocument.createElement('tr');
  const done = new Set<Cell>();
  for (let c = 0; c < width(grid); c++) {
    const covering = grid[r]?.[c];
    if (!covering || done.has(covering)) continue;
    done.add(covering);
    const [cr] = posOf(grid, covering);
    const spansAcross = below ? cr + covering.rowSpan - 1 > r : cr < r;
    if (spansAcross) {
      covering.rowSpan += 1;
    } else {
      for (let k = 0; k < Math.max(1, covering.colSpan); k++) tr.appendChild(blankLike(covering, tr.ownerDocument));
    }
  }
  if (below) ref.after(tr);
  else ref.before(tr);
}

/** 칸이 있는 열 왼쪽·오른쪽에 빈 열. */
export function insertCol(cell: Cell, right: boolean): void {
  const table = tableOf(cell);
  if (!table) return;
  const grid = gridOf(table);
  const [, c0] = posOf(grid, cell);
  const c = right ? c0 + Math.max(1, cell.colSpan) - 1 : c0;
  const done = new Set<Cell>();
  [...table.rows].forEach((row, r) => {
    const covering = grid[r]?.[c];
    if (!covering) {
      row.appendChild(blankLike(row.cells[row.cells.length - 1], row.ownerDocument));
      return;
    }
    if (done.has(covering)) return;
    done.add(covering);
    const [cr, cc] = posOf(grid, covering);
    const spansAcross = right ? cc + covering.colSpan - 1 > c : cc < c;
    if (spansAcross) {
      covering.colSpan += 1;
      return;
    }
    const fresh = blankLike(covering, row.ownerDocument);
    if (Math.max(1, covering.rowSpan) > 1) fresh.rowSpan = covering.rowSpan;
    if (r === cr) {
      if (right) covering.after(fresh);
      else covering.before(fresh);
    }
  });
}

/** 칸이 있는 행을 지운다(마지막 행이면 표째). */
export function deleteRow(cell: Cell): void {
  const table = tableOf(cell);
  if (!table) return;
  const grid = gridOf(table);
  const [r] = posOf(grid, cell);
  if (table.rows.length <= 1) {
    table.remove();
    return;
  }
  const row = table.rows[r];
  const next = table.rows[r + 1];
  const done = new Set<Cell>();
  for (let c = 0; c < width(grid); c++) {
    const covering = grid[r]?.[c];
    if (!covering || done.has(covering)) continue;
    done.add(covering);
    if (Math.max(1, covering.rowSpan) <= 1) continue;
    const [cr] = posOf(grid, covering);
    covering.rowSpan -= 1;
    // 이 행에서 시작한 세로 병합 칸은 다음 행으로 옮긴다.
    if (cr === r && next) {
      const after = [...next.cells].find((x) => posOf(grid, x)[1] > c);
      if (after) after.before(covering);
      else next.appendChild(covering);
    }
  }
  row.remove();
}

/** 칸이 있는 열을 지운다(마지막 열이면 표째). */
export function deleteCol(cell: Cell): void {
  const table = tableOf(cell);
  if (!table) return;
  const grid = gridOf(table);
  const [, c] = posOf(grid, cell);
  if (width(grid) <= 1) {
    table.remove();
    return;
  }
  const done = new Set<Cell>();
  for (let r = 0; r < grid.length; r++) {
    const covering = grid[r]?.[c];
    if (!covering || done.has(covering)) continue;
    done.add(covering);
    if (Math.max(1, covering.colSpan) > 1) covering.colSpan -= 1;
    else covering.remove();
  }
}

/** 두 칸을 꼭짓점으로 하는 사각형 안의 칸들(같은 표일 때만). */
export function rectCells(a: Cell, b: Cell): Cell[] {
  const table = tableOf(a);
  if (!table || tableOf(b) !== table) return [a];
  const grid = gridOf(table);
  const [ar, ac] = posOf(grid, a);
  const [br, bc] = posOf(grid, b);
  const out = new Set<Cell>();
  for (let r = Math.min(ar, br); r <= Math.max(ar, br); r++) {
    for (let c = Math.min(ac, bc); c <= Math.max(ac, bc); c++) {
      const x = grid[r]?.[c];
      if (x) out.add(x);
    }
  }
  return [...out];
}

/** 칸 배경색(없애면 null). 스프레드시트에서 붙은 background 줄임말도 바꾼다. */
export function shade(cells: Cell[], color: string | null): void {
  for (const cell of cells) {
    const rest = (cell.getAttribute('style') ?? '')
      .split(';')
      .map((s) => s.trim())
      .filter((s) => s && !/^background(-color)?\s*:/i.test(s));
    if (color) rest.push(`background-color:${color}`);
    if (rest.length) cell.setAttribute('style', rest.join(';'));
    else cell.removeAttribute('style');
    cell.removeAttribute('bgcolor');
  }
}

/** 고른 칸 표시를 바꾼다. */
export function markSelected(doc: Document, cells: Cell[]): void {
  clearSelected(doc);
  for (const c of cells) c.classList.add(SEL_CLASS);
}

export function clearSelected(doc: Document): void {
  for (const c of doc.querySelectorAll(`.${SEL_CLASS}`)) {
    c.classList.remove(SEL_CLASS);
    if (!c.getAttribute('class')) c.removeAttribute('class');
  }
}

export function selectedCells(doc: Document): Cell[] {
  return [...doc.querySelectorAll<Cell>(`.${SEL_CLASS}`)];
}

/** 고른 칸 표시 스타일을 문서에 넣는다(머리에, 본문 밖이라 저장되지 않는다). */
export function installSelectionStyle(doc: Document): void {
  if (doc.getElementById(SEL_STYLE_ID)) return;
  const st = doc.createElement('style');
  st.id = SEL_STYLE_ID;
  st.textContent = `.${SEL_CLASS}{box-shadow:inset 0 0 0 9999px rgb(15 108 189 / 0.22);outline:1px solid #0f6cbd}`;
  doc.head.appendChild(st);
}

/** 저장할 본문: 고른 칸 표시를 뺀 innerHTML. */
export function cleanHtml(body: HTMLElement): string {
  const copy = body.cloneNode(true) as HTMLElement;
  for (const c of copy.querySelectorAll(`.${SEL_CLASS}`)) {
    c.classList.remove(SEL_CLASS);
    if (!c.getAttribute('class')) c.removeAttribute('class');
  }
  return copy.innerHTML;
}
