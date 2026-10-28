// 스프레드시트 붙여 넣기(0.7.0 사용자 요청): 엑셀·한셀·구글 시트에서 복사한 영역이 선·배경·글꼴·정렬·열 폭·
// 행 높이·병합까지 그대로 보이고, 받는 쪽에도 그대로 보이게 한다.
// 엑셀은 칸의 모양을 <style>의 클래스(.xl65 { border:… })로 주는데, 편집기와 받는 메일 프로그램은 그 묶음을
// 버린다. 그래서 붙여 넣을 때 규칙을 칸마다 style로 직접 넣고(받는 쪽은 칸에 직접 쓴 style을 그대로 그린다),
// <col>의 폭은 칸의 열 폭(colwidth)으로 옮긴다.

/** 스프레드시트에서 온 HTML인가. */
export function isSpreadsheetHtml(html: string): boolean {
  return /urn:schemas-microsoft-com:office:excel|ProgId\s+content=["']?Excel|google-sheets-html-origin|HCell|Hancom\s*Cell|x:str|mso-number-format/i.test(
    html,
  );
}

type Decls = [string, string][];

/** 지켜도 되는 속성(보이는 모양만). mso-*·표시 숨김 같은 것은 뺀다. */
const KEEP =
  /^(border(-(top|right|bottom|left))?(-(width|style|color))?|background(-color)?|color|font(-(family|size|weight|style))?|text-(align|decoration|indent)|vertical-align|white-space|width|height|padding(-(top|right|bottom|left))?|line-height|border-collapse|table-layout|word-break|overflow-wrap)$/;

function parseDecls(text: string): Decls {
  const out: Decls = [];
  for (const part of text.split(';')) {
    const i = part.indexOf(':');
    if (i < 0) continue;
    const prop = part.slice(0, i).trim().toLowerCase();
    const value = part.slice(i + 1).trim();
    if (prop && value) out.push([prop, value]);
  }
  return out;
}

