import { Button, Card, Empty, Input, Space, Typography } from 'antd';
import { ExperimentOutlined } from '@ant-design/icons';
import { useTranslation } from 'react-i18next';
import type { ReactNode } from 'react';
import type { RdTestRun } from '@/types';

const { Text } = Typography;

export function TestInspector({
  tests,
  testCommand,
  canRun,
  loading,
  renderStatusTag,
  onTestCommandChange,
  onRunTest,
}: {
  tests: RdTestRun[];
  testCommand: string;
  canRun: boolean;
  loading?: boolean;
  renderStatusTag: (value?: string | null) => ReactNode;
  onTestCommandChange: (value: string) => void;
  onRunTest: () => void;
}) {
  const { t } = useTranslation();

  return (
    <Space direction="vertical" style={{ width: '100%' }} size={10}>
      <Input.TextArea
        value={testCommand}
        onChange={(event) => onTestCommandChange(event.target.value)}
        placeholder={t('rd.testCommandPlaceholder', '例如 npm test / cargo test --workspace')}
        autoSize={{ minRows: 2, maxRows: 4 }}
      />
      <Button
        icon={<ExperimentOutlined />}
        disabled={!canRun}
        loading={loading}
        onClick={onRunTest}
      >
        {t('rd.runTest', '运行测试')}
      </Button>
      {tests.length === 0 ? (
        <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description={<span style={{ color: '#94a3b8' }}>{t('rd.noTestRuns', '暂无测试记录')}</span>} />
      ) : tests.map((test) => (
        <Card key={test.id} size="small" style={{ background: '#07111f', borderColor: 'rgba(148, 163, 184, 0.2)' }}>
          <Space direction="vertical" size={8} style={{ width: '100%' }}>
            <Space>{renderStatusTag(test.status)}<Text style={{ color: '#94a3b8' }}>{test.durationMs ?? 0}ms</Text></Space>
            <Text style={{ color: '#cbd5e1', fontFamily: 'var(--font-code)' }}>{test.command}</Text>
            <pre style={{ maxHeight: 220, overflow: 'auto', margin: 0, color: '#dbeafe', whiteSpace: 'pre-wrap' }}>
              {test.stdoutText || test.stderrText || t('rd.noTestOutput', '无输出')}
            </pre>
          </Space>
        </Card>
      ))}
    </Space>
  );
}
