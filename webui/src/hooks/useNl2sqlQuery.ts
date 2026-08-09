// ── useNl2sqlQuery — manages the NL2SQL query execution lifecycle ────────────────

import { useState, useCallback, useRef, useMemo } from 'react';
import { message } from 'antd';
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query';
import { nl2sqlApi, dataSourcesApi } from '@/api';
import { queryKeys } from '@/api/queryKeys';
import type {
  Nl2sqlQueryResponse,
  Nl2sqlExecuteResponse,
  DataSourceInfo,
} from '@/types';
import { ApiError } from '@/api/errors';

export interface NlTurn {
  id: string;
  question: string;
  queryId: string | null;
  sql: string | null;
  result: Nl2sqlExecuteResponse | null;
  error: string | null;
  loading: boolean;
  executing: boolean;
  clarification: string | null;
  queryUnderstanding: unknown | null;
  conversationId: string | null;
  selectedDsId: string | null;
}

export function useNl2sqlQuery(selectedDataSourceId: string | null) {
  const qc = useQueryClient();
  const [turns, setTurns] = useState<NlTurn[]>([]);
  const [conversationId, setConversationId] = useState<string | null>(null);
  const [sqlEditingId, setSqlEditingId] = useState<string | null>(null);
  const mountedRef = useRef(true);

  // ── Query mutation ──────────────────────────────────────────────────────────
  const submitMutation = useMutation({
    mutationFn: async (params: { question: string; dataSourceId: string }) => {
      const { question, dataSourceId } = params;
      // Step 1: Generate SQL
      const queryResp = await nl2sqlApi.query({
        data_source_id: dataSourceId,
        question,
        conversation_id: conversationId ?? undefined,
      });

      const turnId = `turn-${Date.now()}`;
      // NOTE: The API may return query_understanding on certain backends.
      // Cast to extended response type to access it safely.
      const extendedResp = queryResp as Nl2sqlQueryResponse & {
        clarification?: string | null;
        query_understanding?: unknown | null;
      };
      const turn: NlTurn = {
        id: turnId,
        question,
        queryId: queryResp.queryId,
        sql: queryResp.sql,
        result: null,
        error: queryResp.error,
        loading: false,
        executing: false,
        clarification: extendedResp.clarification ?? null,
        queryUnderstanding: extendedResp.query_understanding ?? null,
        conversationId: queryResp.conversationId ?? conversationId,
        selectedDsId: dataSourceId,
      };

      if (!mountedRef.current) return { turnId, queryResp, executeResp: null, turn };

      // Add turn immediately
      setTurns(prev => [...prev, turn]);
      if (queryResp.conversationId) setConversationId(queryResp.conversationId);

      // Step 2: Execute if SQL was generated
      if (queryResp.sql && !queryResp.error) {
        setTurns(prev =>
          prev.map(t => (t.id === turnId ? { ...t, executing: true } : t))
        );
        try {
          const executeResp = await nl2sqlApi.execute({
            query_id: queryResp.queryId,
            sql: queryResp.sql,
            data_source_id: dataSourceId,
          });
          if (mountedRef.current) {
            setTurns(prev =>
              prev.map(t =>
                t.id === turnId
                  ? { ...t, result: executeResp, error: executeResp.error ?? null, executing: false }
                  : t
              )
            );
          }
          return { turnId, queryResp, executeResp, turn };
        } catch (execErr) {
          if (mountedRef.current) {
            setTurns(prev =>
              prev.map(t =>
                t.id === turnId
                  ? {
                      ...t,
                      error: execErr instanceof ApiError ? execErr.message : String(execErr),
                      executing: false,
                    }
                  : t
              )
            );
          }
          return { turnId, queryResp, executeResp: null, turn };
        }
      }

      return { turnId, queryResp, executeResp: null, turn };
    },
    onError: (err: unknown) => {
      if (err instanceof ApiError) message.error(err.message);
      else if (err instanceof Error) message.error(err.message);
    },
  });

  const reExecuteMutation = useMutation({
    mutationFn: async (params: {
      turnId: string;
      queryId: string;
      sql: string;
      dataSourceId: string;
    }) => {
      const { turnId, queryId, sql, dataSourceId } = params;
      setTurns(prev =>
        prev.map(t => (t.id === turnId ? { ...t, executing: true, error: null } : t))
      );
      try {
        const executeResp = await nl2sqlApi.execute({ query_id: queryId, sql, data_source_id: dataSourceId });
        setTurns(prev =>
          prev.map(t =>
            t.id === turnId
              ? { ...t, result: executeResp, error: executeResp.error ?? null, executing: false }
              : t
          )
        );
        return executeResp;
      } catch (execErr) {
        setTurns(prev =>
          prev.map(t =>
            t.id === turnId
              ? {
                  ...t,
                  error: execErr instanceof ApiError ? execErr.message : String(execErr),
                  executing: false,
                }
              : t
          )
        );
        throw execErr;
      }
    },
  });

  const updateTurnSql = useCallback((turnId: string, newSql: string) => {
    setTurns(prev => prev.map(t => (t.id === turnId ? { ...t, sql: newSql } : t)));
  }, []);

  const updateTurnResult = useCallback(
    (turnId: string, result: Nl2sqlExecuteResponse) => {
      setTurns(prev => prev.map(t => (t.id === turnId ? { ...t, result } : t)));
    },
    []
  );

  const clearTurns = useCallback(() => {
    setTurns([]);
    setConversationId(null);
  }, []);

  return {
    turns,
    setTurns,
    conversationId,
    setConversationId,
    sqlEditingId,
    setSqlEditingId,
    submitMutation,
    reExecuteMutation,
    updateTurnSql,
    updateTurnResult,
    clearTurns,
    mountedRef,
  };
}