/** 아주 단순한 CSS 읽기: 선택자 → 선언. @규칙은 건너뛴다. */
function parseCss(css: string): Map<string, Decls> {
  const rules = new Map<string, Decls>();
  const text = css.replace(/<!--|-->/g, '').replace(/\/\*[\s\S]*?\*\//g, '');
  for (const m of text.matchAll(/([^{}@]+)\{([^{}]*)\}/g)) {
    const decls = parseDecls(m[2]);
    for (const sel of m[1].split(',')) {
      const s = sel.trim().toLowerCase();
      if (!s || /[\s>:+~[]/.test(s)) continue; // 단순 선택자(태그·.클래스·태그.클래스)만
      rules.set(s, [...(rules.get(s) ?? []), ...decls]);
    }
  }
  return rules;
}

/** 값 손보기: 시스템 색 windowtext는 검정으로, 엑셀의 general 정렬은 뺀다. */
function fixValue(prop: string, value: string): string | null {
  let v = value.replace(/windowtext/gi, '#000000').replace(/\s*!important/i, '');
  if (prop === 'text-align' && /^general$/i.test(v)) return null;
  if (prop.startsWith('font-family')) v = v.replace(/"/g, "'");
  return v;
}

/** 선언을 차례로 합친다(같은 속성은 뒤의 것, 순서는 마지막 자리). */
function merge(into: Decls, add: Decls) {
  for (const [p, v] of add) {
    const i = into.findIndex(([q]) => q === p);
    if (i >= 0) into.splice(i, 1);
    into.push([p, v]);
  }
}

function styleOf(el: Element, rules: Map<string, Decls>, base: Decls = []): string {
  const tag = el.tagName.toLowerCase();
  const decls: Decls = [...base];
  merge(decls, rules.get(tag) ?? []);
  for (const c of el.classList) {
    const k = c.toLowerCase();
    merge(decls, rules.get(`.${k}`) ?? []);
    merge(decls, rules.get(`${tag}.${k}`) ?? []);
  }
  merge(decls, parseDecls(el.getAttribute('style') ?? ''));
  // 엑셀의 기본 정렬 general(숫자는 오른쪽)은 align 속성으로 온다: general은 없는 것으로 본다.
  const general = decls.findIndex(([p, v]) => p === 'text-align' && /^general$/i.test(v.trim()));
  if (general >= 0) decls.splice(general, 1);
  const align = el.getAttribute('align');
  if (align && !decls.some(([p]) => p === 'text-align')) decls.push(['text-align', align.toLowerCase()]);
  const valign = el.getAttribute('valign');
  if (valign && !decls.some(([p]) => p === 'vertical-align')) decls.push(['vertical-align', valign.toLowerCase()]);
  const bg = el.getAttribute('bgcolor');
  if (bg && !decls.some(([p]) => p.startsWith('background'))) decls.push(['background', bg]);
  return decls
    .filter(([p]) => KEEP.test(p))
    .map(([p, v]) => [p, fixValue(p, v)] as const)
    .filter((d): d is readonly [string, string] => d[1] !== null)
    .map(([p, v]) => `${p}:${v}`)
    .join(';');
}

/** 폭 값(px 숫자). "133", "100pt", "120px". */
function px(value: string | null | undefined): number {
  if (!value) return 0;
  const m = /^([\d.]+)\s*(pt|px)?$/i.exec(value.trim());
  if (!m) return 0;
  const n = parseFloat(m[1]);
  return Math.round(m[2]?.toLowerCase() === 'pt' ? (n * 96) / 72 : n);
}

/**
 * 스프레드시트 HTML을 칸마다 style을 직접 쓴 표로 바꾼다. 표가 없으면 그대로.
 */
export function inlineSpreadsheet(html: string): string {
  const doc = new DOMParser().parseFromString(html, 'text/html');
  const rules = new Map<string, Decls>();
  for (const s of doc.querySelectorAll('style')) {
    for (const [k, v] of parseCss(s.textContent ?? '')) rules.set(k, [...(rules.get(k) ?? []), ...v]);
  }
  const tables = [...doc.querySelectorAll('table')];
  if (tables.length === 0) return html;
  // 루비(엑셀 윗주)와 숨긴 것은 뺀다.
  for (const el of doc.querySelectorAll('rt, rp, style, meta, link, title')) el.remove();

  for (const table of tables) {
    // 열 폭: <col width>(span 만큼) → 칸의 colwidth(병합이면 이어서).
    const widths: number[] = [];
    for (const col of table.querySelectorAll('col')) {
      const w = px(col.getAttribute('width')) || px(/width\s*:\s*([\d.]+\s*(?:pt|px)?)/i.exec(col.getAttribute('style') ?? '')?.[1]);
      const span = Math.max(1, parseInt(col.getAttribute('span') ?? '1', 10) || 1);
      for (let i = 0; i < span; i++) widths.push(w);
    }
    for (const el of table.querySelectorAll('col, colgroup')) el.remove();

    // 칸마다 위치를 세며(rowspan 고려) style과 열 폭을 넣는다.
    const taken: boolean[][] = [];
    const rows = [...table.rows];
    rows.forEach((tr, r) => {
      const rs = styleOf(tr, rules);
      if (rs) tr.setAttribute('style', rs);
      else tr.removeAttribute('style');
      tr.removeAttribute('class');
      tr.removeAttribute('height');
      let c = 0;
      for (const cell of [...tr.cells]) {
        while (taken[r]?.[c]) c++;
        const colspan = Math.max(1, cell.colSpan || 1);
        const rowspan = Math.max(1, cell.rowSpan || 1);
        for (let dr = 0; dr < rowspan; dr++) {
          taken[r + dr] ??= [];
          for (let dc = 0; dc < colspan; dc++) taken[r + dr][c + dc] = true;
        }
        // 엑셀 칸 기본 여백(1px)으로 시작해 편집기의 표 기본 여백이 끼지 않게 한다.
        const style = styleOf(cell, rules, [['padding', '1px 2px']]);
        cell.setAttribute('style', style);
        const span = widths.slice(c, c + colspan);
        if (span.length === colspan && span.every((w) => w > 0)) cell.setAttribute('colwidth', span.join(','));
        for (const a of ['class', 'width', 'height', 'align', 'valign', 'bgcolor', 'x:str', 'x:num']) cell.removeAttribute(a);
        c += colspan;
      }
    });

    const ts = styleOf(table, rules)
      .split(';')
      .filter((d) => !/^width:/i.test(d))
      .join(';');
    table.setAttribute('style', ['border-collapse:collapse', ts].filter(Boolean).join(';'));
    for (const a of ['class', 'width', 'border', 'cellpadding', 'cellspacing']) table.removeAttribute(a);
  }

  // 칸 안의 글자 꾸밈(<font class=font5> 등)은 style을 넣은 span으로.
  for (const font of [...doc.querySelectorAll('font')]) {
    const span = doc.createElement('span');
    const st = styleOf(font, rules);
    const color = font.getAttribute('color');
    const face = font.getAttribute('face');
    span.setAttribute(
      'style',
      [st, color && !/color:/.test(st) ? `color:${color}` : '', face && !/font-family:/.test(st) ? `font-family:${face}` : '']
        .filter(Boolean)
        .join(';'),
    );
    span.append(...font.childNodes);
    font.replaceWith(span);
  }
  return doc.body.innerHTML.replace(/<!--(Start|End)Fragment-->/g, '');
}
