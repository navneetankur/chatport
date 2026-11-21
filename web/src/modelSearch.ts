export interface Searchable {
  id: string;
  name: string;
}

/**
 * Word-wise model search: every whitespace-separated word of `query` must appear
 * (case-insensitive) in the id or the display name. Ranked: exact id, id prefix,
 * then the rest, each keeping the input order.
 */
export function searchModels<T extends Searchable>(models: T[], query: string): T[] {
  const q = query.trim().toLowerCase();
  if (!q) return models;
  const words = q.split(/\s+/);
  const exact: T[] = [];
  const prefix: T[] = [];
  const rest: T[] = [];
  for (const m of models) {
    const id = m.id.toLowerCase();
    const hay = `${id} ${m.name.toLowerCase()}`;
    if (!words.every((w) => hay.includes(w))) continue;
    if (id === q) exact.push(m);
    else if (id.startsWith(q)) prefix.push(m);
    else rest.push(m);
  }
  return [...exact, ...prefix, ...rest];
}
