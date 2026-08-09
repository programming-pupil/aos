import { describe, expect, it } from "vitest";
import {
  parseWebSlashCommand,
  resolveEffectiveModel,
  WEB_BUILTIN_SLASH_COMMANDS,
} from "../webBuiltinCommands";

describe("web builtin slash commands", () => {
  it("parses local command arguments without sending prose to the model", () => {
    expect(parseWebSlashCommand(" /model   gpt-5.5 ")).toEqual({
      name: "model",
      args: "gpt-5.5",
    });
    expect(parseWebSlashCommand("What does /model mean?")).toBeNull();
    expect(parseWebSlashCommand("/不存在 参数")).toEqual({
      name: "不存在",
      args: "参数",
    });
  });

  it("only advertises commands with real web actions", () => {
    expect(WEB_BUILTIN_SLASH_COMMANDS.has("model")).toBe(true);
    expect(WEB_BUILTIN_SLASH_COMMANDS.has("commands")).toBe(true);
    expect(WEB_BUILTIN_SLASH_COMMANDS.has("commit")).toBe(false);
    expect(WEB_BUILTIN_SLASH_COMMANDS.has("teleport")).toBe(false);
  });

  it("uses the explicit override before session and response metadata", () => {
    expect(resolveEffectiveModel(" deepseek-v4-pro ", "gpt-5.5", "fallback")).toBe(
      "deepseek-v4-pro",
    );
    expect(resolveEffectiveModel(null, "", "gpt-5.5")).toBe("gpt-5.5");
    expect(resolveEffectiveModel(undefined, " ")).toBeNull();
  });
});
