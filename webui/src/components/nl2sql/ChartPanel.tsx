// ── NL2SQL Chart Panel — auto-selects chart type based on column data ──────────────

import { useMemo } from 'react';
import { Segmented, Typography, Tag } from 'antd';
import ReactECharts from 'echarts-for-react';
import {
  LineChartOutlined, BarChartOutlined, PieChartOutlined,
  DotChartOutlined, HeatMapOutlined,
} from '@ant-design/icons';
import { useTranslation } from 'react-i18next';
import type { ChartType } from '@/pages/nl2sql/types';

const { Text } = Typography;

export interface ChartPanelProps {
  columns: string[];
  rows: Record<string, unknown>[];
  chartType: ChartType;
  onChartTypeChange: (type: ChartType) => void;
}

function detectChartType(columns: string[], rows: Record<string, unknown>[]): ChartType {
  if (rows.length === 0) return 'line';

  const colTypes: Record<string, 'numeric' | 'date' | 'category'> = {};
  const sampleRows = rows.slice(0, 10);

  for (const col of columns) {
    const vals = sampleRows.map(r => r[col]).filter(v => v !== null && v !== undefined);
    const numericCount = vals.filter(v => typeof v === 'number' || !isNaN(Number(v))).length;

    if (numericCount >= vals.length * 0.7) {
      colTypes[col] = 'numeric';
    } else if (vals.length > 0) {
      const first = String(vals[0]);
      if (/^\d{4}-\d{2}(-\d{2})?/.test(first) || /^\d{1,2}\/\d{1,2}\/\d{2,4}$/.test(first)) {
        colTypes[col] = 'date';
      } else {
        colTypes[col] = 'category';
      }
    }
  }

  const numericCols = columns.filter(c => colTypes[c] === 'numeric');
  const categoryCols = columns.filter(c => colTypes[c] === 'category' || colTypes[c] === 'date');

  if (numericCols.length >= 2) {
    return 'scatter';
  }

  if (numericCols.length >= 1 && categoryCols.length >= 2) {
    return 'heatmap';
  }

  if (numericCols.length >= 1 && categoryCols.length >= 1) {
    if (numericCols.length === 1) {
      const uniqueCats = new Set(rows.map(r => String(r[categoryCols[0]]))).size;
      if (uniqueCats <= 8) return 'pie';
      return 'line';
    }
    return 'bar';
  }

  return 'line';
}

function buildLineOption(columns: string[], rows: Record<string, unknown>[], chartType: ChartType) {
  const numericCols = columns.filter(c =>
    rows.slice(0, 5).every(r => typeof r[c] === 'number' || r[c] === null)
  );
  const categoryCol = columns.find(c => c !== numericCols[0]) ?? columns[0];

  const categories = rows.map(r => String(r[categoryCol] ?? ''));
  const series = numericCols.map(col => ({
    name: col,
    type: chartType as 'line' | 'bar',
    data: rows.map(r => r[col] ?? 0),
    smooth: true,
    itemStyle: { borderRadius: chartType === 'bar' ? 4 : 0 },
  }));

  return {
    backgroundColor: 'transparent',
    grid: { left: 50, right: 20, top: 40, bottom: 50 },
    tooltip: { trigger: 'axis' as const, confine: true },
    legend: {
      data: numericCols,
      bottom: 0,
      textStyle: { color: '#8c8c8c', fontSize: 11 },
    },
    xAxis: {
      type: 'category' as const,
      data: categories,
      axisLabel: { color: '#8c8c8c', fontSize: 11, rotate: categories.length > 8 ? 30 : 0 },
      axisLine: { lineStyle: { color: '#303030' } },
    },
    yAxis: {
      type: 'value' as const,
      name: numericCols.length > 1 ? '' : numericCols[0] ?? '',
      nameTextStyle: { color: '#8c8c8c', fontSize: 11 },
      axisLabel: { color: '#8c8c8c', fontSize: 11 },
      axisLine: { lineStyle: { color: '#303030' } },
      splitLine: { lineStyle: { color: '#303030', opacity: 0.3 } },
    },
    series,
    color: ['#7c3aed', '#1890ff', '#52c41a', '#faad14', '#f5222d', '#13c2c2', '#fa8c16'],
  };
}

