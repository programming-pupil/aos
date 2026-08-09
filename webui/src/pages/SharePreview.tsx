import { useMemo } from "react";
import { Alert, Card, Space, Tag, Typography } from "antd";
import { useSearchParams } from "@/router";
import { useTranslation } from "react-i18next";
import { Markdown } from "@/components/chat";

const { Title, Text } = Typography;

interface SharePreviewPayload {
  schema?: string;
  title?: string;
  generatedAt?: string;
  messageId?: string;
  taskId?: string | null;
  content?: string;
  thinking?: string;
  truncated?: boolean;
}

function decodeBase64UrlToUtf8(raw: string): string {
  const normalized = raw.replace(/-/g, "+").replace(/_/g, "/");
  const padding =
    normalized.length % 4 === 0
      ? ""
      : "=".repeat(4 - (normalized.length % 4));
  const binary = atob(normalized + padding);
  const bytes = Uint8Array.from(binary, (char) => char.charCodeAt(0));
  return new TextDecoder().decode(bytes);
}

export default function SharePreview() {
  const { t } = useTranslation();
  const [searchParams] = useSearchParams();
  const encoded = searchParams.get("d") ?? "";

  const parsed = useMemo(() => {
    if (!encoded) {
      return {
        payload: null as SharePreviewPayload | null,
        error: t("operations.pmReplySharePreviewNoContent", "当前回复暂无可预览内容"),
      };
    }
    try {
      const jsonText = decodeBase64UrlToUtf8(decodeURIComponent(encoded));
      const payload = JSON.parse(jsonText) as SharePreviewPayload;
      return { payload, error: "" };
    } catch {
      return {
        payload: null as SharePreviewPayload | null,
        error: t("operations.pmReplySharePreviewFailed", "打开预览页面失败"),
      };
    }
  }, [encoded, t]);

  const payload = parsed.payload;

  return (
    <div
      style={{
        minHeight: "100vh",
        background: "var(--bg-base)",
        padding: "24px 16px",
        display: "flex",
        justifyContent: "center",
      }}
    >
      <div style={{ width: "100%", maxWidth: 980 }}>
        {!payload ? (
          <Alert type="error" showIcon message={parsed.error} />
        ) : (
          <Card
            style={{
              borderRadius: 12,
              borderColor: "var(--border-default)",
              background: "var(--bg-elevated)",
            }}
            styles={{ body: { padding: 20 } }}
          >
            <Space direction="vertical" size={10} style={{ width: "100%" }}>
              <Space size={8} wrap>
                <Tag color="blue">
                  {payload.schema ?? "aos-pm-share-v1"}
                </Tag>
                {payload.taskId ? <Tag color="purple">task: {payload.taskId}</Tag> : null}
                {payload.truncated ? (
                  <Tag color="warning">
                    {t("operations.pmReplySharePreviewTruncated", "内容过长，已截断展示")}
                  </Tag>
                ) : null}
              </Space>

              <Title level={3} style={{ margin: 0 }}>
                {payload.title ?? t("operations.pmReplySharePreviewTitle", "研究结果网页预览")}
              </Title>

              <Text type="secondary">
                {(payload.generatedAt ?? "").trim()
                  ? `${t("operations.pmReplySharePreviewGeneratedAt", "生成时间")}: ${payload.generatedAt}`
                  : ""}
                {(payload.messageId ?? "").trim()
                  ? ` · ${t("operations.pmReplySharePreviewMessageId", "消息ID")}: ${payload.messageId}`
                  : ""}
              </Text>

              {(payload.thinking ?? "").trim() ? (
                <Card
                  size="small"
                  title={t("operations.pmReplySharePreviewThinking", "思考过程")}
                  style={{ background: "var(--bg-surface)" }}
                >
                  <Markdown>{payload.thinking ?? ""}</Markdown>
                </Card>
              ) : null}

              <Card
                size="small"
                title={t("operations.pmReplySharePreviewContent", "结果内容")}
                style={{ background: "var(--bg-surface)" }}
              >
                <Markdown>
                  {(payload.content ?? "").trim() ||
                    t("operations.pmReplySharePreviewNoContent", "当前回复暂无可预览内容")}
                </Markdown>
              </Card>
            </Space>
          </Card>
        )}
      </div>
    </div>
  );
}
