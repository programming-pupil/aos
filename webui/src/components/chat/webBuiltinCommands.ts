export const WEB_BUILTIN_SLASH_COMMANDS = new Set([
  "help",
  "commands",
  "status",
  "compact",
  "model",
  "permissions",
  "clear",
  "cost",
  "mcp",
  "memory",
  "export",
  "skills",
  "session",
]);

export interface ParsedWebSlashCommand {
  name: string;
  args: string;
}

export function parseWebSlashCommand(rawInput: string): ParsedWebSlashCommand | null {
  const match = rawInput.trim().match(/^\/([^\s/]+)(?:\s+([\s\S]*))?$/u);
  if (!match) return null;
  return {
    name: match[1].toLocaleLowerCase(),
    args: (match[2] ?? "").trim(),
  };
}

export function resolveEffectiveModel(...candidates: Array<string | null | undefined>): string | null {
  for (const candidate of candidates) {
    const normalized = candidate?.trim();
    if (normalized) return normalized;
  }
  return null;
}
