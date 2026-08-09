import { useState, useCallback, useMemo, forwardRef, useRef } from 'react';
import {
  Card,
  Table,
  Tag,
  Typography,
  Input,
  Button,
  Space,
  Modal,
  Form,
  Select,
  InputNumber,
  Switch,
  Popconfirm,
  message,
  Tooltip,
  Statistic,
  Row,
  Col,
  Dropdown,
  Alert,
  Radio,
  Divider,
  Drawer,
} from 'antd';
import type { MenuProps } from 'antd';
import type { TableRowSelection } from 'antd/es/table/interface';
import {
  SearchOutlined,
  PlusOutlined,
  EditOutlined,
  DeleteOutlined,
  CodeOutlined,
  PlayCircleOutlined,
  DownOutlined,
  CheckCircleOutlined,
  WarningOutlined,
  CloseCircleOutlined,
  BugOutlined,
  ThunderboltOutlined,
  FileTextOutlined,
  RobotOutlined,
  SafetyCertificateOutlined,
  AuditOutlined,
  BellOutlined,
  ReloadOutlined,
  FileAddOutlined,
  HistoryOutlined,
} from '@ant-design/icons';
import type { ColumnsType } from 'antd/es/table';
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query';
import Editor from '@monaco-editor/react';
import dayjs from 'dayjs';
import relativeTime from 'dayjs/plugin/relativeTime';
import { useTranslation } from 'react-i18next';
import { hooksApi } from '@/api';
import { queryKeys } from '@/api/queryKeys';
import { useSystemEvents } from '@/api/systemEvents';
import { PageSkeleton } from '@/components/Skeleton';
import type { HookInfo, HookEventType, HookLanguage, HookLogEntry, HookValidationResponse, IStandaloneCodeEditor } from '@/types';
import { usePermissions } from '@/store/permissions';

dayjs.extend(relativeTime);

const { Title, Text } = Typography;

const EVENT_TYPE_COLORS: Record<string, string> = {
  pre_tool_use: 'blue',
  post_tool_use: 'green',
  post_tool_use_failure: 'orange',
  message_received: 'cyan',
  before_model_call: 'geekblue',
  after_model_call: 'purple',
  before_route: 'volcano',
  after_route: 'magenta',
  before_final_answer: 'gold',
  after_final_answer: 'lime',
  task_completed: 'green',
  bot_message_received: 'blue',
};

const HOOK_EVENT_TYPES: HookEventType[] = [
  'pre_tool_use',
  'post_tool_use',
  'post_tool_use_failure',
  'message_received',
  'before_model_call',
  'after_model_call',
  'before_route',
  'after_route',
  'before_final_answer',
  'after_final_answer',
  'task_completed',
  'bot_message_received',
];

const TOOL_EVENT_TYPES = new Set<HookEventType>([
  'pre_tool_use',
  'post_tool_use',
  'post_tool_use_failure',
]);

const LANGUAGE_COLORS: Record<string, string> = {
  python: '#3572A4',
  shell: '#89e051',
};

const HOOK_SCENARIOS = ['chat', 'pm', 'materials', 'nl2sql', 'rd', 'bot'] as const;

type HookScenario = typeof HOOK_SCENARIOS[number];

const HOOK_SCENARIO_COLORS: Record<HookScenario, string> = {
  chat: 'blue',
  pm: 'geekblue',
  materials: 'gold',
  nl2sql: 'cyan',
  rd: 'green',
  bot: 'purple',
};

const DIAGNOSTIC_I18N_KEYS: Record<string, string> = {
  'hook stdout/stderr is redirected to /dev/null; provider response cannot be shown': 'hooks.test.diagnosticRedirected',
  'curl failure is ignored by `|| true`; the test can pass even when the notification was not delivered': 'hooks.test.diagnosticCurlIgnored',
};

function scenarioLabelKey(scenario: string): string {
  switch (scenario) {
    case 'chat':
      return 'hooks.scenarioChat';
    case 'pm':
      return 'hooks.scenarioPm';
    case 'materials':
      return 'hooks.scenarioMaterials';
    case 'nl2sql':
      return 'hooks.scenarioNl2sql';
    case 'rd':
      return 'hooks.scenarioRd';
    case 'bot':
      return 'hooks.scenarioBot';
    default:
      return scenario;
  }
}

function safePrettyJson(value?: string | null): string {
  if (!value) return '-';
  try {
    return JSON.stringify(JSON.parse(value), null, 2);
  } catch {
    return value;
  }
}

// ---------------------------------------------------------------------------
// Hook Templates
// ---------------------------------------------------------------------------

const PYTHON_TEMPLATES = {
  securityScan: `import json
import os
import sys
import re

event     = os.environ.get('HOOK_EVENT', '')
tool_name = os.environ.get('HOOK_TOOL_NAME', '')
tool_input = os.environ.get('HOOK_TOOL_INPUT', '{}')

if event != 'PreToolUse':
    print('ALLOW')
    sys.exit(0)

# Dangerous command patterns
DANGEROUS_PATTERNS = [
    (r'rm\\s+-rf\\s+/',                   '禁止递归删除根目录'),
    (r'chmod\\s+777',                      '禁止 chmod 777'),
    (r'drop\\s+(database|table)',          '禁止删除数据库对象'),
    (r'curl.*\\|.*sh',                     '禁止管道执行远程脚本'),
    (r'wget.*\\|.*sh',                     '禁止下载并执行脚本'),
    (r'sudo\\s+su',                        '禁止切换到 root'),
]

try:
    input_data = json.loads(tool_input)
    input_str = json.dumps(input_data)
except:
    input_str = tool_input

for pattern, reason in DANGEROUS_PATTERNS:
    if re.search(pattern, input_str, re.IGNORECASE):
        print(f'DENY: {reason}')
        sys.exit(2)

print('ALLOW')
sys.exit(0)`,
  auditLog: `import json
import os
import sys
from datetime import datetime

event      = os.environ.get('HOOK_EVENT', '')
tool_name  = os.environ.get('HOOK_TOOL_NAME', '')
tool_input = os.environ.get('HOOK_TOOL_INPUT', '{}')
tool_output = os.environ.get('HOOK_TOOL_OUTPUT', '')

log_entry = {
    'timestamp': datetime.now().isoformat(),
    'event': event,
    'tool': tool_name,
    'input': tool_input[:500],
}

if event == 'PostToolUse':
    log_entry['output'] = tool_output[:500]

print(f"AUDIT: {json.dumps(log_entry)}")
print('ALLOW')
sys.exit(0)`,
  complianceCheck: `import json
import os
import sys
import re

event     = os.environ.get('HOOK_EVENT', '')
tool_name = os.environ.get('HOOK_TOOL_NAME', '')
tool_input = os.environ.get('HOOK_TOOL_INPUT', '{}')

if event != 'PreToolUse':
    print('ALLOW')
    sys.exit(0)

# Allowed tools whitelist (example)
ALLOWED_TOOLS = {'read', 'edit', 'bash', 'grep', 'glob', 'write'}
tool_key = tool_name.lower()

if tool_key not in ALLOWED_TOOLS:
    print(f'DENY: 工具 {tool_name} 不在白名单中')
    sys.exit(2)

print('ALLOW')
sys.exit(0)`,
  dingTalkNotify: `import json
import os
import sys
import urllib.request
event      = os.environ.get('HOOK_EVENT', '')
tool_name  = os.environ.get('HOOK_TOOL_NAME', '')
tool_input = os.environ.get('HOOK_TOOL_INPUT', '{}')

# Option 1: set DINGTALK_WEBHOOK in the server environment.
# Option 2: paste the webhook URL directly below.
DINGTALK_WEBHOOK = os.environ.get('DINGTALK_WEBHOOK', '')
# DINGTALK_WEBHOOK = 'https://oapi.dingtalk.com/robot/send?access_token=YOUR_TOKEN'

if not DINGTALK_WEBHOOK:
    print('ALLOW')
    sys.exit(0)

msg = {
    'msgtype': 'text',
    'text': {'content': f'[Hook] {event} - {tool_name}\\nInput: {tool_input[:200]}'}
}

data = json.dumps(msg).encode('utf-8')
req = urllib.request.Request(DINGTALK_WEBHOOK, data=data, headers={'Content-Type': 'application/json'})
try:
    with urllib.request.urlopen(req, timeout=5) as resp:
        body = resp.read().decode('utf-8', errors='replace')
        status = resp.status
except Exception as e:
    print(f'DINGTALK_NOTIFY_FAIL: {e}')
    sys.exit(1)

try:
    parsed = json.loads(body)
except Exception:
    parsed = {}

if status < 200 or status >= 300 or parsed.get('errcode', 0) != 0:
    print(f'DINGTALK_NOTIFY_FAIL: status={status} body={body[:500]}')
    sys.exit(1)

print('ALLOW: DingTalk notification sent')

sys.exit(0)`,
  autoRetry: `import json
import os
import sys
import time
import subprocess

event     = os.environ.get('HOOK_EVENT', '')
tool_name = os.environ.get('HOOK_TOOL_NAME', '')
tool_input = os.environ.get('HOOK_TOOL_INPUT', '{}')
max_retries = 3

if event != 'PostToolUseFailure':
    print('ALLOW')
    sys.exit(0)

# Example: emit a retry recommendation for specific failed tools.
# Hooks cannot safely replay arbitrary tool calls by themselves.
RETRY_TOOLS = {'bash', 'curl', 'wget'}
tool_key = tool_name.lower()

if tool_key not in RETRY_TOOLS:
    print('ALLOW')
    sys.exit(0)

print(f'RETRY_HINT: {tool_name} failed; consider retrying up to {max_retries} times after checking the error.')
print('ALLOW')
sys.exit(0)`,
};

