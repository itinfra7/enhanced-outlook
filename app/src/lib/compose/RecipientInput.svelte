<script lang="ts">
  // 받는 사람 칩 입력 + 자동완성(계획서 9.3, 5.9): 로컬 주소록(빈도순)을 바로 보여 주고 서버(FindPeople) 결과를
  // 이어 붙인다. Enter·Tab·쉼표·세미콜론으로 칩을 만들고, 빈 칸에서 Backspace면 마지막 칩을 지운다.
  // 0.9.0(사용자 지적): 고른 뒤 늦게 온 서버 결과가 제안 창을 다시 띄우지 않게 했고, ↑↓로 고른 사람을 Tab·Enter로
  // 넣는다. 한글 조합 중에 누른 ↑↓·Tab·Enter도 조합을 끝낸 뒤 한 번만 처리한다. 칩에는 이름 옆에 주소를 작게 보인다
  // (이름이 같고 주소가 다른 사람이 있다).
  import { api } from '../api';
  import type { Recipient } from '../types';

  interface Props {
    label: string;
    list: Recipient[];
    onchange: (list: Recipient[]) => void;
    /** 이름표를 누르면 주소록에서 고르기(웹메일의 "받는 사람" 단추) */
    onlabel?: () => void;
  }

  let { label, list, onchange, onlabel }: Props = $props();

  let text = $state('');
  let input: HTMLInputElement;
  let box = $state<HTMLUListElement | null>(null);
  let suggestions = $state<Recipient[]>([]);
  let cursor = $state(-1);
  let composing = false;
  /** Tab·Enter를 눌렀는데 제안이 아직 안 왔다: 곧 오면 고른다(그때까지의 시각, ms). */
  let pickUntil = 0;
  /** 한글 조합 중에 Tab·Enter를 눌렀다: 조합이 끝나면 고른다. */
  let pickAfterIme = false;
  /** 한글 조합 중에 누른 ↑↓·Tab·Enter(key "Process"). WebView2(Chromium)는 조합을 끝낸 뒤 같은 키를 한 번 더
   *  보내므로 그때 한 번만 처리한다(1.0.4 사용자 지적: 이름을 치고 처음 누른 ↓가 두 칸 움직였다). 다시 오지 않는
   *  입력기면 조합이 끝나고 잠시 뒤 처리한다. */
  let imePending: string | null = null;
  let imeTimer: ReturnType<typeof setTimeout> | undefined;
  /** 그렇게 대신 처리한 키: 늦게라도 같은 키가 오면 버린다. */
  let imeDone: { key: string; until: number } | null = null;
  /** 사용자가 ↑↓로 고른 뒤에는 늦게 온 결과가 커서를 맨 위로 되돌리지 않는다. */
  let moved = false;
  /** 지금 제안의 검색어와, 그 검색어의 주소록 결과가 왔는지. 조합이 끝날 때(compositionend) 같은 글자로 다시 찾으면
   *  ↑↓로 고른 것이 맨 위로 돌아가서(사용자 지적) 같은 검색어는 다시 찾지 않는다. */
  let lastQuery = '';
  let ready = false;
  let localTimer: ReturnType<typeof setTimeout> | undefined;
  let serverTimer: ReturnType<typeof setTimeout> | undefined;
  let seq = 0;

  const EMAIL = /^[^\s@<>,;]+@[^\s@<>,;]+\.[^\s@<>,;]+$/;

  /** "이름 <주소>", 주소, 여러 개(쉼표·세미콜론). */
  function parse(s: string): Recipient[] {
    return s
      .split(/[;,]/)
      .map((x) => x.trim())
      .filter(Boolean)
      .map((x) => {
        const m = /^(.*)<([^>]+)>\s*$/.exec(x);
        if (m) return { name: m[1].trim().replace(/^"|"$/g, ''), email: m[2].trim() };
        return { name: '', email: x };
      });
  }

  function add(items: Recipient[]) {
    const next = [...list];
    for (const r of items) {
      if (!next.some((x) => x.email.toLowerCase() === r.email.toLowerCase())) next.push(r);
    }
    onchange(next);
    text = '';
    close();
  }

  /** 제안 창을 닫고, 아직 오지 않은 결과도 버린다(늦게 온 서버 결과가 창을 다시 띄우던 문제). */
  function close() {
    seq++;
    clearTimeout(localTimer);
    clearTimeout(serverTimer);
    suggestions = [];
    cursor = -1;
    moved = false;
    pickUntil = 0;
    pickAfterIme = false;
    imePending = null;
    clearTimeout(imeTimer);
    lastQuery = '';
    ready = false;
  }

  /** 고른 사람(없으면 입력한 글)을 넣는다. */
  function pick() {
    if (cursor >= 0 && suggestions[cursor]) add([suggestions[cursor]]);
    else commit();
  }

  /** Tab·Enter: 지금 글자의 제안이 와 있으면 바로, 아니면 오는 대로 고른다. */
  function pickSoon() {
    // 이미 넣었다(빈 칸에서 "오는 대로 고르기"가 남으면 다음 사람을 치자마자 첫 제안이 들어간다).
    if (!text.trim()) return;
    if (ready && text.trim() === lastQuery) {
      pick();
      return;
    }
    pickUntil = Date.now() + 1500;
    if (text.trim() !== lastQuery) search();
  }

  function move(step: number) {
    if (!suggestions.length) return;
    cursor = (cursor + step + suggestions.length) % suggestions.length;
    moved = true;
    // 고른 줄이 보이게.
    queueMicrotask(() => box?.querySelectorAll('li')[cursor]?.scrollIntoView({ block: 'nearest' }));
  }

  function commit() {
    if (text.trim()) add(parse(text));
  }

  function remove(i: number) {
    onchange(list.filter((_, k) => k !== i));
    input?.focus();
  }

  function search() {
    const q = text.trim();
    if (q === lastQuery) return;
    clearTimeout(localTimer);
    clearTimeout(serverTimer);
    if (q.length < 1) {
      close();
      return;
    }
    lastQuery = q;
    const mine = ++seq;
    moved = false;
    ready = false;
    localTimer = setTimeout(async () => {
      const local = await api.contactsLocal(q).catch(() => []);
      if (mine !== seq) return;
      suggestions = local;
      cursor = local.length ? 0 : -1;
      ready = true;
      if (Date.now() < pickUntil) pick();
    }, 80);
    serverTimer = setTimeout(async () => {
      const server = await api.contactsServer(q).catch(() => []);
      if (mine !== seq || !server.length) return;
      const chosen = suggestions[cursor];
      const merged = [...suggestions];
      for (const s of server) {
        if (!merged.some((m) => m.email.toLowerCase() === s.email.toLowerCase())) merged.push(s);
      }
      suggestions = merged.slice(0, 12);
      // 고르던 사람은 그대로 고른 채로.
      const keep = chosen ? suggestions.indexOf(chosen) : -1;
      cursor = keep >= 0 ? keep : suggestions.length ? 0 : -1;
    }, 350);
  }

  /** ↑↓ 옮기기, Tab·Enter 고르기. */
  function run(key: string) {
    if (key === 'ArrowDown' || key === 'ArrowUp') {
      move(key === 'ArrowDown' ? 1 : -1);
    } else if (composing) {
      // 조합이 아직 안 끝났으면 글자가 확정된 뒤에(지금 칸을 비우면 조합하던 글자가 다시 들어온다).
      pickAfterIme = true;
    } else {
      pickSoon();
    }
  }

  function onkeydown(e: KeyboardEvent) {
    // 한글 조합 중에 누른 키는 key가 "Process"로 온다: 실제 키(code)로 본다.
    const imeKey = e.isComposing || e.keyCode === 229;
    const key = imeKey ? e.code.replace(/^Numpad/, '') : e.key;
    const arrow = key === 'ArrowDown' || key === 'ArrowUp';
    if (arrow || key === 'Enter' || key === 'Tab') {
      if (arrow ? !suggestions.length : !text.trim()) {
        // 대신 처리한 Tab·Enter 뒤에 늦게 온 같은 키(칸은 이미 비었다): 다음 칸으로 넘어가지 않게.
        if (imeDone?.key === key && Date.now() < imeDone.until) {
          imeDone = null;
          e.preventDefault();
        }
        return;
      }
      e.preventDefault();
      if (imeKey) {
        // 조합이 끝난 뒤 같은 키가 다시 온다: 그때 처리한다.
        imePending = key;
        clearTimeout(imeTimer);
        return;
      }
      if (imeDone?.key === key && Date.now() < imeDone.until) {
        imeDone = null;
        return;
      }
      imePending = null;
      clearTimeout(imeTimer);
      run(key);
      return;
    }
    if (imeKey) return;
    if (e.key === ',' || e.key === ';') {
      e.preventDefault();
      commit();
    } else if (e.key === 'Backspace' && !text && list.length) {
      remove(list.length - 1);
    } else if (e.key === 'Escape' && suggestions.length) {
      e.preventDefault();
      e.stopPropagation();
      close();
    }
  }

  function onpaste(e: ClipboardEvent) {
    const t = e.clipboardData?.getData('text') ?? '';
    if (/[;,]/.test(t)) {
      e.preventDefault();
      add(parse(t));
    }
  }

  export function focus() {
    input?.focus();
  }

  /** 입력하다 만 주소를 칩으로(이름 확인·보내기 전에). */
  export function commitPending() {
    commit();
  }
