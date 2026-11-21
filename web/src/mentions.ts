/**
 * `@path` mentions in composer text. A mention starts at line start or after
 * whitespace. `@"a b.md"` ends at the closing quote (no escapes); `@a.md` ends
 * at whitespace. With `atEnd`, an unquoted mention at the very end counts too
 * (on send); otherwise it is still being typed.
 */
export function mentions(text: string, atEnd: boolean): string[] {
  const re = atEnd
    ? /(?:^|\s)@(?:"([^"]+)"|([^\s"]\S*)(?=\s|$))/g
    : /(?:^|\s)@(?:"([^"]+)"|([^\s"]\S*)(?=\s))/g;
  const out: string[] = [];
  for (const m of text.matchAll(re)) {
    const p = m[1] ?? m[2];
    if (!out.includes(p)) out.push(p);
  }
  return out;
}

/** Mention being typed at the cursor: where its `@` is and the path so far. */
export interface Typing {
  start: number;
  path: string;
  quoted: boolean;
}

export function typingAt(text: string, cursor: number): Typing | null {
  const before = text.slice(0, cursor);
  const m = /(?:^|\s)@(?:"([^"]*)|([^\s"]*))$/.exec(before);
  if (!m) return null;
  const quoted = m[1] !== undefined;
  const path = quoted ? m[1] : m[2];
  return { start: before.length - path.length - (quoted ? 2 : 1), path, quoted };
}

/** Text for a picked completion: dirs stay open, files close the token. */
export function completion(path: string, dir: boolean, quoted: boolean): string {
  const q = quoted || /\s/.test(path);
  if (dir) return q ? `@"${path}` : `@${path}`;
  return q ? `@"${path}" ` : `@${path} `;
}
