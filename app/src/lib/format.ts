// 날짜·크기 표시(이 PC의 지역 시간 기준).

const DAYS = ['일', '월', '화', '수', '목', '금', '토'];

function pad(n: number): string {
  return n < 10 ? `0${n}` : String(n);
}

/** 목록용: 오늘이면 시:분, 올해면 월/일, 그 밖에는 연-월-일. */
export function listDate(ms: number, now = new Date()): string {
  const d = new Date(ms);
  if (d.toDateString() === now.toDateString()) return `${pad(d.getHours())}:${pad(d.getMinutes())}`;
  if (d.getFullYear() === now.getFullYear()) return `${d.getMonth() + 1}/${d.getDate()}`;
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
}

/** 읽기 창용: 2026-10-16 (금) 14:06 */
export function fullDate(ms: number): string {
  const d = new Date(ms);
  return (
    `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} (${DAYS[d.getDay()]}) ` +
    `${pad(d.getHours())}:${pad(d.getMinutes())}`
  );
}

export function clock(ms: number): string {
  const d = new Date(ms);
  return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}

export function bytes(n: number): string {
  if (n < 1024) return `${n}B`;
  if (n < 1024 * 1024) return `${Math.round(n / 1024)}KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(n < 10 * 1024 * 1024 ? 1 : 0)}MB`;
  return `${(n / 1024 / 1024 / 1024).toFixed(1).replace(/\.0$/, '')}GB`;
}
