<script lang="ts">
  import { onMount, tick } from 'svelte';
  import * as api from './api';
  import type { Attachment, Message, Meta } from './api';
  import { pathTo, siblings } from './tree';
  import { renderMarkdown } from './markdown';
  import Settings from './Settings.svelte';

  let convs = $state<Meta[]>([]);
  let current = $state<Meta | null>(null);
  let messages = $state<Message[]>([]);
  let leaf = $state<string | null>(null);
  let streaming = $state<Message | null>(null);
  let input = $state('');
  let busy = $state(false);
  let error = $state('');
  let scroller: HTMLElement | undefined = $state();

  const path = $derived(pathTo(messages, leaf));
  let showSettings = $state(false);

  /** Usage summed over the shown branch. */
  const totals = $derived.by(() => {
    let tokIn = 0, tokOut = 0, cached = 0, cost = 0;
    for (const m of path) {
      if (!m.usage) continue;
      tokIn += m.usage.prompt_tokens;
      tokOut += m.usage.completion_tokens;
      cached += m.usage.cached_tokens;
      cost += m.usage.cost ?? 0;
    }
    return { tokIn, tokOut, cached, cost };
  });

  function onSaved(m: Meta) {
    current = m;
    refreshList();
  }

  async function refreshList() {
    convs = await api.listConversations();
    const fresh = current && convs.find((c) => c.id === current!.id);
    if (fresh) current = fresh;
  }

  async function open(id: string) {
    error = '';
    const c = await api.getConversation(id);
    current = c.meta;
    showSettings = false;
    messages = c.messages;
    leaf = c.leaf;
    location.hash = id;
    await scrollDown();
  }

  async function newChat() {
    const m = await api.createConversation();
    await refreshList();
    await open(m.id);
  }

  let renaming = $state<string | null>(null);
  let renameText = $state('');

  function startRename(c: Meta) {
    renaming = c.id;
    renameText = c.title;
  }

  async function submitRename() {
    const id = renaming;
    const title = renameText.trim();
    renaming = null;
    if (!id || !title) return;
    try {
      await api.patchConversation(id, { title });
      await refreshList();
    } catch (e) {
      error = String(e);
    }
  }

  function focus(el: HTMLInputElement) {
    el.focus();
    el.select();
  }

  async function remove(id: string) {
    if (!confirm('Delete this conversation?')) return;
    await api.deleteConversation(id);
    if (current?.id === id) {
      current = null;
      messages = [];
      leaf = null;
      location.hash = '';
    }
    await refreshList();
  }

  async function scrollDown() {
    await tick();
    scroller?.scrollTo({ top: scroller.scrollHeight });
  }

  function placeholder(parent: string, model: string): Message {
    return { id: '', parent_id: parent, role: 'assistant', content: '', model, created_at: Date.now() };
  }

  let attachments = $state<Attachment[]>([]);
  let fileInput: HTMLInputElement | undefined = $state();

  async function addFiles(files: FileList | null) {
    for (const f of files ?? []) {
      if (f.size > 2_000_000) {
        error = `${f.name}: too large (max 2 MB)`;
        continue;
      }
      attachments.push({ name: f.name, content: await f.text() });
    }
    if (fileInput) fileInput.value = '';
  }

  async function send() {
    const text = input.trim();
    if ((!text && !attachments.length) || busy) return;
    if (!current) await newChat();
    const atts = attachments;
    input = '';
    attachments = [];
    const ok = await run((conv, on) => api.sendMessage(conv.id, leaf, text, atts, on));
    if (!ok && !messages.some((m) => m.content === text)) {
      input = text;
      attachments = atts;
    }
  }

  async function stopGen() {
    if (!current) return;
    try {
      await api.stop(current.id);
    } catch (e) {
      error = String(e);
    }
  }

  // Inline edit of a user message: re-send as a sibling (same parent).
  let editing = $state<string | null>(null);
  let editText = $state('');

  function startEdit(m: Message) {
    editing = m.id;
    editText = m.content;
  }

  async function submitEdit(m: Message) {
    const text = editText.trim();
    if ((!text && !m.attachments?.length) || busy) return;
    editing = null;
    // Hide the old branch while the new one streams.
    leaf = m.parent_id;
    await run((conv, on) => api.sendMessage(conv.id, m.parent_id, text, m.attachments ?? [], on));
  }

  async function regen(m: Message) {
    if (busy || !m.parent_id) return;
    const userId = m.parent_id;
    leaf = userId;
    streaming = placeholder(userId, current!.model);
    await run((conv, on) => api.regenerate(conv.id, userId, on));
  }

  async function switchTo(m: Message, delta: number) {
    if (busy || !current) return;
    const sibs = siblings(messages, m);
    const next = sibs[sibs.indexOf(m) + delta];
    if (!next) return;
    try {
      leaf = await api.setLeaf(current.id, next.id);
    } catch (e) {
      error = String(e);
    }
  }

  /** Runs one streaming request against the current conversation. Returns false on failure. */
  async function run(
    start: (conv: Meta, on: (e: api.StreamEvent) => void) => Promise<void>,
  ): Promise<boolean> {
    const conv = current!;
    busy = true;
    error = '';
    let ok = true;
    try {
      await start(conv, (e) => {
        if (current?.id !== conv.id) return;
        switch (e.event) {
          case 'user_saved':
            messages.push(e.data);
            leaf = e.data.id;
            streaming = placeholder(e.data.id, conv.model);
            break;
          case 'model':
            if (streaming) streaming.model = e.data.model;
            break;
          case 'delta':
            if (streaming) streaming.content += e.data.text;
            break;
          case 'error':
            error = e.data.message;
            break;
          case 'done':
            streaming = null;
            messages.push(e.data);
            leaf = e.data.id;
            break;
        }
        scrollDown();
      });
    } catch (e) {
      error = String(e);
      ok = false;
    } finally {
      busy = false;
      streaming = null;
      refreshList();
    }
    return ok;
  }

  function onKey(e: KeyboardEvent) {
    if (e.key === 'Enter' && !e.shiftKey && !e.isComposing) {
      e.preventDefault();
      send();
    }
  }

  function usageLine(m: Message): string {
    const u = m.usage;
    const model = m.requested_model ? `${m.model} (via ${m.requested_model})` : m.model;
    if (!u) return model ?? '';
    const parts = [model, `${u.prompt_tokens} in`, `${u.completion_tokens} out`];
    if (u.cached_tokens) parts.push(`${u.cached_tokens} cached`);
    if (u.cost != null) parts.push(`$${u.cost.toFixed(5)}`);
    return parts.filter(Boolean).join(' · ');
  }

  onMount(async () => {
    try {
      await refreshList();
      const id = location.hash.slice(1);
      if (id) await open(id);
    } catch (e) {
      error = String(e);
    }
  });
