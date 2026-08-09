// ── NL2SQL Management — enterprise features for admins ──────────────────────────────────
// Tabs: Business Domains | Synonyms | Metrics | Time Patterns | Validation Rules
//
// Business Domains: auto-discovered or manually curated table groupings for better routing.
// Synonyms: term -> canonical (table, column) mappings for NL routing recall.
// Metrics: reusable business metrics with SQL expressions and aliases.
// Time Patterns: regex -> resolved time expression mapping for QU time intelligence.
// Validation Rules: per-column sanity checks on SQL result sets.

import { useState, useMemo, useCallback } from 'react';
import {
  Layout, Tabs, Table, Tag, Button, Space, Modal, Form, Input,
  Select, message, Popconfirm, Typography, Card,
  Badge, Empty, Switch, InputNumber, Alert, Tooltip, Progress, Upload,
  type TablePaginationConfig,
} from 'antd';
const { Dragger } = Upload;
const { Search } = Input;
import {
  PlusOutlined, DeleteOutlined, EditOutlined,
  CheckOutlined, CloseOutlined,
  AppstoreOutlined, ClockCircleOutlined, SafetyOutlined,
  SyncOutlined, GlobalOutlined,
  BranchesOutlined, LineChartOutlined,
  // P2-2: Cross-Datasource Relations
  NodeIndexOutlined,
  // P2-3: Cross-Domain Clusters
  ClusterOutlined,
  UploadOutlined,
  // F-10: Query Permission Policies
  LockOutlined,
} from '@ant-design/icons';
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query';
import { nl2sqlApi, dataSourcesApi } from '@/api';
import { queryKeys } from '@/api/queryKeys';
import { exportToCsv, importCsvFile } from '@/utils/csvUtils';
import type {
  BusinessDomain, TimePattern, ValidationRule,
  CreateTimePatternRequest, UpdateTimePatternRequest,
  CreateValidationRuleRequest,
  SynonymItem, CreateSynonymRequest, UpdateSynonymRequest,
  MetricItem, CreateMetricRequest, UpdateMetricRequest,
  // P2-2: Cross-Datasource Relations
  CrossDSRelationItem, CreateCrossDSRelationRequest, UpdateCrossDSRelationRequest,
  // P2-3: Cross-Domain Clusters
  CrossDomainClusterItem, CreateCrossDomainClusterRequest, UpdateCrossDomainClusterRequest,
  // P1-3: Join Paths
  JoinPathItem, CreateJoinPathRequest, UpdateJoinPathRequest,
} from '@/types';
import { useTranslation } from 'react-i18next';
import { ErrorBoundary } from '@/components/ErrorBoundary';
import { QueryPoliciesTab } from '@/components/nl2sql/QueryPoliciesTab';
import { DomainsTab } from '@/components/nl2sql/DomainsTab';
import { TimePatternsTab } from '@/components/nl2sql/TimePatternsTab';
import { ValidationRulesTab } from '@/components/nl2sql/ValidationRulesTab';
import { SynonymsTab } from '@/components/nl2sql/SynonymsTab';
import { MetricsTab } from '@/components/nl2sql/MetricsTab';
import { CrossDSRelationsTab } from '@/components/nl2sql/CrossDSRelationsTab';
import { ClustersTab } from '@/components/nl2sql/ClustersTab';
import { MaskingRulesTab } from '@/components/nl2sql/MaskingRulesTab';
import { RelationshipModelingTab } from '@/components/nl2sql/RelationshipModelingTab';

const { Text, Title } = Typography;

// ─── Main Page ────────────────────────────────────────────────────────────────

export default function Nl2sqlManagement() {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const [activeTab, setActiveTab] = useState('domains');
  const refreshTabData = useCallback(() => {
    qc.invalidateQueries({ queryKey: queryKeys.nl2sql.all });
    qc.invalidateQueries({ queryKey: queryKeys.dataSources.all() });
  }, [qc]);
  const handleTabClick = useCallback((key: string) => {
    if (key === activeTab) {
      refreshTabData();
    }
  }, [activeTab, refreshTabData]);
  const handleTabChange = useCallback((key: string) => {
    setActiveTab(key);
    refreshTabData();
  }, [refreshTabData]);

  const tabItems = useMemo(() => [
    {
      key: 'domains',
      label: <span><AppstoreOutlined /> {t('management.tabs.domains')}</span>,
      children: <div style={{ padding: '16px 24px' }}><DomainsTab /></div>,
    },
    {
      key: 'synonyms',
      label: <span><GlobalOutlined /> {t('management.tabs.synonyms')}</span>,
      children: <div style={{ padding: '16px 24px' }}><SynonymsTab /></div>,
    },
    {
      key: 'metrics',
      label: <span><LineChartOutlined /> {t('management.tabs.metrics')}</span>,
      children: <div style={{ padding: '16px 24px' }}><MetricsTab /></div>,
    },
    {
      key: 'time-patterns',
      label: <span><ClockCircleOutlined /> {t('management.tabs.timePatterns')}</span>,
      children: <div style={{ padding: '16px 24px' }}><TimePatternsTab /></div>,
    },
    {
      key: 'validation-rules',
      label: <span><SafetyOutlined /> {t('management.tabs.validationRules')}</span>,
      children: <div style={{ padding: '16px 24px' }}><ValidationRulesTab /></div>,
    },
    {
      key: 'cross-ds-relations',
      label: <span><NodeIndexOutlined /> {t('management.tabs.crossDsRelations')}</span>,
      children: <div style={{ padding: '16px 24px' }}><CrossDSRelationsTab /></div>,
    },
    {
      key: 'cross-domain-clusters',
      label: <span><ClusterOutlined /> {t('management.tabs.crossDomainClusters')}</span>,
      children: <div style={{ padding: '16px 24px' }}><ClustersTab /></div>,
    },
    {
      key: 'query-policies',
      label: <span><LockOutlined /> {t('management.tabs.queryPolicies')}</span>,
      children: <div style={{ padding: '16px 24px' }}><QueryPoliciesTab /></div>,
    },
    {
      key: 'relationship-modeling',
      label: <span><NodeIndexOutlined /> {t('management.tabs.relationshipModeling')}</span>,
      children: <div style={{ padding: '16px 24px' }}><RelationshipModelingTab /></div>,
    },
    {
      key: 'masking-rules',
      label: <span><SafetyOutlined /> {t('management.tabs.maskingRules')}</span>,
      children: <div style={{ padding: '16px 24px' }}><MaskingRulesTab /></div>,
    },
  ], [t]);

  return (
    <ErrorBoundary>
    <Layout style={{ minHeight: '100vh', background: 'var(--bg-void)' }}>
      <Layout.Content style={{ padding: '24px 24px', maxWidth: 1200, margin: '0 auto', width: '100%' }}>
        <div style={{ marginBottom: 24 }}>
          <Title level={4} style={{ margin: 0, color: 'var(--text-primary)' }}>
            <SafetyOutlined style={{ marginRight: 8 }} />
            {t('management.pageTitle')}
          </Title>
          <Text type="secondary">
            {t('management.pageSubtitle')}
          </Text>
        </div>

        <Card style={{ background: 'var(--bg-surface)' }} bodyStyle={{ padding: 0 }}>
          <Tabs
            activeKey={activeTab}
            onChange={handleTabChange}
            onTabClick={handleTabClick}
            items={tabItems}
            tabBarStyle={{ paddingLeft: 16 }}
          />
        </Card>
      </Layout.Content>
    </Layout>
    </ErrorBoundary>
  );
}