</script>

<div class="field">
  {#if onlabel}
    <button class="label pick" title="주소록에서 고르기" onclick={onlabel}>{label}</button>
  {:else}
    <span class="label">{label}</span>
  {/if}
  <div class="chips">
    {#each list as r, i (r.email + i)}
      <span class="chip" class:bad={!EMAIL.test(r.email)} title={r.name ? `${r.name} <${r.email}>` : r.email}>
        <span class="cn">{r.name || r.email}</span>{#if r.name && r.name !== r.email}<span class="ce">{r.email}</span>{/if}
        <button tabindex="-1" aria-label="{r.email} 빼기" onclick={() => remove(i)}>✕</button>
      </span>
    {/each}
    <input
      bind:this={input}
      bind:value={text}
      aria-label={label}
      autocomplete="off"
      spellcheck="false"
      oninput={search}
      {onkeydown}
      {onpaste}
      oncompositionstart={() => (composing = true)}
      oncompositionend={() => {
        composing = false;
        text = input.value;
        search();
        if (pickAfterIme) {
          pickAfterIme = false;
          setTimeout(pickSoon, 0);
        }
        // 조합 중에 누른 키가 다시 오지 않으면(그런 입력기) 여기서 처리한다.
        if (imePending) {
          const key = imePending;
          clearTimeout(imeTimer);
          imeTimer = setTimeout(() => {
            if (imePending !== key) return;
            imePending = null;
            imeDone = { key, until: Date.now() + 500 };
            run(key);
          }, 150);
        }
      }}
      onblur={() => setTimeout(() => {
        if (document.activeElement !== input) {
          commit();
          close();
        }
      }, 150)}
    />
  </div>
  {#if suggestions.length}
    <ul class="suggest" role="listbox" aria-label="주소 제안" bind:this={box}>
      {#each suggestions as s, i (s.email)}
        <li role="option" aria-selected={i === cursor}>
          <button class:on={i === cursor} tabindex="-1" onmouseenter={() => (cursor = i)} onmousedown={(e) => { e.preventDefault(); add([s]); }}>
            <span class="n">{s.name || s.email}</span>{#if s.name}<span class="e">{s.email}</span>{/if}
          </button>
        </li>
      {/each}
    </ul>
  {/if}
</div>

<style>
  .field {
    position: relative;
    display: flex;
    align-items: flex-start;
    gap: 8px;
    padding: 4px 0;
    border-bottom: 1px solid var(--border-soft);
  }

  .label {
    flex: none;
    width: 72px;
    padding-top: 5px;
    color: var(--fg-muted);
    font-size: 12.5px;
  }

  .label.pick {
    height: 24px;
    margin-top: 1px;
    padding: 0 6px;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--bg-pane);
    color: var(--fg);
    font: inherit;
    font-size: 12.5px;
    cursor: pointer;
  }

  .label.pick:hover {
    background: var(--bg-hover);
  }

  .chips {
    flex: 1;
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
    min-width: 0;
  }

  .chip {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    max-width: 420px;
    height: 24px;
    padding: 0 4px 0 8px;
    overflow: hidden;
    white-space: nowrap;
    border-radius: 12px;
    background: var(--bg-sel);
    font-size: 12.5px;
  }

  .cn {
    flex: none;
    max-width: 200px;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  /* 주소는 이름 오른쪽에 한 단계 작게(이름이 같고 주소가 다른 사람 구별, 0.9.0 사용자 요청). */
  .ce {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    color: var(--fg-muted);
    font-size: 11.5px;
  }

  .chip.bad .ce {
    color: inherit;
  }

  .chip.bad {
    background: #fde7e9;
    color: #c50f1f;
  }

  .chip button {
    border: 0;
    background: none;
    color: var(--fg-muted);
    font-size: 10px;
    cursor: pointer;
  }

  input {
    flex: 1;
    min-width: 160px;
    height: 26px;
    border: 0;
    outline: none;
    background: none;
    color: var(--fg);
    font: inherit;
  }

  .suggest {
    position: absolute;
    left: 80px;
    top: 100%;
    z-index: 5;
    min-width: 320px;
    max-height: 260px;
    margin: 0;
    padding: 4px 0;
    overflow-y: auto;
    list-style: none;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--bg);
    box-shadow: 0 6px 20px rgb(0 0 0 / 0.15);
  }

  .suggest button {
    display: flex;
    gap: 8px;
    width: 100%;
    padding: 5px 12px;
    border: 0;
    background: none;
    color: var(--fg);
    font: inherit;
    text-align: left;
    cursor: pointer;
  }

  /* 고른 줄(방향키·마우스 모두 cursor를 바꾼다): 흰 바탕에서도 분명히 보이게(1.0.3 사용자 지적, 전에는 --bg-hover라
     거의 안 보였다). */
  .suggest button.on {
    background: var(--bg-sel-focus);
  }

  .e {
    color: var(--fg-muted);
    font-size: 12px;
  }
</style>
