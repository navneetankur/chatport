export type Role = 'user' | 'assistant';

export interface Usage {
  prompt_tokens: number;
  completion_tokens: number;
  cached_tokens: number;
  reasoning_tokens?: number;
  cost?: number;
  /** BYOK: what the upstream provider billed to the user's own key. */
  upstream_cost?: number;
}

export interface Attachment {
  name: string;
  content: string;
}

export interface ToolCall {
  id: string;
  name: string;
  arguments: string;
}

export interface ToolResult {
  call_id: string;
  output: string;
  error?: boolean;
  diff?: string;
}

export interface ToolRound {
  content?: string;
  calls: ToolCall[];
  results: ToolResult[];
}

export interface Message {
  id: string;
  parent_id: string | null;
  role: Role;
  content: string;
  reasoning?: string;
  attachments?: Attachment[];
  tool_rounds?: ToolRound[];
  model?: string;
  requested_model?: string;
  provider_name?: string;
  usage?: Usage;
  partial?: boolean;
  error?: string;
  warning?: string;
  created_at: number;
}

export const EFFORTS = ['max', 'xhigh', 'high', 'medium', 'low', 'minimal', 'none'] as const;
export type ReasoningEffort = (typeof EFFORTS)[number];

export interface Meta {
  id: string;
  title: string;
  created_at: number;
  updated_at: number;
  model: string;
  provider?: unknown;
  system?: string;
  cache: 'auto' | 'off';
  reasoning_effort?: ReasoningEffort | null;
  key_name?: string | null;
  file_root?: string | null;
  tools?: boolean;
  leaf?: string | null;
  /** Batch reply being waited for; sends are blocked until it is fetched or forgotten. */
  pending_batch?: PendingBatch | null;
}

export interface PendingBatch {
  id: string;
  user_id: string;
  model: string;
  key_name?: string;
  submitted_at: number;
  /** Last known: validating, in_progress, finalizing, … */
  status?: string;
  provider?: string;
  /** Unix seconds; the provider gives up after this. */
  expires_at?: number;
}

/** Batch models (`<slug>:batch`) reply asynchronously; ⟳ fetches the reply. */
export const isBatchModel = (model: string) => model.endsWith(':batch');

export interface Conversation {
  meta: Meta;
  leaf: string | null;
  messages: Message[];
}

async function json<T>(res: Response): Promise<T> {
  if (!res.ok) throw new Error(`${res.status}: ${await res.text()}`);
  return res.json() as Promise<T>;
}

/** Sidebar row: from folder names and file times, not meta.json. */
export interface Summary {
  id: string;
  group: string | null;
  title: string;
  updated_at: number;
}

export const listConversations = () => fetch('api/conversations').then((r) => json<Summary[]>(r));

export async function renameGroup(name: string, to: string): Promise<void> {
  const r = await fetch(`api/groups/${encodeURIComponent(name)}`, {
    method: 'PATCH',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ name: to }),
  });
  if (!r.ok) throw new Error(`${r.status}: ${await r.text()}`);
}

/** withChats: delete the chats too; else they become ungrouped. */
export async function deleteGroup(name: string, withChats: boolean): Promise<void> {
  const r = await fetch(`api/groups/${encodeURIComponent(name)}?chats=${withChats}`, { method: 'DELETE' });
  if (!r.ok) throw new Error(`${r.status}: ${await r.text()}`);
}

export const createConversation = () =>
  fetch('api/conversations', { method: 'POST' }).then((r) => json<Meta>(r));

export const getConversation = (id: string) =>
  fetch(`api/conversations/${id}`).then((r) => json<Conversation>(r));

export async function deleteConversation(id: string): Promise<void> {
  const r = await fetch(`api/conversations/${id}`, { method: 'DELETE' });
  if (!r.ok) throw new Error(`${r.status}: ${await r.text()}`);
}

export type StreamEvent =
  | { event: 'user_saved'; data: Message }
  | { event: 'model'; data: { model: string } }
  | { event: 'provider'; data: { provider: string } }
  | { event: 'delta'; data: { text: string } }
  | { event: 'reasoning'; data: { text: string } }
  | { event: 'usage'; data: Usage }
  | { event: 'tool_round'; data: ToolRound }
  | { event: 'error'; data: { message: string } }
  | { event: 'batch'; data: PendingBatch }
  | { event: 'done'; data: Message };

