// ── NL2SQL Time Patterns Tab ───────────────────────────────────────────────────

import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import {
  Table, Button, Modal, Form, Input, Select, Space, Tag, message, Popconfirm,
  Typography, Switch, InputNumber, Alert, Tooltip,
} from 'antd';
import {
  PlusOutlined, DeleteOutlined, EditOutlined, ClockCircleOutlined, QuestionCircleOutlined,
} from '@ant-design/icons';
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query';
import { nl2sqlApi } from '@/api';
import { queryKeys } from '@/api/queryKeys';
import type {
  TimePattern, CreateTimePatternRequest, UpdateTimePatternRequest,
  ListTimePatternsResponse,
} from '@/types';

const { Text } = Typography;
const { TextArea } = Input;

const RESOLVED_TYPES = [
  'today', 'yesterday', 'this_week', 'this_month', 'last_month',
  'this_quarter', 'last_quarter', 'this_year', 'ytd', 'mom', 'yoy',
  'wow', 'woww', 'qoq', 'custom',
];

const REGEX_EXAMPLES = [
  '(?:最近|近)(\\d+)天',
  '(?:last\\s*)(\\d+)\\s*days',
  '(?:本月|这个月)',
  '(?:上月|上个月)',
];

export function TimePatternsTab() {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const [modalOpen, setModalOpen] = useState(false);
  const [editingId, setEditingId] = useState<number | null>(null);
  const [regexTestResult, setRegexTestResult] = useState<{
    pattern: string;
    text: string;
    matched: boolean;
    fullMatch?: string;
    groups: string[];
  } | null>(null);
  const [form] = Form.useForm();
  const resolvedType = Form.useWatch('resolvedType', form);

  const resolvedTypeOptions = RESOLVED_TYPES.map((v) => ({
    value: v,
    label: t(`management.timePatterns.resolvedTypeOptions.${v}`),
  }));
  const regexValidator = (_: unknown, value?: string) => {
    const text = (value ?? '').trim();
    if (!text) {
      return Promise.reject(new Error(t('management.timePatterns.regexRequired')));
    }
    try {
      // Frontend fast validation to reduce trial/error; backend still validates authoritatively.
      new RegExp(text);
      return Promise.resolve();
    } catch {
      return Promise.reject(new Error(t('management.timePatterns.regexInvalid')));
    }
  };

  const { data, isLoading } = useQuery<ListTimePatternsResponse>({
    queryKey: queryKeys.nl2sql.timePatterns.all(),
    queryFn: () => nl2sqlApi.listTimePatterns(),
  });

  const create = useMutation({
    mutationFn: (payload: CreateTimePatternRequest) => nl2sqlApi.createTimePattern(payload),
    onSuccess: () => {
      message.success(t('management.timePatterns.createSuccess'));
      setModalOpen(false);
      form.resetFields();
      qc.invalidateQueries({ queryKey: queryKeys.nl2sql.timePatterns.all() });
    },
    onError: (e: Error) => message.error(e?.message ?? t('common.failed')),
  });

  const update = useMutation({
    mutationFn: ({ id, data: vals }: { id: number; data: UpdateTimePatternRequest }) =>
      nl2sqlApi.updateTimePattern(id, vals),
    onSuccess: () => {
      message.success(t('management.timePatterns.updateSuccess'));
      setEditingId(null);
      setModalOpen(false);
      form.resetFields();
      qc.invalidateQueries({ queryKey: queryKeys.nl2sql.timePatterns.all() });
    },
    onError: (e: Error) => message.error(e?.message ?? t('common.failed')),
  });

  const remove = useMutation({
    mutationFn: (id: number) => nl2sqlApi.deleteTimePattern(id),
    onSuccess: () => {
      message.success(t('management.timePatterns.deleteSuccess'));
      qc.invalidateQueries({ queryKey: queryKeys.nl2sql.timePatterns.all() });
    },
    onError: (e: Error) => message.error(e?.message ?? t('common.failed')),
  });

  const toggle = useMutation({
    mutationFn: ({ id, enabled }: { id: number; enabled: boolean }) =>
      nl2sqlApi.updateTimePattern(id, { enabled }),
    onSuccess: () => {
      message.success(t('common.success'));
      qc.invalidateQueries({ queryKey: queryKeys.nl2sql.timePatterns.all() });
    },
    onError: (e: Error) => message.error(e?.message ?? t('common.failed')),
  });

  const openCreate = () => {
    setEditingId(null);
    form.resetFields();
    form.setFieldsValue({ customResolvedType: '' });
    setRegexTestResult(null);
    setModalOpen(true);
  };

  const openEdit = (record: TimePattern) => {
    setEditingId(record.id);
    form.setFieldsValue({
      patternDisplay: record.patternDisplay,
      patternRegex: record.patternRegex,
      resolvedType: record.resolvedType,
      customResolvedType: record.resolvedType === 'custom' ? record.patternDisplay : '',
      priority: record.priority,
      testText: '',
    });
    setRegexTestResult(null);
    setModalOpen(true);
  };

  const insertRegexExample = (example: string) => {
    form.setFieldsValue({ patternRegex: example });
    setRegexTestResult(null);
  };

  const evaluateRegex = () => {
    const pattern = String(form.getFieldValue('patternRegex') ?? '').trim();
    const text = String(form.getFieldValue('testText') ?? '').trim();
    if (!pattern || !text) {
      message.warning(t('management.timePatterns.testRequired'));
      return null;
    }
    try {
      const match = new RegExp(pattern).exec(text);
      const result = {
        pattern,
        text,
        matched: !!match,
        fullMatch: match?.[0],
        groups: match ? match.slice(1).map((value) => value ?? '') : [],
      };
      setRegexTestResult(result);
      return result;
    } catch {
      message.error(t('management.timePatterns.regexInvalid'));
      return null;
    }
  };

  const handleModalOk = () => {
    form.validateFields().then((values) => {
      if (values.testText?.trim()) {
        const currentResult = regexTestResult?.pattern === values.patternRegex?.trim()
          && regexTestResult?.text === values.testText.trim()
          ? regexTestResult
          : evaluateRegex();
        if (!currentResult?.matched) {
          message.error(t('management.timePatterns.testNoMatch'));
          return;
        }
      }
      const payload = { ...values };
      if (payload.resolvedType === 'custom') {
        payload.patternDisplay = payload.customResolvedType?.trim() || payload.patternDisplay;
      }
      delete payload.customResolvedType;
      if (editingId !== null) {
        update.mutate({ id: editingId, data: payload });
      } else {
        create.mutate(payload as CreateTimePatternRequest);
      }
    });
  };

  const columns = [
    {
      title: t('management.timePatterns.enabled'),
      dataIndex: 'enabled',
      key: 'enabled',
      width: 80,
      render: (v: boolean, record: TimePattern) => (
        <Switch
          size="small"
          checked={v}
          onChange={(checked) => toggle.mutate({ id: record.id, enabled: checked })}
        />
      ),
    },
    {
      title: t('management.timePatterns.showName'),
      dataIndex: 'patternDisplay',
      key: 'patternDisplay',
      width: 130,
      render: (v: string, record: TimePattern) => (
        <Text strong>{v || <Text type="secondary" style={{ fontStyle: 'italic' }}>{record.patternRegex}</Text>}</Text>
      ),
    },
    {
      title: t('management.timePatterns.patternRegex'),
      dataIndex: 'patternRegex',
      key: 'patternRegex',
      width: 200,
      render: (v: string) => (
        <Text code style={{ fontSize: 12, maxWidth: 190, overflow: 'hidden', textOverflow: 'ellipsis', display: 'block' }}>
          {v}
        </Text>
      ),
    },
    {
      title: t('management.timePatterns.resolvedType'),
      dataIndex: 'resolvedType',
      key: 'resolvedType',
      width: 120,
      render: (v: string) => <Tag color="blue">{t(`management.timePatterns.resolvedTypeOptions.${v}`)}</Tag>,
    },
    {
      title: (
        <Space size={4}>
          {t('management.timePatterns.priority')}
          <Tooltip title={t('management.timePatterns.priorityHelp')}>
            <QuestionCircleOutlined style={{ color: 'var(--text-muted)', fontSize: 12 }} />
          </Tooltip>
        </Space>
      ),
      dataIndex: 'priority',
      key: 'priority',
      width: 70,
      render: (n: number) => n,
    },
    {
      title: t('management.timePatterns.actions'),
      key: 'actions',
      width: 120,
      render: (_: unknown, record: TimePattern) => (
        <Space size="small">
          <Tooltip title={t('management.timePatterns.editTitle')}>
            <Button size="small" icon={<EditOutlined />} onClick={() => openEdit(record)} />
          </Tooltip>
          <Popconfirm
            title={t('management.timePatterns.deleteConfirm')}
            onConfirm={() => remove.mutate(record.id)}
          >
            <Button size="small" danger icon={<DeleteOutlined />} />
          </Popconfirm>
        </Space>
      ),
    },
  ];

  return (
    <div>
      <div style={{ display: 'flex', gap: 12, marginBottom: 16, alignItems: 'center' }}>
        <Alert
          message={t('management.timePatterns.hint')}
          type="info"
          showIcon
          icon={<ClockCircleOutlined />}
          style={{ flex: 1 }}
        />
        <Button
          type="primary"
          icon={<PlusOutlined />}
          onClick={openCreate}
        >
          {t('management.timePatterns.newPattern')}
        </Button>
        <Text type="secondary" style={{ marginLeft: 'auto' }}>
          {data?.patterns?.length ?? 0} {t('management.timePatterns.patternsCount')}
        </Text>
      </div>

      <Table
        dataSource={data?.patterns ?? []}
        columns={columns}
        rowKey="id"
        loading={isLoading}
        pagination={{ pageSize: 15, showSizeChanger: false }}
        size="small"
      />

      <Modal
        title={editingId !== null ? t('management.timePatterns.editTitle') : t('management.timePatterns.createTitle')}
        open={modalOpen}
        onOk={handleModalOk}
        onCancel={() => { setModalOpen(false); setEditingId(null); setRegexTestResult(null); form.resetFields(); }}
        okText={t('management.timePatterns.create')}
        cancelText={t('common.cancel')}
        confirmLoading={create.isPending || update.isPending}
        width={560}
      >
        <Form
          form={form}
          layout="vertical"
          initialValues={{ priority: 50 }}
        >
          <Alert
            type="info"
            showIcon
            style={{ marginBottom: 12 }}
            message={t('management.timePatterns.fieldGuide')}
            description={t('management.timePatterns.fieldGuideDesc')}
          />
          <Form.Item
            name="patternDisplay"
            label={t('management.timePatterns.displayName')}
          >
            <Input placeholder={t('management.timePatterns.displayNamePlaceholder')} />
          </Form.Item>

          <Form.Item
            name="patternRegex"
            label={t('management.timePatterns.regex')}
            tooltip={t('management.timePatterns.regexHelp')}
            rules={[{ validator: regexValidator }]}
          >
            <TextArea
              rows={2}
              placeholder={t('management.timePatterns.regexPlaceholder')}
              style={{ fontFamily: 'monospace' }}
              onChange={() => setRegexTestResult(null)}
            />
          </Form.Item>
          <div style={{ marginTop: -8, marginBottom: 12 }}>
            <Text type="secondary" style={{ fontSize: 12, marginRight: 8 }}>
              {t('management.timePatterns.commonExamples')}:
            </Text>
            <Space wrap size={[6, 6]}>
              {REGEX_EXAMPLES.map((example) => (
                <Tag
                  key={example}
                  style={{ cursor: 'pointer', userSelect: 'none', marginInlineEnd: 0 }}
                  onClick={() => insertRegexExample(example)}
                >
                  {example}
                </Tag>
              ))}
            </Space>
          </div>

          <Form.Item
            name="testText"
            label={t('management.timePatterns.testText')}
            tooltip={t('management.timePatterns.testTextHelp')}
          >
            <Input
              placeholder={t('management.timePatterns.testTextPlaceholder')}
              onChange={() => setRegexTestResult(null)}
              addonAfter={(
                <Button type="text" size="small" onClick={evaluateRegex}>
                  {t('management.timePatterns.testAction')}
                </Button>
              )}
            />
          </Form.Item>
          {regexTestResult && (
            <Alert
              type={regexTestResult.matched ? 'success' : 'error'}
              showIcon
              style={{ marginTop: -8, marginBottom: 12 }}
              message={regexTestResult.matched
                ? t('management.timePatterns.testMatched', { match: regexTestResult.fullMatch })
                : t('management.timePatterns.testNoMatch')}
              description={regexTestResult.matched && regexTestResult.groups.length > 0
                ? t('management.timePatterns.testGroups', { groups: regexTestResult.groups.join(', ') })
                : undefined}
            />
          )}

          <Form.Item
            name="resolvedType"
            label={t('management.timePatterns.type')}
            tooltip={t('management.timePatterns.typeHelp')}
            rules={[{ required: true }]}
          >
            <Select
              options={resolvedTypeOptions}
              showSearch
              optionFilterProp="label"
            />
          </Form.Item>

          {resolvedType === 'custom' && (
            <Form.Item
              name="customResolvedType"
              label={t('management.timePatterns.customTypeLabel')}
              tooltip={t('management.timePatterns.customTypeHelp')}
              rules={[{ required: true, message: t('management.timePatterns.customTypeRequired') }]}
            >
              <Input placeholder={t('management.timePatterns.customTypePlaceholder')} />
            </Form.Item>
          )}

          <Space size={16}>
            <Form.Item
              name="priority"
              label={t('management.timePatterns.priority')}
              tooltip={t('management.timePatterns.priorityHelp')}
              style={{ minWidth: 100 }}
            >
              <InputNumber min={0} max={100} style={{ width: '100%' }} />
            </Form.Item>
          </Space>
        </Form>
      </Modal>
    </div>
  );
}
