<script lang="ts">
  import type { ModelInfo } from './api';
  import { searchModels } from './modelSearch';

  let {
    models,
    value = $bindable(),
    price,
  }: { models: ModelInfo[]; value: string; price: (m: ModelInfo) => string } = $props();

  const LIMIT = 100;
  let open = $state(false);
  let active = $state(0);
  let listEl = $state<HTMLUListElement>();

  const hits = $derived(searchModels(models, value));
  const shown = $derived(hits.slice(0, LIMIT));

  function pick(m: ModelInfo) {
    value = m.id;
    open = false;
  }

  function onkeydown(e: KeyboardEvent) {
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault();
      if (!open) {
        open = true;
        return;
      }
      const n = shown.length;
      if (!n) return;
      active = (active + (e.key === 'ArrowDown' ? 1 : n - 1)) % n;
      listEl?.children[active]?.scrollIntoView({ block: 'nearest' });
    } else if (e.key === 'Enter' && open && shown[active]) {
      e.preventDefault();
      pick(shown[active]);
    } else if (e.key === 'Escape' && open) {
      e.stopPropagation();
      open = false;
    }
  }
</script>

<div class="picker">
  <input
    bind:value
    placeholder="vendor/model, or words: gemini flash"
    autocomplete="off"
    spellcheck="false"
    onfocus={() => (open = true)}
    onblur={() => (open = false)}
    oninput={() => {
      open = true;
      active = 0;
    }}
    {onkeydown}
  />
  {#if open && shown.length}
    <ul bind:this={listEl} role="listbox">
      {#each shown as m, i (m.id)}
        <!-- mousedown + preventDefault keeps focus in the input so blur doesn't close first -->
        <li
          role="option"
          aria-selected={i === active}
          class:active={i === active}
          onmousedown={(e) => {
            e.preventDefault();
            pick(m);
          }}
          onmouseenter={() => (active = i)}
        >
          <span class="id">{m.id}</span>
          <span class="meta">{m.name} · {price(m)}</span>
        </li>
      {/each}
      {#if hits.length > LIMIT}
        <li class="more">…{hits.length - LIMIT} more, type more words</li>
      {/if}
    </ul>
  {/if}
</div>

<style>
  .picker { position: relative; display: flex; flex-direction: column; }
  input { font: inherit; padding: 6px; }
  ul {
    position: absolute;
    top: 100%;
    left: 0;
    right: 0;
    z-index: 10;
    margin: 2px 0 0;
    padding: 0;
    list-style: none;
    max-height: 280px;
    overflow-y: auto;
    background: Canvas;
    color: CanvasText;
    border: 1px solid #8886;
    border-radius: 4px;
    box-shadow: 0 4px 12px #0003;
  }
  li { padding: 4px 8px; cursor: pointer; display: flex; flex-direction: column; }
  li.active { background: #8883; }
  .id { font-family: ui-monospace, monospace; font-size: 0.9em; }
  .meta, .more { color: #888; font-size: 0.8em; }
  .more { cursor: default; }
</style>
