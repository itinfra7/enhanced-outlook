// 문단 들여쓰기·내어쓰기와 쓰기 방향(웹메일 작성 도구와 같게). 목록 안에서는 목록 단계를 바꾼다(Editor에서).
// 보내는 HTML에는 style="margin-left:40px"·dir="rtl"로 남아 받는 쪽에서도 같게 보인다.
import { Extension } from '@tiptap/core';
import type { EditorState, Transaction } from '@tiptap/pm/state';

const TYPES = ['paragraph', 'heading'];
const STEP = 40;
const MAX = 8;

declare module '@tiptap/core' {
  interface Commands<ReturnType> {
    blockFormat: {
      indent: () => ReturnType;
      outdent: () => ReturnType;
      setDirection: (dir: 'ltr' | 'rtl') => ReturnType;
    };
  }
}

/// 선택한 문단들의 속성을 바꾼다(체인 안의 앞 명령이 바꾼 문서도 보도록 tr 기준).
function update(
  _state: EditorState,
  tr: Transaction,
  f: (attrs: Record<string, unknown>) => Record<string, unknown>,
): boolean {
  const { from, to } = tr.selection;
  let changed = false;
  tr.doc.nodesBetween(from, to, (node, pos) => {
    if (!TYPES.includes(node.type.name)) return true;
    const next = { ...node.attrs, ...f(node.attrs) };
    if (JSON.stringify(next) !== JSON.stringify(node.attrs)) {
      tr.setNodeMarkup(pos, undefined, next);
      changed = true;
    }
    return false;
  });
  return changed;
}

export const BlockFormat = Extension.create({
  name: 'blockFormat',

  addGlobalAttributes() {
    return [
      {
        types: TYPES,
        attributes: {
          indent: {
            default: 0,
            parseHTML: (el) => {
              const px = parseFloat((el as HTMLElement).style.marginLeft || '0');
              return Number.isFinite(px) ? Math.min(MAX, Math.max(0, Math.round(px / STEP))) : 0;
            },
            renderHTML: (a) => (a.indent ? { style: `margin-left:${Number(a.indent) * STEP}px` } : {}),
          },
          dir: {
            default: null,
            parseHTML: (el) => (el.getAttribute('dir') === 'rtl' ? 'rtl' : null),
            renderHTML: (a) => (a.dir ? { dir: a.dir } : {}),
          },
        },
      },
    ];
  },

  // TipTap 명령: dispatch가 있을 때만 tr을 바꾼다(보내는 것은 TipTap이 한다). 없으면 할 수 있는지만 답한다.
  addCommands() {
    return {
      indent:
        () =>
        ({ state, tr, dispatch }) =>
          dispatch ? update(state, tr, (a) => ({ indent: Math.min(MAX, Number(a.indent ?? 0) + 1) })) : true,
      outdent:
        () =>
        ({ state, tr, dispatch }) =>
          dispatch ? update(state, tr, (a) => ({ indent: Math.max(0, Number(a.indent ?? 0) - 1) })) : true,
      setDirection:
        (dir) =>
        ({ state, tr, dispatch }) =>
          dispatch ? update(state, tr, () => ({ dir: dir === 'rtl' ? 'rtl' : null })) : true,
    };
  },
});
