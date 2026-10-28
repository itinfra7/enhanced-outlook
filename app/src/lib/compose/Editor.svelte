<script lang="ts">
  // 서식 편집(계획서 9.3, TipTap). 새로 쓰는 부분만 편집한다. 인용할 원문은 서식이 망가지지 않게 편집기 밖(작성
  // 창의 인용문 칸, 그 자리에서 고친다, 0.9.0)에 두고 보낼 때 붙인다(TipTap은 HTML을 자기 형식으로 정리하므로
  // 표·글꼴이 많은 원문을 넣으면 깨진다). 강조 표시는 표 칸을 골랐으면 칸 배경색으로 칠한다(0.9.0).
  // 도구는 웹메일 작성 창과 같게(사용자 요청 2026-10-20): 글꼴·크기·굵게·기울임꼴·밑줄·강조 표시·글꼴 색·
  // 글머리 기호·번호 매기기·내어쓰기·들여쓰기, "기타"에 맞춤·하이퍼링크·첨자·취소선·대체 텍스트·쓰기 방향·
  // 실행 취소·다시 실행·서식 제거·표 삽입. 그림은 붙여 넣기·인라인 그림 삽입으로 넣고, 보낼 때 본문 그림(cid)이 된다.
  import { onDestroy, onMount, tick } from 'svelte';
  import { Editor } from '@tiptap/core';
  import StarterKit from '@tiptap/starter-kit';
  import { BackgroundColor, Color, FontFamily, FontSize, TextStyle } from '@tiptap/extension-text-style';
  import { TextAlign } from '@tiptap/extension-text-align';
  import { Subscript } from '@tiptap/extension-subscript';
  import { Superscript } from '@tiptap/extension-superscript';
  import { StyledCell, StyledHeader, StyledRow, StyledTable, emailHtml } from './tables';
  import { inlineSpreadsheet, isSpreadsheetHtml } from './pasteSheet';
  import { EXCEL_SAMPLE } from './sheetSample';
  import { Image } from '@tiptap/extension-image';
  import { Placeholder } from '@tiptap/extensions';
  import { BlockFormat } from './blockFormat';
  import Icon from './Icon.svelte';
  import Modal from '../Modal.svelte';
  import { api } from '../api';
  import type { SheetPaste } from '../types';
  import { fitInView, placeAt } from '../fit';
  import { NodeSelection, TextSelection } from '@tiptap/pm/state';
  import { DOMSerializer, type Node as PMNode } from '@tiptap/pm/model';
  import { CellSelection } from '@tiptap/pm/tables';

  interface Props {
    html: string;
    /** 서식 도구 모음 보이기(작성 창의 "서식 옵션") */
    toolbar: boolean;
    /** 일반 텍스트(웹메일 "일반 텍스트로 전환"): 도구 모음을 숨기고 붙여 넣기는 글만 */
    plain?: boolean;
    onchange: (html: string) => void;
  }

  let { html, toolbar, plain = false, onchange }: Props = $props();
  let el: HTMLDivElement;
  let picker: HTMLInputElement;
  let editor = $state<Editor | null>(null);
  let version = $state(0);
  let menu = $state<'' | 'highlight' | 'color' | 'table'>('');
  let grid = $state<[number, number]>([0, 0]);
  /** 강조 표시·글꼴 색 단추 아래 막대에 보이는 마지막으로 고른 색 */
  let lastHighlight = $state('#ffff00');
  let lastColor = $state('#c00000');
  /** 스프레드시트 붙여 넣기 방식을 묻는 중(0.7.1 사용자 요청): 붙여 넣을 HTML과 함께 온 그림 */
  let sheetChoice = $state<{ html: string; image: Promise<string>; name: string; remember: boolean } | null>(null);

  const FONTS: [string, string][] = [
    ['맑은 고딕', "'맑은 고딕', 'Malgun Gothic', sans-serif"],
    ['굴림', "굴림, Gulim, sans-serif"],
    ['돋움', "돋움, Dotum, sans-serif"],
    ['바탕', "바탕, Batang, serif"],
    ['궁서', "궁서, Gungsuh, serif"],
    ['Arial', 'Arial, sans-serif'],
    ['Calibri', 'Calibri, sans-serif'],
    ['Cambria', 'Cambria, serif'],
    ['Courier New', "'Courier New', monospace"],
    ['Georgia', 'Georgia, serif'],
    ['Segoe UI', "'Segoe UI', sans-serif"],
    ['Tahoma', 'Tahoma, sans-serif'],
    ['Times New Roman', "'Times New Roman', serif"],
    ['Verdana', 'Verdana, sans-serif'],
  ];
  const SIZES = [8, 9, 10, 11, 12, 14, 16, 18, 20, 22, 24, 26, 28, 36, 48, 72];
  const COLORS = [
    '#000000', '#444444', '#666666', '#999999', '#cccccc', '#ffffff',
    '#c00000', '#ff0000', '#ffc000', '#ffff00', '#92d050', '#00b050',
    '#00b0f0', '#0070c0', '#002060', '#7030a0', '#ff00ff', '#8a5300',
  ];
  const HIGHLIGHTS = [
    '#ffff00', '#00ff00', '#00ffff', '#ff00ff', '#ff0000', '#0000ff',
    '#008080', '#008000', '#800080', '#800000', '#808000', '#c0c0c0',
  ];

  onMount(() => {
    editor = new Editor({
      element: el,
      extensions: [
        StarterKit.configure({ link: { openOnClick: false, autolink: true, defaultProtocol: 'https' } }),
        TextStyle,
        Color,
        BackgroundColor,
        FontFamily,
        FontSize,
        TextAlign.configure({ types: ['heading', 'paragraph'] }),
        Subscript,
        Superscript,
        // 열 경계를 끌어 폭을 바꾼다(웹메일과 같게). 칸·행·표의 style은 지킨다(스프레드시트 붙여 넣기).
        StyledTable.configure({ resizable: true, cellMinWidth: 24, HTMLAttributes: { style: 'border-collapse:collapse' } }),
        StyledRow,
        StyledCell.configure({ HTMLAttributes: { style: 'border:1px solid #ababab;padding:4px 6px;min-width:24px' } }),
        StyledHeader.configure({ HTMLAttributes: { style: 'border:1px solid #ababab;padding:4px 6px;min-width:24px' } }),
        Image.configure({ inline: true, allowBase64: true }),
        BlockFormat,
        Placeholder.configure({ placeholder: '내용을 입력하세요' }),
      ],
      content: html,
      editorProps: {
        // 스프레드시트(엑셀·한셀·구글 시트) 영역: 선·배경·글꼴·열 폭을 칸마다 직접 넣어 그대로 붙인다.
        transformPastedHTML: (pasted) => (isSpreadsheetHtml(pasted) ? inlineSpreadsheet(pasted) : pasted),
        // 화면 캡처 등 그림만 붙여 넣으면 본문에 넣는다(글이 함께 있으면 글로 붙인다: Word는 그림도 같이 준다).
        handlePaste: (_view, event) => {
          const data = event.clipboardData;
          // 일반 텍스트 메일: 서식 없이 글만.
          if (plain && data) {
            editor?.commands.insertContent(paragraphs(data.getData('text/plain')));
            return true;
          }
          if (!data) return false;
          // 엑셀·한셀은 영역을 표(HTML)와 그림으로 함께 준다: 어느 쪽으로 붙일지 고른다.
          const sheet = data.getData('text/html');
          const picture = Array.from(data.files).find((f) => f.type.startsWith('image/'));
          if (sheet && picture && isSpreadsheetHtml(sheet)) {
            void pasteSheet(sheet, picture);
            return true;
          }
          if (data.types.includes('text/html') || data.types.includes('text/plain')) return false;
          return insertImages(data.files);
        },
      },
      onUpdate: ({ editor }) => onchange(editor.getHTML()),
      onTransaction: () => version++,
    });
  });

  onDestroy(() => editor?.destroy());

  export function focus() {
    editor?.commands.focus('start');
  }

  /** 인라인 그림 삽입(파일 고르기). */
  export function pickImage() {
    picker?.click();
  }

  export function insertText(text: string) {
    editor?.chain().focus().insertContent(text).run();
  }

  function escapeHtml(s: string): string {
    return s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
  }

  /** 글 → 줄마다 문단. */
  function paragraphs(text: string): string {
    return text
      .replace(/\r\n/g, '\n')
      .split('\n')
      .map((l) => `<p>${escapeHtml(l)}</p>`)
      .join('');
  }

  /** 커서 자리에 HTML을 넣는다(서명·내 템플릿). */
  export function insertHtml(h: string) {
    editor?.chain().focus().insertContent(h).run();
  }

  /** 고른 부분의 HTML(고른 것이 없으면 본문 전체). 내 템플릿 만들기용. */
  export function selectionHtml(): string {
    const e = editor;
    if (!e) return '';
    const { from, to, empty } = e.state.selection;
    if (empty) return e.getHTML();
    const div = document.createElement('div');
    div.appendChild(DOMSerializer.fromSchema(e.schema).serializeFragment(e.state.doc.slice(from, to).content));
    return div.innerHTML;
  }

  /** 일반 텍스트로 바꾼다: 서식·그림을 빼고 글만 문단으로 남긴다. */
  export function toPlain() {
    const e = editor;
    if (!e) return;
    e.commands.setContent(paragraphs(e.getText({ blockSeparator: '\n' })), { emitUpdate: true });
  }

  /** 접근성 문제(웹메일 "접근성 문제 확인"): 대체 텍스트 없는 그림, 머리글 행 없는 표, 주소만 적힌 링크. */
  export function accessibilityIssues(): string[] {
    const doc = new DOMParser().parseFromString(editor?.getHTML() ?? '', 'text/html');
    const out: string[] = [];
    const noAlt = [...doc.querySelectorAll('img')].filter((i) => !(i.getAttribute('alt') ?? '').trim()).length;
    if (noAlt) out.push(`대체 텍스트가 없는 그림 ${noAlt}개`);
    const noHead = [...doc.querySelectorAll('table')].filter((t) => !t.querySelector('th')).length;
    if (noHead) out.push(`머리글 행이 없는 표 ${noHead}개`);
    const rawLinks = [...doc.querySelectorAll('a[href]')].filter(
      (a) => (a.textContent ?? '').trim() === (a.getAttribute('href') ?? '').trim(),
    ).length;
    if (rawLinks) out.push(`글 대신 주소가 그대로 보이는 링크 ${rawLinks}개`);
    return out;
  }

  /** 화면 자체 점검(`--ui-test`): 웹메일과 같은 도구가 모두 있고 각 기능이 보낼 HTML에 남는지. 실패한 것들. */
  export async function selfTest(): Promise<string[]> {
    const fails: string[] = [];
    const e = editor;
    if (!e) return ['편집기 없음'];
    // 편집기는 스타일을 "margin-left: 80px"처럼 띄어 쓰고 색을 rgb()로 낼 수 있어 공백을 빼고 본다.
    // 실패하면 그때의 HTML 앞부분을 붙인다(합성 글자뿐).
    let h = '';
    const compact = () => h.replace(/\s+/g, '');
    const want = (name: string, ok: boolean) => {
      if (!ok) fails.push(`${name}[${compact().slice(0, 160)}]`);
    };
    const hasStyle = (...alts: string[]) => alts.some((a) => compact().includes(a));
    // 모든 도구가 도구 모음에 바로 보이고(숨긴 메뉴 없음) 아이콘이 있다.
    const buttons = Array.from(el.parentElement?.querySelectorAll<HTMLElement>('.bar > [title], .bar > .has-pop > [title]') ?? []);
    const have = buttons.map((b) => b.title);
    for (const t of [
      '글꼴', '글꼴 크기', '굵게', '기울임꼴', '밑줄', '강조 표시', '글꼴 색', '글머리 기호', '번호 매기기', '내어쓰기',
      '들여쓰기', '왼쪽 맞춤', '가운데', '오른쪽 맞춤', '하이퍼링크 삽입', '하이퍼링크 제거', '위 첨자', '아래 첨자',
      '취소선', '이미지에 대체 텍스트 추가', '왼쪽에서 오른쪽', '오른쪽에서 왼쪽', '실행 취소', '다시 실행', '서식 제거', '표 삽입',
    ]) {
      want(`단추 ${t}`, have.some((x) => x === t || x.startsWith(`${t} (`)));
    }
    want('숨긴 메뉴 없음', !have.includes('기타'));
    want(
      '아이콘',
      buttons.filter((b) => b.tagName === 'BUTTON').every((b) => b.querySelector('svg path') !== null),
    );
    e.commands.setContent('<p>가나다라</p>');
    e.chain().selectAll().setFontFamily(FONTS[1][1]).setFontSize('14pt').toggleBold().toggleItalic().toggleUnderline().run();
    e.chain().selectAll().setBackgroundColor('#ffff00').setColor('#ff0000').toggleStrike().run();
    h = e.getHTML();
    want('글꼴', hasStyle('font-family:굴림'));
    want('글꼴 크기', hasStyle('font-size:14pt'));
    want('굵게·기울임꼴·밑줄·취소선', ['<strong>', '<em>', '<u>', '<s>'].every((t) => h.includes(t)));
    want('강조 표시', hasStyle('background-color:#ffff00', 'background-color:rgb(255,255,0)'));
    want('글꼴 색', hasStyle(';color:#ff0000', '"color:#ff0000', ';color:rgb(255,0,0)', '"color:rgb(255,0,0)'));
    e.chain().selectAll().unsetAllMarks().setTextAlign('center').indent().indent().setDirection('rtl').toggleSuperscript().run();
    h = e.getHTML();
    want('가운데', hasStyle('text-align:center'));
    want('들여쓰기', hasStyle('margin-left:80px'));
    want('오른쪽에서 왼쪽', h.includes('dir="rtl"'));
    want('위 첨자', h.includes('<sup>'));
    e.chain().selectAll().outdent().setDirection('ltr').toggleSubscript().run();
    h = e.getHTML();
    want('내어쓰기', hasStyle('margin-left:40px') && !hasStyle('margin-left:80px'));
    want('왼쪽에서 오른쪽', !h.includes('dir='));
    want('아래 첨자', h.includes('<sub>'));
    e.commands.setContent('<p>목록</p>');
    e.chain().selectAll().toggleBulletList().run();
    h = e.getHTML();
    want('글머리 기호', h.includes('<ul>'));
    e.chain().selectAll().toggleOrderedList().run();
    h = e.getHTML();
    want('번호 매기기', h.includes('<ol>'));
    e.commands.setContent('<p>링크</p>');
    e.chain().selectAll().setLink({ href: 'https://www.example.com' }).run();
    h = e.getHTML();
    want('하이퍼링크 삽입', h.includes('href="https://www.example.com"'));
    e.chain().selectAll().unsetLink().run();
    h = e.getHTML();
    want('하이퍼링크 제거', !h.includes('href='));
    e.commands.setContent('<p>표</p>');
    e.chain().focus('end').insertTable({ rows: 2, cols: 3, withHeaderRow: false }).run();
    h = e.getHTML();
    want('표 삽입', (h.match(/<td/g) ?? []).length === 6 && hasStyle('border:1pxsolid'));
    const png =
      'data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==';
    e.commands.setContent('<p>그림</p>');
    e.chain().focus('end').setImage({ src: png, alt: 'a.png' }).run();
    h = e.getHTML();
    want('인라인 그림', h.includes('src="data:image/png'));
    e.chain().undo().run();
    h = e.getHTML();
    want('실행 취소', !h.includes('data:image/png'));
    e.chain().redo().run();
    h = e.getHTML();
    want('다시 실행', h.includes('data:image/png'));
    e.commands.setContent('<p>서식</p>');
    e.chain().selectAll().toggleBold().setColor('#ff0000').run();
    e.chain().selectAll().unsetAllMarks().clearNodes().run();
    h = e.getHTML();
    want('서식 제거', !h.includes('<strong>') && !h.includes('color'));
    // 표 칸에서 오른쪽 클릭 → 표 메뉴, "아래에 행 삽입"으로 행이 는다.
    e.commands.setContent('<p>표</p>');
    e.chain().focus('end').insertTable({ rows: 2, cols: 2, withHeaderRow: false }).run();
    const td = el.querySelector('td');
    if (td) {
      const r = td.getBoundingClientRect();
      td.dispatchEvent(
        new MouseEvent('contextmenu', { bubbles: true, cancelable: true, clientX: r.left + 4, clientY: r.top + 4 }),
      );
      await tick();
      const labels = Array.from(document.querySelectorAll<HTMLElement>('.ctx button')).map((b) => b.textContent ?? '');
      want('오른쪽 클릭 표 메뉴', ['위에 행 삽입', '셀 병합', '표 삭제', '잘라내기'].every((l) => labels.some((x) => x.startsWith(l))));
      const add = Array.from(document.querySelectorAll<HTMLButtonElement>('.ctx button')).find((b) =>
        (b.textContent ?? '').startsWith('아래에 행 삽입'),
      );
      add?.click();
      await tick();
      h = e.getHTML();
      want('표 메뉴 행 삽입', (h.match(/<tr/g) ?? []).length === 3);
      want('메뉴 닫힘', !document.querySelector('.ctx'));
      // 오른쪽 클릭 "셀 배경색" → 색 고르기 → 그 칸 배경(실제 클릭으로).
      const td2 = el.querySelector('td');
      const r2 = td2?.getBoundingClientRect();
      if (td2 && r2) {
        td2.dispatchEvent(
          new MouseEvent('contextmenu', { bubbles: true, cancelable: true, clientX: r2.left + 4, clientY: r2.top + 4 }),
        );
        await tick();
        Array.from(document.querySelectorAll<HTMLButtonElement>('.ctx button'))
          .find((b) => (b.textContent ?? '').startsWith('셀 배경색'))
          ?.click();
        await tick();
        const pal = !!document.querySelector('.cellpal');
        document.querySelector<HTMLButtonElement>('.cellpal .swatch[title="#ffc000"]')?.click();
        await tick();
        h = e.getHTML();
        want(
          '오른쪽 클릭 셀 배경색',
          pal && /background-color:(#ffc000|rgb\(255,192,0\))/.test(compact()) && !document.querySelector('.cellpal'),
        );
      }
    } else want('오른쪽 클릭 표 메뉴', false);
    // 고르기 창은 창 안에 들어온다.
    menu = 'table';
    await tick();
    const pop = document.querySelector<HTMLElement>('.pop.table');
    const pr = pop?.getBoundingClientRect();
    want('표 고르기 창이 창 안', !!pr && pr.left >= 0 && pr.right <= window.innerWidth && pr.bottom <= window.innerHeight);
    menu = '';
    // 열 경계 끌기: 칸의 열 폭을 바꾸면 보낼 HTML의 칸 width로 나간다(웹메일처럼 끌어 바꾸는 것과 같은 결과).
    e.commands.setContent('<p>폭</p>');
    e.chain().focus('end').insertTable({ rows: 1, cols: 2, withHeaderRow: false }).run();
    want('열 경계 끌기 손잡이', e.extensionManager.extensions.some((x) => x.name === 'table' && x.options.resizable));
    e.chain().focus().setCellAttribute('colwidth', [180]).run();
    h = e.getHTML();
    want('열 폭이 칸 width로', hasStyle('width:180px'));
    // 칸 배경색(0.9.0): 커서만 있는 칸, 여러 칸 고르기, 없음.
    e.commands.setContent('<p>배경</p>');
    e.chain().focus('end').insertTable({ rows: 1, cols: 2, withHeaderRow: false }).run();
    // 편집기는 색을 rgb()로 낸다.
    const tds = () => (compact().match(/<td[^>]*>/g) ?? []).join(' ');
    const yellow = /background-color:(#ffff00|rgb\(255,255,0\))/g;
    const green = /background-color:(#00ff00|rgb\(0,255,0\))/g;
    highlight('#ffff00');
    h = e.getHTML();
    if ((compact().match(yellow) ?? []).length !== 1) fails.push(`칸 배경색(커서 칸)[${tds()}]`);
    const cellAt: number[] = [];
    e.state.doc.descendants((n, pos) => {
      if (n.type.name === 'tableCell') cellAt.push(pos);
    });
    e.view.dispatch(e.state.tr.setSelection(CellSelection.create(e.state.doc, cellAt[0], cellAt[cellAt.length - 1])));
    highlight('#00ff00');
    h = e.getHTML();
    if ((compact().match(green) ?? []).length !== 2 || yellow.test(compact()))
      fails.push(`칸 배경색(여러 칸)[${tds()}]`);
    highlight(null);
    h = e.getHTML();
    want('칸 배경색 없음', !h.includes('background-color'));
    // 도구 모음의 강조 표시 단추를 실제로 눌러서(여러 칸을 고른 채로).
    e.view.dispatch(e.state.tr.setSelection(CellSelection.create(e.state.doc, cellAt[0], cellAt[cellAt.length - 1])));
    el.parentElement?.querySelector<HTMLButtonElement>('.bar [title="강조 표시"]')?.click();
    await tick();
    document.querySelector<HTMLButtonElement>('.pop.palette .swatch[title="#00ffff"]')?.click();
    await tick();
    h = e.getHTML();
    want(
      '강조 표시 단추로 칸 배경',
      (compact().match(/background-color:(#00ffff|rgb\(0,255,255\))/g) ?? []).length === 2,
    );
    // 스프레드시트 붙여 넣기: 선·배경·굵게·가운데·빨간 글자·오른쪽 정렬·병합·열 폭이 칸마다 남는다.
    e.commands.setContent('<p></p>');
    e.commands.focus('end');
    e.view.pasteHTML(EXCEL_SAMPLE);
    await tick();
    h = emailHtml(e.getHTML());
    // 무엇이 빠졌는지 알 수 있게 하나씩 본다(실패하면 그 부분 앞뒤를 남긴다).
    const near = (s: string) => {
      const c = compact();
      const i = c.indexOf('<td');
      return c.slice(Math.max(0, i), i + 400) || c.slice(0, 400);
    };
    const sheet = (name: string, ok: boolean) => {
      if (!ok) fails.push(`엑셀 붙여 넣기 ${name}[${near(name)}]`);
    };
    sheet('배경', hasStyle('background:#FFC000', 'background:rgb(255,192,0)', 'background-color:#FFC000'));
    sheet('테두리', hasStyle('border:.5ptsolid#000000', 'border:0.5ptsolid'));
    sheet('가운데', hasStyle('text-align:center'));
    sheet('오른쪽', hasStyle('text-align:right'));
    sheet('빨간 글자', hasStyle('color:#C00000', 'color:rgb(192,0,0)'));
    sheet('굵게', hasStyle('font-weight:700'));
    sheet('병합', compact().includes('colspan="2"'));
    sheet('열 폭', hasStyle('width:133px'));
    sheet('글자', compact().includes('합계'));
    sheet('문단 여백', hasStyle('margin:0'));
    // 엑셀처럼 표와 그림을 함께 붙여 넣으면 어느 쪽으로 붙일지 묻는다(표로·그림으로 둘 다 본다).
    const bin = Uint8Array.from(atob(png.split(',')[1]), (c) => c.charCodeAt(0));
    for (const choice of ['표로', '그림으로']) {
      e.commands.setContent('<p></p>');
      e.commands.focus('end');
      const dt = new DataTransfer();
      dt.setData('text/html', EXCEL_SAMPLE);
      dt.setData('text/plain', '합계');
      dt.items.add(new File([bin], 'image.png', { type: 'image/png' }));
      e.view.dom.dispatchEvent(new ClipboardEvent('paste', { clipboardData: dt, bubbles: true, cancelable: true }));
      let asked: HTMLElement | null = null;
      for (let i = 0; i < 30 && !asked; i++) {
        await tick();
        await new Promise((r) => setTimeout(r, 50));
        asked = document.querySelector<HTMLElement>('[data-sheet-paste]');
      }
      want(`스프레드시트 붙여넣기 방식 묻기(${choice})`, !!asked);
      [...(asked?.querySelectorAll<HTMLButtonElement>('button') ?? [])].find((b) => b.textContent === choice)?.click();
      for (let i = 0; i < 30 && !/<table|<img/.test(e.getHTML()); i++) await new Promise((r) => setTimeout(r, 50));
      h = e.getHTML();
      want(
        `스프레드시트 ${choice} 붙여넣기`,
        choice === '표로' ? h.includes('<table') && !h.includes('<img') : h.includes('<img') && !h.includes('<table'),
      );
    }
    e.commands.setContent('');
    return fails;
  }

  function dataUrl(f: File): Promise<string> {
    return new Promise((resolve, reject) => {
      const r = new FileReader();
      r.onload = () => (typeof r.result === 'string' ? resolve(r.result) : reject(new Error('그림 읽기 실패')));
      r.onerror = () => reject(r.error ?? new Error('그림 읽기 실패'));
      r.readAsDataURL(f);
    });
  }

  /** 스프레드시트 영역: 설정이 "매번 묻기"면 고르는 창을 띄우고, 아니면 정한 방식으로 붙인다. */
  async function pasteSheet(sheet: string, picture: File) {
    // 붙여 넣기 이벤트가 끝나면 파일을 못 읽을 수 있어 바로 읽기 시작한다.
    const image = dataUrl(picture);
    image.catch(() => {});
    let mode: SheetPaste = 'ask';
    try {
      mode = (await api.getSettings()).settings.sheetPaste;
    } catch {
      // 설정을 못 읽으면 묻는다.
    }
    if (mode === 'ask') sheetChoice = { html: sheet, image, name: picture.name, remember: false };
    else await placeSheet(mode, sheet, image, picture.name);
  }

  async function chooseSheet(mode: 'table' | 'image') {
    const c = sheetChoice;
    sheetChoice = null;
    if (!c) return;
    if (c.remember) {
      try {
        const v = await api.getSettings();
        await api.setSettings({ ...v.settings, sheetPaste: mode });
      } catch {
        // 기억하지 못해도 이번 붙여 넣기는 한다.
      }
    }
    await placeSheet(mode, c.html, c.image, c.name);
  }

  async function placeSheet(mode: 'table' | 'image', sheet: string, image: Promise<string>, name: string) {
    if (!editor) return;
    if (mode === 'image') {
      try {
        editor.chain().focus().setImage({ src: await image, alt: name }).run();
        return;
      } catch {
        // 그림을 못 읽으면 표로.
      }
    }
    editor.commands.focus();
    editor.view.pasteHTML(sheet);
  }

  function autofocus(node: HTMLElement) {
    setTimeout(() => node.focus());
  }

  function insertImages(files: FileList | null | undefined): boolean {
    const images = Array.from(files ?? []).filter((f) => f.type.startsWith('image/'));
    for (const f of images) {
      const r = new FileReader();
      r.onload = () => {
        if (typeof r.result === 'string') editor?.chain().focus().setImage({ src: r.result, alt: f.name }).run();
      };
      r.readAsDataURL(f);
    }
    return images.length > 0;
  }

  function picked() {
    insertImages(picker.files);
    picker.value = '';
  }

  function active(name: string, attrs?: Record<string, unknown>): boolean {
    void version;
    return editor?.isActive(name, attrs) ?? false;
  }

  function can(f: (e: Editor) => boolean): boolean {
    void version;
    return editor ? f(editor) : false;
  }

  let font = $derived.by(() => {
    void version;
    const v = (editor?.getAttributes('textStyle').fontFamily as string | undefined) ?? '';
    return FONTS.find(([, css]) => css === v)?.[1] ?? '';
  });

  let size = $derived.by(() => {
    void version;
    const v = (editor?.getAttributes('textStyle').fontSize as string | undefined) ?? '';
    return v.endsWith('pt') ? v : '';
  });

  function chain() {
    return editor!.chain().focus();
  }

  function setFont(e: Event) {
    const v = (e.target as HTMLSelectElement).value;
    if (v) chain().setFontFamily(v).run();
    else chain().unsetFontFamily().run();
  }

  function setSize(e: Event) {
    const v = (e.target as HTMLSelectElement).value;
    if (v) chain().setFontSize(v).run();
    else chain().unsetFontSize().run();
  }

  function indent(more: boolean) {
    if (!editor) return;
    if (editor.isActive('listItem')) {
      if (more) chain().sinkListItem('listItem').run();
      else chain().liftListItem('listItem').run();
    } else if (more) chain().indent().run();
    else chain().outdent().run();
  }

  function link() {
    if (!editor) return;
    const prev = (editor.getAttributes('link').href as string | undefined) ?? '';
    const url = window.prompt('하이퍼링크 주소', prev || 'https://');
    if (url === null || !url.trim() || url.trim() === 'https://') return;
    chain().extendMarkRange('link').setLink({ href: url.trim() }).run();
  }

  function altText() {
    if (!editor?.isActive('image')) return;
    const prev = (editor.getAttributes('image').alt as string | undefined) ?? '';
    const alt = window.prompt('대체 텍스트', prev);
    if (alt !== null) chain().updateAttributes('image', { alt }).run();
  }

  function insertTable(rows: number, cols: number) {
    menu = '';
    chain().insertTable({ rows, cols, withHeaderRow: false }).run();
  }

  /** 칸 style의 배경색을 바꾼다(없애면 null). 스프레드시트에서 붙인 background 줄임말도 바꾼다. */
  function withBackground(style: unknown, color: string | null): string | null {
    const rest = (typeof style === 'string' ? style : '')
      .split(';')
      .map((s) => s.trim())
      .filter((s) => s && !/^background(-color)?\s*:/i.test(s));
    if (color) rest.push(`background-color:${color}`);
    return rest.length ? rest.join(';') : null;
  }

  /**
   * 칸 배경색(0.9.0 사용자 지적: 표에서 고른 칸에 배경색이 안 들어감 — 강조 표시는 글자에만 입혀져 빈 칸·칸 전체에는
   * 보이지 않았다). 여러 칸을 골랐거나 고른 글자 없이 커서가 칸 안에 있으면 그 칸들에 입히고 true.
   */
  function shadeCells(color: string | null): boolean {
    const e = editor;
    if (!e) return false;
    const sel = e.state.selection;
    const cells: { pos: number; node: PMNode }[] = [];
    if (sel instanceof CellSelection) {
      sel.forEachCell((node, pos) => cells.push({ pos, node }));
    } else if (sel.empty) {
      const at = sel.$from;
      for (let d = at.depth; d > 0; d--) {
        const n = at.node(d);
        if (n.type.name === 'tableCell' || n.type.name === 'tableHeader') {
          cells.push({ pos: at.before(d), node: n });
          break;
        }
      }
    }
    if (!cells.length) return false;
    const tr = e.state.tr;
    for (const { pos, node } of cells) {
      tr.setNodeMarkup(pos, undefined, { ...node.attrs, style: withBackground(node.attrs.style, color) });
    }
    e.view.dispatch(tr);
    e.view.focus();
    return true;
  }

  /** 강조 표시 색: 표 칸을 골랐으면 칸 배경, 아니면 글자 강조. */
  function highlight(color: string | null) {
    if (shadeCells(color)) return;
    if (color) chain().setBackgroundColor(color).run();
    else chain().unsetBackgroundColor().run();
  }

  function run(f: () => void) {
    menu = '';
    f();
  }

  /** 도구 단추를 눌러도 편집기의 선택이 풀리지 않게. */
  function keep(e: MouseEvent) {
    e.preventDefault();
  }

  function outside(e: MouseEvent) {
    if (menu && !(e.target as HTMLElement).closest('.pop, .has-pop')) menu = '';
    if (ctx && !(e.target as HTMLElement).closest('.ctx')) ctx = null;
    if (cellPalette && !(e.target as HTMLElement).closest('.cellpal')) cellPalette = null;
  }

  // ---- 오른쪽 클릭 메뉴(사용자 요청 2026-10-21): 자리에 따라 표·링크·그림 기능 ----
  type CtxItem = { label: string; key?: string; run: () => void; disabled?: boolean } | { sep: true };
  let ctx = $state<{ x: number; y: number; items: CtxItem[] } | null>(null);
  let ctxNote = $state('');
  /** 오른쪽 클릭 "셀 배경색"의 색 고르기 자리 */
  let cellPalette = $state<{ x: number; y: number } | null>(null);
  let ctxAt = { x: 0, y: 0 };

  function oncontextmenu(e: MouseEvent) {
    if (!editor) return;
    e.preventDefault();
    menu = '';
    const view = editor.view;
    const target = e.target as HTMLElement;
    const img = target.closest('img');
    const sel = editor.state.selection;
    if (img && view.dom.contains(img)) {
      // 그림을 누르면 그 그림을 고른다.
      const pos = view.posAtDOM(img, 0);
      view.dispatch(editor.state.tr.setSelection(NodeSelection.create(editor.state.doc, pos)));
    } else {
      const at = view.posAtCoords({ left: e.clientX, top: e.clientY })?.pos;
      // 고른 글자·여러 칸 안을 누르면 그대로(잘라내기·복사·셀 합치기), 밖이면 누른 자리로 커서를 옮긴다.
      const inside = at != null && at >= sel.from && at <= sel.to && !sel.empty;
      if (at != null && !inside && !(sel instanceof CellSelection && target.closest('.selectedCell'))) {
        view.dispatch(editor.state.tr.setSelection(TextSelection.create(editor.state.doc, at)));
      }
    }
    view.focus();
    ctxAt = { x: e.clientX, y: e.clientY };
    ctx = { x: e.clientX, y: e.clientY, items: ctxItems() };
  }

  function ctxItems(): CtxItem[] {
    const e = editor!;
    const c = () => e.chain().focus();
    const items: CtxItem[] = [
      { label: '잘라내기', key: 'Ctrl+X', disabled: e.state.selection.empty, run: () => document.execCommand('cut') },
      { label: '복사', key: 'Ctrl+C', disabled: e.state.selection.empty, run: () => document.execCommand('copy') },
      { label: '붙여넣기', key: 'Ctrl+V', run: () => void pasteText() },
      { label: '모두 선택', key: 'Ctrl+A', run: () => c().selectAll().run() },
    ];
    if (e.isActive('link')) {
      const href = (e.getAttributes('link').href as string | undefined) ?? '';
      items.push(
        { sep: true },
        { label: '하이퍼링크 편집', run: link },
        { label: '하이퍼링크 제거', run: () => c().extendMarkRange('link').unsetLink().run() },
        { label: '하이퍼링크 열기', disabled: !/^https?:\/\//i.test(href), run: () => window.open(href, '_blank') },
      );
    }
    if (e.isActive('image')) {
      items.push(
        { sep: true },
        { label: '이미지에 대체 텍스트 추가', run: altText },
        { label: '그림 삭제', run: () => c().deleteSelection().run() },
      );
    }
    if (e.isActive('table')) {
      items.push(
        { sep: true },
        { label: '위에 행 삽입', run: () => c().addRowBefore().run() },
        { label: '아래에 행 삽입', run: () => c().addRowAfter().run() },
        { label: '왼쪽에 열 삽입', run: () => c().addColumnBefore().run() },
        { label: '오른쪽에 열 삽입', run: () => c().addColumnAfter().run() },
        { sep: true },
        { label: '행 삭제', run: () => c().deleteRow().run() },
        { label: '열 삭제', run: () => c().deleteColumn().run() },
        { label: '셀 병합', disabled: !e.can().mergeCells(), run: () => c().mergeCells().run() },
        { label: '셀 분할', disabled: !e.can().splitCell(), run: () => c().splitCell().run() },
        { label: '셀 배경색', run: () => (cellPalette = { ...ctxAt }) },
        { label: '머리글 행', run: () => c().toggleHeaderRow().run() },
        { sep: true },
        { label: '표 삭제', run: () => c().deleteTable().run() },
      );
    }
    return items;
  }

  /** 붙여넣기: 클립보드 글을 읽어 넣는다. 읽을 수 없으면(권한) Ctrl+V를 알려 준다. */
  async function pasteText() {
    try {
      const text = await navigator.clipboard.readText();
      if (text) editor?.chain().focus().insertContent(text.replace(/[&<>]/g, (ch) => `&#${ch.charCodeAt(0)};`).replace(/\r?\n/g, '<br>')).run();
    } catch {
      ctxNote = 'Ctrl+V로 붙여 넣으세요';
      setTimeout(() => (ctxNote = ''), 2000);
    }
  }

  function runCtx(item: CtxItem) {
    ctx = null;
    if ('sep' in item || item.disabled) return;
    item.run();
  }
</script>

<svelte:window onmousedown={outside} />

<div class="wrap">
  {#if toolbar && !plain}
    <div class="bar" role="toolbar" aria-label="서식">
      <select class="font" title="글꼴" aria-label="글꼴" value={font} onchange={setFont}>
        <option value="">글꼴</option>
        {#each FONTS as [name, css] (name)}<option value={css} style:font-family={css}>{name}</option>{/each}
      </select>
      <select class="size" title="글꼴 크기" aria-label="글꼴 크기" value={size} onchange={setSize}>
        <option value="">크기</option>
        {#each SIZES as s (s)}<option value="{s}pt">{s}</option>{/each}
      </select>
      <span class="sep"></span>
      <button class:on={active('bold')} title="굵게 (Ctrl+B)" aria-label="굵게" onmousedown={keep} onclick={() => chain().toggleBold().run()}><Icon name="bold" /></button>
      <button class:on={active('italic')} title="기울임꼴 (Ctrl+I)" aria-label="기울임꼴" onmousedown={keep} onclick={() => chain().toggleItalic().run()}><Icon name="italic" /></button>
      <button class:on={active('underline')} title="밑줄 (Ctrl+U)" aria-label="밑줄" onmousedown={keep} onclick={() => chain().toggleUnderline().run()}><Icon name="underline" /></button>
      <span class="has-pop">
        <button class="swatched" title="강조 표시" aria-label="강조 표시" aria-haspopup="true" aria-expanded={menu === 'highlight'} onmousedown={keep} onclick={() => (menu = menu === 'highlight' ? '' : 'highlight')}><Icon name="highlight" /><span class="cbar" style:background={lastHighlight}></span></button>
        {#if menu === 'highlight'}
          <div class="pop palette" role="menu" use:fitInView>
            {#each HIGHLIGHTS as c (c)}<button class="swatch" style:background={c} title={c} aria-label={c} onmousedown={keep} onclick={() => run(() => { lastHighlight = c; highlight(c); })}></button>{/each}
            <button class="wide" onmousedown={keep} onclick={() => run(() => highlight(null))}>없음</button>
          </div>
        {/if}
      </span>
      <span class="has-pop">
        <button class="swatched" title="글꼴 색" aria-label="글꼴 색" aria-haspopup="true" aria-expanded={menu === 'color'} onmousedown={keep} onclick={() => (menu = menu === 'color' ? '' : 'color')}><Icon name="color" /><span class="cbar" style:background={lastColor}></span></button>
        {#if menu === 'color'}
          <div class="pop palette" role="menu" use:fitInView>
            {#each COLORS as c (c)}<button class="swatch" style:background={c} title={c} aria-label={c} onmousedown={keep} onclick={() => run(() => { lastColor = c; chain().setColor(c).run(); })}></button>{/each}
            <button class="wide" onmousedown={keep} onclick={() => run(() => chain().unsetColor().run())}>자동</button>
          </div>
        {/if}
      </span>
      <span class="sep"></span>
      <button class:on={active('bulletList')} title="글머리 기호" aria-label="글머리 기호" onmousedown={keep} onclick={() => chain().toggleBulletList().run()}><Icon name="bullets" /></button>
      <button class:on={active('orderedList')} title="번호 매기기" aria-label="번호 매기기" onmousedown={keep} onclick={() => chain().toggleOrderedList().run()}><Icon name="numbers" /></button>
      <button title="내어쓰기" aria-label="내어쓰기" onmousedown={keep} onclick={() => indent(false)}><Icon name="outdent" /></button>
      <button title="들여쓰기" aria-label="들여쓰기" onmousedown={keep} onclick={() => indent(true)}><Icon name="indent" /></button>
      <span class="sep"></span>
      <button class:on={active('paragraph', { textAlign: 'left' })} title="왼쪽 맞춤" aria-label="왼쪽 맞춤" onmousedown={keep} onclick={() => chain().setTextAlign('left').run()}><Icon name="alignLeft" /></button>
      <button class:on={active('paragraph', { textAlign: 'center' })} title="가운데" aria-label="가운데" onmousedown={keep} onclick={() => chain().setTextAlign('center').run()}><Icon name="alignCenter" /></button>
      <button class:on={active('paragraph', { textAlign: 'right' })} title="오른쪽 맞춤" aria-label="오른쪽 맞춤" onmousedown={keep} onclick={() => chain().setTextAlign('right').run()}><Icon name="alignRight" /></button>
      <span class="sep"></span>
      <button class:on={active('link')} title="하이퍼링크 삽입" aria-label="하이퍼링크 삽입" onmousedown={keep} onclick={link}><Icon name="link" /></button>
      <button disabled={!active('link')} title="하이퍼링크 제거" aria-label="하이퍼링크 제거" onmousedown={keep} onclick={() => chain().extendMarkRange('link').unsetLink().run()}><Icon name="unlink" /></button>
      <button class:on={active('superscript')} title="위 첨자" aria-label="위 첨자" onmousedown={keep} onclick={() => chain().toggleSuperscript().run()}><Icon name="superscript" /></button>
      <button class:on={active('subscript')} title="아래 첨자" aria-label="아래 첨자" onmousedown={keep} onclick={() => chain().toggleSubscript().run()}><Icon name="subscript" /></button>
      <button class:on={active('strike')} title="취소선" aria-label="취소선" onmousedown={keep} onclick={() => chain().toggleStrike().run()}><Icon name="strike" /></button>
      <button disabled={!active('image')} title="이미지에 대체 텍스트 추가" aria-label="이미지에 대체 텍스트 추가" onmousedown={keep} onclick={altText}><Icon name="altText" /></button>
      <button title="왼쪽에서 오른쪽" aria-label="왼쪽에서 오른쪽" onmousedown={keep} onclick={() => chain().setDirection('ltr').run()}><Icon name="ltr" /></button>
      <button title="오른쪽에서 왼쪽" aria-label="오른쪽에서 왼쪽" onmousedown={keep} onclick={() => chain().setDirection('rtl').run()}><Icon name="rtl" /></button>
      <span class="sep"></span>
      <button disabled={!can((e) => e.can().undo())} title="실행 취소 (Ctrl+Z)" aria-label="실행 취소" onmousedown={keep} onclick={() => chain().undo().run()}><Icon name="undo" /></button>
      <button disabled={!can((e) => e.can().redo())} title="다시 실행 (Ctrl+Y)" aria-label="다시 실행" onmousedown={keep} onclick={() => chain().redo().run()}><Icon name="redo" /></button>
      <button title="서식 제거" aria-label="서식 제거" onmousedown={keep} onclick={() => chain().unsetAllMarks().clearNodes().run()}><Icon name="clearFormat" /></button>
      <span class="has-pop">
        <button title="표 삽입" aria-label="표 삽입" aria-haspopup="true" aria-expanded={menu === 'table'} onmousedown={keep} onclick={() => (menu = menu === 'table' ? '' : 'table')}><Icon name="table" /></button>
        {#if menu === 'table'}
          <div class="pop table" role="menu" aria-label="표 삽입" use:fitInView>
            <div class="grid" role="presentation" onmouseleave={() => (grid = [0, 0])}>
              {#each Array.from({ length: 8 }, (_, r) => r + 1) as r (r)}
                {#each Array.from({ length: 8 }, (_, c) => c + 1) as c (c)}
                  <button
                    class="cell"
                    class:hot={r <= grid[0] && c <= grid[1]}
                    aria-label="{r}×{c}"
                    onmouseenter={() => (grid = [r, c])}
                    onmousedown={keep}
                    onclick={() => insertTable(r, c)}
                  ></button>
                {/each}
              {/each}
            </div>
            <div class="dims">{grid[0] ? `${grid[0]} × ${grid[1]}` : '표 삽입'}</div>
          </div>
        {/if}
      </span>
      <button class:on={active('blockquote')} title="인용" aria-label="인용" onmousedown={keep} onclick={() => chain().toggleBlockquote().run()}><Icon name="quote" /></button>
    </div>
  {/if}
  <div class="editor" bind:this={el} role="presentation" {oncontextmenu}></div>
  {#if ctx}
    <div class="ctx" role="menu" use:placeAt={{ x: ctx.x, y: ctx.y }}>
      {#each ctx.items as item, i (i)}
        {#if 'sep' in item}<div class="ctx-sep"></div>{:else}
          <button role="menuitem" disabled={item.disabled} onmousedown={keep} onclick={() => runCtx(item)}
            ><span>{item.label}</span>{#if item.key}<span class="ctx-key">{item.key}</span>{/if}</button
          >
        {/if}
      {/each}
    </div>
  {/if}
  {#if cellPalette}
    <div class="cellpal palette" role="menu" aria-label="셀 배경색" use:placeAt={cellPalette}>
      {#each COLORS as c (c)}<button class="swatch" style:background={c} title={c} aria-label={c} onmousedown={keep} onclick={() => { cellPalette = null; shadeCells(c); }}></button>{/each}
      <button class="wide" onmousedown={keep} onclick={() => { cellPalette = null; shadeCells(null); }}>없음</button>
    </div>
  {/if}
  {#if ctxNote}<div class="ctx-note" role="status">{ctxNote}</div>{/if}
  <input bind:this={picker} class="picker" type="file" accept="image/*" multiple onchange={picked} />
</div>

{#if sheetChoice}
  <Modal title="스프레드시트 붙여넣기" width={340} onclose={() => (sheetChoice = null)}>
    <div class="sheet-choice" data-sheet-paste>
      <button class="btn primary" use:autofocus onclick={() => void chooseSheet('table')}>표로</button>
      <button class="btn" onclick={() => void chooseSheet('image')}>그림으로</button>
    </div>
    <label class="sheet-remember"><input type="checkbox" bind:checked={sheetChoice.remember} /> 다음부터 묻지 않기</label>
  </Modal>
{/if}

<style>
  .sheet-choice {
    display: flex;
    gap: 8px;
  }

  .sheet-choice .btn {
    flex: 1;
    height: 36px;
  }

  .sheet-remember {
    display: flex;
    align-items: center;
    gap: 4px;
    margin-top: 10px;
    font-size: 12.5px;
    color: var(--fg-muted);
  }

  .wrap {
    display: flex;
    flex-direction: column;
    min-height: 0;
  }

  /* 작성 화면 전체가 스크롤되므로(1.0.5) 본문을 길게 써도 서식 도구가 위에 붙어 있게. */
  .bar {
    position: sticky;
    top: 0;
    z-index: 6;
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 2px;
    padding: 4px 6px;
    border-bottom: 1px solid var(--border);
    background: var(--bg);
  }

  .bar > button,
  .has-pop > button {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 28px;
    height: 26px;
    padding: 0;
    border: 0;
    border-radius: 4px;
    background: none;
    color: var(--fg);
    cursor: pointer;
  }

  .bar select {
    height: 26px;
    padding: 0 4px;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--bg);
    color: var(--fg);
    font: inherit;
    cursor: pointer;
  }

  .bar select.font {
    width: 112px;
  }

  .bar select.size {
    width: 62px;
  }

  .bar > button:hover:not(:disabled),
  .has-pop > button:hover {
    background: var(--bg-hover);
  }

  .bar button.on {
    background: var(--bg-sel-focus);
  }

  .bar > button:disabled {
    color: var(--fg-muted);
    opacity: 0.45;
    cursor: default;
  }

  /* 강조 표시·글꼴 색: 아이콘 아래 마지막 색 막대 */
  .swatched {
    flex-direction: column;
    gap: 1px;
  }

  .cbar {
    width: 14px;
    height: 3px;
    border-radius: 1px;
  }

  .sep {
    width: 1px;
    height: 18px;
    margin: 0 3px;
    background: var(--border);
  }

  .has-pop {
    position: relative;
    display: inline-flex;
  }

  .pop {
    position: absolute;
    top: calc(100% + 2px);
    left: 0;
    z-index: 20;
    padding: 6px;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--bg);
    box-shadow: 0 4px 14px rgb(0 0 0 / 0.16);
  }

  .palette {
    display: grid;
    grid-template-columns: repeat(6, 20px);
    gap: 4px;
  }

  .swatch {
    width: 20px;
    height: 20px;
    padding: 0;
    border: 1px solid rgb(0 0 0 / 0.2);
    border-radius: 3px;
    cursor: pointer;
  }

  .wide {
    grid-column: 1 / -1;
    height: 24px;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--bg-pane);
    color: var(--fg);
    font: inherit;
    font-size: 12px;
    cursor: pointer;
  }

  .table {
    right: 0;
    left: auto;
  }

  .grid {
    display: grid;
    grid-template-columns: repeat(8, 16px);
    gap: 2px;
  }

  .cell {
    width: 16px;
    height: 16px;
    padding: 0;
    border: 1px solid var(--border);
    background: var(--bg);
    cursor: pointer;
  }

  .cell.hot {
    border-color: var(--accent);
    background: rgb(15 108 189 / 0.18);
  }

  .dims {
    margin-top: 4px;
    font-size: 12px;
    text-align: center;
    color: var(--fg-muted);
  }

  .picker {
    display: none;
  }

  .ctx {
    position: fixed;
    z-index: 50;
    display: flex;
    flex-direction: column;
    min-width: 200px;
    padding: 4px 0;
    overflow-y: auto;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--bg);
    box-shadow: 0 6px 20px rgb(0 0 0 / 0.18);
  }

  .ctx button {
    display: flex;
    justify-content: space-between;
    gap: 24px;
    padding: 5px 14px;
    border: 0;
    background: none;
    color: var(--fg);
    font: inherit;
    font-size: 12.5px;
    text-align: left;
    white-space: nowrap;
    cursor: pointer;
  }

  .ctx button:hover:not(:disabled) {
    background: var(--bg-hover);
  }

  .ctx button:disabled {
    color: var(--fg-muted);
    cursor: default;
  }

  .ctx-key {
    color: var(--fg-muted);
  }

  .cellpal {
    position: fixed;
    z-index: 50;
    padding: 6px;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--bg);
    box-shadow: 0 6px 20px rgb(0 0 0 / 0.18);
  }

  .ctx-sep {
    height: 1px;
    margin: 4px 0;
    background: var(--border);
  }

  .ctx-note {
    position: fixed;
    right: 12px;
    bottom: 12px;
    padding: 6px 12px;
    border-radius: 6px;
    background: #323130;
    color: #fff;
    font-size: 12.5px;
  }

  .editor {
    flex: 1;
    min-height: 160px;
    overflow-y: auto;
    padding: 10px 14px;
    cursor: text;
  }

  .editor :global(.tiptap) {
    min-height: 140px;
    outline: none;
    font-family: 'Malgun Gothic', 'Segoe UI', sans-serif;
    font-size: 13.5px;
    line-height: 1.55;
  }

  .editor :global(.tiptap p) {
    margin: 0 0 6px;
  }

  .editor :global(.tiptap blockquote) {
    margin: 0 0 6px 0.6em;
    padding-left: 0.8em;
    border-left: 2px solid var(--border);
  }

  .editor :global(.tiptap a) {
    color: var(--accent);
  }

  .editor :global(.tiptap img) {
    max-width: 100%;
    height: auto;
  }

  .editor :global(.tiptap img.ProseMirror-selectednode) {
    outline: 2px solid var(--accent);
  }

  .editor :global(.tiptap table) {
    margin: 4px 0;
  }

  .editor :global(.tiptap td p),
  .editor :global(.tiptap th p) {
    margin: 0;
  }

  .editor :global(.tiptap .selectedCell) {
    background: rgb(15 108 189 / 0.12);
  }

  /* 열 경계 끌기(prosemirror-tables) */
  .editor :global(.tiptap .tableWrapper) {
    overflow-x: auto;
  }

  .editor :global(.tiptap td),
  .editor :global(.tiptap th) {
    position: relative;
  }

  .editor :global(.tiptap .column-resize-handle) {
    position: absolute;
    top: 0;
    right: -2px;
    bottom: -2px;
    z-index: 2;
    width: 4px;
    background: var(--accent);
    pointer-events: none;
  }

  .editor :global(.tiptap.resize-cursor) {
    cursor: col-resize;
  }

  .editor :global(.tiptap p.is-editor-empty:first-child::before) {
    content: attr(data-placeholder);
    float: left;
    height: 0;
    color: var(--fg-muted);
    pointer-events: none;
  }
</style>