const SHELL_TEMPLATES = {
  securityScan: `#!/bin/bash
# Security scan hook
EVENT="\${HOOK_EVENT}"
TOOL="\${HOOK_TOOL_NAME}"
INPUT="\${HOOK_TOOL_INPUT}"

if [ "$EVENT" != "PreToolUse" ]; then
  echo "ALLOW"
  exit 0
fi

# Block dangerous patterns
if echo "$INPUT" | grep -qiE 'rm[[:space:]]+-rf[[:space:]]+/'; then
  echo "DENY: 禁止递归删除根目录"
  exit 2
fi
if echo "$INPUT" | grep -qiE 'chmod[[:space:]]+777'; then
  echo "DENY: 禁止 chmod 777"
  exit 2
fi
if echo "$INPUT" | grep -qiE 'drop[[:space:]]+(database|table)'; then
  echo "DENY: 禁止删除数据库对象"
  exit 2
fi
if echo "$INPUT" | grep -qiE '(curl|wget).*[|][[:space:]]*(sh|bash)'; then
  echo "DENY: 禁止下载并执行远程脚本"
  exit 2
fi
if echo "$INPUT" | grep -qiE 'sudo[[:space:]]+su'; then
  echo "DENY: 禁止切换到 root"
  exit 2
fi

echo "ALLOW"
exit 0`,
  auditLog: `#!/bin/bash
# Audit log hook
EVENT="\${HOOK_EVENT}"
TOOL="\${HOOK_TOOL_NAME}"
INPUT="\${HOOK_TOOL_INPUT}"
OUTPUT="\${HOOK_TOOL_OUTPUT}"
TIMESTAMP=$(date -u +"%Y-%m-%dT%H:%M:%SZ")

LOG_FILE="\${AOS_HOOK_AUDIT_LOG:-./aos-hook-audit.log}"
if ! echo "[$TIMESTAMP] $EVENT | $TOOL | $INPUT" >> "$LOG_FILE" 2>/dev/null; then
  echo "AUDIT_WARN: failed to write $LOG_FILE"
fi

echo "ALLOW"
exit 0`,
  complianceCheck: `#!/bin/bash
# Compliance check hook
EVENT="\${HOOK_EVENT}"
TOOL="\${HOOK_TOOL_NAME}"
TOOL_KEY=$(printf '%s' "$TOOL" | tr '[:upper:]' '[:lower:]')

if [ "$EVENT" != "PreToolUse" ]; then
  echo "ALLOW"
  exit 0
fi

# Example whitelist
case "$TOOL_KEY" in
  read|edit|bash|grep|glob|write)
    echo "ALLOW"
    exit 0
    ;;
  *)
    echo "DENY: 工具 $TOOL 不在白名单中"
    exit 2
    ;;
esac`,
  dingTalkNotify: `#!/bin/bash
# DingTalk notification hook
EVENT="\${HOOK_EVENT}"
TOOL="\${HOOK_TOOL_NAME}"
INPUT="\${HOOK_TOOL_INPUT}"
DINGTALK_WEBHOOK="\${DINGTALK_WEBHOOK:-}"

if [ -z "$DINGTALK_WEBHOOK" ]; then
  echo "ALLOW"
  exit 0
fi

MSG="[Hook] $EVENT - $TOOL"
CONTENT=$(printf '%s\\nInput: %s' "$MSG" "\${INPUT:0:200}" | python3 -c 'import json,sys; print(json.dumps(sys.stdin.read()))')
RESP=$(curl -sS -m 8 -w '\\n%{http_code}' -X POST "$DINGTALK_WEBHOOK" \\
  -H 'Content-Type: application/json' \\
  -d "{\\"msgtype\\":\\"text\\",\\"text\\":{\\"content\\":$CONTENT}}" 2>&1)
CURL_EXIT=$?
HTTP_CODE=$(printf '%s' "$RESP" | tail -n 1)
BODY=$(printf '%s' "$RESP" | sed '$d')

if [ "$CURL_EXIT" -ne 0 ]; then
  echo "DINGTALK_NOTIFY_FAIL: curl_exit=$CURL_EXIT response=$RESP"
  exit 1
fi

case "$HTTP_CODE" in
  2*) ;;
  *)
    echo "DINGTALK_NOTIFY_FAIL: http=$HTTP_CODE body=$BODY"
    exit 1
    ;;
esac

if ! printf '%s' "$BODY" | grep -q '"errcode"[[:space:]]*:[[:space:]]*0'; then
  echo "DINGTALK_NOTIFY_FAIL: http=$HTTP_CODE body=$BODY"
  exit 1
fi

echo "ALLOW: DingTalk notification sent"
exit 0`,
  autoRetry: `#!/bin/bash
# Auto retry hook
EVENT="\${HOOK_EVENT}"
TOOL="\${HOOK_TOOL_NAME}"
TOOL_KEY=$(printf '%s' "$TOOL" | tr '[:upper:]' '[:lower:]')

if [ "$EVENT" != "PostToolUseFailure" ]; then
  echo "ALLOW"
  exit 0
fi

if echo "$TOOL_KEY" | grep -qE '^(bash|curl|wget)$'; then
  echo "RETRY_HINT: $TOOL failed; consider retrying up to 3 times after checking the error."
fi

echo "ALLOW"
exit 0`,
};

function getTemplateCode(templateKey: string, language: string): string {
  if (language === 'python') {
    return PYTHON_TEMPLATES[templateKey as keyof typeof PYTHON_TEMPLATES] ?? '';
  }
  return SHELL_TEMPLATES[templateKey as keyof typeof SHELL_TEMPLATES] ?? '';
}

// ---------------------------------------------------------------------------
// Hook Editor Modal
// ---------------------------------------------------------------------------

interface HookEditorValues {
  name: string;
  description?: string;
  scenarios?: HookScenario[];
  event_type: HookEventType;
  language: HookLanguage;
  code: string;
  command: string;
  timeout_seconds: number;
  priority: number;
  enabled: boolean;
  fail_fast: boolean;
}

