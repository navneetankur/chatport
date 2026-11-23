<script lang="ts">
  import { onMount } from 'svelte';
  import * as api from './api';
  import { EFFORTS } from './api';
  import type { Meta, ModelInfo, ReasoningEffort } from './api';
  import ModelPicker from './ModelPicker.svelte';

  let { meta, onsaved, onclose }: {
    meta: Meta;
    onsaved: (m: Meta) => void;
    onclose: () => void;
  } = $props();

  // svelte-ignore state_referenced_locally
  const init = meta;
  let model = $state(init.model);
  let system = $state(init.system ?? '');
  let cache = $state(init.cache);
  let effort = $state<ReasoningEffort | ''>(init.reasoning_effort ?? '');
  let provText = $state(init.provider ? JSON.stringify(init.provider, null, 2) : '');
  let keyName = $state(init.key_name ?? '');
  let fileRoot = $state(init.file_root ?? '');
  let tools = $state(init.tools ?? false);
  let keys = $state<{ names: string[]; default: string }>({ names: [], default: '' });
  let models = $state<ModelInfo[]>([]);
  let recent = $state<string[]>([]);
  let error = $state('');
  let saving = $state(false);

  /** Parsed provider JSON, or null when empty / invalid. */
  const prov = $derived.by((): Record<string, unknown> | null => {
    if (!provText.trim()) return {};
    try {
      const v = JSON.parse(provText);
      return v && typeof v === 'object' && !Array.isArray(v) ? v : null;
    } catch {
      return null;
    }
  });

  /** Form fields write through to the raw JSON (the source of truth). */
  function setProv(key: string, value: unknown) {
    if (!prov) return;
    const next = { ...prov };
    if (value === undefined || value === '' || (Array.isArray(value) && !value.length)) delete next[key];
    else next[key] = value;
    provText = Object.keys(next).length ? JSON.stringify(next, null, 2) : '';
  }

  const order = $derived(Array.isArray(prov?.order) ? (prov.order as string[]).join(', ') : '');
  const selected = $derived(models.find((m) => m.id === model));

  function price(m: ModelInfo): string {
    const p = Number(m.pricing?.prompt ?? NaN) * 1e6;
    const c = Number(m.pricing?.completion ?? NaN) * 1e6;
    if (isNaN(p) || isNaN(c)) return '';
    return p === 0 && c === 0 ? 'free' : `$${p.toFixed(2)} / $${c.toFixed(2)} per Mtok`;
  }

  async function save() {
    if (!prov) {
      error = 'Provider options: invalid JSON object';
      return;
    }
    saving = true;
    error = '';
    try {
      const m = await api.patchConversation(meta.id, {
        model: model.trim(),
        system,
        cache,
        reasoning_effort: effort || null,
        key_name: keyName || null,
        file_root: fileRoot.trim(),
        tools,
        provider: Object.keys(prov).length ? prov : null,
      });
      onsaved(m);
      onclose();
    } catch (e) {
      error = String(e);
    } finally {
      saving = false;
    }
  }

  onMount(async () => {
    api.recentModels().then((r) => (recent = r), () => {});
    api.listKeys().then((k) => (keys = k), (e) => (error = `Loading keys: ${e}`));
    try {
      models = (await api.listModels()).sort((a, b) => a.id.localeCompare(b.id));
    } catch (e) {
      error = `Loading models: ${e}`;
    }
  });
</script>

