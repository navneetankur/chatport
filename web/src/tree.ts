import type { Message } from './api';

/** Root → leaf path following parent_id. */
export function pathTo(messages: Message[], leaf: string | null): Message[] {
  const byId = new Map(messages.map((m) => [m.id, m]));
  const out: Message[] = [];
  let cur = leaf ? byId.get(leaf) : undefined;
  while (cur) {
    out.push(cur);
    cur = cur.parent_id ? byId.get(cur.parent_id) : undefined;
  }
  return out.reverse();
}

/** Messages sharing `m`'s parent (incl. `m`), in append order. */
export function siblings(messages: Message[], m: Message): Message[] {
  return messages.filter((x) => x.parent_id === m.parent_id && x.role === m.role);
}
