// 펼친 고르기 창·메뉴가 창 밖으로 잘리지 않게(작성 창을 작게 열었을 때, 사용자 요청 2026-10-21).

const M = 4;

/** 단추 아래에 펼치는 창(position:absolute): 좌우로 넘치면 밀고, 아래가 모자라면 위로 펼친다. */
export function fitInView(node: HTMLElement) {
  const place = () => {
    node.style.transform = '';
    node.style.top = '';
    node.style.bottom = '';
    node.style.maxHeight = '';
    node.style.overflowY = '';
    const r = node.getBoundingClientRect();
    const vw = window.innerWidth;
    const vh = window.innerHeight;
    let dx = 0;
    if (r.right > vw - M) dx = vw - M - r.right;
    if (r.left + dx < M) dx = M - r.left;
    if (r.bottom > vh - M) {
      const below = vh - M - r.top;
      node.style.top = 'auto';
      node.style.bottom = 'calc(100% + 2px)';
      const up = node.getBoundingClientRect();
      if (up.top < M) {
        // 위아래 다 모자라면 더 넓은 쪽에 두고 스크롤.
        const above = up.bottom - M;
        if (below >= above) {
          node.style.top = '';
          node.style.bottom = '';
          node.style.maxHeight = `${below}px`;
        } else node.style.maxHeight = `${above}px`;
        node.style.overflowY = 'auto';
      }
    }
    if (dx) node.style.transform = `translateX(${dx}px)`;
  };
  place();
  window.addEventListener('resize', place);
  return {
    destroy() {
      window.removeEventListener('resize', place);
    },
  };
}

/** 마우스 위치에 띄우는 메뉴(position:fixed): 창 안에 들어오게 자리를 잡는다. */
export function placeAt(node: HTMLElement, at: { x: number; y: number }) {
  const place = (p: { x: number; y: number }) => {
    const r = node.getBoundingClientRect();
    const x = Math.max(M, Math.min(p.x, window.innerWidth - M - r.width));
    const y = Math.max(M, Math.min(p.y, window.innerHeight - M - r.height));
    node.style.left = `${x}px`;
    node.style.top = `${y}px`;
    node.style.maxHeight = `${window.innerHeight - 2 * M}px`;
  };
  place(at);
  return { update: place };
}
