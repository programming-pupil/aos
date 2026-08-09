export type SuperAssistantSlashMode =
  | "data_attribution"
  | "deep_research"
  | "super_adversarial";

export interface ParsedSuperAssistantSlashCommand {
  mode: SuperAssistantSlashMode;
  prompt: string;
}

export interface SuperAdversarialModelConfig {
  enabled?: boolean;
  model_type?: string;
  model?: string | null;
  scenarios?: string[] | null;
  runtime_available?: boolean;
}

export const SUPER_ADVERSARIAL_NEEDS_MODELS_ERROR =
  "super adversarial mode requires at least 2 distinct usable AI Chat models";
export const SUPER_ADVERSARIAL_NEEDS_MODELS_ERROR_CODE =
  "super_adversarial_requires_distinct_models";

export function countDistinctUsableChatModels(
  configs: SuperAdversarialModelConfig[],
): number {
  const models = new Set<string>();
  for (const config of configs) {
    const scenarios = config.scenarios;
    const appliesToChat =
      !scenarios || scenarios.length === 0 || scenarios.includes("chat");
    const model = config.model?.trim();
    if (
      config.enabled &&
      config.model_type === "chat" &&
      config.runtime_available !== false &&
      appliesToChat &&
      model
    ) {
      models.add(model.toLowerCase());
    }
  }
  return models.size;
}

export function isSuperAdversarialNeedsModelsError(error: string): boolean {
  return (
    error.includes(SUPER_ADVERSARIAL_NEEDS_MODELS_ERROR) ||
    error.includes(SUPER_ADVERSARIAL_NEEDS_MODELS_ERROR_CODE)
  );
}

const SUPER_ASSISTANT_SLASH_ALIASES = new Map<string, SuperAssistantSlashMode>([
  ["数据归因", "data_attribution"],
  ["归因", "data_attribution"],
  ["attribution", "data_attribution"],
  ["data-attribution", "data_attribution"],
  ["data_attribution", "data_attribution"],
  ["attr", "data_attribution"],
  ["深度研究", "deep_research"],
  ["深研", "deep_research"],
  ["deep-research", "deep_research"],
  ["deep_research", "deep_research"],
  ["deepresearch", "deep_research"],
  ["research", "deep_research"],
  ["超级对抗", "super_adversarial"],
  ["对抗", "super_adversarial"],
  ["super-adversarial", "super_adversarial"],
  ["super_adversarial", "super_adversarial"],
  ["superadversarial", "super_adversarial"],
  ["adversarial", "super_adversarial"],
  ["debate", "super_adversarial"],
]);

export function parseSuperAssistantSlashCommand(
  rawInput: string,
): ParsedSuperAssistantSlashCommand | null {
  const trimmed = rawInput.trim();
  const match = trimmed.match(/^\/([^\s]+)(?:\s+([\s\S]*))?$/);
  if (!match) return null;
  const mode = SUPER_ASSISTANT_SLASH_ALIASES.get(match[1].trim().toLowerCase());
  if (!mode) return null;
  return { mode, prompt: (match[2] ?? "").trim() };
}

export function superAssistantSlashRequestOptions(
  mode: SuperAssistantSlashMode,
): { dataAttribution?: true; explicitCapability?: string } {
  switch (mode) {
    case "data_attribution":
      return { dataAttribution: true };
    case "deep_research":
      return { explicitCapability: "pm_assistant" };
    case "super_adversarial":
      return { explicitCapability: "super_adversarial" };
  }
}
