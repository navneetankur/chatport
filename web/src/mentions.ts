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