function buildPieOption(columns: string[], rows: Record<string, unknown>[]) {
  const numericCol = columns.find(c =>
    rows.slice(0, 5).every(r => typeof r[c] === 'number' || r[c] === null)
  ) ?? columns[0];
  const categoryCol = columns.find(c => c !== numericCol) ?? columns[0];

  const data = rows.map(r => ({
    name: String(r[categoryCol] ?? ''),
    value: Number(r[numericCol]) || 0,
  }));

  return {
    backgroundColor: 'transparent',
    tooltip: { trigger: 'item' as const, confine: true, formatter: '{b}: {c} ({d}%)' },
    legend: {
      orient: 'vertical' as const,
      right: 10,
      top: 'center',
      textStyle: { color: '#8c8c8c', fontSize: 11 },
    },
    series: [{
      name: numericCol,
      type: 'pie' as const,
      radius: ['35%', '65%'],
      center: ['40%', '50%'],
      avoidLabelOverlap: true,
      itemStyle: { borderRadius: 6, borderColor: '#1f1f1f', borderWidth: 2 },
      label: { show: data.length <= 6, color: '#8c8c8c', fontSize: 11 },
      labelLine: { show: data.length <= 6 },
      data,
      emphasis: {
        itemStyle: { shadowBlur: 10, shadowOffsetX: 0, shadowColor: 'rgba(0, 0, 0, 0.5)' },
      },
    }],
    color: ['#7c3aed', '#1890ff', '#52c41a', '#faad14', '#f5222d', '#13c2c2', '#fa8c16', '#2f54ed', '#722ed1', '#eb2f96'],
  };
}

function buildScatterOption(columns: string[], rows: Record<string, unknown>[]) {
  const numericCols = columns.filter(c =>
    rows.slice(0, 5).every(r => typeof r[c] === 'number' || r[c] === null)
  );
  const xCol = numericCols[0] ?? columns[0];
  const yCol = numericCols[1] ?? columns.find(c => c !== xCol) ?? columns[0];
  const sizeCol = numericCols.length >= 3 ? numericCols[2] : null;

  const data = rows.map(r => {
    const point: (number | string)[] = [Number(r[xCol]) || 0, Number(r[yCol]) || 0];
    if (sizeCol) point.push(Math.abs(Number(r[sizeCol]) || 1));
    return point;
  });

  return {
    backgroundColor: 'transparent',
    grid: { left: 50, right: 20, top: 40, bottom: 50 },
    tooltip: {
      trigger: 'item' as const,
      confine: true,
      formatter: (p: { data: (number | string)[] }) =>
        `${xCol}: ${p.data[0]}<br/>${yCol}: ${p.data[1]}${sizeCol ? `<br/>${sizeCol}: ${p.data[2]}` : ''}`,
    },
    xAxis: {
      type: 'value' as const,
      name: xCol,
      nameTextStyle: { color: '#8c8c8c', fontSize: 11 },
      axisLabel: { color: '#8c8c8c', fontSize: 11 },
      axisLine: { lineStyle: { color: '#303030' } },
      splitLine: { lineStyle: { color: '#303030', opacity: 0.3 } },
    },
    yAxis: {
      type: 'value' as const,
      name: yCol,
      nameTextStyle: { color: '#8c8c8c', fontSize: 11 },
      axisLabel: { color: '#8c8c8c', fontSize: 11 },
      axisLine: { lineStyle: { color: '#303030' } },
      splitLine: { lineStyle: { color: '#303030', opacity: 0.3 } },
    },
    series: [{
      name: `${xCol} vs ${yCol}`,
      type: 'scatter' as const,
      symbolSize: sizeCol ? (data: number[]) => Math.sqrt(data[2]) * 4 + 4 : 10,
      data,
      itemStyle: { opacity: 0.7 },
    }],
    color: ['#7c3aed'],
  };
}