/** POST that answers with SSE; calls `on` for each event until the stream ends. */
async function streamPost(url: string, body: unknown, on: (e: StreamEvent) => void) {
  const res = await fetch(url, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  });
  if (!res.ok || !res.body) throw new Error(`${res.status}: ${await res.text()}`);
  const reader = res.body.pipeThrough(new TextDecoderStream()).getReader();
  let buf = '';
  for (;;) {
    const { value, done } = await reader.read();
    if (done) break;
    buf += value.replace(/\r\n/g, '\n');
    let i;
    while ((i = buf.indexOf('\n\n')) >= 0) {
      const block = buf.slice(0, i);
      buf = buf.slice(i + 2);
      let event = 'message';
      const data: string[] = [];
      for (const line of block.split('\n')) {
        if (line.startsWith('event:')) event = line.slice(6).trim();
        else if (line.startsWith('data:')) data.push(line.slice(5).replace(/^ /, ''));
      }
      if (data.length === 0) continue; // keep-alive comment
      on({ event, data: JSON.parse(data.join('\n')) } as StreamEvent);
    }
  }
}

export const sendMessage = (
  id: string,
  parent_id: string | null,
  content: string,
  attachments: Attachment[],
  on: (e: StreamEvent) => void,
) => streamPost(`api/conversations/${id}/messages`, { parent_id, content, attachments }, on);

export const stop = (id: string) =>
  fetch(`api/conversations/${id}/stop`, { method: 'POST' }).then((r) => json<{ stopped: boolean }>(r));

/** Checks the pending batch once; `message` is set when the reply arrived. */
export const refreshBatch = (id: string) =>
  fetch(`api/conversations/${id}/batch/refresh`, { method: 'POST' }).then((r) =>
    json<{ status: string; provider?: string; expires_at?: number; message?: Message }>(r),
  );

/** Forgets the pending batch (it keeps running upstream if not finished). */
export const forgetBatch = (id: string) =>
  fetch(`api/conversations/${id}/batch/forget`, { method: 'POST' }).then((r) =>
    json<{ upstream_deleted: boolean }>(r),
  );

export const regenerate = (id: string, user_id: string, on: (e: StreamEvent) => void) =>
  streamPost(`api/conversations/${id}/regenerate`, { user_id }, on);

export async function setLeaf(id: string, msgId: string): Promise<string> {
  const r = await fetch(`api/conversations/${id}/leaf`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ id: msgId }),
  });
  return (await json<{ leaf: string }>(r)).leaf;
}

export interface ModelInfo {
  id: string;
  name: string;
  context_length?: number;
  pricing?: { prompt?: string; completion?: string };
}

let modelsCache: Promise<ModelInfo[]> | null = null;

/** Fetched once per page load; a failed fetch is not cached. */
export function listModels(): Promise<ModelInfo[]> {
  if (!modelsCache) {
    modelsCache = fetch('api/models').then((r) => json<ModelInfo[]>(r));
    modelsCache.catch(() => (modelsCache = null));
  }
  return modelsCache;
}

export const listKeys = () =>
  fetch('api/keys').then((r) => json<{ names: string[]; default: string }>(r));

export type MetaPatch = Partial<{
  title: string;
  model: string;
  provider: unknown; // null clears
  system: string; // "" clears
  cache: 'auto' | 'off';
  reasoning_effort: ReasoningEffort | null; // null = model default
  key_name: string | null; // null = config default
  file_root: string; // "" clears
  tools: boolean;
  group: string | null; // null or "" = ungrouped
}>;

export interface FsEntry {
  path: string;
  dir: boolean;
}

export async function listFiles(conv: string | undefined, prefix: string): Promise<FsEntry[]> {
  const q = new URLSearchParams({ prefix });
  if (conv) q.set('conv', conv);
  return json<FsEntry[]>(await fetch(`api/ls?${q}`));
}

/** Server-side file for an `@path` mention; throws with the server's reason. */
export async function fetchFile(conv: string | undefined, path: string): Promise<Attachment> {
  const q = new URLSearchParams({ path });
  if (conv) q.set('conv', conv);
  const r = await fetch(`api/file?${q}`);
  if (r.ok) return r.json();
  const body = await r.text();
  try {
    throw new Error(JSON.parse(body).error);
  } catch (e) {
    if (e instanceof SyntaxError) throw new Error(`${r.status}: ${body}`);
    throw e;
  }
}

export const patchConversation = (id: string, patch: MetaPatch) =>
  fetch(`api/conversations/${id}`, {
    method: 'PATCH',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(patch),
  }).then((r) => json<Meta>(r));

/** Total spend for one usage record: OpenRouter's charge plus upstream (BYOK). */
export function totalCost(u: Usage): number | undefined {
  if (u.cost == null && u.upstream_cost == null) return undefined;
  return (u.cost ?? 0) + (u.upstream_cost ?? 0);
}
