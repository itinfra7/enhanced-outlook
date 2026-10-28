// 메일 인쇄(0.7.0, 웹메일에 있는 기능): 제목·보낸 사람·날짜·받는 사람·참조·첨부 이름을 위에 붙여 본문과 함께
// Windows 인쇄 창으로 보낸다. 본문은 읽기 창과 같은 정화된 문서이고 스크립트는 돌지 않는다(allow-scripts 없음).
import { fullDate } from './format';
import type { MessageView } from './types';

function esc(s: string): string {
  return s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');
}

/** 인쇄할 문서(머리글 + 본문). */
export function printDocument(msg: MessageView): string {
  const row = (k: string, v: string) =>
    v ? `<tr><td style="padding:1px 12px 1px 0;color:#555;white-space:nowrap;vertical-align:top">${k}</td><td>${esc(v)}</td></tr>` : '';
  const head =
    `<div style="font-family:'Malgun Gothic','맑은 고딕',sans-serif;font-size:10pt;color:#000;` +
    `border-bottom:1px solid #999;margin:0 0 12px;padding:0 0 8px">` +
    `<div style="font-size:14pt;font-weight:bold;margin:0 0 6px">${esc(msg.subject || '(제목 없음)')}</div>` +
    `<table style="border-collapse:collapse;font-size:10pt">` +
    row('보낸 사람', msg.from) +
    row('날짜', fullDate(msg.date)) +
    row('받는 사람', msg.to) +
    row('참조', msg.cc) +
    row('첨부', msg.attachments.map((a) => a.name).join(', ')) +
    `</table></div>`;
  return /<body[^>]*>/i.test(msg.html)
    ? msg.html.replace(/<body([^>]*)>/i, (_m, a: string) => `<body${a}>${head}`)
    : `<!doctype html><html><head><meta charset="utf-8"></head><body>${head}${msg.html}</body></html>`;
}

/** 인쇄 창을 연다(본문 그림을 받을 때까지 조금 기다린다). */
export function printMessage(msg: MessageView) {
  const frame = document.createElement('iframe');
  // allow-modals: 인쇄 창을 띄우는 데 필요. allow-scripts는 넣지 않는다.
  frame.setAttribute('sandbox', 'allow-same-origin allow-modals');
  frame.setAttribute('aria-hidden', 'true');
  frame.style.cssText = 'position:fixed;left:-10000px;top:0;width:800px;height:600px;border:0';
  frame.srcdoc = printDocument(msg);
  frame.onload = () => {
    setTimeout(() => {
      try {
        frame.contentWindow?.print();
      } finally {
        setTimeout(() => frame.remove(), 1000);
      }
    }, 300);
  };
  document.body.appendChild(frame);
}