interface HookEditorModalProps {
  open: boolean;
  editing?: HookInfo;
  defaultLanguage?: HookLanguage;
  onSave: (values: HookEditorValues) => void;
  onCancel: () => void;
}

const HookEditorModal = forwardRef<IStandaloneCodeEditor | null, HookEditorModalProps>(function HookEditorModal({
  open,
  editing,
  defaultLanguage,
  onSave,
  onCancel,
}: HookEditorModalProps, ref) {
  const { t } = useTranslation();
  const [form] = Form.useForm<HookEditorValues>();
  const [language, setLanguage] = useState<HookLanguage>(editing?.language ?? defaultLanguage ?? 'python');
  const [code, setCode] = useState(editing?.code ?? '');
  const [command, setCommand] = useState(editing?.command ?? '');
  const [validation, setValidation] = useState<HookValidationResponse | null>(null);
  const [validating, setValidating] = useState(false);
  const editorRef = useRef<IStandaloneCodeEditor | null>(null);
  const monacoRef = useRef<any>(null);
  const [activeTab, setActiveTab] = useState<'code' | 'shell'>(
    editing?.language === 'shell' ? 'shell' : 'code'
  );

  const handleLanguageChange = useCallback((lang: HookLanguage) => {
    setLanguage(lang);
    setActiveTab(lang === 'shell' ? 'shell' : 'code');
    setValidation(null);
    form.setFieldsValue({ language: lang });
  }, [form]);

  const handleValidate = useCallback(async () => {
    const currentCode = language === 'shell' ? command : code;
    if (!currentCode.trim()) {
      message.warning(t('hooks.form.codePlaceholder'));
      return;
    }
    setValidating(true);
    let validationResult: HookValidationResponse | null = null;
    try {
      validationResult = await hooksApi.validate({
        code: currentCode,
        language,
      });
      setValidation(validationResult);

      if (validationResult.valid && validationResult.warnings.length === 0) {
        message.success(t('hooks.validation.valid'));
      } else if (validationResult.valid && validationResult.warnings.length > 0) {
        message.warning(`${t('hooks.validation.valid')}: ${validationResult.warnings.length} ${t('hooks.validation.warnings')}`);
      }
    } catch {
      message.error(t('common.operateFailed'));
    } finally {
      setValidating(false);
    }

    // Apply Monaco editor markers after the API call — separate from the
    // validation try-catch so Monaco errors don't shadow a successful response.
    if (validationResult && editorRef.current) {
      const model = editorRef.current.getModel();
      if (model) {
        const markers = [
          ...validationResult.errors.map((e) => ({
            // MarkerSeverity.Error = 8
            severity: 8 as const,
            message: e.message,
            startLineNumber: e.line ?? 1,
            startColumn: e.column ?? 1,
            endLineNumber: e.line ?? 1,
            endColumn: (e.column ?? 1) + (e.message.length || 10),
          })),
          ...validationResult.warnings.map((w) => ({
            // MarkerSeverity.Warning = 4
            severity: 4 as const,
            message: w,
            startLineNumber: 1,
            startColumn: 1,
            endLineNumber: 1,
            endColumn: 10,
          })),
        ];
        monacoRef.current.editor.setModelMarkers(model, 'hook-validation', markers);
      }
    }
  }, [code, command, language, t]);

  const handleTemplateMenuClick = useCallback((templateKey: string) => {
    const templateCode = getTemplateCode(templateKey, language);
    setCode(templateCode);
    setCommand(templateCode);
    setValidation(null);
  }, [language]);

  const handleOk = useCallback(() => {
    form.validateFields().then((values) => {
      const finalCode = language === 'shell' ? command : code;
      const finalCommand = language === 'shell' ? command : '';
      onSave({
        ...values,
        code: finalCode,
        command: finalCommand,
      });
    });
  }, [form, code, command, onSave, language]);

  const editorCode = activeTab === 'shell' ? command : code;
  const setEditorCode = activeTab === 'shell' ? setCommand : setCode;
  const monacoLanguage = language === 'python' ? 'python' : 'shell';
  const editorHeight = 380;

  const templateItems: MenuProps['items'] = [
    {
      key: 'securityScan',
      icon: <SafetyCertificateOutlined />,
      label: (
        <span>
          <strong>{t('hooks.template.securityScan')}</strong>
          <br />
          <Text type="secondary" style={{ fontSize: 12 }}>{t('hooks.template.securityScanDesc')}</Text>
        </span>
      ),
      onClick: () => handleTemplateMenuClick('securityScan'),
    },
    {
      key: 'auditLog',
      icon: <AuditOutlined />,
      label: (
        <span>
          <strong>{t('hooks.template.auditLog')}</strong>
          <br />
          <Text type="secondary" style={{ fontSize: 12 }}>{t('hooks.template.auditLogDesc')}</Text>
        </span>
      ),
      onClick: () => handleTemplateMenuClick('auditLog'),
    },
    {
      key: 'complianceCheck',
      icon: <FileTextOutlined />,
      label: (
        <span>
          <strong>{t('hooks.template.complianceCheck')}</strong>
          <br />
          <Text type="secondary" style={{ fontSize: 12 }}>{t('hooks.template.complianceCheckDesc')}</Text>
        </span>
      ),
      onClick: () => handleTemplateMenuClick('complianceCheck'),
    },
    {
      key: 'dingTalkNotify',
      icon: <BellOutlined />,
      label: (
        <span>
          <strong>{t('hooks.template.dingTalkNotify')}</strong>
          <br />
          <Text type="secondary" style={{ fontSize: 12 }}>{t('hooks.template.dingTalkNotifyDesc')}</Text>
        </span>
      ),
      onClick: () => handleTemplateMenuClick('dingTalkNotify'),
    },
    {
      key: 'autoRetry',
      icon: <ReloadOutlined />,
      label: (
        <span>
          <strong>{t('hooks.template.autoRetry')}</strong>
          <br />
          <Text type="secondary" style={{ fontSize: 12 }}>{t('hooks.template.autoRetryDesc')}</Text>
        </span>
      ),
      onClick: () => handleTemplateMenuClick('autoRetry'),
    },
  ];

  const languageItems: MenuProps['items'] = [
    {
      key: 'python',
      icon: <RobotOutlined />,
      label: t('hooks.language.python'),
      onClick: () => handleLanguageChange('python'),
    },
    {
      key: 'shell',
      icon: <CodeOutlined />,
      label: t('hooks.language.shell'),
      onClick: () => handleLanguageChange('shell'),
    },
  ];

  return (
    <Modal
      title={editing ? t('hooks.form.editTitle') : t('hooks.form.title')}
      open={open}
      onOk={handleOk}
      onCancel={onCancel}
      okText={t('common.confirm')}
      cancelText={t('common.cancel')}
      width={860}
      destroyOnHidden
      afterOpenChange={(visible) => {
        if (visible) {
          setLanguage(editing?.language ?? defaultLanguage ?? 'python');
          setCode(editing?.code ?? '');
          setCommand(editing?.command ?? '');
          setValidation(null);
          setActiveTab(editing?.language === 'shell' ? 'shell' : 'code');
          form.setFieldsValue({
            name: editing?.name ?? '',
            description: editing?.description ?? '',
            event_type: editing?.event_type ?? 'pre_tool_use',
            scenarios: (editing?.scenarios ?? []) as HookScenario[],
            language: editing?.language ?? defaultLanguage ?? 'python',
            timeout_seconds: editing?.timeout_seconds ?? 30,
            priority: editing?.priority ?? 0,
            enabled: editing?.enabled ?? true,
            fail_fast: editing?.fail_fast ?? true,
          });
        }
      }}
    >
      <Form form={form} layout="vertical" style={{ marginTop: 8 }}>
        {/* Basic Info Row */}
        <Row gutter={16}>
          <Col span={12}>
            <Form.Item
              name="name"
              label={t('hooks.form.name')}
              rules={[{ required: true, message: t('hooks.form.required') }]}
            >
              <Input placeholder={t('hooks.form.namePlaceholder')} maxLength={64} showCount />
            </Form.Item>
          </Col>
          <Col span={12}>
            <Form.Item
              name="event_type"
              label={t('hooks.form.eventType')}
              rules={[{ required: true, message: t('hooks.form.required') }]}
              tooltip={t('hooks.form.eventTypeHelp')}
            >
              <Select
                options={HOOK_EVENT_TYPES.map((value) => ({
                  value,
                  label: t(`hooks.eventType.${value}`),
                }))}
              />
            </Form.Item>
          </Col>
        </Row>

        <Form.Item name="description" label={t('hooks.form.description')}>
          <Input.TextArea
            placeholder={t('hooks.form.descriptionPlaceholder')}
            rows={1}
            maxLength={512}
            showCount
          />
        </Form.Item>

        <Form.Item
          name="scenarios"
          label={t('hooks.form.scenarios')}
          extra={t('hooks.form.scenariosHelp')}
        >
          <Select
            mode="multiple"
            allowClear
            placeholder={t('hooks.scenariosAll')}
            options={HOOK_SCENARIOS.map((scenario) => ({
              value: scenario,
              label: t(scenarioLabelKey(scenario)),
            }))}
          />
        </Form.Item>

        {/* Language + Template Row */}
        <div style={{ display: 'flex', alignItems: 'center', gap: 8, marginBottom: 8 }}>
          <Text strong>{t('hooks.form.language')}:</Text>
          <Dropdown menu={{ items: languageItems }} trigger={['click']}>
            <Button>
              <Space>
                {language === 'python' ? <RobotOutlined /> : <CodeOutlined />}
                {t(`hooks.language.${language}`)}
                <DownOutlined />
              </Space>
            </Button>
          </Dropdown>
          <Form.Item name="language" noStyle>
            <Input type="hidden" />
          </Form.Item>
          <Dropdown menu={{ items: templateItems }} trigger={['click']}>
            <Button icon={<FileAddOutlined />}>
              <Space>
                {t('hooks.template.menuTitle')}
                <DownOutlined />
              </Space>
            </Button>
          </Dropdown>
        </div>

        {/* Code Editor */}
        <div
          style={{
            border: '1px solid var(--border-subtle, #d9d9d9)',
            borderRadius: 6,
            overflow: 'hidden',
            marginBottom: 8,
          }}
        >
          <div
            style={{
              padding: '4px 12px',
              background: 'var(--bg-color, #fafafa)',
              borderBottom: '1px solid var(--border-subtle, #d9d9d9)',
              display: 'flex',
              alignItems: 'center',
              justifyContent: 'space-between',
            }}
          >
            <Space>
              <Tag color={LANGUAGE_COLORS[language]} style={{ margin: 0 }}>
                {language === 'python' ? 'Python 3' : 'Shell'}
              </Tag>
              <Text type="secondary" style={{ fontSize: 12 }}>
                {t('hooks.envVars.title')}
              </Text>
            </Space>
            <Text type="secondary" style={{ fontSize: 11 }}>
              {t('hooks.envVars.summary')} · Ctrl+S {t('common.save')}
            </Text>
          </div>
          <Editor
            height={editorHeight}
            language={monacoLanguage}
            value={editorCode}
            onChange={(v) => setEditorCode(v ?? '')}
            theme="vs-dark"
            options={{
              minimap: { enabled: false },
              fontSize: 13,
              lineNumbers: 'on',
              scrollBeyondLastLine: false,
              automaticLayout: true,
              tabSize: 4,
              wordWrap: 'on',
              folding: true,
              renderLineHighlight: 'line',
              padding: { top: 8 },
            }}
            onMount={(editorInstance, monacoInstance) => {
              editorRef.current = editorInstance;
              monacoRef.current = monacoInstance;
              editorInstance.addCommand(
                2048 | 49, // Ctrl+S
                () => {
                  handleOk();
                }
              );
            }}
          />
          <div
            style={{
              padding: '6px 12px',
              background: 'var(--bg-color, #fafafa)',
              borderTop: '1px solid var(--border-subtle, #d9d9d9)',
              fontSize: 11,
              color: '#888',
              fontFamily: 'monospace',
            }}
          >
            <code>
              HOOK_EVENT, HOOK_TOOL_NAME, HOOK_TOOL_INPUT, HOOK_TOOL_INPUT_JSON, HOOK_TOOL_OUTPUT, HOOK_TOOL_IS_ERROR
            </code>
          </div>
        </div>

        {/* Validation Results */}
        {validation && (
          <div style={{ marginBottom: 8 }}>
            {validation.errors.length > 0 && (
              <Alert
                type="error"
                message={
                  <span>
                    <strong>{t('hooks.validation.errors')}:</strong>
                    <ul style={{ margin: '4px 0 0 0', paddingLeft: 16 }}>
                      {validation.errors.map((err, i) => (
                        <li key={i}>
                          {err.line != null && `${t('hooks.validation.line')} ${err.line}`}
                          {err.column != null && `, ${t('hooks.validation.col')} ${err.column}`}
                          : {err.message}
                        </li>
                      ))}
                    </ul>
                  </span>
                }
                style={{ marginBottom: 4 }}
                showIcon
              />
            )}
            {validation.warnings.length > 0 && (
              <Alert
                type="warning"
                message={
                  <span>
                    <strong>{t('hooks.validation.warnings')}:</strong>
                    <ul style={{ margin: '4px 0 0 0', paddingLeft: 16 }}>
                      {validation.warnings.map((w, i) => (
                        <li key={i}>{w}</li>
                      ))}
                    </ul>
                  </span>
                }
                style={{ marginBottom: 4 }}
                showIcon
              />
            )}
            {validation.valid && validation.errors.length === 0 && validation.warnings.length === 0 && (
              <Alert type="success" message={t('hooks.validation.valid')} showIcon />
            )}
          </div>
        )}

        {/* Bottom toolbar */}
        <div style={{ display: 'flex', alignItems: 'center', gap: 8, marginBottom: 12 }}>
          <Button
            onClick={handleValidate}
            loading={validating}
            icon={<BugOutlined />}
            size="small"
          >
            {t('hooks.validation.syntaxCheck')}
          </Button>
          <div style={{ flex: 1 }} />
          <Form.Item name="enabled" valuePropName="checked" style={{ marginBottom: 0 }}>
            <Switch checkedChildren={t('hooks.enabled')} unCheckedChildren={t('hooks.disabled')} />
          </Form.Item>
        </div>

        <Divider style={{ margin: '8px 0' }} />

        {/* Execution Parameters */}
        <Row gutter={16}>
          <Col span={8}>
            <Form.Item name="timeout_seconds" label={t('hooks.form.timeoutSeconds')} tooltip={t('hooks.form.timeoutSecondsHelp')}>
              <InputNumber min={1} max={300} style={{ width: '100%' }} suffix={<span style={{ userSelect: 'none', color: 'rgba(0,0,0,0.45)', fontSize: 14 }}>s</span>} />
            </Form.Item>
          </Col>
          <Col span={8}>
            <Form.Item name="priority" label={t('hooks.form.priority')} tooltip={t('hooks.form.priorityHelp')}>
              <InputNumber min={0} max={9999} style={{ width: '100%' }} />
            </Form.Item>
          </Col>
          <Col span={8}>
            <Form.Item name="fail_fast" label={t('hooks.form.failFast')} valuePropName="checked" tooltip={t('hooks.form.failFastHelp')}>
              <Switch />
            </Form.Item>
          </Col>
        </Row>
      </Form>
    </Modal>
  );
});

