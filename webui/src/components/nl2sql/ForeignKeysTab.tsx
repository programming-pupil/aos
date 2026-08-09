// Manual foreign-key CRUD per datasource (F-2).
//
// The backend has full CRUD already exposed at /nl2sql/foreign-keys/{ds}; this
// tab fills the long-standing WebUI gap so admins can drive JOIN suggestions
// without going to mysql directly.

import { useEffect, useMemo, useState } from 'react';
import {
  Alert,
  Button,
  Empty,
  Form,
  Input,
  Modal,
  Popconfirm,
  Select,
  Space,
  Table,
  Tag,
  Typography,
  message,
} from 'antd';
import {
  PlusOutlined,
  EditOutlined,
  DeleteOutlined,
  NodeIndexOutlined,
} from '@ant-design/icons';
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query';
import { useTranslation } from 'react-i18next';
import { nl2sqlApi, dataSourcesApi } from '@/api';
import { queryKeys } from '@/api/queryKeys';
import type { CreateForeignKeyRequest, ForeignKeyResponse } from '@/types';

const { Text } = Typography;

export function ForeignKeysTab() {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const [selectedDs, setSelectedDs] = useState<string | undefined>();
  const [modalOpen, setModalOpen] = useState(false);
  const [editing, setEditing] = useState<ForeignKeyResponse | null>(null);
  const [form] = Form.useForm<CreateForeignKeyRequest>();

  const { data: dsResp } = useQuery({
    queryKey: queryKeys.dataSources.list(),
    queryFn: () => dataSourcesApi.list(),
  });
  const datasources = useMemo(
    () => (Array.isArray(dsResp) ? dsResp : dsResp?.data_sources ?? []),
    [dsResp]
  );

  useEffect(() => {
    const firstDatasourceId = datasources[0]?.id;
    if (!selectedDs && firstDatasourceId) {
      setSelectedDs(firstDatasourceId);
    }
  }, [datasources, selectedDs]);

  const { data: fkResp, isLoading } = useQuery({
    queryKey: queryKeys.nl2sql.foreignKeys(selectedDs ?? ''),
    queryFn: () => nl2sqlApi.listForeignKeys(selectedDs!),
    enabled: !!selectedDs,
  });
  const fks: ForeignKeyResponse[] = fkResp?.foreignKeys ?? [];

  const createMu = useMutation({
    mutationFn: (data: CreateForeignKeyRequest) =>
      nl2sqlApi.createForeignKey(selectedDs!, data),
    onSuccess: () => {
      qc.invalidateQueries({
        queryKey: queryKeys.nl2sql.foreignKeys(selectedDs ?? ''),
      });
      message.success(t('management.foreignKeys.createSuccess'));
      setModalOpen(false);
      setEditing(null);
      form.resetFields();
    },
    onError: (err: unknown) => {
      const msg = (err as { message?: string })?.message;
      message.error(msg || t('common.failed'));
    },
  });

  const updateMu = useMutation({
    mutationFn: ({
      id,
      data,
    }: {
      id: string;
      data: Partial<CreateForeignKeyRequest>;
    }) => nl2sqlApi.updateForeignKey(selectedDs!, id, data),
    onSuccess: () => {
      qc.invalidateQueries({
        queryKey: queryKeys.nl2sql.foreignKeys(selectedDs ?? ''),
      });
      message.success(t('management.foreignKeys.updateSuccess'));
      setModalOpen(false);
      setEditing(null);
      form.resetFields();
    },
    onError: (err: unknown) => {
      const msg = (err as { message?: string })?.message;
      message.error(msg || t('common.failed'));
    },
  });

  const deleteMu = useMutation({
    mutationFn: (id: string) => nl2sqlApi.deleteForeignKey(selectedDs!, id),
    onSuccess: () => {
      qc.invalidateQueries({
        queryKey: queryKeys.nl2sql.foreignKeys(selectedDs ?? ''),
      });
      message.success(t('management.foreignKeys.deleteSuccess'));
    },
    onError: (err: unknown) => {
      const msg = (err as { message?: string })?.message;
      message.error(msg || t('common.failed'));
    },
  });

  const openCreate = () => {
    setEditing(null);
    form.resetFields();
    setModalOpen(true);
  };

  const openEdit = (fk: ForeignKeyResponse) => {
    setEditing(fk);
    form.setFieldsValue({
      sourceTable: fk.sourceTable,
      sourceColumn: fk.sourceColumn,
      sourceType: fk.sourceType,
      targetTable: fk.targetTable,
      targetColumn: fk.targetColumn,
      targetType: fk.targetType,
    });
    setModalOpen(true);
  };

  const onSubmit = async () => {
    const values = await form.validateFields();
    if (editing) {
      updateMu.mutate({ id: editing.id, data: values });
    } else {
      createMu.mutate(values);
    }
  };

  return (
    <div>
      <Alert
        showIcon
        type="info"
        icon={<NodeIndexOutlined />}
        message={t('management.foreignKeys.pageTitle')}
        description={t('management.foreignKeys.pageSubtitle')}
        style={{ marginBottom: 16 }}
      />
      <Space style={{ marginBottom: 12 }}>
        <Select
          placeholder={t('management.foreignKeys.selectDatasource')}
          style={{ minWidth: 240 }}
          value={selectedDs}
          onChange={setSelectedDs}
          options={datasources.map((d) => ({ label: d.name, value: d.id }))}
        />
        <Button
          type="primary"
          icon={<PlusOutlined />}
          disabled={!selectedDs}
          onClick={openCreate}
        >
          {t('management.foreignKeys.newFk')}
        </Button>
      </Space>
      {selectedDs ? (
        <Table<ForeignKeyResponse>
          rowKey="id"
          loading={isLoading}
          dataSource={fks}
          pagination={{ pageSize: 20, showSizeChanger: true }}
          locale={{ emptyText: <Empty description={t('management.foreignKeys.noFks')} /> }}
          columns={[
            {
              title: t('management.foreignKeys.sourceTable'),
              dataIndex: 'sourceTable',
              render: (v) => <Text code>{v}</Text>,
            },
            {
              title: t('management.foreignKeys.sourceColumn'),
              dataIndex: 'sourceColumn',
              render: (v) => <Text code>{v}</Text>,
            },
            {
              title: t('management.foreignKeys.sourceType'),
              dataIndex: 'sourceType',
              render: (v: string) => (v ? <Tag>{v}</Tag> : null),
            },
            {
              title: t('management.foreignKeys.arrow'),
              key: 'arrow',
              width: 72,
              align: 'center' as const,
              onHeaderCell: () => ({ style: { whiteSpace: 'nowrap' as const } }),
              render: () => (
                <span style={{ whiteSpace: 'nowrap' }}>{t('common.arrow')}</span>
              ),
            },
            {
              title: t('management.foreignKeys.targetTable'),
              dataIndex: 'targetTable',
              render: (v) => <Text code>{v}</Text>,
            },
            {
              title: t('management.foreignKeys.targetColumn'),
              dataIndex: 'targetColumn',
              render: (v) => <Text code>{v}</Text>,
            },
            {
              title: t('management.foreignKeys.targetType'),
              dataIndex: 'targetType',
              render: (v: string) => (v ? <Tag>{v}</Tag> : null),
            },
            {
              title: t('common.actions'),
              key: 'actions',
              width: 160,
              render: (_, record) => (
                <Space>
                  <Button
                    size="small"
                    icon={<EditOutlined />}
                    onClick={() => openEdit(record)}
                  />
                  <Popconfirm
                    title={t('management.foreignKeys.deleteConfirm')}
                    onConfirm={() => deleteMu.mutate(record.id)}
                    okButtonProps={{ danger: true }}
                  >
                    <Button size="small" danger icon={<DeleteOutlined />} />
                  </Popconfirm>
                </Space>
              ),
            },
          ]}
        />
      ) : (
        <Empty description={t('management.foreignKeys.selectDatasource')} />
      )}

      <Modal
        open={modalOpen}
        title={
          editing
            ? t('management.foreignKeys.newFk') // edit and create share the title
            : t('management.foreignKeys.newFk')
        }
        onCancel={() => {
          setModalOpen(false);
          setEditing(null);
          form.resetFields();
        }}
        onOk={onSubmit}
        confirmLoading={createMu.isPending || updateMu.isPending}
        destroyOnHidden
      >
        <Form form={form} layout="vertical">
          <Form.Item
            label={t('management.foreignKeys.sourceTable')}
            name="sourceTable"
            rules={[{ required: true }]}
          >
            <Input />
          </Form.Item>
          <Form.Item
            label={t('management.foreignKeys.sourceColumn')}
            name="sourceColumn"
            rules={[{ required: true }]}
          >
            <Input />
          </Form.Item>
          <Form.Item
            label={t('management.foreignKeys.sourceType')}
            name="sourceType"
          >
            <Input placeholder={t('management.foreignKeys.placeholderSourceType')} />
          </Form.Item>
          <Form.Item
            label={t('management.foreignKeys.targetTable')}
            name="targetTable"
            rules={[{ required: true }]}
          >
            <Input />
          </Form.Item>
          <Form.Item
            label={t('management.foreignKeys.targetColumn')}
            name="targetColumn"
            rules={[{ required: true }]}
          >
            <Input />
          </Form.Item>
          <Form.Item
            label={t('management.foreignKeys.targetType')}
            name="targetType"
          >
            <Input placeholder={t('management.foreignKeys.placeholderTargetType')} />
          </Form.Item>
        </Form>
      </Modal>
    </div>
  );
}
