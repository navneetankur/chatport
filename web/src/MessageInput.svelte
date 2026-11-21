<script lang="ts">
  // Message editor shared by the composer and inline edit: textarea, 📎 upload,
  // `@path` mentions (chips + autocomplete), removable attachment chips.
  import { tick, untrack, type Snippet } from 'svelte';
  import * as api from './api';
  import type { Attachment, FsEntry } from './api';
  import { completion, mentions, typingAt, type Typing } from './mentions';

  let {
    text = $bindable(''),
    attachments = $bindable([]),
    conv,
    busy = false,
    compact = false,
    rows = 3,
    placeholder = '',
    onsubmit,
    oncancel,
    actions,
  }: {
    text?: string;
    /** Explicit attachments (uploads, or the originals when editing). */
    attachments?: Attachment[];
    /** Conversation whose file root applies; undefined = config default. */
    conv: string | undefined;
    busy?: boolean;
    /** Inline (edit) layout instead of the bottom composer bar. */
    compact?: boolean;
    rows?: number;
    placeholder?: string;
    /** Trimmed text plus explicit and `@` attachments. */
    onsubmit: (text: string, atts: Attachment[]) => void;
    /** Esc with no dropdown open. */
    oncancel?: () => void;
    /** Buttons after the textarea; gets `submit` and whether it can submit. */
    actions: Snippet<[() => void, boolean]>;
  } = $props();

  let error = $state('');
  let fileInput: HTMLInputElement | undefined = $state();
  let textarea: HTMLTextAreaElement | undefined = $state();

  async function addFiles(files: FileList | null) {
    error = '';
    for (const f of files ?? []) {
      if (f.size > 2_000_000) {
        error = `${f.name}: too large (max 2 MB)`;
        continue;
      }
      attachments.push({ name: f.name, content: await f.text() });
    }
    if (fileInput) fileInput.value = '';
  }

  // `@path` mentions: resolved on the server once the token is complete
  // (followed by whitespace, or closing quote), shown as chips above the input.
  type AtFile = { status: 'loading' } | { status: 'ok'; att: Attachment } | { status: 'err'; msg: string };
  let atFiles = $state<Record<string, AtFile>>({});
  /** Mentions whose chip was removed with ×; not re-attached while in the text. */
  let atDismissed = $state<string[]>([]);

  function resolveAt(path: string): Promise<void> {
    atFiles[path] = { status: 'loading' };
    return api.fetchFile(conv, path).then(
      (att) => {
        if (atFiles[path]) atFiles[path] = { status: 'ok', att };
      },
      (e) => {
        if (atFiles[path]) atFiles[path] = { status: 'err', msg: e instanceof Error ? e.message : String(e) };
      },
    );
  }

  // Depends on `text` only; chip state is read/written untracked.
  $effect(() => {
    const present = mentions(text, true);
    const complete = mentions(text, false);
    untrack(() => {
      for (const k of Object.keys(atFiles)) if (!present.includes(k)) delete atFiles[k];
      if (atDismissed.some((k) => !present.includes(k))) atDismissed = atDismissed.filter((k) => present.includes(k));
      for (const p of complete) if (!(p in atFiles) && !atDismissed.includes(p)) resolveAt(p);
    });
  });

  function dismissAt(path: string) {
    delete atFiles[path];
    atDismissed.push(path);
  }

  /** Resolves mentions still pending (e.g. one at the very end) and returns the found files. */
  async function atAttachments(t: string): Promise<Attachment[]> {
    const paths = mentions(t, true).filter((p) => !atDismissed.includes(p));
    await Promise.all(paths.map((p) => (p in atFiles && atFiles[p].status !== 'loading' ? null : resolveAt(p))));
    const out: Attachment[] = [];
    for (const p of paths) {
      const f = atFiles[p];
      if (f?.status === 'ok') out.push(f.att);
    }
    return out;
  }

  const canSubmit = $derived(!busy && (!!text.trim() || attachments.length > 0));

  async function submit() {
    if (!canSubmit) return;
    const t = text.trim();
    onsubmit(t, [...attachments, ...(await atAttachments(t))]);
  }

  // `@` autocomplete: debounced listing of the directory being typed.
  let comp = $state<{ typing: Typing; items: FsEntry[]; sel: number } | null>(null);
  let compTimer: ReturnType<typeof setTimeout> | undefined;
  let compSeq = 0;

  function updateComp() {
    clearTimeout(compTimer);
    const t = textarea ? typingAt(text, textarea.selectionStart) : null;
    if (!t) {
      comp = null;
      return;
    }
    const seq = ++compSeq;
    compTimer = setTimeout(async () => {
      try {
        const items = await api.listFiles(conv, t.path);
        if (seq === compSeq) comp = items.length ? { typing: t, items, sel: 0 } : null;
      } catch {
        if (seq === compSeq) comp = null;
      }
    }, 150);
  }

  function closeComp() {
    clearTimeout(compTimer);
    compSeq++;
    comp = null;
  }

  async function pick(e: FsEntry) {
    if (!comp || !textarea) return;
    const { start } = comp.typing;
    const end = textarea.selectionStart;
    const ins = completion(e.path, e.dir, comp.typing.quoted);
    text = text.slice(0, start) + ins + text.slice(end);
    comp = null;
    await tick();
    textarea.focus();
    textarea.setSelectionRange(start + ins.length, start + ins.length);
    if (e.dir) updateComp();
  }

  function onKey(e: KeyboardEvent) {
    if (comp) {
      const n = comp.items.length;
      if (e.key === 'ArrowDown' || (e.key === 'Tab' && !e.shiftKey)) comp.sel = (comp.sel + 1) % n;
      else if (e.key === 'ArrowUp' || (e.key === 'Tab' && e.shiftKey)) comp.sel = (comp.sel + n - 1) % n;
      else if (e.key === 'Enter' && !e.shiftKey) pick(comp.items[comp.sel]);
      else if (e.key === 'Escape') comp = null;
      else return;
      e.preventDefault();
      return;
    }
    if (e.key === 'Enter' && !e.shiftKey && !e.isComposing) {
      e.preventDefault();
      submit();
    } else if (e.key === 'Escape' && oncancel) {
      e.preventDefault();
      oncancel();
    }
  }
