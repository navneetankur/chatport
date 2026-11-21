export type Role = 'user' | 'assistant';

export interface Usage {
  prompt_tokens: number;
  completion_tokens: number;
  cached_tokens: number;
  cost?: number;
}

export interface Attachment {
  name: string;
  content: string;
}

export interface Message {
  id: string;
  parent_id: string | null;
  role: Role;
  content: string;
  attachments?: Attachment[];
  model?: string;
  requested_model?: string;
  provider_name?: string;
  usage?: Usage;
  partial?: boolean;
  error?: string;
  created_at: number;
}

export interface Meta {
  id: string;
  title: string;
  created_at: number;
  updated_at: number;
  model: string;
  provider?: unknown;
  system?: string;
  cache: 'auto' | 'off';
  leaf?: string | null;
}

export interface Conversation {
  meta: Meta;
  leaf: string | null;
  messages: Message[];
}

async function json<T>(res: Response): Promise<T> {
  if (!res.ok) throw new Error(`${res.status}: ${await res.text()}`);
  return res.json() as Promise<T>;
}

export const listConversations = () => fetch('/api/conversations').then((r) => json<Meta[]>(r));

export const createConversation = () =>
  fetch('/api/conversations', { method: 'POST' }).then((r) => json<Meta>(r));

export const getConversation = (id: string) =>
  fetch(`/api/conversations/${id}`).then((r) => json<Conversation>(r));

export async function deleteConversation(id: string): Promise<void> {
  const r = await fetch(`/api/conversations/${id}`, { method: 'DELETE' });
  if (!r.ok) throw new Error(`${r.status}: ${await r.text()}`);
}

export type StreamEvent =
  | { event: 'user_saved'; data: Message }
  | { event: 'model'; data: { model: string } }
  | { event: 'provider'; data: { provider: string } }
  | { event: 'delta'; data: { text: string } }
  | { event: 'usage'; data: Usage }
  | { event: 'error'; data: { message: string } }
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
) => streamPost(`/api/conversations/${id}/messages`, { parent_id, content, attachments }, on);

export const stop = (id: string) =>
  fetch(`/api/conversations/${id}/stop`, { method: 'POST' }).then((r) => json<{ stopped: boolean }>(r));

export const regenerate = (id: string, user_id: string, on: (e: StreamEvent) => void) =>
  streamPost(`/api/conversations/${id}/regenerate`, { user_id }, on);

export async function setLeaf(id: string, msgId: string): Promise<string> {
  const r = await fetch(`/api/conversations/${id}/leaf`, {
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

export const listModels = () => fetch('/api/models').then((r) => json<ModelInfo[]>(r));

export type MetaPatch = Partial<{
  title: string;
  model: string;
  provider: unknown; // null clears
  system: string; // "" clears
  cache: 'auto' | 'off';
}>;

export const patchConversation = (id: string, patch: MetaPatch) =>
  fetch(`/api/conversations/${id}`, {
    method: 'PATCH',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(patch),
  }).then((r) => json<Meta>(r));
