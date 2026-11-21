<script lang="ts">
  import { onMount, tick } from 'svelte';
  import * as api from './api';
  import type { Message, Meta } from './api';
  import { pathTo, siblings } from './tree';
  import { renderMarkdown } from './markdown';

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

  async function refreshList() {
    convs = await api.listConversations();
  }

  async function open(id: string) {
    error = '';
    const c = await api.getConversation(id);
    current = c.meta;
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

  async function send() {
    const text = input.trim();
    if (!text || busy) return;
    if (!current) await newChat();
    input = '';
    const ok = await run((conv, on) => api.sendMessage(conv.id, leaf, text, on));
    if (!ok && !messages.some((m) => m.content === text)) input = text;
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
    if (!text || busy) return;
    editing = null;
    // Hide the old branch while the new one streams.
    leaf = m.parent_id;
    await run((conv, on) => api.sendMessage(conv.id, m.parent_id, text, on));
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
    if (!u) return m.model ?? '';
    const parts = [m.model, `${u.prompt_tokens} in`, `${u.completion_tokens} out`];
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
          <button class="title" onclick={() => open(c.id)}>{c.title}</button>
          <button class="del" title="Delete" onclick={() => remove(c.id)}>×</button>
        </li>
      {/each}
    </ul>
  </aside>

  <main>
    <header>{current ? `${current.title} — ${current.model}` : 'chatport'}</header>
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
    <form onsubmit={(e) => { e.preventDefault(); send(); }}>
      <textarea
        bind:value={input}
        onkeydown={onKey}
        rows="3"
        placeholder="Message (Enter to send, Shift+Enter for newline)"
      ></textarea>
      <button type="submit" disabled={busy || !input.trim()}>Send</button>
    </form>
  </main>
</div>

<style>
  :global(html, body) {
    margin: 0;
    height: 100%;
    font-family: system-ui, sans-serif;
    background: #f6f6f4;
    color: #222;
  }
  :global(#app) { height: 100%; }
  @media (prefers-color-scheme: dark) {
    :global(html, body) { background: #1b1b1d; color: #ddd; }
  }
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
  .del { background: none; border: none; color: #888; cursor: pointer; }
  main { flex: 1; display: flex; flex-direction: column; min-width: 0; }
  header { padding: 10px 16px; border-bottom: 1px solid #8884; font-weight: 600; }
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
  form { display: flex; gap: 8px; padding: 12px 16px; border-top: 1px solid #8884; }
  textarea { flex: 1; resize: vertical; font: inherit; padding: 8px; }
  @media (max-width: 640px) {
    aside { display: none; }
  }
</style>
