// 검색어 하이라이트(사용자 요청 2026-10-21): 검색 결과 목록의 보낸 사람·제목과 읽기 창 본문.

const FIELDS = '제목|보낸이|받는이|참조|숨은참조|첨부|기간';

/** 검색 문장에서 칠할 낱말들(긴 것부터). `기간:`·제외어(-)·AND/OR는 뺀다. */
export function searchTerms(q: string): string[] {
  const out: string[] = [];
  const re = new RegExp(`(?:(${FIELDS}):)?(?:"([^"]+)"|(\\S+))`, 'g');
  for (const m of q.matchAll(re)) {
    const field = m[1];
    const t = (m[2] ?? m[3] ?? '').trim();
    if (!t || field === '기간' || t.startsWith('-') || /^(and|or|not)$/i.test(t)) continue;
    out.push(t);
  }
  return [...new Set(out)].sort((a, b) => b.length - a.length);
}

function pattern(terms: string[]): RegExp | null {
  const parts = terms.filter((t) => t.length > 0).map((t) => t.replace(/[.*+?^${}()|[\]\\]/g, '\\$&'));
  return parts.length ? new RegExp(parts.join('|'), 'gi') : null;
}

/** 글을 검색어 부분과 나머지로 나눈다(목록 행 표시용). */
export function splitHits(text: string, terms: string[]): { t: string; hit: boolean }[] {
  const re = pattern(terms);
  if (!re || !text) return [{ t: text, hit: false }];
  const out: { t: string; hit: boolean }[] = [];
  let at = 0;
  for (const m of text.matchAll(re)) {
    const i = m.index ?? 0;
    if (!m[0]) continue;
    if (i > at) out.push({ t: text.slice(at, i), hit: false });
    out.push({ t: m[0], hit: true });
    at = i + m[0].length;
  }
  if (at < text.length) out.push({ t: text.slice(at), hit: false });
  return out;
}

/** 본문 문서의 글자에서 검색어를 <mark>로 칠하고 첫 번째를 보이게 한다. 칠한 수. */
export function highlightDocument(doc: Document | null | undefined, terms: string[], max = 500): number {
  const re = pattern(terms);
  const body = doc?.body;
  if (!re || !doc || !body) return 0;
  const walker = doc.createTreeWalker(body, NodeFilter.SHOW_TEXT, {
    acceptNode: (n) => {
      const p = n.parentElement;
      if (!p || /^(SCRIPT|STYLE|TITLE|MARK)$/.test(p.tagName)) return NodeFilter.FILTER_REJECT;
      re.lastIndex = 0;
      return re.test(n.nodeValue ?? '') ? NodeFilter.FILTER_ACCEPT : NodeFilter.FILTER_REJECT;
    },
  });
  const nodes: Text[] = [];
  while (walker.nextNode()) nodes.push(walker.currentNode as Text);
  let count = 0;
  let first: HTMLElement | null = null;
  for (const node of nodes) {
    if (count >= max) break;
    const frag = doc.createDocumentFragment();
    for (const p of splitHits(node.nodeValue ?? '', terms)) {
      if (p.hit && count < max) {
        const mark = doc.createElement('mark');
        mark.textContent = p.t;
        mark.style.cssText = 'background:#ffe066;color:inherit;padding:0';
        frag.appendChild(mark);
        first ??= mark;
        count++;
      } else frag.appendChild(doc.createTextNode(p.t));
    }
    node.replaceWith(frag);
  }
  first?.scrollIntoView({ block: 'center' });
  return count;
}