</script>

<div class="mi" class:compact>
  {#if attachments.length || Object.keys(atFiles).length || error}
    <div class="chips">
      {#each attachments as a, i}
        <span class="chip">📎 {a.name} <button type="button" title="Remove" onclick={() => attachments.splice(i, 1)}>×</button></span>
      {/each}
      {#each Object.entries(atFiles) as [p, f] (p)}
        {#if f.status === 'ok'}
          <span class="chip at">📎 @{p} <button type="button" title="Don't attach" onclick={() => dismissAt(p)}>×</button></span>
        {:else if f.status === 'loading'}
          <span class="chip at">… @{p}</span>
        {:else}
          <span class="chip at-err" title={f.msg}>⚠ @{p}: {f.msg} <button type="button" title="Dismiss" onclick={() => dismissAt(p)}>×</button></span>
        {/if}
      {/each}
      {#if error}<span class="chip at-err">⚠ {error} <button type="button" title="Dismiss" onclick={() => (error = '')}>×</button></span>{/if}
    </div>
  {/if}
  <form onsubmit={(e) => { e.preventDefault(); submit(); }}>
    <input
      type="file"
      accept=".txt,.md,.markdown,text/plain,text/markdown"
      multiple
      hidden
      bind:this={fileInput}
      onchange={(e) => addFiles(e.currentTarget.files)}
    />
    <button type="button" title="Attach text/markdown files" onclick={() => fileInput?.click()}>📎</button>
    {#if comp}
      <ul class="comp" role="listbox">
        {#each comp.items as it, i (it.path)}
          <li role="option" aria-selected={i === comp.sel}>
            <button type="button" class:sel={i === comp.sel} onmousedown={(e) => { e.preventDefault(); pick(it); }}>
              {it.dir ? '📁' : '📄'} {it.path}
            </button>
          </li>
        {/each}
      </ul>
    {/if}
    <textarea
      bind:this={textarea}
      bind:value={text}
      oninput={updateComp}
      onclick={updateComp}
      onblur={closeComp}
      onkeydown={onKey}
      {rows}
      {placeholder}
    ></textarea>
    {@render actions(submit, canSubmit)}
  </form>
</div>

<style>
  .chips { display: flex; flex-wrap: wrap; gap: 6px; padding: 8px 16px 0; font-size: 0.85em; }
  .compact .chips { padding: 0 0 6px; }
  .chip { background: #8882; border-radius: 12px; padding: 2px 8px; }
  .chip.at { background: #4a8af433; }
  .chip.at-err { background: #e5484d33; max-width: 100%; overflow-wrap: anywhere; }
  .chip button { background: none; border: none; color: #888; cursor: pointer; }
  form { display: flex; gap: 8px; padding: 12px 16px; border-top: 1px solid #8884; position: relative; }
  /* Edit: textarea on its own row, then 📎 left and the actions right. */
  .compact form { padding: 0; border-top: none; flex-wrap: wrap; justify-content: flex-end; }
  .compact textarea { order: -1; flex-basis: 100%; }
  .compact form > button:first-of-type { margin-right: auto; }
  .comp {
    position: absolute; bottom: 100%; left: 16px; right: 16px; z-index: 5;
    max-height: 40vh; overflow-y: auto; margin: 0 0 4px; padding: 4px 0; list-style: none;
    background: var(--field); border: 1px solid var(--border); border-radius: 6px; font-size: 0.9em;
  }
  .compact .comp { left: 0; right: 0; }
  .comp button {
    display: block; width: 100%; text-align: left; background: none; border: none; border-radius: 0;
    padding: 3px 10px; color: inherit; font: inherit; cursor: pointer; overflow-wrap: anywhere;
  }
  .comp button.sel, .comp button:hover { background: #4a8af433; }
  textarea { flex: 1; min-width: 0; resize: vertical; font: inherit; padding: 8px; }
</style>