// ---------------------------------------------------------------------------
// Test Run Modal
// ---------------------------------------------------------------------------

interface TestRunValues {
  event_type: HookEventType;
  scenario?: string;
  tool_name: string;
  tool_input: string;
  expected: 'allow' | 'deny' | 'fail' | 'skipped';
}

function TestRunModal({
  open,
  hook,
  onClose,
}: {
  open: boolean;
  hook: HookInfo | null;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  const [form] = Form.useForm<TestRunValues>();
  const [running, setRunning] = useState(false);
  const [result, setResult] = useState<{
    decision: 'allow' | 'deny' | 'failed' | 'skipped';
    stdout: string;
    stderr: string;
    exitCode: number;
    durationMs: number;
    diagnostics: string[];
  } | null>(null);

  const handleRun = useCallback(async () => {
    if (!hook) return;
    form.validateFields().then(async (values) => {
      setRunning(true);
      setResult(null);
      const startedAt = Date.now();
      try {
        const parsedInput = values.tool_input?.trim()
          ? JSON.parse(values.tool_input)
          : {};
        const response = await hooksApi.dryRun(hook.id, {
          event_type: values.event_type,
          scenario: values.scenario === 'all' ? undefined : values.scenario,
          tool_name: values.tool_name,
          tool_input: parsedInput,
        });
        const decision = response.status === 'skipped'
          ? 'skipped'
          : response.exit_code === 2
          ? 'deny'
          : response.exit_code === 0
            ? 'allow'
            : 'failed';
        setResult({
          decision,
          stdout: response.stdout ?? '',
          stderr: response.stderr ?? response.error ?? '',
          exitCode: response.exit_code,
          durationMs: response.duration_ms,
          diagnostics: response.diagnostics ?? [],
        });
      } catch (err) {
        const messageText = err instanceof Error ? err.message : String(err);
        setResult({
          decision: 'failed',
          stdout: '',
          stderr: messageText,
          exitCode: 1,
          durationMs: Date.now() - startedAt,
          diagnostics: [],
        });
      } finally {
        setRunning(false);
      }
    });
  }, [hook, form]);

  const expected = Form.useWatch('expected', form) ?? 'allow';
  const decision = result?.decision;

  const getDecisionIcon = (d: string) => {
    switch (d) {
      case 'allow': return <CheckCircleOutlined style={{ color: '#52c41a' }} />;
      case 'deny': return <CloseCircleOutlined style={{ color: '#ff4d4f' }} />;
      case 'failed': return <WarningOutlined style={{ color: '#fa8c16' }} />;
      case 'skipped': return <WarningOutlined style={{ color: '#8c8c8c' }} />;
      default: return null;
    }
  };

  const getDecisionLabel = (d: string) => {
    switch (d) {
      case 'allow': return t('hooks.test.allowDecision');
      case 'deny': return t('hooks.test.denyDecision');
      case 'failed': return t('hooks.test.failDecision');
      case 'skipped': return t('hooks.test.skippedDecision');
      default: return '';
    }
  };

  return (
    <Modal
      title={t('hooks.test.title', { name: hook?.name ?? '' })}
      open={open}
      onOk={handleRun}
      onCancel={onClose}
      okText={running ? t('hooks.test.running') : t('hooks.test.run')}
      cancelText={t('common.cancel')}
      confirmLoading={running}
      width={640}
      destroyOnHidden
      afterOpenChange={(visible) => {
        if (visible && hook) {
          form.setFieldsValue({
            event_type: hook.event_type,
            scenario: hook.scenarios?.[0] ?? 'all',
            tool_name: 'bash',
            tool_input: '{"command": "echo hello"}',
            expected: 'allow',
          });
          setResult(null);
        }
      }}
    >
      <Form form={form} layout="vertical">
        <Row gutter={16}>
          <Col span={12}>
            <Form.Item name="event_type" label={t('hooks.test.selectEvent')}>
              <Select
                options={HOOK_EVENT_TYPES.map((value) => ({
                  value,
                  label: t(`hooks.eventType.${value}`),
                }))}
              />
            </Form.Item>
          </Col>
          <Col span={12}>
            <Form.Item name="scenario" label={t('hooks.test.scenario')} tooltip={t('hooks.test.scenarioHelp')}>
              <Select
                options={[
                  { value: 'all', label: t('hooks.scenariosAll') },
                  ...HOOK_SCENARIOS.map((scenario) => ({
                    value: scenario,
                    label: t(scenarioLabelKey(scenario)),
                  })),
                ]}
              />
            </Form.Item>
          </Col>
          <Col span={12}>
            <Form.Item name="tool_name" label={t('hooks.test.toolName')}>
              <Input placeholder={t('hooks.test.toolNamePlaceholder')} />
            </Form.Item>
          </Col>
        </Row>

        <Form.Item name="tool_input" label={t('hooks.test.toolInput')}>
          <Input.TextArea
            placeholder={t('hooks.test.toolInputPlaceholder')}
            rows={3}
            style={{ fontFamily: 'monospace', fontSize: 12 }}
          />
        </Form.Item>

        <Form.Item name="expected" label={t('hooks.test.expectedBehavior')}>
          <Radio.Group>
            <Radio.Button value="allow">{t('hooks.test.allow')}</Radio.Button>
            <Radio.Button value="deny">{t('hooks.test.deny')}</Radio.Button>
            <Radio.Button value="fail">{t('hooks.test.fail')}</Radio.Button>
            <Radio.Button value="skipped">{t('hooks.test.skipped')}</Radio.Button>
          </Radio.Group>
        </Form.Item>
      </Form>

      {result && (
        <div
          style={{
            marginTop: 16,
            padding: 16,
            borderRadius: 8,
            border: '1px solid',
            borderColor: result.decision === 'allow'
              ? '#52c41a'
              : result.decision === 'deny'
              ? '#ff4d4f'
              : result.decision === 'skipped'
              ? '#8c8c8c'
              : '#fa8c16',
            background: '#1e1e1e',
          }}
        >
          <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', marginBottom: 12 }}>
            <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
              {getDecisionIcon(result.decision)}
              <Title level={5} style={{ margin: 0, color: '#fff' }}>{t('hooks.test.result')}</Title>
              <span style={{
                padding: '2px 8px',
                borderRadius: 4,
                fontSize: 12,
                fontWeight: 600,
                background: result.decision === 'allow'
                  ? 'rgba(82, 196, 26, 0.15)'
                  : result.decision === 'deny'
                  ? 'rgba(255, 77, 79, 0.15)'
                  : result.decision === 'skipped'
                  ? 'rgba(140, 140, 140, 0.15)'
                  : 'rgba(250, 140, 22, 0.15)',
                color: result.decision === 'allow'
                  ? '#52c41a'
                  : result.decision === 'deny'
                  ? '#ff4d4f'
                  : result.decision === 'skipped'
                  ? '#bfbfbf'
                  : '#fa8c16',
              }}>
                {getDecisionLabel(result.decision)}
              </span>
            </div>
            <Text style={{ fontSize: 12, color: '#8b949e' }}>
              {t('hooks.test.duration')}: {result.durationMs < 1000
                ? `${result.durationMs}ms`
                : `${(result.durationMs / 1000).toFixed(2)}s`}
            </Text>
          </div>

          {result.stdout && (
            <div style={{ marginBottom: 8 }}>
              <Text strong style={{ color: '#8b949e', fontSize: 12 }}>{t('hooks.test.stdout')}:</Text>
              <pre
                style={{
                  color: '#4ec9b0',
                  padding: 8,
                  borderRadius: 4,
                  fontSize: 12,
                  fontFamily: 'monospace',
                  maxHeight: 150,
                  overflow: 'auto',
                  marginTop: 4,
                  marginBottom: 0,
                  background: '#111',
                  border: '1px solid #333',
                }}
              >
                {result.stdout}
              </pre>
            </div>
          )}

          {result.diagnostics.length > 0 && (
            <Alert
              type="warning"
              showIcon
              style={{ marginBottom: 8 }}
              message={t('hooks.test.diagnostics')}
              description={
                <ul style={{ paddingLeft: 18, margin: 0 }}>
                  {result.diagnostics.map((item) => (
                    <li key={item}>{t(DIAGNOSTIC_I18N_KEYS[item] ?? item)}</li>
                  ))}
                </ul>
              }
            />
          )}

          {result.stderr && (
            <div style={{ marginBottom: 8 }}>
              <Text strong style={{ color: '#8b949e', fontSize: 12 }}>{t('hooks.test.stderr')}:</Text>
              <pre
                style={{
                  color: '#f48771',
                  padding: 8,
                  borderRadius: 4,
                  fontSize: 12,
                  fontFamily: 'monospace',
                  maxHeight: 100,
                  overflow: 'auto',
                  marginTop: 4,
                  marginBottom: 0,
                  background: '#111',
                  border: '1px solid #333',
                }}
              >
                {result.stderr}
              </pre>
            </div>
          )}

          <div style={{ display: 'flex', alignItems: 'center', gap: 12 }}>
            <Text style={{ color: '#8b949e', fontSize: 12 }}>
              <strong>{t('hooks.test.exitCode')}:</strong>{' '}
              <code style={{
                background: '#111',
                color: result.exitCode === 0 ? '#4ec9b0' : '#f48771',
                padding: '1px 6px',
                borderRadius: 3,
                border: '1px solid #333',
                fontSize: 12,
              }}>{result.exitCode}</code>
            </Text>
            {decision && (
              <Text style={{ color: '#8b949e', fontSize: 12 }}>
                <strong>{t('hooks.test.expected')}:</strong>{' '}
                <Tag
                  color={
                    expected === 'allow'
                      ? 'green'
                      : expected === 'deny'
                      ? 'red'
                      : expected === 'skipped'
                      ? 'default'
                      : 'orange'
                  }
                  style={{ margin: 0 }}
                >
                  {expected === 'allow'
                    ? t('hooks.test.allow')
                    : expected === 'deny'
                    ? t('hooks.test.deny')
                    : expected === 'skipped'
                    ? t('hooks.test.skipped')
                    : t('hooks.test.fail')}
                </Tag>
                <Text style={{
                  color: decision === expected ? '#52c41a' : '#ff4d4f',
                  fontWeight: 500,
                  marginLeft: 4,
                }}>
                  ({decision === expected ? t('hooks.test.match') : t('hooks.test.mismatch')})
                </Text>
              </Text>
            )}
          </div>
        </div>
      )}
    </Modal>
  );
}

// ---------------------------------------------------------------------------
// Hook Execution Logs Drawer
// ---------------------------------------------------------------------------

function HookLogsDrawer({
  open,
  hook,
  onClose,
}: {
  open: boolean;
  hook: HookInfo | null;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  const [page, setPage] = useState(1);
  const pageSize = 10;

  const logsQ = useQuery({
    queryKey: hook?.id ? ['hooks', 'logs', hook.id, page, pageSize] : ['hooks', 'logs', 'empty'],
    queryFn: () => hooksApi.logs(hook!.id, { page, per_page: pageSize }),
    enabled: open && Boolean(hook?.id),
  });

  const logs = logsQ.data?.logs ?? [];

  const logColumns: ColumnsType<HookLogEntry> = [
    {
      title: t('hooks.logs.executedAt'),
      dataIndex: 'executed_at',
      key: 'executed_at',
      width: 180,
      render: (value: string) => dayjs(value).format('YYYY-MM-DD HH:mm:ss'),
    },
    {
      title: t('hooks.columns.eventType'),
      dataIndex: 'event_type',
      key: 'event_type',
      width: 150,
      render: (value: HookEventType) => (
        <Tag color={EVENT_TYPE_COLORS[value] ?? 'default'}>{t(`hooks.eventType.${value}`)}</Tag>
      ),
    },
    {
      title: t('hooks.columns.scenarios'),
      dataIndex: 'scenario',
      key: 'scenario',
      width: 130,
      render: (value?: string | null) => value ? (
        <Tag color={HOOK_SCENARIO_COLORS[value as HookScenario] ?? 'default'}>
          {t(scenarioLabelKey(value))}
        </Tag>
      ) : (
        <Tag color="default">{t('hooks.scenariosAll')}</Tag>
      ),
    },
    {
      title: t('hooks.logs.toolName'),
      dataIndex: 'tool_name',
      key: 'tool_name',
      ellipsis: true,
      render: (value: string) => <Text code>{value}</Text>,
    },
    {
      title: t('hooks.logs.exitCode'),
      dataIndex: 'exit_code',
      key: 'exit_code',
      width: 100,
      render: (value?: number | null) => {
        if (value == null) return <Text type="secondary">-</Text>;
        return <Tag color={value === 0 ? 'green' : value === 2 ? 'red' : 'orange'}>{value}</Tag>;
      },
    },
    {
      title: t('hooks.logs.duration'),
      dataIndex: 'duration_ms',
      key: 'duration_ms',
      width: 110,
      render: (value?: number | null) => value == null ? '-' : `${value}ms`,
    },
  ];

  return (
    <Drawer
      title={hook ? t('hooks.logs.titleWithName', { name: hook.name }) : t('hooks.logs.title')}
      open={open}
      onClose={onClose}
      width={920}
      destroyOnHidden
      afterOpenChange={(visible) => {
        if (visible) setPage(1);
      }}
    >
      <Table
        rowKey="id"
        loading={logsQ.isLoading || logsQ.isFetching}
        columns={logColumns}
        dataSource={logs}
        pagination={{
          current: page,
          pageSize,
          total: logsQ.data?.total ?? 0,
          showSizeChanger: false,
          onChange: setPage,
        }}
        locale={{ emptyText: t('hooks.logs.empty') }}
        expandable={{
          expandedRowRender: (row) => (
            <Space direction="vertical" size={12} style={{ width: '100%' }}>
              <div>
                <Text strong>{t('hooks.logs.inputJson')}</Text>
                <pre style={{ marginTop: 6, whiteSpace: 'pre-wrap', wordBreak: 'break-word' }}>
                  {safePrettyJson(row.input_json)}
                </pre>
              </div>
              <div>
                <Text strong>{t('hooks.logs.outputJson')}</Text>
                <pre style={{ marginTop: 6, whiteSpace: 'pre-wrap', wordBreak: 'break-word' }}>
                  {safePrettyJson(row.output_json)}
                </pre>
              </div>
              {row.error_message ? (
                <Alert
                  type="error"
                  showIcon
                  message={t('hooks.logs.errorMessage')}
                  description={<pre style={{ margin: 0, whiteSpace: 'pre-wrap' }}>{row.error_message}</pre>}
                />
              ) : null}
            </Space>
          ),
        }}
      />
    </Drawer>
  );
}

// ---------------------------------------------------------------------------
// Main Hooks Page
// ---------------------------------------------------------------------------

export default function Hooks() {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const { hasPermission } = usePermissions();
  const [hookKeyword, setHookKeyword] = useState('');
  const [eventFilter, setEventFilter] = useState<string | null>(null);
  const [hookModal, setHookModal] = useState<{
    open: boolean;
    editing?: HookInfo;
    defaultLanguage?: HookLanguage;
  }>({ open: false });
  const [testModal, setTestModal] = useState<{
    open: boolean;
    hook: HookInfo | null | undefined;
  }>({ open: false, hook: null });
  const [logsDrawer, setLogsDrawer] = useState<{
    open: boolean;
    hook: HookInfo | null | undefined;
  }>({ open: false, hook: null });
  const [selectedRowKeys, setSelectedRowKeys] = useState<React.Key[]>([]);
  const [batchLoading, setBatchLoading] = useState(false);

  useSystemEvents({
    onHooksUpdated: () => {
      qc.invalidateQueries({ queryKey: queryKeys.hooks.all });
    },
  });

  const { data: hooksData, isLoading } = useQuery({
    queryKey: queryKeys.hooks.list(),
    queryFn: () => hooksApi.list({ per_page: 100 }),
  });

  const hooks = hooksData?.hooks ?? [];

  const upsertHook = useMutation({
    mutationFn: async (values: Parameters<typeof hooksApi.create>[0]) => {
      if (hookModal.editing) {
        return hooksApi.update(hookModal.editing.id, values as Parameters<typeof hooksApi.update>[1]);
      }
      return hooksApi.create(values as Parameters<typeof hooksApi.create>[0]);
    },
    onSuccess: () => {
      message.success(hookModal.editing ? t('hooks.editSuccess') : t('hooks.addSuccess'));
      qc.invalidateQueries({ queryKey: queryKeys.hooks.all });
      setHookModal({ open: false });
    },
    onError: (err: Error) => {
      message.error(err.message ?? t('common.operateFailed'));
    },
  });

  const deleteHook = useMutation({
    mutationFn: (id: string) => hooksApi.delete(id),
    onSuccess: () => {
      message.success(t('hooks.deleteSuccess'));
      qc.invalidateQueries({ queryKey: queryKeys.hooks.all });
      setSelectedRowKeys((keys) => keys.filter((k) => !hooks.find((h) => h.id === k)));
    },
    onError: (err: Error) => {
      message.error(err.message ?? t('common.operateFailed'));
    },
  });

  const toggleHook = useMutation({
    mutationFn: ({ id, enabled }: { id: string; enabled: boolean }) =>
      hooksApi.update(id, { enabled }),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: queryKeys.hooks.all });
    },
    onError: (err: Error) => {
      message.error(err.message ?? t('common.operateFailed'));
    },
  });

  const filteredHooks = useMemo(() => {
    return hooks.filter((h) => {
      const matchKeyword =
        !hookKeyword ||
        h.name.toLowerCase().includes(hookKeyword.toLowerCase()) ||
        (h.code ?? '').toLowerCase().includes(hookKeyword.toLowerCase()) ||
        h.command.toLowerCase().includes(hookKeyword.toLowerCase());
      const matchEvent = !eventFilter || h.event_type === eventFilter;
      return matchKeyword && matchEvent;
    });
  }, [hooks, hookKeyword, eventFilter]);

  const toolHookCount = hooks.filter((h) => TOOL_EVENT_TYPES.has(h.event_type)).length;
  const lifecycleHookCount = hooks.length - toolHookCount;
  const enabledCount = hooks.filter((h) => h.enabled).length;

  const openAddHook = useCallback((lang?: HookLanguage) => {
    setHookModal({ open: true, defaultLanguage: lang });
  }, []);

  const openEditHook = useCallback((hook: HookInfo) => {
    setHookModal({ open: true, editing: hook });
  }, []);

  const handleHookEditorSave = useCallback(
    (values: HookEditorValues) => {
      const payload = {
        name: values.name,
        description: values.description,
        event_type: values.event_type,
        language: values.language,
        code: values.code,
        command: values.command,
        enabled: values.enabled,
        priority: values.priority,
        timeout_seconds: values.timeout_seconds,
        fail_fast: values.fail_fast,
        scenarios: values.scenarios ?? [],
      };
      upsertHook.mutate(payload as Parameters<typeof hooksApi.create>[0]);
    },
    [upsertHook]
  );

  const handleBatchEnable = useCallback(async () => {
    setBatchLoading(true);
    try {
      await Promise.all(selectedRowKeys.map((id) => hooksApi.update(String(id), { enabled: true })));
      message.success(t('common.operateSuccess'));
      qc.invalidateQueries({ queryKey: queryKeys.hooks.all });
      setSelectedRowKeys([]);
    } catch {
      message.error(t('common.operateFailed'));
    } finally {
      setBatchLoading(false);
    }
  }, [selectedRowKeys, qc, t]);

  const handleBatchDisable = useCallback(async () => {
    setBatchLoading(true);
    try {
      await Promise.all(selectedRowKeys.map((id) => hooksApi.update(String(id), { enabled: false })));
      message.success(t('common.operateSuccess'));
      qc.invalidateQueries({ queryKey: queryKeys.hooks.all });
      setSelectedRowKeys([]);
    } catch {
      message.error(t('common.operateFailed'));
    } finally {
      setBatchLoading(false);
    }
  }, [selectedRowKeys, qc, t]);

  const handleBatchDelete = useCallback(() => {
    Modal.confirm({
      title: t('hooks.deleteConfirm'),
      okText: t('common.confirm'),
      cancelText: t('common.cancel'),
      onOk: async () => {
        setBatchLoading(true);
        try {
          await Promise.all(selectedRowKeys.map((id) => hooksApi.delete(String(id))));
          message.success(t('hooks.deleteSuccess'));
          qc.invalidateQueries({ queryKey: queryKeys.hooks.all });
          setSelectedRowKeys([]);
        } catch {
          message.error(t('common.operateFailed'));
        } finally {
          setBatchLoading(false);
        }
      },
    });
  }, [selectedRowKeys, qc, t]);

  const canWrite = hasPermission('hooks:write');

  const addHookDropdownItems: MenuProps['items'] = [
    {
      key: 'addPython',
      label: t('hooks.language.python'),
      icon: <RobotOutlined />,
      onClick: () => openAddHook('python'),
    },
    {
      key: 'addShell',
      label: t('hooks.language.shell'),
      icon: <CodeOutlined />,
      onClick: () => openAddHook('shell'),
    },
  ];

  const rowSelection: TableRowSelection<HookInfo> = {
    selectedRowKeys,
    onChange: (keys) => setSelectedRowKeys(keys),
  };

  const hookColumns: ColumnsType<HookInfo> = [
    {
      title: t('hooks.columns.name'),
      dataIndex: 'name',
      key: 'name',
      render: (v: string, r: HookInfo) => (
        <Space>
          {r.enabled
            ? <span style={{ width: 8, height: 8, borderRadius: '50%', background: '#52c41a', display: 'inline-block' }} />
            : <span style={{ width: 8, height: 8, borderRadius: '50%', background: '#d9d9d9', display: 'inline-block' }} />
          }
          <Text strong>{v}</Text>
          {!r.enabled && <Tag color="default">{t('hooks.disabled')}</Tag>}
        </Space>
      ),
    },
    {
      title: t('hooks.columns.eventType'),
      dataIndex: 'event_type',
      key: 'event_type',
      width: 130,
      render: (v: HookEventType) => (
        <Tag color={EVENT_TYPE_COLORS[v] ?? 'default'}>{t(`hooks.eventType.${v}`)}</Tag>
      ),
    },
    {
      title: t('hooks.columns.language'),
      dataIndex: 'language',
      key: 'language',
      width: 100,
      render: (v: string) => (
        <Tag color={LANGUAGE_COLORS[v] ?? 'default'} style={{ fontFamily: 'monospace', fontSize: 12 }}>
          {v === 'python' ? 'Python' : 'Shell'}
        </Tag>
      ),
    },
    {
      title: t('hooks.columns.scenarios'),
      dataIndex: 'scenarios',
      key: 'scenarios',
      width: 170,
      render: (scenarios?: string[] | null) => {
        if (!scenarios || scenarios.length === 0) {
          return <Tag color="default">{t('hooks.scenariosAll')}</Tag>;
        }
        return (
          <Space size={[4, 4]} wrap>
            {scenarios.map((scenario) => (
              <Tag
                key={scenario}
                color={HOOK_SCENARIO_COLORS[scenario as HookScenario] ?? 'default'}
                style={{ margin: 0 }}
              >
                {t(scenarioLabelKey(scenario))}
              </Tag>
            ))}
          </Space>
        );
      },
    },
    {
      title: t('hooks.columns.command'),
      dataIndex: 'code',
      key: 'code',
      ellipsis: { showTitle: false },
      render: (_: string, r: HookInfo) => {
        const preview = (r.code || r.command || '').slice(0, 80).replace(/\n/g, ' ');
        return (
          <Tooltip title={<pre style={{ margin: 0, fontFamily: 'monospace', fontSize: 11 }}>{r.code || r.command}</pre>}>
            <code style={{ fontSize: 11, opacity: 0.8 }}>{preview}{preview.length >= 80 ? '...' : ''}</code>
          </Tooltip>
        );
      },
    },
    {
      title: t('hooks.columns.priority'),
      dataIndex: 'priority',
      key: 'priority',
      width: 80,
      sorter: (a, b) => a.priority - b.priority,
      render: (v: number) => <Text code>{v}</Text>,
    },
    {
      title: t('hooks.columns.timeout'),
      dataIndex: 'timeout_seconds',
      key: 'timeout_seconds',
      width: 70,
      render: (v: number) => <Text type="secondary">{v}s</Text>,
    },
    {
      title: t('hooks.columns.createdAt'),
      dataIndex: 'created_at',
      key: 'created_at',
      width: 120,
      sorter: (a, b) => dayjs(a.created_at).unix() - dayjs(b.created_at).unix(),
      render: (v: string) => dayjs(v).fromNow(),
    },
    {
      title: t('hooks.columns.actions'),
      key: 'actions',
      width: 180,
      render: (_: unknown, r: HookInfo) => (
        <Space size="small">
          <Tooltip title={t('hooks.test.button')}>
            <Button type="text" size="small" icon={<PlayCircleOutlined />} onClick={() => setTestModal({ open: true, hook: r })} />
          </Tooltip>
          <Tooltip title={t('hooks.logs.button')}>
            <Button type="text" size="small" icon={<HistoryOutlined />} onClick={() => setLogsDrawer({ open: true, hook: r })} />
          </Tooltip>
          {canWrite && (
            <Button type="text" size="small" icon={<EditOutlined />} onClick={() => openEditHook(r)} />
          )}
          {canWrite && (
            <Popconfirm
              title={t('hooks.deleteConfirm')}
              onConfirm={() => deleteHook.mutate(r.id)}
              okText={t('common.confirm')}
              cancelText={t('common.cancel')}
            >
              <Button type="text" size="small" danger icon={<DeleteOutlined />} />
            </Popconfirm>
          )}
        </Space>
      ),
    },
  ];

  if (isLoading) return <PageSkeleton rows={6} />;

  return (
    <div style={{ padding: 24 }}>
      <div style={{ marginBottom: 20 }}>
        <Title level={3} style={{ margin: '0 0 4px' }}>{t('hooks.title')}</Title>
        <Text type="secondary">{t('hooks.subtitle')}</Text>
      </div>

      {/* Statistics Cards */}
      <Row gutter={16} style={{ marginBottom: 16 }}>
        <Col span={6}>
          <Card size="small">
            <Statistic
              title={
                <Space>
                  <ThunderboltOutlined style={{ color: '#1677ff' }} />
                  {t('hooks.stats.toolHooks')}
                </Space>
              }
              value={toolHookCount}
              suffix={hooks.length > 0 ? `/ ${hooks.length}` : ''}
              valueStyle={{ color: '#1677ff' }}
            />
          </Card>
        </Col>
        <Col span={6}>
          <Card size="small">
            <Statistic
              title={
                <Space>
                  <CheckCircleOutlined style={{ color: '#52c41a' }} />
                  {t('hooks.stats.lifecycleHooks')}
                </Space>
              }
              value={lifecycleHookCount}
              valueStyle={{ color: '#52c41a' }}
            />
          </Card>
        </Col>
        <Col span={6}>
          <Card size="small">
            <Statistic
              title={
                <Space>
                  <CloseCircleOutlined style={{ color: '#fa8c16' }} />
                  {t('hooks.enabled')}
                </Space>
              }
              value={enabledCount}
              suffix={`/ ${hooks.length}`}
              valueStyle={{ color: '#fa8c16' }}
            />
          </Card>
        </Col>
        <Col span={6}>
          <Card size="small">
            <Statistic
              title={t('hooks.stats.totalHooks')}
              value={hooks.length}
            />
          </Card>
        </Col>
      </Row>

      {/* Main Table Card */}
      <Card styles={{ body: { padding: 0 } }}>
        {/* Toolbar */}
        <div
          style={{
            padding: '12px 16px',
            display: 'flex',
            gap: 8,
            borderBottom: '1px solid var(--border-subtle)',
            flexWrap: 'wrap',
            alignItems: 'center',
          }}
        >
          <Input
            prefix={<SearchOutlined />}
            placeholder={t('hooks.searchPlaceholder')}
            value={hookKeyword}
            onChange={(e) => setHookKeyword(e.target.value)}
            style={{ width: 280 }}
            allowClear
          />
          {eventFilter && (
            <Tag
              closable
              onClose={() => setEventFilter(null)}
              color={EVENT_TYPE_COLORS[eventFilter]}
            >
              {t(`hooks.eventType.${eventFilter}`)}
            </Tag>
          )}
          <div style={{ flex: 1 }} />
          {selectedRowKeys.length > 0 && (
            <Space>
              <Text type="secondary">
                {t('hooks.batch.selected', { count: selectedRowKeys.length })}
              </Text>
              {canWrite && (
                <>
                  <Button size="small" onClick={handleBatchEnable} loading={batchLoading}>
                    {t('hooks.batch.enable')}
                  </Button>
                  <Button size="small" onClick={handleBatchDisable} loading={batchLoading}>
                    {t('hooks.batch.disable')}
                  </Button>
                  <Button size="small" danger onClick={handleBatchDelete} loading={batchLoading}>
                    {t('hooks.batch.delete')}
                  </Button>
                  <Button size="small" type="text" onClick={() => setSelectedRowKeys([])}>
                    {t('hooks.batch.clear')}
                  </Button>
                </>
              )}
            </Space>
          )}
          {canWrite && selectedRowKeys.length === 0 && (
            <Dropdown menu={{ items: addHookDropdownItems }} trigger={['click']}>
              <Button type="primary" icon={<PlusOutlined />}>
                {t('hooks.add')}
                <DownOutlined />
              </Button>
            </Dropdown>
          )}
        </div>

        <Table
          rowKey="id"
          columns={hookColumns}
          dataSource={filteredHooks}
          rowSelection={canWrite ? rowSelection : undefined}
          loading={isLoading}
          pagination={
            filteredHooks.length > 10
              ? { pageSize: 10, showSizeChanger: false }
              : false
          }
          locale={{
            emptyText: (
              <div style={{ padding: 48, textAlign: 'center' }}>
                <Text type="secondary">{t('hooks.empty.description')}</Text>
                <br />
                {canWrite && (
                  <Dropdown menu={{ items: addHookDropdownItems }} trigger={['click']}>
                    <Button type="link" icon={<PlusOutlined />}>
                      {t('hooks.add')}
                    </Button>
                  </Dropdown>
                )}
              </div>
            ),
          }}
        />
      </Card>

      {/* Hook Editor Modal */}
      <HookEditorModal
        open={hookModal.open}
        editing={hookModal.editing}
        defaultLanguage={hookModal.defaultLanguage}
        onSave={handleHookEditorSave}
        onCancel={() => setHookModal({ open: false })}
      />

      {/* Test Run Modal */}
      <TestRunModal
        open={testModal.open}
        hook={testModal.hook ?? null}
        onClose={() => setTestModal({ open: false, hook: null })}
      />

      {/* Hook Execution Logs Drawer */}
      <HookLogsDrawer
        open={logsDrawer.open}
        hook={logsDrawer.hook ?? null}
        onClose={() => setLogsDrawer({ open: false, hook: null })}
      />
    </div>
  );
}
