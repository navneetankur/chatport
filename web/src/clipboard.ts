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