// ── useNl2sqlViewState — table/chart/explain tab and chart type state ────────────

export type ViewTab = 'table' | 'chart' | 'explain';

export function useNl2sqlViewState() {
  const [viewTab, setViewTab] = useState<ViewTab>('table');
  const [chartType, setChartType] = useState<'line' | 'bar' | 'pie' | 'scatter' | 'heatmap'>('line');
  const [activeExplainQueryId, setActiveExplainQueryId] = useState<string | null>(null);

  return {
    viewTab,
    setViewTab,
    chartType,
    setChartType,
    activeExplainQueryId,
    setActiveExplainQueryId,
  };
}

// ── useDataSourceSelector — datasource selection with routing ────────────────────

export function useDataSourceSelector() {
  const [selectedDataSourceId, setSelectedDataSourceId] = useState<string | null>(null);
  const [showSourceSelector, setShowSourceSelector] = useState(false);

  const { data: dataSourcesData, isLoading: dsLoading } = useQuery({
    queryKey: queryKeys.dataSources.list(),
    queryFn: () => dataSourcesApi.list().then((r) => r as any),
    staleTime: 60_000,
  });

  const { dataSourceName, dataSourceOptions } = useMemo(() => {
    const opts = (dataSourcesData?.data_sources ?? []).map((ds: DataSourceInfo) => ({
      label: ds.name,
      value: ds.id,
    }));
    const name = dataSourcesData?.data_sources.find((ds: DataSourceInfo) => ds.id === selectedDataSourceId)?.name ?? null;
    return { dataSourceName: name, dataSourceOptions: opts };
  }, [dataSourcesData, selectedDataSourceId]);

  return {
    selectedDataSourceId,
    setSelectedDataSourceId,
    showSourceSelector,
    setShowSourceSelector,
    dataSourcesData,
    dsLoading,
    dataSourceName,
    dataSourceOptions,
  };
}

// Re-export useQuery and useMemo for convenience
export { useQuery, useMutation, useQueryClient } from '@tanstack/react-query';
