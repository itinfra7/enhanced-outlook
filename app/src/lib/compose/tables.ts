// 표(0.7.0): 칸·행·표의 style을 지키고(스프레드시트에서 붙여 넣은 선·배경·글꼴·정렬이 받는 쪽에도 그대로),
// 열 경계를 끌어 폭을 바꾼다(웹메일과 같게). 끌어 정한 폭은 칸의 width로도 내보낸다(colgroup을 무시하는
// 메일 프로그램이 있어서).
import { Table, TableCell, TableHeader, TableRow } from '@tiptap/extension-table';

/** style 속성을 그대로 읽고 쓴다. */
const styleAttr = {
  style: {
    default: null as string | null,
    parseHTML: (el: HTMLElement) => el.getAttribute('style'),
    renderHTML: (attrs: Record<string, unknown>) => (attrs.style ? { style: attrs.style } : {}),
  },
};

/** 칸: style + 끌어 정한 폭(colwidth, 여러 열이면 합)을 width로. */
const cellAttrs = {
  style: {
    default: null as string | null,
    parseHTML: (el: HTMLElement) => el.getAttribute('style'),
    renderHTML: (attrs: Record<string, unknown>) => {
      const widths = Array.isArray(attrs.colwidth) ? (attrs.colwidth as number[]) : null;
      const total = widths?.every((w) => w > 0) ? widths.reduce((a, b) => a + b, 0) : 0;
      // 원래 style의 width는 끌어 정한 폭이 이긴다.
      const own = typeof attrs.style === 'string' ? attrs.style.replace(/(^|;)\s*width\s*:[^;]*/gi, '$1') : '';
      const style = [own.replace(/^[;\s]+|[;\s]+$/g, ''), total ? `width:${total}px` : '']
        .filter(Boolean)
        .join(';');
      return style ? { style } : {};
    },
  },
};

export const StyledTable = Table.extend({
  addAttributes() {
    return { ...this.parent?.(), ...styleAttr };
  },
});

export const StyledRow = TableRow.extend({
  addAttributes() {
    return { ...this.parent?.(), ...styleAttr };
  },
});

export const StyledCell = TableCell.extend({
  addAttributes() {
    return { ...this.parent?.(), ...cellAttrs };
  },
});

export const StyledHeader = TableHeader.extend({
  addAttributes() {
    return { ...this.parent?.(), ...cellAttrs };
  },
});

/**
 * 보낼 HTML: 표 칸 안 문단의 바깥 여백을 없앤다(편집기는 칸 내용을 문단으로 감싸는데, 받는 쪽 기본 여백이
 * 붙으면 엑셀 표가 늘어진다).
 */
export function emailHtml(html: string): string {
  if (!html.includes('<td') && !html.includes('<th')) return html;
  const doc = new DOMParser().parseFromString(`<body>${html}</body>`, 'text/html');
  for (const p of doc.querySelectorAll<HTMLElement>('td p, th p')) {
    if (!/margin/i.test(p.getAttribute('style') ?? '')) p.style.margin = '0';
  }
  return doc.body.innerHTML;
}
