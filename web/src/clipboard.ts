// navigator.clipboard only exists in secure contexts (HTTPS/localhost);
// on plain-HTTP LAN we fall back to the legacy execCommand path.
export async function copyText(text: string): Promise<void> {
  if (navigator.clipboard && window.isSecureContext) {
    return navigator.clipboard.writeText(text);
  }
  const ta = document.createElement('textarea');
  ta.value = text;
  ta.style.position = 'fixed';
  ta.style.opacity = '0';
  document.body.appendChild(ta);
  ta.select();
  const ok = document.execCommand('copy');
  ta.remove();
  if (!ok) throw new Error('copy failed');
}

export function downloadText(name: string, text: string): void {
  const url = URL.createObjectURL(new Blob([text], { type: 'text/markdown' }));
  const a = document.createElement('a');
  a.href = url;
  a.download = name;
  a.click();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}

/** Open `body` in a new tab via a blob URL (gone on reload of that tab). */
function openBlob(body: string, type: string): void {
  const url = URL.createObjectURL(new Blob([body], { type }));
  window.open(url, '_blank', 'noopener');
  setTimeout(() => URL.revokeObjectURL(url), 60_000);
}

/** Show an attachment's text as plain text in a new tab. */
export function openText(text: string): void {
  openBlob(text, 'text/plain;charset=utf-8');
}

const esc = (s: string) => s.replace(/[&<>"]/g, (c) => `&#${c.charCodeAt(0)};`);

/** Show already-sanitized HTML (from `renderMarkdown`) as a page in a new tab. */
export function openHtml(title: string, html: string): void {
  const page = `<!doctype html><meta charset="utf-8"><meta name="viewport" content="width=device-width">
<title>${esc(title)}</title>
<style>body{max-width:50em;margin:1em auto;padding:0 16px;font-family:sans-serif;line-height:1.5}
pre{overflow-x:auto;background:#8882;padding:.5em}code{background:#8882}
table{border-collapse:collapse}td,th{border:1px solid #8886;padding:.2em .5em}
@media(prefers-color-scheme:dark){body{background:#1b1b1b;color:#ddd}a{color:#8bf}}</style>
${html}`;
  openBlob(page, 'text/html;charset=utf-8');
}

function flash(btn: HTMLElement, label: string) {
  const old = btn.textContent;
  btn.textContent = label;
  setTimeout(() => (btn.textContent = old), 1000);
}

export async function copyWithFeedback(btn: HTMLElement, text: string) {
  try {
    await copyText(text);
    flash(btn, '✓');
  } catch {
    flash(btn, '✗');
  }
}

// Svelte action: add a copy button to every <pre> inside the node.
// The param changes whenever the rendered HTML does, so buttons are re-added.
export function codeCopy(node: HTMLElement, _html: string) {
  const add = () => {
    for (const pre of node.querySelectorAll('pre')) {
      if (pre.querySelector('.copy-code')) continue;
      const btn = document.createElement('button');
      btn.className = 'copy-code';
      btn.title = 'Copy code';
      btn.textContent = '⧉';
      btn.onclick = () => copyWithFeedback(btn, pre.querySelector('code')?.textContent ?? '');
      pre.appendChild(btn);
    }
  };
  add();
  return { update: add };
}