</script>

<div class="app">
  <aside>
    <button class="new" onclick={newChat}>+ New chat</button>
    <ul>
      {#each convs as c (c.id)}
        <li class:active={current?.id === c.id}>
          {#if renaming === c.id}
            <input
              class="rename"
              bind:value={renameText}
              use:focus
              onblur={submitRename}
              onkeydown={(e) => {
                if (e.key === 'Enter') submitRename();
                else if (e.key === 'Escape') renaming = null;
              }}
            />
          {:else}
            <button class="title" onclick={() => open(c.id)} ondblclick={() => startRename(c)}>{c.title}</button>
            <button class="del" title="Rename" onclick={() => startRename(c)}>✎</button>
            <button class="del" title="Delete" onclick={() => remove(c.id)}>×</button>
          {/if}
        </li>
      {/each}
    </ul>
  </aside>

  <main>
    <header>
      <span class="htitle">{current ? `${current.title} — ${current.model}` : 'chatport'}</span>
      {#if current}
        {#if totals.tokIn || totals.tokOut}
          <span class="totals" title="Sum over the shown branch">
            {totals.tokIn} in · {totals.tokOut} out{totals.cached ? ` · ${totals.cached} cached` : ''} · ${totals.cost.toFixed(4)}
          </span>
        {/if}
        <button class="gear" title="Settings" onclick={() => (showSettings = !showSettings)}>⚙</button>
      {/if}
    </header>
    {#if showSettings && current}
      {#key current.id}
        <Settings meta={current} onsaved={onSaved} onclose={() => (showSettings = false)} />
      {/key}
    {/if}
    <div class="msgs" bind:this={scroller}>
      {#each [...path, ...(streaming ? [streaming] : [])] as m (m.id || 'streaming')}
        <div class="msg {m.role}">
          {#if m.role === 'user'}
            {#if editing === m.id}
              <textarea class="edit" bind:value={editText} rows="4"></textarea>
              <div class="actions">
                <button onclick={() => submitEdit(m)} disabled={busy || !editText.trim()}>Send</button>
                <button onclick={() => (editing = null)}>Cancel</button>
              </div>
            {:else}
              <div class="text plain">{m.content}</div>
            {/if}
            {#each m.attachments ?? [] as a}
              <div class="att">📎 {a.name}</div>
            {/each}
          {:else}
            <div class="text md">{@html renderMarkdown(m.content)}</div>
            {#if m.partial && !m.error && m !== streaming}<div class="att">(stopped)</div>{/if}
            {#if m.error}<div class="err">⚠ {m.error}</div>{/if}
          {/if}
          {#if m !== streaming && editing !== m.id}
            {@const sibs = siblings(messages, m)}
            <div class="meta">
              {#if sibs.length > 1}
                <span class="nav">
                  <button onclick={() => switchTo(m, -1)} disabled={busy || sibs[0] === m}>‹</button>
                  {sibs.indexOf(m) + 1}/{sibs.length}
                  <button onclick={() => switchTo(m, 1)} disabled={busy || sibs.at(-1) === m}>›</button>
                </span>
              {/if}
              {#if m.role === 'user'}
                <button onclick={() => startEdit(m)} disabled={busy}>Edit</button>
              {:else}
                <button onclick={() => regen(m)} disabled={busy}>Regenerate</button>
                <span>{usageLine(m)}</span>
              {/if}
            </div>
          {:else if m === streaming}
            <div class="meta">streaming…</div>
          {/if}
        </div>
      {/each}
      {#if error}<div class="err">⚠ {error}</div>{/if}
    </div>
    {#if attachments.length}
      <div class="chips">
        {#each attachments as a, i}
          <span class="chip">📎 {a.name} <button title="Remove" onclick={() => attachments.splice(i, 1)}>×</button></span>
        {/each}
      </div>
    {/if}
    <form onsubmit={(e) => { e.preventDefault(); send(); }}>
      <input
        type="file"
        accept=".txt,.md,.markdown,text/plain,text/markdown"
        multiple
        hidden
        bind:this={fileInput}
        onchange={(e) => addFiles(e.currentTarget.files)}
      />
      <button type="button" title="Attach text/markdown files" onclick={() => fileInput?.click()}>📎</button>
      <textarea
        bind:value={input}
        onkeydown={onKey}
        rows="3"
        placeholder="Message (Enter to send, Shift+Enter for newline)"
      ></textarea>
      {#if busy}
        <button type="button" onclick={stopGen}>Stop</button>
      {:else}
        <button type="submit" disabled={!input.trim() && !attachments.length}>Send</button>
      {/if}
    </form>
  </main>
</div>

<style>
  :global(:root) {
    color-scheme: light dark;
    --bg: #f6f6f4;
    --fg: #222;
    --field: #fff;
    --border: #c8c8c4;
    --btn: #e6e6e2;
    --accent: #3b6fd8;
    --accent-fg: #fff;
  }
  @media (prefers-color-scheme: dark) {
    :global(:root) {
      --bg: #1b1b1d;
      --fg: #ddd;
      --field: #26262a;
      --border: #3a3a40;
      --btn: #2f2f35;
      --accent: #4a6fb3;
      --accent-fg: #f0f0f0;
    }
  }
  :global(html, body) {
    margin: 0;
    height: 100%;
    font-family: system-ui, sans-serif;
    background: var(--bg);
    color: var(--fg);
  }
  :global(textarea, input:not([type='checkbox']), select) {
    background: var(--field);
    color: var(--fg);
    border: 1px solid var(--border);
    border-radius: 6px;
  }
  :global(textarea:focus, input:focus, select:focus) {
    outline: 1px solid var(--accent);
    border-color: var(--accent);
  }
  :global(button) {
    background: var(--btn);
    color: var(--fg);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 6px 12px;
    font: inherit;
    cursor: pointer;
  }
  :global(button:disabled) { opacity: 0.5; cursor: default; }
  :global(button.primary, form button[type='submit']) {
    background: var(--accent);
    color: var(--accent-fg);
    border-color: var(--accent);
  }
  :global(#app) { height: 100%; }
  .app { display: flex; height: 100%; }
  aside {
    width: 240px;
    border-right: 1px solid #8884;
    display: flex;
    flex-direction: column;
    overflow-y: auto;
  }
  .new { margin: 8px; padding: 8px; }
  ul { list-style: none; margin: 0; padding: 0; }
  li { display: flex; }
  li.active { background: #8882; }
  .title {
    flex: 1;
    text-align: left;
    background: none;
    border: none;
    padding: 8px 10px;
    color: inherit;
    cursor: pointer;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .del { background: none; border: none; color: #888; cursor: pointer; padding: 0 6px; }
  .rename { flex: 1; margin: 4px 6px; padding: 4px 6px; font: inherit; }
  main { flex: 1; display: flex; flex-direction: column; min-width: 0; }
  header {
    padding: 10px 16px;
    border-bottom: 1px solid #8884;
    font-weight: 600;
    display: flex;
    align-items: center;
    gap: 12px;
  }
  .htitle { flex: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .totals { font-weight: normal; font-size: 0.8em; color: #888; white-space: nowrap; }
  .gear { background: none; border: none; color: inherit; cursor: pointer; font-size: 1.1em; }
  .msgs { flex: 1; overflow-y: auto; padding: 16px; }
  .msg { max-width: 800px; margin: 0 auto 16px; }
  .msg.user .text {
    background: #8882;
    border-radius: 8px;
    padding: 8px 12px;
  }
  .plain { white-space: pre-wrap; }
  .md :global(pre) { background: #8882; padding: 8px; overflow-x: auto; border-radius: 6px; }
  .md :global(code) { font-family: ui-monospace, monospace; font-size: 0.9em; }
  .md :global(table) { border-collapse: collapse; }
  .md :global(td), .md :global(th) { border: 1px solid #8886; padding: 4px 8px; }
  .meta, .att { font-size: 0.8em; color: #888; margin-top: 4px; }
  .meta { display: flex; gap: 8px; align-items: center; }
  .msg.user .meta { justify-content: flex-end; }
  .meta button {
    background: none;
    border: none;
    color: inherit;
    cursor: pointer;
    font-size: inherit;
    padding: 0 2px;
  }
  .meta button:disabled { opacity: 0.4; cursor: default; }
  .edit { width: 100%; box-sizing: border-box; font: inherit; padding: 8px; }
  .actions { display: flex; gap: 8px; justify-content: flex-end; margin-top: 4px; }
  .err { color: #c44; margin-top: 4px; }
  .chips { display: flex; flex-wrap: wrap; gap: 6px; padding: 8px 16px 0; font-size: 0.85em; }
  .chip { background: #8882; border-radius: 12px; padding: 2px 8px; }
  .chip button { background: none; border: none; color: #888; cursor: pointer; }
  form { display: flex; gap: 8px; padding: 12px 16px; border-top: 1px solid #8884; }
  textarea { flex: 1; resize: vertical; font: inherit; padding: 8px; }
  @media (max-width: 640px) {
    aside { display: none; }
  }
</style>
