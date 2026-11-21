// Emacs/readline-style editing keys for a textarea:
//   C-a  start of line (pressed again right away: select all)
//   C-e  end of line
//   Alt-←/→  word back/forward
//   C-w, Alt-Backspace  delete word back (browsers usually reserve C-w for closing the tab)
//   C-u  delete to start of line
//   Alt-Enter  newline (like Shift-Enter)
// "Line" = logical line (between '\n'), not the visual wrapped line.
// Deletions go through execCommand so the native undo stack and `input` events keep working.

const isWord = (c: string) => /[\p{L}\p{N}_]/u.test(c);

export function lineStart(s: string, i: number): number {
  return s.lastIndexOf('\n', i - 1) + 1;
}

export function lineEnd(s: string, i: number): number {
  const j = s.indexOf('\n', i);
  return j < 0 ? s.length : j;
}

/** Start of the word before `i` (skips non-word chars first). */
export function wordBack(s: string, i: number): number {
  while (i > 0 && !isWord(s[i - 1])) i--;
  while (i > 0 && isWord(s[i - 1])) i--;
  return i;
}

/** End of the word after `i` (skips non-word chars first). */
export function wordForward(s: string, i: number): number {
  while (i < s.length && !isWord(s[i])) i++;
  while (i < s.length && isWord(s[i])) i++;
  return i;
}

let lastWasCtrlA = false;

function deleteRange(ta: HTMLTextAreaElement, from: number, to: number) {
  if (from >= to) return;
  ta.setSelectionRange(from, to);
  if (!document.execCommand('delete')) {
    ta.setRangeText('', from, to, 'end');
    ta.dispatchEvent(new Event('input', { bubbles: true }));
  }
}

/** Handles the key if it is one of ours; returns true if handled (default prevented). */
export function emacsKey(e: KeyboardEvent, ta: HTMLTextAreaElement): boolean {
  const s = ta.value;
  const pos = ta.selectionDirection === 'backward' ? ta.selectionStart : ta.selectionEnd;
  const ctrl = e.ctrlKey && !e.altKey && !e.metaKey && !e.shiftKey;
  const alt = e.altKey && !e.ctrlKey && !e.metaKey && !e.shiftKey;
  const wasCtrlA = lastWasCtrlA;
  lastWasCtrlA = false;

  if (ctrl && e.key === 'a') {
    if (wasCtrlA) ta.select();
    else {
      const p = lineStart(s, pos);
      ta.setSelectionRange(p, p);
      lastWasCtrlA = true;
    }
  } else if (ctrl && e.key === 'e') {
    const p = lineEnd(s, pos);
    ta.setSelectionRange(p, p);
  } else if (alt && e.key === 'ArrowLeft') {
    const p = wordBack(s, pos);
    ta.setSelectionRange(p, p);
  } else if (alt && e.key === 'ArrowRight') {
    const p = wordForward(s, pos);
    ta.setSelectionRange(p, p);
  } else if ((ctrl && e.key === 'w') || (alt && e.key === 'Backspace')) {
    if (ta.selectionStart !== ta.selectionEnd) deleteRange(ta, ta.selectionStart, ta.selectionEnd);
    else deleteRange(ta, wordBack(s, pos), pos);
  } else if (alt && e.key === 'Enter') {
    if (!document.execCommand('insertText', false, '\n')) {
      ta.setRangeText('\n', ta.selectionStart, ta.selectionEnd, 'end');
      ta.dispatchEvent(new Event('input', { bubbles: true }));
    }
  } else if (ctrl && e.key === 'u') {
    deleteRange(ta, lineStart(s, pos), pos);
  } else {
    return false;
  }
  e.preventDefault();
  return true;
}
