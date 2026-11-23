<script lang="ts">
  import { onMount, tick } from 'svelte';
  import * as api from './api';
  import type { Attachment, Message, Meta, Summary } from './api';
  import { pathTo, siblings } from './tree';
  import { renderMarkdown } from './markdown';
  import { codeCopy, copyWithFeedback, downloadText, openHtml, openText } from './clipboard';
  import Settings from './Settings.svelte';
  import MessageInput from './MessageInput.svelte';

  let convs = $state<Summary[]>([]);
  /** Group shown in the main pane (no chat open). */
  let viewGroup = $state<string | null>(null);
  let deletingGroup = $state<string | null>(null);

  const EXPANDED_KEY = 'chatport.expandedGroups';
  function loadExpanded(): string[] {
    try {
      return JSON.parse(localStorage.getItem(EXPANDED_KEY) ?? '[]');
    } catch {
      return [];
    }
  }
  let expanded = $state<string[]>(loadExpanded());
  function toggleGroup(g: string) {
    expanded = expanded.includes(g) ? expanded.filter((x) => x !== g) : [...expanded, g];
    try {
      localStorage.setItem(EXPANDED_KEY, JSON.stringify(expanded));
    } catch {
      /* collapse state is optional */
    }
  }

  /** Group name -> chats (newest first), names sorted. */
  const groups = $derived.by(() => {
    const m = new Map<string, Summary[]>();
    for (const c of convs) if (c.group) m.set(c.group, [...(m.get(c.group) ?? []), c]);
    return new Map([...m].sort(([a], [b]) => a.localeCompare(b)));
  });
  const ungrouped = $derived(convs.filter((c) => !c.group));
  /** Open chat; a draft (id "") lives only here until its first message. */
  let current = $state<Meta | null>(null);
  const isDraft = $derived(current?.id === '');
  /** Group a draft will be created in. */
  let draftGroup = $state<string | null>(null);
  let messages = $state<Message[]>([]);
  let leaf = $state<string | null>(null);
  let streaming = $state<Message | null>(null);
  let input = $state('');
  let busy = $state(false);
  let error = $state('');
  let scroller: HTMLElement | undefined = $state();

  const path = $derived(pathTo(messages, leaf));
  let showSettings = $state(false);
  let showSidebar = $state(false); // phone only
  /** Row whose action buttons are expanded ('c:<id>' or 'g:<name>'). */
  let rowMenu = $state<string | null>(null);
  const toggleMenu = (k: string) => (rowMenu = rowMenu === k ? null : k);

  // Desktop sidebar width, drag-resizable, remembered per browser.
  let sideWidth = $state(240);
  try {
    const w = Number(localStorage.getItem('sideWidth'));
    if (w) sideWidth = w;
  } catch {}
  function startResize(e: PointerEvent) {
    e.preventDefault();
    const x0 = e.clientX, w0 = sideWidth;
    const move = (ev: PointerEvent) => {
      sideWidth = Math.min(600, Math.max(160, w0 + ev.clientX - x0));
    };
    const up = () => {
      window.removeEventListener('pointermove', move);
      window.removeEventListener('pointerup', up);
      try { localStorage.setItem('sideWidth', String(sideWidth)); } catch {}
    };
    window.addEventListener('pointermove', move);
    window.addEventListener('pointerup', up);
  }

  /** Usage summed over the shown branch. */
  const totals = $derived.by(() => {
    let tokIn = 0, tokOut = 0, cached = 0, cost = 0;
    for (const m of path) {
      if (!m.usage) continue;
      tokIn += m.usage.prompt_tokens;
      tokOut += m.usage.completion_tokens;
      cached += m.usage.cached_tokens;
      cost += api.totalCost(m.usage) ?? 0;
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
    // List titles are folder labels; only pick up server-side title changes.
    if (fresh && current && fresh.title !== current.title) current.title = fresh.title;
  }

  async function open(id: string) {
    error = '';
    const c = await api.getConversation(id);
    current = c.meta;
    viewGroup = null;
    showSettings = false;
    showSidebar = false;
    messages = c.messages;
    leaf = c.leaf;
    location.hash = id;
    await scrollDown();
  }

  /** Shows a draft chat; nothing is saved until the first message. */
  async function newChat() {
    error = '';
    const d = await api.draftConversation();
    draftGroup = viewGroup;
    current = d;
    viewGroup = null;
    showSettings = false;
    showSidebar = false;
    messages = [];
    leaf = null;
    if (location.hash) history.pushState(null, '', location.pathname + location.search);
  }

  /** Saves the draft (or a fresh chat in the shown group) as a real chat. */
  async function materialize() {
    const d = current ?? (await api.draftConversation());
    const group = current ? draftGroup : viewGroup;
    const m = await api.createConversation({ ...api.draftPatch(d), group });
    current = m;
    viewGroup = null;
    draftGroup = null;
    messages = [];
    leaf = null;
    location.hash = m.id;
    await refreshList();
  }

  let renaming = $state<string | null>(null);
  let renameText = $state('');

  function openGroup(g: string) {
    current = null;
    messages = [];
    leaf = null;
    showSettings = false;
    showSidebar = false;
    viewGroup = g;
    location.hash = 'g/' + encodeURIComponent(g);
  }

  async function act(f: () => Promise<void>) {
    try {
      error = '';
      await f();
    } catch (e) {
      error = String(e);
    }
  }

  function moveChat(c: Summary) {
    const to = prompt('Move to group (new name creates it, empty = ungrouped):', c.group ?? '');
    if (to === null || to.trim() === (c.group ?? '')) return;
    act(async () => {
      await api.patchConversation(c.id, { group: to.trim() || null });
      await refreshList();
    });
  }

  function renameGroupUi(g: string) {
    const to = prompt('Rename group:', g)?.trim();
    if (!to || to === g) return;
    act(async () => {
      await api.renameGroup(g, to);
      if (expanded.includes(g)) {
        toggleGroup(g);
        toggleGroup(to);
      }
      if (viewGroup === g) openGroup(to);
      await refreshList();
    });
  }

  function removeGroup(g: string, withChats: boolean) {
    deletingGroup = null;
    if (withChats && !confirm(`Delete group "${g}" and all its chats? This can't be undone.`)) return;
    const ids = (groups.get(g) ?? []).map((c) => c.id);
    act(async () => {
      await api.deleteGroup(g, withChats);
      if (viewGroup === g) {
        viewGroup = null;
        location.hash = '';
      }
      if (withChats && current && ids.includes(current.id)) {
        current = null;
        messages = [];
        leaf = null;
        location.hash = '';
      }
      await refreshList();
    });
  }

  function startRename(c: Summary) {
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

  async function send(text: string, atts: Attachment[]) {
    if (busy) return;
    if (current?.pending_batch) {
      error = 'A batch reply is pending: ⟳ to fetch it, ✕ to forget it.';
      return;
    }
    if (!current || isDraft) {
      try {
        await materialize();
      } catch (e) {
        error = String(e);
        return;
      }
    }
    const prevText = input;
    const prevAtts = attachments;
    input = '';
    attachments = [];
    const ok = await run((conv, on) => api.sendMessage(conv.id, leaf, text, atts, on));
    if (!ok && !messages.some((m) => m.content === text)) {
      input = prevText;
      attachments = prevAtts;
    }
  }

  let batchBusy = $state(false);

  async function refreshBatch() {
    if (!current?.pending_batch || batchBusy) return;
    const conv = current;
    batchBusy = true;
    error = '';
    try {
      const r = await api.refreshBatch(conv.id);
      if (current?.id !== conv.id) return;
      if (r.message) {
        messages.push(r.message);
        leaf = r.message.id;
        current.pending_batch = null;
        if (r.message.error) error = r.message.error;
        await scrollDown();
        refreshList();
      } else if (r.status === 'done') {
        // Saved elsewhere (the server's auto-poll): reload to show it.
        await open(conv.id);
      } else if (current.pending_batch) {
        const pb = current.pending_batch;
        pb.status = r.status;
        if (r.provider) pb.provider = r.provider;
        if (r.expires_at) pb.expires_at = r.expires_at;
      }
    } catch (e) {
      error = String(e);
    } finally {
      batchBusy = false;
    }
  }

  async function forgetBatch() {
    if (!current?.pending_batch) return;
    const ok = confirm(
      'Forget this batch? OpenRouter cannot cancel a running batch: it keeps running and is billed, its reply is just not saved.',
    );
    if (!ok) return;
    try {
      await api.forgetBatch(current.id);
      current.pending_batch = null;
    } catch (e) {
      error = String(e);
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

  /** Attachments kept for the edited message; × removes one. */
  let editAtts = $state<Attachment[]>([]);

  function startEdit(m: Message) {
    editing = m.id;
    editText = m.content;
    editAtts = [...(m.attachments ?? [])];
  }

  async function submitEdit(m: Message, text: string, atts: Attachment[]) {
    if (busy) return;
    editing = null;
    // Hide the old branch while the new one streams.
    leaf = m.parent_id;
    await run((conv, on) => api.sendMessage(conv.id, m.parent_id, text, atts, on));
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
          case 'provider':
            if (streaming) streaming.provider_name = e.data.provider;
            break;
          case 'reasoning':
            if (streaming) streaming.reasoning = (streaming.reasoning ?? '') + e.data.text;
            break;
          case 'delta':
            if (streaming) streaming.content += e.data.text;
            break;
          case 'tool_round':
            if (streaming) {
              streaming.tool_rounds = [...(streaming.tool_rounds ?? []), e.data];
              streaming.content = '';
            }
            break;
          case 'error':
            error = e.data.message;
            break;
          case 'batch':
            streaming = null;
            if (current) current.pending_batch = e.data;
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

  /** Short line: model name without vendor prefix, plus cost. */
  function shortLine(m: Message): string {
    const name = m.model?.split('/').pop();
    const t = m.usage && api.totalCost(m.usage);
    const cost = t != null ? `$${t.toFixed(4)}` : '';
    return [name, cost].filter(Boolean).join(' · ');
  }

  /** Cost rows: whichever of cost / upstream are non-zero; "Cost $0" if neither. */
  function costRows(u: Message['usage']): [string, string | undefined][] {
    if (!u || (u.cost == null && u.upstream_cost == null)) return [];
    const rows: [string, string][] = [];
    if (u.cost) rows.push(['Cost', `$${u.cost.toFixed(6)}`]);
    if (u.upstream_cost) rows.push(['Upstream cost', `$${u.upstream_cost.toFixed(6)}`]);
    return rows.length ? rows : [['Cost', '$0']];
  }

  /** Details rows for the ⓘ toggle. */
  function details(m: Message): [string, string][] {
    const u = m.usage;
    const rows: [string, string | undefined][] = [
      ['Model', m.model],
      ['Requested', m.requested_model],
      ['Provider', m.provider_name],
      [
        'Tokens',
        u &&
          `${u.prompt_tokens} in · ${u.completion_tokens} out` +
            (u.reasoning_tokens ? ` (${u.reasoning_tokens} reasoning)` : '') +
            (u.cached_tokens ? ` · ${u.cached_tokens} cached` : ''),
      ],
      ...costRows(u),
      ['Time', new Date(m.created_at).toLocaleString()],
    ];
    return rows.filter((r): r is [string, string] => !!r[1]);
  }

  /** One-line label for a tool call: name and its path argument. */
  function callLabel(c: api.ToolCall): string {
    try {
      const a = JSON.parse(c.arguments || '{}');
      if (typeof a.path === 'string') return `${c.name} ${a.path}`;
    } catch {
      /* show raw name only */
    }
    return c.name;
  }

  /** "+3 −1" for a unified diff. */
  function diffStat(d: string): string {
    let add = 0, del = 0;
    for (const l of d.split('\n')) {
      if (l.startsWith('+') && !l.startsWith('+++')) add++;
      else if (l.startsWith('-') && !l.startsWith('---')) del++;
    }
    return `+${add} −${del}`;
  }

  function diffClass(l: string): string {
    if (l.startsWith('+++') || l.startsWith('---')) return 'dh';
    if (l.startsWith('@@')) return 'dh';
    if (l.startsWith('+')) return 'da';
    if (l.startsWith('-')) return 'dd';
    return '';
  }

  let openDetails = $state<Record<string, boolean>>({});
  let openRaw = $state<Record<string, boolean>>({});

  function slug(t: string): string {
    return t.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '').slice(0, 40) || 'chat';
  }
  function download(m: Message) {
    downloadText(`${slug(current?.title ?? '')}-${m.id}.md`, m.content);
  }

  /** Shows what the URL hash names: `#<id>` a chat, `#g/<name>` a group. */
  async function route() {
    const h = location.hash.slice(1);
    try {
      if (h.startsWith('g/')) {
        const g = decodeURIComponent(h.slice(2));
        if (viewGroup !== g || current) openGroup(g);
      } else if (h && current?.id !== h) {
        await open(h);
      } else if (!h && !isDraft) {
        await newChat();
      }
    } catch (e) {
      error = String(e);
    }
  }

  onMount(() => {
    window.addEventListener('hashchange', route);
    refreshList().then(route, (e) => (error = String(e)));
    return () => window.removeEventListener('hashchange', route);
  });
</script>

<div class="app" style="--side-w: {sideWidth}px">
  {#if showSidebar}
    <button class="backdrop" aria-label="Close menu" onclick={() => (showSidebar = false)}></button>
  {/if}
  <aside class:open={showSidebar}>
    <button class="new" onclick={newChat}>+ New chat</button>
    {#snippet chatRow(c: Summary)}
      <li class:active={current?.id === c.id} class:ingroup={!!c.group}>
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
          <a class="title" href="#{c.id}" ondblclick={() => startRename(c)}>{c.title}</a>
          {#if rowMenu === 'c:' + c.id}
            <button class="del" title="Move to group" onclick={() => { rowMenu = null; moveChat(c); }}>⇄</button>
            <button class="del" title="Rename" onclick={() => { rowMenu = null; startRename(c); }}>✎</button>
            <button class="del" title="Delete" onclick={() => { rowMenu = null; remove(c.id); }}>×</button>
          {/if}
          <button class="del" title="Actions" onclick={() => toggleMenu('c:' + c.id)}>{rowMenu === 'c:' + c.id ? '‹' : '⋯'}</button>
        {/if}
      </li>
    {/snippet}
    <ul>
      {#each [...groups] as [g, chats] (g)}
        {@const open_ = expanded.includes(g)}
        <li class="group" class:active={viewGroup === g}>
          <button class="tog" title={open_ ? 'Collapse' : 'Expand'} onclick={() => toggleGroup(g)}>{open_ ? '▾' : '▸'}</button>
          <a class="title" href="#g/{encodeURIComponent(g)}">{g} <span class="count">{chats.length}</span></a>
          {#if rowMenu === 'g:' + g}
            <button class="del" title="Rename group" onclick={() => { rowMenu = null; renameGroupUi(g); }}>✎</button>
            <button class="del" title="Delete group" onclick={() => { rowMenu = null; deletingGroup = deletingGroup === g ? null : g; }}>×</button>
          {/if}
          <button class="del" title="Actions" onclick={() => toggleMenu('g:' + g)}>{rowMenu === 'g:' + g ? '‹' : '⋯'}</button>
        </li>
        {#if deletingGroup === g}
          <li class="gconfirm">
            <button onclick={() => removeGroup(g, false)}>Ungroup chats</button>
            <button class="danger" onclick={() => removeGroup(g, true)}>Delete all</button>
            <button onclick={() => (deletingGroup = null)}>Cancel</button>
          </li>
        {/if}
        {#if open_}
          {#each chats as c (c.id)}{@render chatRow(c)}{/each}
        {/if}
      {/each}
      {#each ungrouped as c (c.id)}{@render chatRow(c)}{/each}
    </ul>
  </aside>
  <div class="resizer" role="separator" aria-orientation="vertical" onpointerdown={startResize}></div>

  <main>
    <header>
      <button class="menu" title="Chats" onclick={() => (showSidebar = !showSidebar)}>☰</button>
      <span class="htitle">
        {current ? `${current.title} — ${current.model}` : viewGroup ? `Group: ${viewGroup}` : 'chatport'}
        {#if current?.reasoning_effort}
          <span class="effort" title="Reasoning effort">· {current.reasoning_effort} effort</span>
        {/if}
      </span>
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
    {#snippet content(m: Message)}
      {#if openRaw[m.id]}
        <div class="text plain raw">{m.content}</div>
      {:else if m === streaming}
        <div class="text md">{@html renderMarkdown(m.content)}</div>
      {:else}
        {@const html = renderMarkdown(m.content)}
        <div class="text md" use:codeCopy={html}>{@html html}</div>
      {/if}
    {/snippet}
    <div class="msgs" bind:this={scroller}>
      {#if viewGroup && !current}
        <div class="gview">
          <ul>
            {#each groups.get(viewGroup) ?? [] as c (c.id)}
              <li><a class="title" href="#{c.id}">{c.title}</a></li>
            {/each}
          </ul>
          <p class="hint">Send a message below to start a new chat in this group.</p>
        </div>
      {/if}
      {#each [...path, ...(streaming ? [streaming] : [])] as m (m.id || 'streaming')}
        <div class="msg {m.role}">
          {#if m.role === 'user'}
            {#if editing === m.id}
              <MessageInput
                bind:text={editText}
                bind:attachments={editAtts}
                conv={current?.id}
                {busy}
                compact
                rows={4}
                onsubmit={(t, a) => submitEdit(m, t, a)}
                oncancel={() => (editing = null)}
              >
                {#snippet actions(submit, can)}
                  <button type="button" onclick={submit} disabled={!can}>Send</button>
                  <button type="button" onclick={() => (editing = null)}>Cancel</button>
                {/snippet}
              </MessageInput>
            {:else}
              {@render content(m)}
              {#each m.attachments ?? [] as a}
                <div class="att">
                  <button class="link" title="Open in new tab" onclick={() => openText(a.content)}>📎 {a.name}</button>
                  {#if /\.(md|markdown)$/i.test(a.name)}
                    <button class="link" title="Open rendered in new tab" onclick={() => openHtml(a.name, renderMarkdown(a.content))}>rendered</button>
                  {/if}
                </div>
              {/each}
            {/if}
          {:else}
            {#if m.reasoning}
              <details class="thinking" open={m === streaming && !m.content}>
                <summary>{m === streaming && !m.content ? 'Thinking…' : 'Thinking'}</summary>
                <div class="plain">{m.reasoning}</div>
              </details>
            {/if}
            {#each m.tool_rounds ?? [] as r}
              {#if r.content}<div class="text md">{@html renderMarkdown(r.content)}</div>{/if}
              {#each r.calls as c, i}
                {@const res = r.results[i]}
                <details class="tool" class:failed={res?.error}>
                  <summary>
                    🔧 {callLabel(c)}
                    {#if res?.error}<span class="terr">failed</span>{/if}
                    {#if res?.diff}<span class="tstat">{diffStat(res.diff)}</span>{/if}
                  </summary>
                  <div class="tlabel">arguments</div>
                  <pre>{c.arguments}</pre>
                  {#if res?.diff}
                    <div class="tlabel">diff</div>
                    <pre class="diff">{#each res.diff.split('\n') as l}<span class={diffClass(l)}>{l}</span>{'\n'}{/each}</pre>
                  {/if}
                  <div class="tlabel">result</div>
                  <pre>{res?.output}</pre>
                </details>
              {/each}
            {/each}
            {@render content(m)}
            {#if m.partial && !m.error && m !== streaming}<div class="att">(stopped)</div>{/if}
            {#if m.warning}<div class="warn">⚠ {m.warning}</div>{/if}
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
              <button class="icon" title="Copy markdown" onclick={(e) => copyWithFeedback(e.currentTarget, m.content)}>⧉</button>
              <button class="icon" title="Download .md" onclick={() => download(m)}>⤓</button>
              <button
                class="icon"
                title="Show raw markdown"
                class:on={openRaw[m.id]}
                onclick={() => (openRaw[m.id] = !openRaw[m.id])}>&lt;/&gt;</button>
              {#if m.role === 'user'}
                <button class="icon" title="Edit" onclick={() => startEdit(m)} disabled={busy || !!current?.pending_batch}>✎</button>
              {:else}
                <button class="icon" title="Regenerate" onclick={() => regen(m)} disabled={busy || !!current?.pending_batch}>↻</button>
                <span>{shortLine(m)}</span>
                <button
                  class="icon"
                  title="Details"
                  class:on={openDetails[m.id]}
                  onclick={() => (openDetails[m.id] = !openDetails[m.id])}>ⓘ</button>
              {/if}
            </div>
            {#if openDetails[m.id]}
              <dl class="details">
                {#each details(m) as [k, v]}
                  <dt>{k}</dt><dd>{v}</dd>
                {/each}
              </dl>
            {/if}
          {:else if m === streaming}
            <div class="meta">streaming…</div>
          {/if}
        </div>
      {/each}
      {#if current?.pending_batch}
        {@const pb = current.pending_batch}
        <div class="msg assistant">
          <div class="batch">
            <button class="icon round" title="Check for the reply" onclick={refreshBatch} disabled={batchBusy}>⟳</button>
            <span>
              Batch {batchBusy ? 'checking…' : pb.status || 'pending'} · {pb.model.split('/').pop()}
              {#if pb.provider}via {pb.provider}{/if}
              · sent {new Date(pb.submitted_at).toLocaleString()}
              {#if pb.expires_at}· expires {new Date(pb.expires_at * 1000).toLocaleString()}{/if}
            </span>
            <button class="icon" title="Forget this batch" onclick={forgetBatch} disabled={batchBusy}>✕</button>
          </div>
        </div>
      {/if}
      {#if error}<div class="err">⚠ {error}</div>{/if}
    </div>
    <MessageInput
      bind:text={input}
      bind:attachments
      conv={current?.id}
      {busy}
      placeholder="Message (Enter to send, Shift+Enter for newline)"
      onsubmit={send}
    >
      {#snippet actions(submit, can)}
        {#if busy}
          <button type="button" onclick={stopGen}>Stop</button>
        {:else}
          <button type="submit" disabled={!can || !!current?.pending_batch}>Send</button>
        {/if}
      {/snippet}
    </MessageInput>
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
    width: var(--side-w, 240px);
    flex-shrink: 0;
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
    display: block;
    text-decoration: none;
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
  li.group .title { font-weight: 600; }
  li.ingroup .title { padding-left: 26px; }
  .count { font-weight: normal; color: #888; font-size: 0.85em; }
  .tog { background: none; border: none; color: #888; cursor: pointer; padding: 0 0 0 8px; width: 20px; }
  .gconfirm { gap: 4px; padding: 4px 8px; flex-wrap: wrap; }
  .gconfirm button { font-size: 0.85em; padding: 2px 6px; }
  .gconfirm .danger { color: #e55; }
  .gview { max-width: 800px; margin: 0 auto; }
  .gview .hint { color: #888; }
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
  .effort { font-weight: normal; color: #888; }
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
  .md :global(pre) { background: #8882; padding: 8px; overflow-x: auto; border-radius: 6px; position: relative; }
  .md :global(.copy-code) {
    position: absolute; top: 4px; right: 4px; padding: 0 6px;
    background: none; border: 1px solid #8886; border-radius: 4px;
    color: inherit; cursor: pointer; opacity: 0.6; font-size: 0.9em;
  }
  .md :global(.copy-code:hover) { opacity: 1; }
  .raw { font-family: ui-monospace, monospace; font-size: 0.9em; }
  .md :global(code) { font-family: ui-monospace, monospace; font-size: 0.9em; }
  .md :global(table) { border-collapse: collapse; }
  .md :global(td), .md :global(th) { border: 1px solid #8886; padding: 4px 8px; }
  .meta, .att { font-size: 0.8em; color: #888; margin-top: 4px; }
  .att .link { background: none; border: none; padding: 0; color: inherit; font: inherit; cursor: pointer; text-decoration: underline dotted; }
  .att .link + .link { margin-left: 0.5em; }
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
  .meta .icon { font-size: 1.15em; line-height: 1; }
  .meta .icon.on { color: var(--accent); }
  .batch { display: flex; gap: 8px; align-items: center; font-size: 0.9em; color: #888; }
  .batch .round {
    width: 2em; height: 2em; border-radius: 50%; padding: 0;
    display: inline-flex; align-items: center; justify-content: center; font-size: 1.1em;
  }
  .details {
    display: grid;
    grid-template-columns: max-content 1fr;
    gap: 2px 12px;
    margin: 6px 0 0;
    padding: 6px 10px;
    font-size: 0.8em;
    color: #888;
    border-left: 2px solid var(--border);
  }
  .details dt { font-weight: 600; }
  .thinking {
    font-size: 0.85em;
    color: #888;
    border-left: 2px solid var(--border);
    padding: 2px 10px;
    margin-bottom: 8px;
  }
  .thinking summary { cursor: pointer; user-select: none; }
  .thinking .plain { margin-top: 4px; max-height: 40vh; overflow-y: auto; }
  .details dd { margin: 0; overflow-wrap: anywhere; }
  .err { color: #c44; margin-top: 4px; }
  .warn { color: #b8860b; margin-top: 4px; }
  .tool { border: 1px solid var(--border); border-radius: 6px; margin: 6px 0; padding: 4px 8px; font-size: 0.9em; }
  .tool summary { cursor: pointer; font-family: monospace; }
  .tool.failed { border-color: #c44; }
  .tool pre { max-height: 300px; overflow: auto; margin: 2px 0 6px; padding: 6px; background: var(--field); white-space: pre-wrap; word-break: break-word; }
  .tlabel { opacity: 0.6; font-size: 0.85em; margin-top: 4px; }
  .terr { color: #c44; margin-left: 6px; }
  .tstat { opacity: 0.7; margin-left: 6px; }
  .diff .da { color: #2a9d4a; }
  .diff .dd { color: #d0453f; }
  .diff .dh { opacity: 0.6; }
  .resizer { width: 5px; margin-left: -3px; cursor: col-resize; flex-shrink: 0; }
  .resizer:hover { background: #8884; }
  .menu, .backdrop { display: none; }
  @media (max-width: 640px) {
    .resizer { display: none; }
    .menu {
      display: block;
      background: none;
      border: none;
      padding: 0 4px;
      font-size: 1.2em;
    }
    aside {
      position: fixed;
      inset: 0 auto 0 0;
      width: min(80vw, 300px);
      background: var(--bg);
      z-index: 20;
      transform: translateX(-100%);
      transition: transform 0.2s ease;
    }
    aside.open { transform: none; }
    .backdrop {
      display: block;
      position: fixed;
      inset: 0;
      z-index: 10;
      background: #0006;
      border: none;
      border-radius: 0;
      padding: 0;
    }
    .totals { display: none; }
  }
</style>
