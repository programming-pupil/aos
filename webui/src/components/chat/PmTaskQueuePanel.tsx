import { ArrowDownOutlined, ArrowUpOutlined, CloseOutlined, SwapOutlined } from "@ant-design/icons";
import { Button, Space, Tag, Tooltip, Typography } from "antd";
import type { TFunction } from "i18next";
import type { ContentBlock } from "@/types";
import { contentToPlain } from "./chatCore.utils";
import type { PmQueuedPrompt } from "./chatCore.pmTypes";

const { Text } = Typography;

export function PmTaskQueuePanel({
  t,
  queue,
  backgroundRunning,
  backgroundStatus,
  hasReplacementDraft,
  canReplaceOrRunHead,
  onCancelCurrent,
  onReplaceCurrent,
  onMoveQueuedPrompt,
  onRemoveQueuedPrompt,
}: {
  t: TFunction;
  queue: PmQueuedPrompt[];
  backgroundRunning: boolean;
  backgroundStatus: string | null;
  hasReplacementDraft: boolean;
  canReplaceOrRunHead: boolean;
  onCancelCurrent: () => void;
  onReplaceCurrent: () => void;
  onMoveQueuedPrompt: (id: string, direction: -1 | 1) => void;
  onRemoveQueuedPrompt: (id: string) => void;
}) {
  if (!backgroundRunning && queue.length === 0) return null;

  return (
    <div
      style={{
        marginBottom: 8,
        display: "grid",
        gridTemplateColumns: "44px minmax(0, 1fr) 44px",
        gap: 8,
        alignItems: "start",
      }}
    >
      <div
        style={{
          gridColumn: "2",
          minWidth: 0,
          border: "1px solid var(--border-subtle)",
          borderRadius: 10,
          background: "var(--bg-elevated)",
          padding: "8px 10px",
          display: "grid",
          gap: 8,
          overflow: "hidden",
        }}
      >
        <div
          style={{
            display: "flex",
            alignItems: "center",
            justifyContent: "space-between",
            gap: 8,
            minWidth: 0,
            flexWrap: "wrap",
          }}
        >
          <Space size={8} wrap style={{ minWidth: 0 }}>
            <Text strong style={{ fontSize: 12, color: "var(--text-secondary)" }}>
              {t("operations.pmQueueTitle", "产运任务队列")}
            </Text>
            {backgroundRunning && (
              <Tag color="processing" style={{ marginRight: 0 }}>
                {backgroundStatus === "cancelling"
                  ? t("operations.pmBackgroundCancellingShort", "取消中")
                  : t("operations.statusRunning", "运行中")}
              </Tag>
            )}
            {queue.length > 0 && (
              <Tag color="default" style={{ marginRight: 0 }}>
                {t("operations.pmQueuePending", {
                  count: queue.length,
                  defaultValue: "待执行 {{count}} 条",
                })}
              </Tag>
            )}
          </Space>
          <Space size={6} wrap style={{ marginLeft: "auto" }}>
            {backgroundRunning && (
              <Tooltip
                title={t(
                  "operations.pmCancelCooperativeHint",
                  "产运任务采用协作式取消；正在进行的外部检索/模型请求可能需要几秒收敛。",
                )}
              >
                <Button
                  size="small"
                  danger
                  icon={<CloseOutlined />}
                  onClick={onCancelCurrent}
                  disabled={backgroundStatus === "cancelling"}
                >
                  {t("operations.pmCancelCurrent", "取消当前")}
                </Button>
              </Tooltip>
            )}
            {backgroundRunning && (
              <Button
                size="small"
                icon={<SwapOutlined />}
                onClick={onReplaceCurrent}
                disabled={!canReplaceOrRunHead}
              >
                {hasReplacementDraft
                  ? t("operations.pmReplaceCurrent", "替换当前")
                  : t("operations.pmRunQueueHead", "执行队首")}
              </Button>
            )}
          </Space>
        </div>

        {backgroundStatus === "cancelling" && (
          <Text style={{ fontSize: 11, color: "var(--text-muted)", lineHeight: 1.5 }}>
            {t(
              "operations.pmCancelCooperativeInline",
              "正在请求取消。产运助手会停止后续阶段；少量已发出的外部请求可能需要几秒返回后才会完全结束。",
            )}
          </Text>
        )}

        {queue.length > 0 && (
          <div style={{ display: "grid", gap: 6 }}>
            {queue.slice(0, 4).map((item, index) => {
              const plain = contentToPlain(item.content as string | ContentBlock[]);
              return (
                <div
                  key={item.id}
                  style={{
                    display: "flex",
                    alignItems: "center",
                    gap: 8,
                    minWidth: 0,
                    borderTop:
                      index === 0 ? "1px solid var(--border-subtle)" : undefined,
                    paddingTop: index === 0 ? 6 : 0,
                  }}
                >
                  <Tag style={{ marginRight: 0 }}>#{index + 1}</Tag>
                  <Tooltip title={plain}>
                    <Text
                      style={{
                        flex: 1,
                        minWidth: 0,
                        fontSize: 12,
                        color: "var(--text-secondary)",
                        whiteSpace: "nowrap",
                        overflow: "hidden",
                        textOverflow: "ellipsis",
                      }}
                    >
                      {plain || t("operations.pmQueuedAttachmentOnly", "附件任务")}
                    </Text>
                  </Tooltip>
                  <Space size={2} style={{ flexShrink: 0 }}>
                    {queue.length > 1 && (
                      <>
                        <Button
                          type="text"
                          size="small"
                          icon={<ArrowUpOutlined />}
                          disabled={index === 0}
                          onClick={() => onMoveQueuedPrompt(item.id, -1)}
                        />
                        <Button
                          type="text"
                          size="small"
                          icon={<ArrowDownOutlined />}
                          disabled={index === queue.length - 1}
                          onClick={() => onMoveQueuedPrompt(item.id, 1)}
                        />
                      </>
                    )}
                    <Button
                      type="text"
                      size="small"
                      danger
                      icon={<CloseOutlined />}
                      onClick={() => onRemoveQueuedPrompt(item.id)}
                    />
                  </Space>
                </div>
              );
            })}
            {queue.length > 4 && (
              <Text style={{ fontSize: 11, color: "var(--text-muted)" }}>
                {t("operations.pmQueueMore", {
                  count: queue.length - 4,
                  defaultValue: "还有 {{count}} 条待执行",
                })}
              </Text>
            )}
          </div>
        )}
      </div>
    </div>
  );
}