<div class="panel">
  <label>
    Model
    <ModelPicker {models} {recent} bind:value={model} {price} />
    {#if selected}
      <small>{selected.name} · {selected.context_length ?? '?'} ctx · {price(selected)}</small>
    {:else if models.length && model}
      <small class="warn">Unknown model id</small>
    {/if}
  </label>

  {#if keys.names.length > 1 || (keyName && !keys.names.includes(keyName))}
    <label>
      API key
      <select bind:value={keyName}>
        <option value="">default ({keys.default})</option>
        {#each keys.names as n}
          <option value={n}>{n}</option>
        {/each}
        {#if keyName && !keys.names.includes(keyName)}
          <option value={keyName}>{keyName} (missing)</option>
        {/if}
      </select>
    </label>
  {/if}

  <label>
    File root
    <input bind:value={fileRoot} placeholder="~/notes" />
    <small>Directory for <code>@relative/path</code> attachments. <code>@/abs</code> and <code>@~/x</code> work without it.</small>
  </label>

  <label class="row">
    <input type="checkbox" bind:checked={tools} disabled={!fileRoot.trim()} />
    File tools: the model can list, read, write and edit text files in the file root (applied directly)
  </label>

  <label>
    System prompt
    <textarea rows="5" bind:value={system}></textarea>
  </label>

  <label>
    Reasoning effort
    <select bind:value={effort}>
      <option value="">model default</option>
      {#each EFFORTS as e}
        <option value={e}>{e}</option>
      {/each}
    </select>
  </label>

  <label class="row">
    <input type="checkbox" checked={cache === 'auto'} onchange={(e) => (cache = e.currentTarget.checked ? 'auto' : 'off')} />
    Prompt caching (explicit markers for anthropic/*)
  </label>

  <fieldset>
    <legend>Provider routing</legend>
    <label>
      Order (comma-separated)
      <input
        value={order}
        disabled={!prov}
        onchange={(e) => setProv('order', e.currentTarget.value.split(',').map((s) => s.trim()).filter(Boolean))}
        placeholder="anthropic, google-vertex"
      />
    </label>
    <label class="row">
      <input
        type="checkbox"
        disabled={!prov}
        checked={prov?.allow_fallbacks !== false}
        onchange={(e) => setProv('allow_fallbacks', e.currentTarget.checked ? undefined : false)}
      />
      Allow fallbacks
    </label>
    <label>
      Sort
      <select
        disabled={!prov}
        value={(prov?.sort as string) ?? ''}
        onchange={(e) => setProv('sort', e.currentTarget.value)}
      >
        <option value="">default</option>
        <option value="price">price</option>
        <option value="throughput">throughput</option>
        <option value="latency">latency</option>
      </select>
    </label>
    <label>
      Data collection
      <select
        disabled={!prov}
        value={(prov?.data_collection as string) ?? ''}
        onchange={(e) => setProv('data_collection', e.currentTarget.value)}
      >
        <option value="">default</option>
        <option value="allow">allow</option>
        <option value="deny">deny</option>
      </select>
    </label>
    <label>
      Raw JSON
      <textarea rows="4" class="mono" class:bad={!prov} bind:value={provText} placeholder="(none)"></textarea>
    </label>
  </fieldset>

  {#if error}<div class="err">⚠ {error}</div>{/if}
  <div class="actions">
    <button onclick={onclose}>Cancel</button>
    <button class="primary" onclick={save} disabled={saving || !prov || !model.trim()}>Save</button>
  </div>
</div>

<style>
  .panel {
    padding: 12px 16px;
    border-bottom: 1px solid #8884;
    display: flex;
    flex-direction: column;
    gap: 10px;
    max-height: 70vh;
    overflow-y: auto;
  }
  label { display: flex; flex-direction: column; gap: 4px; font-size: 0.9em; }
  label.row { flex-direction: row; align-items: center; gap: 6px; }
  input:not([type='checkbox']), textarea, select { font: inherit; padding: 6px; }
  .mono { font-family: ui-monospace, monospace; font-size: 0.85em; }
  .bad { outline: 2px solid #c44; }
  fieldset { border: 1px solid #8884; border-radius: 6px; display: flex; flex-direction: column; gap: 8px; }
  small { color: #888; }
  .warn { color: #c84; }
  .err { color: #c44; }
  .actions { display: flex; justify-content: flex-end; gap: 8px; }
</style>
