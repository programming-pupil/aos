import { Button, Empty, Input, Space, Tag, Typography } from 'antd';
import { ExperimentOutlined } from '@ant-design/icons';
import { useMemo } from 'react';
import { useTranslation } from 'react-i18next';
import type { RdTaskWorkbenchResponse } from '@/types';

const { Text } = Typography;

function previewText(value?: string | null) {
  return value?.trim() || '';
}

export function TerminalPanel({
  workbench,
  testCommand,
  canRun,
  loading,
  onTestCommandChange,
  onRunTest,
}: {
  workbench?: RdTaskWorkbenchResponse | null;
  testCommand: string;
  canRun: boolean;
  loading?: boolean;
  onTestCommandChange: (value: string) => void;
  onRunTest: () => void;
}) {
  const { t } = useTranslation();
  const preview = workbench?.terminalOutputPreview;
  const processes = workbench?.runtimeProcesses ?? [];
  const output = useMemo(() => {
    const stdout = previewText(preview?.stdoutPreview);
    const stderr = previewText(preview?.stderrPreview);
    if (stdout && stderr) return `${stdout}\n\n--- stderr ---\n${stderr}`;
    return stdout || stderr;
  }, [preview?.stderrPreview, preview?.stdoutPreview]);

  return (
    <div className="rd-terminal-panel">
      <div className="rd-terminal-toolbar">
        <Space className="rd-terminal-status" size={6}>
          <Text strong>{t('rd.terminal', 'Terminal')}</Text>
          {preview?.status ? <Tag color={preview.status === 'running' ? 'processing' : preview.status === 'failed' ? 'error' : 'default'}>{preview.status}</Tag> : null}
          {preview?.exitCode !== undefined && preview?.exitCode !== null ? <Tag>exit {preview.exitCode}</Tag> : null}
          {processes.length > 0 ? <Tag color="blue">{t('rd.agentRuntimeProcesses', '运行进程')}: {processes.length}</Tag> : null}
        </Space>
        <Space.Compact className="rd-terminal-runner">
          <Input
            value={testCommand}
            onChange={(event) => onTestCommandChange(event.target.value)}
            placeholder={t('rd.testCommandPlaceholder', '例如 npm test / cargo test --workspace')}
          />
          <Button
            icon={<ExperimentOutlined />}
            disabled={!canRun}
            loading={loading}
            onClick={onRunTest}
          >
            {t('rd.runTest', '运行测试')}
          </Button>
        </Space.Compact>
      </div>
      {preview?.command ? (
        <pre className="rd-terminal-command">{preview.command}</pre>
      ) : null}
      {output ? (
        <pre className="rd-terminal-output">{output}</pre>
      ) : (
        <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description={t('rd.noTerminalOutput', '暂无终端输出')} />
      )}
    </div>
  );
}
