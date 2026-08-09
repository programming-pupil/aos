const LONG_PASTE_MIN_CHARS = 4_000;
const LONG_PASTE_MIN_LINES = 80;

const SQL_START = /^\s*(?:(?:--[^\n]*|\/\*[\s\S]*?\*\/)\s*)*(?:with\b|select\b|insert\b|update\b|delete\b|create\b|alter\b|drop\b|merge\b|explain\b)/i;

export function shouldAttachPastedText(text: string): boolean {
  if (text.length >= LONG_PASTE_MIN_CHARS) return true;
  let lines = 1;
  for (const character of text) {
    if (character !== '\n') continue;
    lines += 1;
    if (lines >= LONG_PASTE_MIN_LINES) return true;
  }
  return false;
}

export function pastedTextLooksLikeSql(text: string): boolean {
  return SQL_START.test(text);
}

export function pastedTextFileName(text: string, now = new Date()): string {
  const stamp = now.toISOString().replace(/[:.]/g, '-');
  return `pasted-${stamp}.${pastedTextLooksLikeSql(text) ? 'sql' : 'txt'}`;
}
