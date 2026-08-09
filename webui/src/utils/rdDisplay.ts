const MARKDOWN_RULE_LINE_RE = /^\s{0,3}([-*_])(?:\s*\1){2,}\s*$/;
const VISUAL_RULE_CHARS_RE = /^[\s\-_=*~─━╌╍═—]{3,}$/;
const MARKDOWN_TABLE_RULE_LINE_RE = /^\s*\|?[\s:|-]+\|[\s|:-]*$/;
const FENCE_LINE_RE = /^\s*(```|~~~)/;

function isVisualSeparatorLine(line: string): boolean {
  const trimmed = line.replace(/\\n/g, '').trim();
  if (!trimmed) return false;
  if (MARKDOWN_RULE_LINE_RE.test(line) || VISUAL_RULE_CHARS_RE.test(trimmed) || MARKDOWN_TABLE_RULE_LINE_RE.test(trimmed)) {
    return true;
  }
  const hasReadableText = /[A-Za-z0-9\u4e00-\u9fa5]/.test(trimmed);
  const hasRuleChars = /[-_=*~─━╌╍═—|:]/.test(trimmed);
  return !hasReadableText && hasRuleChars;
}

export function cleanRdPromptForDisplay(value?: string | null): string {
  let inFence = false;
  return (value ?? '')
    .split(/\r?\n/)
    .filter((line) => {
      if (FENCE_LINE_RE.test(line)) {
        inFence = !inFence;
        return true;
      }
      return inFence || !isVisualSeparatorLine(line);
    })
    .join('\n')
    .trim();
}
