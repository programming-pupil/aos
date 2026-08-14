import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import {
  AttributionMarkdownDetail,
  isAttributionTaskTerminalStatus,
  normalizeAttributionMarkdown,
  shouldShowAttributionPreparing,
} from "../AttributionAuditPanel";

describe("AttributionAuditPanel", () => {
  it("renders observation details as GFM instead of escaped plain text", () => {
    const html = renderToStaticMarkup(
      <AttributionMarkdownDetail>
        {[
          "## 渠道结论",
          "",
          "- **收入**下降",
          "",
          "| 日期 | ROI |",
          "| --- | ---: |",
          "| 2026-07-23 | 1.25 |",
        ].join("\n")}
      </AttributionMarkdownDetail>,
    );

    expect(html).toContain("<h2");
    expect(html).toContain("<strong>收入</strong>");
    expect(html).toContain("<table");
    expect(html).not.toContain("| --- | ---: |");
  });

  it("restores flattened attribution transcripts before rendering Markdown", () => {
    const source = "用户：查最近七天 ROI 助手： ## 渠道结论 收入下降。 | 日期 | ROI ||---|---:|| 2026-07-23 | 1.25 |";
    const normalized = normalizeAttributionMarkdown(source);
    const html = renderToStaticMarkup(
      <AttributionMarkdownDetail>{source}</AttributionMarkdownDetail>,
    );

    expect(normalized).toContain("**用户:**");
    expect(normalized).toContain("\n\n## 渠道结论");
    expect(html).toContain("<h2");
    expect(html).toContain("<table");
    expect(html).not.toContain("||---|---:||");
  });

  it("treats every durable attribution outcome as terminal", () => {
    for (const status of [
      "completed",
      "clarification_needed",
      "no_data",
      "partial",
      "timed_out",
      "failed",
      "cancelled",
    ]) {
      expect(isAttributionTaskTerminalStatus(status)).toBe(true);
    }
    expect(isAttributionTaskTerminalStatus("running")).toBe(false);
    expect(isAttributionTaskTerminalStatus("queued")).toBe(false);
  });

  it("hides the generic preparing placeholder after execution details arrive", () => {
    expect(shouldShowAttributionPreparing(true, 0, 0)).toBe(true);
    expect(shouldShowAttributionPreparing(true, 0, 1)).toBe(false);
    expect(shouldShowAttributionPreparing(true, 1, 0)).toBe(false);
    expect(shouldShowAttributionPreparing(true, 0, 0, 1)).toBe(false);
    expect(shouldShowAttributionPreparing(false, 0, 0)).toBe(false);
  });
});