function buildHeatmapOption(columns: string[], rows: Record<string, unknown>[]) {
  const numericCols = columns.filter(c =>
    rows.slice(0, 5).every(r => typeof r[c] === 'number' || r[c] === null)
  );
  const valueCol = numericCols[0] ?? columns[0];
  const catCols = columns.filter(c => c !== valueCol).slice(0, 2);
  if (catCols.length < 2) return buildLineOption(columns, rows, 'line');

  const xCol = catCols[0];
  const yCol = catCols[1];

  const xCats = [...new Set(rows.map(r => String(r[xCol] ?? '')))];
  const yCats = [...new Set(rows.map(r => String(r[yCol] ?? '')))];

  const data: number[][] = rows.map(r => [
    xCats.indexOf(String(r[xCol] ?? '')),
    yCats.indexOf(String(r[yCol] ?? '')),
    Number(r[valueCol]) || 0,
  ]);

  const maxVal = Math.max(...data.map(d => d[2]), 1);

  return {
    backgroundColor: 'transparent',
    grid: { left: 60, right: 20, top: 20, bottom: 60 },
    tooltip: {
      trigger: 'item' as const,
      confine: true,
      formatter: (p: { data: number[] }) =>
        `${xCol}: ${xCats[p.data[0]]}<br/>${yCol}: ${yCats[p.data[1]]}<br/>${valueCol}: ${p.data[2]}`,
    },
    xAxis: {
      type: 'category' as const,
      data: xCats,
      name: xCol,
      nameTextStyle: { color: '#8c8c8c', fontSize: 11 },
      axisLabel: { color: '#8c8c8c', fontSize: 10, rotate: xCats.length > 6 ? 30 : 0 },
      axisLine: { lineStyle: { color: '#303030' } },
      splitArea: { show: false },
    },
    yAxis: {
      type: 'category' as const,
      data: yCats,
      name: yCol,
      nameTextStyle: { color: '#8c8c8c', fontSize: 11 },
      axisLabel: { color: '#8c8c8c', fontSize: 10 },
      axisLine: { lineStyle: { color: '#303030' } },
      splitArea: { show: false },
    },
    visualMap: {
      min: 0,
      max: maxVal,
      calculable: true,
      orient: 'horizontal' as const,
      left: 60,
      bottom: 0,
      textStyle: { color: '#8c8c8c', fontSize: 10 },
      inRange: { color: ['#1f1f1f', '#4a148c', '#7c3aed', '#a78bfa', '#c4b5fd'] },
    },
    series: [{
      name: valueCol,
      type: 'heatmap' as const,
      data,
      label: { show: data.length <= 30, color: '#fff', fontSize: 10 },
      itemStyle: { borderRadius: 2, borderColor: '#1f1f1f', borderWidth: 1 },
      emphasis: { itemStyle: { shadowBlur: 10, shadowColor: 'rgba(0, 0, 0, 0.5)' } },
    }],
  };
}

export function ChartPanel({ columns, rows, chartType, onChartTypeChange }: ChartPanelProps) {
  const { t } = useTranslation();
  const autoDetected = useMemo(() => detectChartType(columns, rows), [columns, rows]);

  const option = useMemo(() => {
    if (chartType === 'pie') return buildPieOption(columns, rows);
    if (chartType === 'scatter') return buildScatterOption(columns, rows);
    if (chartType === 'heatmap') return buildHeatmapOption(columns, rows);
    return buildLineOption(columns, rows, chartType);
  }, [columns, rows, chartType]);

  // F-11: Show empty state instead of silent null when no data for chart
  if (rows.length === 0) {
    return (
      <div style={{ padding: 16, textAlign: 'center', color: 'var(--text-muted)' }}>
        <Text style={{ fontSize: 13 }}>{t('nl2sql.noResults')}</Text>
      </div>
    );
  }

  return (
    <div>
      {/* Chart type selector */}
      <div style={{ display: 'flex', alignItems: 'center', gap: 8, marginBottom: 8 }}>
        <Text style={{ fontSize: 11, color: '#8c8c8c' }}>{t('nl2sql.chart')}:</Text>
        <Segmented
          size="small"
          value={chartType}
          onChange={(v) => onChartTypeChange(v as ChartType)}
          options={[
            { value: 'line', label: <span style={{ fontSize: 11, display: 'flex', alignItems: 'center', gap: 4 }}><LineChartOutlined /> {t('nl2sql.line')}</span> },
            { value: 'bar', label: <span style={{ fontSize: 11, display: 'flex', alignItems: 'center', gap: 4 }}><BarChartOutlined /> {t('nl2sql.bar')}</span> },
            { value: 'pie', label: <span style={{ fontSize: 11, display: 'flex', alignItems: 'center', gap: 4 }}><PieChartOutlined /> {t('nl2sql.pie')}</span> },
            { value: 'scatter', label: <span style={{ fontSize: 11, display: 'flex', alignItems: 'center', gap: 4 }}><DotChartOutlined /> {t('nl2sql.scatter')}</span> },
            { value: 'heatmap', label: <span style={{ fontSize: 11, display: 'flex', alignItems: 'center', gap: 4 }}><HeatMapOutlined /> {t('nl2sql.heatmap')}</span> },
          ]}
        />
        <Tag style={{ fontSize: 10 }} color="purple">{t('nl2sql.auto')}: {autoDetected}</Tag>
      </div>

      {/* Chart */}
      <div style={{
        border: '1px solid #303030',
        borderRadius: 8,
        padding: '8px 4px 4px',
        background: '#1f1f1f',
        minHeight: 280,
      }}>
        <ReactECharts
          option={option}
          style={{ height: 280, width: '100%' }}
          opts={{ renderer: 'svg' }}
          notMerge={true}
        />
      </div>
    </div>
  );
}
