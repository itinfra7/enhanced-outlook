<script lang="ts">
  // 작성 창 도구 아이콘(icons.ts의 선 그림).
  import { ICONS } from './icons';

  interface Props {
    name: string;
    size?: number;
  }

  let { name, size = 16 }: Props = $props();
  let paths = $derived(ICONS[name] ?? []);
</script>

<svg
  class="icon"
  viewBox="0 0 16 16"
  width={size}
  height={size}
  fill="none"
  stroke="currentColor"
  stroke-width="1.4"
  stroke-linecap="round"
  stroke-linejoin="round"
  aria-hidden="true"
>
  {#each paths as d, i (i)}
    {#if d.startsWith('!')}<path d={d.slice(1)} fill="currentColor" stroke="none" />{:else}<path {d} />{/if}
  {/each}
</svg>

<style>
  .icon {
    display: block;
    flex: none;
  }
</style>
