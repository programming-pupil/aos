import { describe, expect, it } from "vitest";

import {
  countDistinctUsableChatModels,
  isSuperAdversarialNeedsModelsError,
  parseSuperAssistantSlashCommand,
  SUPER_ADVERSARIAL_NEEDS_MODELS_ERROR,
  SUPER_ADVERSARIAL_NEEDS_MODELS_ERROR_CODE,
  superAssistantSlashRequestOptions,
  type SuperAssistantSlashMode,
} from "../superAssistantSlashCommands";

describe("Super Assistant slash commands", () => {
  const aliases: Array<[string, SuperAssistantSlashMode]> = [
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
  ];

  it.each(aliases)("parses /%s", (alias, mode) => {
    expect(parseSuperAssistantSlashCommand("/" + alias + "  分析这个问题 ")).toEqual({
      mode,
      prompt: "分析这个问题",
    });
  });

  it("parses English aliases case-insensitively", () => {
    expect(parseSuperAssistantSlashCommand("/DEEP-RESEARCH latest AI news")).toEqual({
      mode: "deep_research",
      prompt: "latest AI news",
    });
  });

  it("returns an empty prompt so the caller can block accidental submission", () => {
    expect(parseSuperAssistantSlashCommand("/超级对抗")).toEqual({
      mode: "super_adversarial",
      prompt: "",
    });
  });

  it("does not consume unrelated slash commands", () => {
    expect(parseSuperAssistantSlashCommand("/memory search this")).toBeNull();
  });

  it("maps each mode to the stable API request contract", () => {
    expect(superAssistantSlashRequestOptions("data_attribution")).toEqual({
      dataAttribution: true,
    });
    expect(superAssistantSlashRequestOptions("deep_research")).toEqual({
      explicitCapability: "pm_assistant",
    });
    expect(superAssistantSlashRequestOptions("super_adversarial")).toEqual({
      explicitCapability: "super_adversarial",
    });
  });

  it("counts distinct usable chat models instead of API key rows", () => {
    expect(
      countDistinctUsableChatModels([
        {
          enabled: true,
          model_type: "chat",
          model: "deepseek-v4-pro",
          scenarios: ["chat"],
          runtime_available: true,
        },
        {
          enabled: true,
          model_type: "chat",
          model: "DEEPSEEK-V4-PRO",
          scenarios: ["chat"],
          runtime_available: true,
        },
        {
          enabled: false,
          model_type: "chat",
          model: "disabled-model",
          scenarios: ["chat"],
          runtime_available: true,
        },
        {
          enabled: true,
          model_type: "chat",
          model: "broken-model",
          scenarios: ["chat"],
          runtime_available: false,
        },
        {
          enabled: true,
          model_type: "embedding",
          model: "embedding-model",
          scenarios: null,
          runtime_available: true,
        },
      ]),
    ).toBe(1);
  });

  it("accepts two distinct usable chat models and recognizes the server error", () => {
    expect(
      countDistinctUsableChatModels([
        {
          enabled: true,
          model_type: "chat",
          model: "deepseek-v4-pro",
          scenarios: null,
          runtime_available: true,
        },
        {
          enabled: true,
          model_type: "chat",
          model: "gpt-5.2",
          scenarios: [],
          runtime_available: true,
        },
      ]),
    ).toBe(2);
    expect(
      isSuperAdversarialNeedsModelsError(
        `Request failed: 400 {"error":"${SUPER_ADVERSARIAL_NEEDS_MODELS_ERROR}"}`,
      ),
    ).toBe(true);
    expect(isSuperAdversarialNeedsModelsError(SUPER_ADVERSARIAL_NEEDS_MODELS_ERROR_CODE)).toBe(true);
  });
});
