import { client } from './client';
import type {
  DataSourceInfo,
  DataSourceListResponse,
  DataSourceSchemaInfo,
  TestConnectionResult,
  Nl2sqlQueryResponse,
  Nl2sqlQueryTaskEvent,
  Nl2sqlQueryTaskStartResponse,
  Nl2sqlQueryTaskStatusResponse,
  Nl2sqlExecuteResponse,
  Nl2sqlQueryHistoryResponse,
  SlowQueriesResponse,
  AttributionAnalyzeRequest,
  AttributionConversationDetailResponse,
  AttributionConversationListResponse,
  AttributionTaskEvent,
  AttributionTaskStartResponse,
  AttributionTaskStatusResponse,
} from '@/types';

// ---- Data Sources ----
export const dataSourcesApi = {
  list: (params?: { page?: number; per_page?: number }) =>
    client.get<DataSourceListResponse>('/data-sources', { params }).then((r) => r.data),

  get: (id: string) =>
    client.get<DataSourceInfo>(`/data-sources/${encodeURIComponent(id)}`).then((r) => r.data),

  create: (data: {
    name: string;
    description?: string;
    type: string;
    visibility?: string;
    config: Record<string, unknown>;
    schema_info?: Record<string, unknown>;
  }) =>
    client.post<DataSourceInfo>('/data-sources', data).then((r) => r.data),

  update: (id: string, data: {
    name?: string;
    description?: string;
    visibility?: string;
    config?: Record<string, unknown>;
    schema_info?: Record<string, unknown>;
    enabled?: boolean;
  }) =>
    client.patch<DataSourceInfo>(`/data-sources/${encodeURIComponent(id)}`, data).then((r) => r.data),

  delete: (id: string) =>
    client.delete(`/data-sources/${encodeURIComponent(id)}`).then((r) => r.data),

  testConnection: (id: string) =>
    client.post<TestConnectionResult>(`/data-sources/${encodeURIComponent(id)}/test`, {}).then((r) => r.data),

  discoverTrinoSchemas: (data: {
    host: string;
    port?: number;
    catalog: string;
    username: string;
    password?: string;
    ssl?: boolean;
    basic_auth?: boolean;
  }) =>
    client
      .post<{
        catalog: string;
        schemas: string[];
        method: string;
        warnings: string[];
      }>('/data-sources/trino/schemas', data)
      .then((r) => r.data),

  discoverSchema: (id: string, mode: 'incremental' | 'force' = 'incremental') =>
    client
      .post<{
        schemas: DataSourceSchemaInfo[];
        skipped_tables?: Array<{ table: string; error: string }>;
        cap_hit?: boolean;
        schema_changed?: boolean;
        force_refresh?: boolean;
        needs_embedding_refresh?: boolean;
        refresh_task_id?: string | null;
      }>(`/data-sources/${encodeURIComponent(id)}/discover`, { mode })
      .then((r) => r.data),

  /** Discover and re-index a single table. */
  discoverTableSchema: (id: string, tableName: string) =>
    client
      .post<{
        table_name: string;
        schema_changed: boolean;
        refresh_task_id?: string | null;
      }>(`/data-sources/${encodeURIComponent(id)}/discover/${encodeURIComponent(tableName)}`, {})
      .then((r) => r.data),

  importSqlSchema: (id: string, data: { sql: string; overwriteExisting?: boolean }) =>
    client
      .post<{
        success: boolean;
        imported: number;
        updated: number;
        skipped: number;
        refreshTaskId?: string | null;
        tables: Array<{ tableName: string; columnCount: number; status: string }>;
      }>(`/data-sources/${encodeURIComponent(id)}/import-sql-schema`, data)
      .then((r) => r.data),

  // ── Manual table/column management ───────────────────────────────────────

  addManualTable: (id: string, data: import('@/types').AddManualTableRequest) =>
    client.post<{ success: boolean; table_name: string }>(
      `/data-sources/${encodeURIComponent(id)}/tables`,
      data
    ).then((r) => r.data),

  putManualTable: (id: string, tableName: string, data: import('@/types').PutManualTableRequest) =>
    client.put<{ success: boolean }>(
      `/data-sources/${encodeURIComponent(id)}/tables/${encodeURIComponent(tableName)}`,
      data
    ).then((r) => r.data),

  deleteManualTable: (id: string, tableName: string) =>
    client.delete(`/data-sources/${encodeURIComponent(id)}/tables/${encodeURIComponent(tableName)}`).then((r) => r.data),

  addManualColumn: (id: string, tableName: string, data: import('@/types').AddManualColumnRequest) =>
    client.post<{ success: boolean; column_name: string }>(
      `/data-sources/${encodeURIComponent(id)}/tables/${encodeURIComponent(tableName)}/columns`,
      data
    ).then((r) => r.data),

  putManualColumn: (id: string, tableName: string, columnName: string, data: import('@/types').PutManualColumnRequest) =>
    client.put<{ success: boolean }>(
      `/data-sources/${encodeURIComponent(id)}/tables/${encodeURIComponent(tableName)}/columns/${encodeURIComponent(columnName)}`,
      data
    ).then((r) => r.data),

  deleteManualColumn: (id: string, tableName: string, columnName: string) =>
    client.delete(
      `/data-sources/${encodeURIComponent(id)}/tables/${encodeURIComponent(tableName)}/columns/${encodeURIComponent(columnName)}`
    ).then((r) => r.data),
};

// ---- NL2SQL ----
export const nl2sqlApi = {
  query: (data: import('@/types').Nl2sqlQueryRequest) =>
    client.post<Nl2sqlQueryResponse>('/nl2sql/query', data).then((r) => r.data),

  queryAsync: (data: import('@/types').Nl2sqlQueryRequest) =>
    client.post<Nl2sqlQueryTaskStartResponse>('/nl2sql/query-async', data).then((r) => r.data),

  getQueryTaskStatus: (taskId: string) =>
    client.get<Nl2sqlQueryTaskStatusResponse>(`/nl2sql/query-tasks/${encodeURIComponent(taskId)}`).then((r) => r.data),

  // P3-Enterprise: Parse NL into structured intent (route+generate side-effect).
  queryUnderstanding: (dataSourceId: string, question: string) =>
    client.post<import('@/types').QueryUnderstandingResponse>(
      `/nl2sql/query-understanding/${encodeURIComponent(dataSourceId)}`,
      { question }
    ).then((r) => r.data),

  execute: (data: { query_id: string; sql: string; data_source_id: string; limit?: number; offset?: number }) =>
    client.post<Nl2sqlExecuteResponse>('/nl2sql/execute', data).then((r) => r.data),

  getResultPage: (queryId: string, params?: { page?: number; per_page?: number }) =>
    client.get<{
      queryId: string;
      columns: string[];
      rows: Record<string, unknown>[];
      page: number;
      perPage: number;
      totalRows: number;
      hasMore: boolean;
    }>(`/nl2sql/results/${encodeURIComponent(queryId)}`, { params }).then((r) => r.data),

  history: (params?: { page?: number; per_page?: number; data_source_id?: string; executed?: boolean }) =>
    client.get<Nl2sqlQueryHistoryResponse>('/nl2sql/history', { params }).then((r) => r.data),

  deleteQuery: (queryId: string) =>
    client.delete(`/nl2sql/history/${encodeURIComponent(queryId)}`).then((r) => r.data),

  // P3-2: Conversation summary API
  listConversations: (params?: { page?: number; per_page?: number }) =>
    client.get<import('@/types').ConversationListResponse>('/nl2sql/conversations', { params }).then((r) => r.data),

  getConversation: (conversationId: string, params?: { page?: number; per_page?: number }) =>
    client.get<import('@/types').ConversationDetail>(
      `/nl2sql/conversations/${encodeURIComponent(conversationId)}`,
      { params }
    ).then((r) => r.data),

  /** Update conversation metadata (summary, regenerate). */
  patchConversation: (conversationId: string, data: {
    summary?: string;
    regenerate_summary?: boolean;
  }) =>
    client.patch<import('@/types').ConversationDetail>(
      `/nl2sql/conversations/${encodeURIComponent(conversationId)}`,
      data
    ).then((r) => r.data),

  /** Soft-delete a conversation. */
  deleteConversation: (conversationId: string) =>
    client.delete(`/nl2sql/conversations/${encodeURIComponent(conversationId)}`).then((r) => r.data),

  /** Save the current query result as a named view for reuse. */
  saveView: (data: {
    query_id: string;
    name: string;
    description?: string;
    conversation_id?: string;
  }) =>
    client.post<{ id: string; name: string }>('/nl2sql/views', data).then((r) => r.data),

  /** List saved views. */
  listViews: () =>
    client.get<{ views: Array<{
      query_id: string;
      data_source_id: string | null;
      conversation_id?: string | null;
      sql: string;
      name: string;
      description: string | null;
      created_at: string;
    }> }>('/nl2sql/views').then((r) => r.data),

  /** Rename or update a saved view. */
  updateView: (queryId: string, data: { name?: string; description?: string }) =>
    client.patch<{
      query_id: string; data_source_id: string | null; conversation_id?: string | null; sql: string; name: string; description: string | null; created_at: string;
    }>(`/nl2sql/views/${encodeURIComponent(queryId)}`, data).then((r) => r.data),

  /** Delete a saved view by clearing its saved-view fields. */
  deleteView: (queryId: string) =>
    client.delete<{ deleted: boolean; query_id: string }>(
      `/nl2sql/views/${encodeURIComponent(queryId)}`
    ).then((r) => r.data),

  /** Semantic routing: uses embeddings to auto-select the best data source.
   *  Returns the recommended data source and matched tables with confidence scores. */
  route: (question: string) =>
    client.post<import('@/types').RouteResponse>(
      '/nl2sql/route', { question }
    ).then((r) => r.data),

  routeAsync: (data: { question: string; data_source_id?: string | null }) =>
    client.post<import('@/types').RouteTaskStartResponse>(
      '/nl2sql/route-async', data
    ).then((r) => r.data),

  /** Get column semantics (AI + user descriptions) for a data source. */
  getSemantics: (dataSourceId: string) =>
    client.get<{
      columns: Array<{
        table_name: string;
        column_name: string;
        ai_description: string;
        user_description: string;
        is_indexed: boolean;
      }>;
    }>(`/nl2sql/semantics/${encodeURIComponent(dataSourceId)}`).then((r) => r.data),

  /** Regenerate embeddings and AI descriptions for all tables of a data source. */
  refreshSemantics: (dataSourceId: string) =>
    client.post<{ tables_processed: number; columns_processed: number }>(
      `/nl2sql/semantics/${encodeURIComponent(dataSourceId)}`
    ).then((r) => r.data),

  /** Start an async semantics refresh. Returns immediately with a task_id.
   *  Pass `tables` to refresh only a subset — used by the "retry failed
   *  tables" workflow. */
  refreshSemanticsAsync: (dataSourceId: string, tables?: string[]) =>
    client.post<{ task_id: string; status: string }>(
      `/nl2sql/semantics/${encodeURIComponent(dataSourceId)}/refresh-async`,
      tables && tables.length > 0 ? { tables } : {}
    ).then((r) => r.data),

  /** Poll the status of an async refresh task. */
  getRefreshTaskStatus: (taskId: string) =>
    client.get<import('@/types').RefreshTaskStatus>(
      `/nl2sql/semantics-tasks/${encodeURIComponent(taskId)}`
    ).then((r) => r.data),

  listRefreshTasks: (params?: { active_only?: boolean; limit?: number }) =>
    client.get<import('@/types').RefreshTaskListResponse>(
      '/nl2sql/semantics-tasks',
      { params }
    ).then((r) => r.data),

  /** Check whether the current tenant has an embedding model configured (via api_keys or env vars).
   *  Returns { available, model, base_url, configured_via }. */
  getEmbeddingConfig: () =>
    client.get<import('@/types').EmbeddingConfigResponse>('/nl2sql/embedding-config').then((r) => r.data),

  /** Runtime health of the embedding store / ANN runtime. */
  getEmbeddingHealth: () =>
    client.get<import('@/types').EmbeddingHealthResponse>('/nl2sql/embedding-health').then((r) => r.data),

  // ── Reusable query references ─────────────────────────────────────────────

  listReferencePacks: (dataSourceId: string) =>
    client.get<import('@/types').Nl2sqlReferencePack[]>(
      '/nl2sql/reference/packs',
      { params: { datasource_id: dataSourceId } }
    ).then((r) => r.data),

  createReferencePack: (data: {
    datasourceId: string;
    name: string;
    description?: string;
    tags?: string[];
  }) =>
    client.post<import('@/types').Nl2sqlReferencePack>('/nl2sql/reference/packs', {
      datasourceId: data.datasourceId,
      name: data.name,
      description: data.description,
      tags: data.tags,
    }).then((r) => r.data),

  updateReferencePack: (packId: string, data: {
    name?: string;
    description?: string | null;
    enabled?: boolean;
    tags?: string[];
  }) =>
    client.patch<import('@/types').Nl2sqlReferencePack>(
      `/nl2sql/reference/packs/${encodeURIComponent(packId)}`,
      data
    ).then((r) => r.data),

  deleteReferencePack: (packId: string) =>
    client.delete<{ deleted: boolean }>(
      `/nl2sql/reference/packs/${encodeURIComponent(packId)}`
    ).then((r) => r.data),

  uploadReferenceFile: (packId: string, file: File) => {
    const form = new FormData();
    form.append('file', file);
    return client.post<import('@/types').Nl2sqlReferenceFile>(
      `/nl2sql/reference/packs/${encodeURIComponent(packId)}/files`,
      form,
      { headers: { 'Content-Type': 'multipart/form-data' } }
    ).then((r) => r.data);
  },

  deleteReferenceFile: (fileId: string) =>
    client.delete<{ deleted: boolean }>(
      `/nl2sql/reference/files/${encodeURIComponent(fileId)}`
    ).then((r) => r.data),

  searchReferences: (data: {
    datasourceId: string;
    question: string;
    referenceBindings: import('@/types').Nl2sqlReferenceBindings;
    limit?: number;
  }) =>
    client.post<{ references: import('@/types').Nl2sqlReferenceUsage[] }>(
      '/nl2sql/reference/search',
      {
        datasourceId: data.datasourceId,
        question: data.question,
        referenceBindings: data.referenceBindings,
        limit: data.limit,
      }
    ).then((r) => r.data),

  // ── SQL Knowledge Base ────────────────────────────────────────────────────

  listSqlKnowledgeSpaces: (params?: { datasourceId?: string; includeGlobal?: boolean }) =>
    client.get<import('@/types').Nl2sqlReferencePack[]>(
      '/nl2sql/sql-knowledge/spaces',
      {
        params: {
          datasourceId: params?.datasourceId,
          includeGlobal: params?.includeGlobal,
        },
      }
    ).then((r) => r.data),

  createSqlKnowledgeSpace: (data: {
    name: string;
    description?: string;
    datasourceIds?: string[];
    global?: boolean;
    tags?: string[];
    verified?: boolean;
  }) =>
    client.post<import('@/types').Nl2sqlReferencePack>(
      '/nl2sql/sql-knowledge/spaces',
      data
    ).then((r) => r.data),

  updateSqlKnowledgeSpace: (spaceId: string, data: {
    name?: string;
    description?: string | null;
    enabled?: boolean;
    datasourceIds?: string[];
    tags?: string[];
    verified?: boolean;
    stale?: boolean;
  }) =>
    client.patch<import('@/types').Nl2sqlReferencePack>(
      `/nl2sql/sql-knowledge/spaces/${encodeURIComponent(spaceId)}`,
      data
    ).then((r) => r.data),

  deleteSqlKnowledgeSpace: (spaceId: string) =>
    client.delete<{ deleted: boolean }>(
      `/nl2sql/sql-knowledge/spaces/${encodeURIComponent(spaceId)}`
    ).then((r) => r.data),

  uploadSqlKnowledgeFiles: (spaceId: string, files: File[]) => {
    const form = new FormData();
    files.forEach((file) => {
      const relativePath = (file as File & { webkitRelativePath?: string }).webkitRelativePath;
      form.append('file', file, relativePath || file.name);
    });
    return client.post<{ files: import('@/types').Nl2sqlReferenceFile[] }>(
      `/nl2sql/sql-knowledge/spaces/${encodeURIComponent(spaceId)}/files`,
      form,
      { headers: { 'Content-Type': 'multipart/form-data' } }
    ).then((r) => r.data);
  },

  deleteSqlKnowledgeFile: (fileId: string) =>
    client.delete<{ deleted: boolean }>(
      `/nl2sql/sql-knowledge/files/${encodeURIComponent(fileId)}`
    ).then((r) => r.data),

  updateSqlKnowledgeFile: (fileId: string, data: { content: string }) =>
    client.patch<import('@/types').Nl2sqlReferenceFile>(
      `/nl2sql/sql-knowledge/files/${encodeURIComponent(fileId)}`,
      data
    ).then((r) => r.data),

  readSqlKnowledgeFile: (fileId: string, params?: { startLine?: number; endLine?: number }) =>
    client.get<import('@/types').SqlKnowledgeReadResponse>(
      `/nl2sql/sql-knowledge/files/${encodeURIComponent(fileId)}/read`,
      {
        params: {
          startLine: params?.startLine,
          endLine: params?.endLine,
        },
      }
    ).then((r) => r.data),

  searchSqlKnowledge: (data: {
    question: string;
    datasourceId?: string;
    limit?: number;
  }) =>
    client.post<import('@/types').SqlKnowledgeSearchResponse>(
      '/nl2sql/sql-knowledge/search',
      data
    ).then((r) => r.data),

  /** Update user override description for a single column. */
  updateSemantics: (dataSourceId: string, data: {
    table_name: string;
    column_name: string;
    user_description: string;
  }) =>
    client.patch<{ success: boolean }>(
      `/nl2sql/semantics/${encodeURIComponent(dataSourceId)}`,
      data
    ).then((r) => r.data),

  // ── Table-level semantics ─────────────────────────────────────────────────

  /** Get AI + user descriptions for all tables in a data source. */
  getAllTableSemantics: (dataSourceId: string) =>
    client.get<{ tables: import('@/types').TableSemantics[] }>(
      `/nl2sql/semantics/${encodeURIComponent(dataSourceId)}/tables`
    ).then((r) => r.data.tables),

  /** Get semantics for a specific table. */
  getTableSemantics: (dataSourceId: string, tableName: string) =>
    client.get<import('@/types').TableSemantics>(
      `/nl2sql/semantics/${encodeURIComponent(dataSourceId)}/tables/${encodeURIComponent(tableName)}`
    ).then((r) => r.data),

  /** Update user description for a table. */
  updateTableSemantics: (dataSourceId: string, tableName: string, userDescription: string) =>
    client.patch<{ success: boolean }>(
      `/nl2sql/semantics/${encodeURIComponent(dataSourceId)}/tables/${encodeURIComponent(tableName)}`,
      { user_description: userDescription }
    ).then((r) => r.data),

  // ── Datasource-level semantics ─────────────────────────────────────────────

  /** Get AI + user description for a data source. */
  getDatasourceSemantics: (dataSourceId: string) =>
    client.get<import('@/types').DatasourceSemantics>(
      `/nl2sql/semantics/${encodeURIComponent(dataSourceId)}/datasource`
    ).then((r) => r.data),

  /** Update user description for a data source. */
  updateDatasourceSemantics: (dataSourceId: string, userDescription: string) =>
    client.patch<{ success: boolean }>(
      `/nl2sql/semantics/${encodeURIComponent(dataSourceId)}/datasource`,
      { user_description: userDescription }
    ).then((r) => r.data),

  // ── Manual Foreign Keys CRUD ─────────────────────────────────────────────────

  /** List all manual FK definitions for a datasource. */
  listForeignKeys: (dataSourceId: string) =>
    client.get<import('@/types').ForeignKeyListResponse>(
      `/nl2sql/foreign-keys/${encodeURIComponent(dataSourceId)}`
    ).then((r) => {
      const raw = r.data as unknown as {
        foreign_keys?: Array<{
          id: string;
          datasource_id: string;
          source_table: string;
          source_column: string;
          source_type: string;
          target_table: string;
          target_column: string;
          target_type: string;
          updated_by?: string | null;
          created_at: string;
        }>;
      };
      return {
        foreignKeys: (raw.foreign_keys ?? []).map((fk) => ({
          id: fk.id,
          datasourceId: fk.datasource_id,
          sourceTable: fk.source_table,
          sourceColumn: fk.source_column,
          sourceType: fk.source_type,
          targetTable: fk.target_table,
          targetColumn: fk.target_column,
          targetType: fk.target_type,
          createdBy: null,
          updatedBy: fk.updated_by ?? null,
          createdAt: fk.created_at,
        })),
      } as import('@/types').ForeignKeyListResponse;
    }),

  /** Create a new manual FK definition. */
  createForeignKey: (dataSourceId: string, data: import('@/types').CreateForeignKeyRequest) =>
    client.post<import('@/types').ForeignKeyResponse>(
      `/nl2sql/foreign-keys/${encodeURIComponent(dataSourceId)}`,
      {
        source_table: data.sourceTable,
        source_column: data.sourceColumn,
        source_type: data.sourceType,
        target_table: data.targetTable,
        target_column: data.targetColumn,
        target_type: data.targetType,
      }
    ).then((r) => {
      const fk = r.data as unknown as {
        id: string;
        datasource_id: string;
        source_table: string;
        source_column: string;
        source_type: string;
        target_table: string;
        target_column: string;
        target_type: string;
        updated_by?: string | null;
        created_at: string;
      };
      return {
        id: fk.id,
        datasourceId: fk.datasource_id,
        sourceTable: fk.source_table,
        sourceColumn: fk.source_column,
        sourceType: fk.source_type,
        targetTable: fk.target_table,
        targetColumn: fk.target_column,
        targetType: fk.target_type,
        createdBy: null,
        updatedBy: fk.updated_by ?? null,
        createdAt: fk.created_at,
      } as import('@/types').ForeignKeyResponse;
    }),

  /** Delete a manual FK definition. */
  deleteForeignKey: (dataSourceId: string, fkId: string) =>
    client.delete(
      `/nl2sql/foreign-keys/${encodeURIComponent(dataSourceId)}/${encodeURIComponent(fkId)}`
    ).then((r) => r.data),

  /** Update a manual FK definition (partial update). */
  updateForeignKey: (dataSourceId: string, fkId: string, data: Partial<import('@/types').CreateForeignKeyRequest>) =>
    client.patch<{ id: string; updated_by: string }>(
      `/nl2sql/foreign-keys/${encodeURIComponent(dataSourceId)}/${encodeURIComponent(fkId)}`,
      {
        source_table: data.sourceTable,
        source_column: data.sourceColumn,
        source_type: data.sourceType,
        target_table: data.targetTable,
        target_column: data.targetColumn,
        target_type: data.targetType,
      }
    ).then((r) => r.data),

  // ── Multi-turn Clarification ─────────────────────────────────────────────────

  /** Submit a clarification response (option selection or free text). */
  clarify: (data: import('@/types').ClarifyRequest) =>
    client.post<import('@/types').ClarifyResponse>(
      '/nl2sql/clarify',
      data
    ).then((r) => r.data),
  clarifyAsync: (data: import('@/types').ClarifyRequest) =>
    client.post<import('@/types').ClarifyTaskStartResponse>(
      '/nl2sql/clarify-async',
      data
    ).then((r) => r.data),
  getClarifyTaskStatus: (taskId: string) =>
    client.get<import('@/types').ClarifyTaskStatusResponse>(
      `/nl2sql/clarify-tasks/${encodeURIComponent(taskId)}`
    ).then((r) => r.data),

  /** Get any pending clarification context for a session (for page refresh recovery). */
  getClarify: (sessionId: string) =>
    client.get<import('@/types').ClarifyPendingResponse>(
      `/nl2sql/clarify/${encodeURIComponent(sessionId)}`
    ).then((r) => r.data),
  cancelClarify: (sessionId: string) =>
    client.delete<{ cancelled: boolean; sessionId: string }>(
      `/nl2sql/clarify/${encodeURIComponent(sessionId)}`
    ).then((r) => r.data),

  // ── P0-2: Multi-datasource agent ───────────────────────────────────────────────

  /** Execute a cross-datasource query using the multi-step agent planner. */
  agentExecute: (data: { question: string; conversation_id?: string; max_steps?: number }) =>
    client.post<import('@/types').AgentExecuteResponse>(
      '/nl2sql/agent/execute',
      data
    ).then((r) => r.data),
  agentExecuteAsync: (data: { question: string; conversation_id?: string; max_steps?: number }) =>
    client.post<import('@/types').AgentTaskStartResponse>(
      '/nl2sql/agent/execute-async',
      data
    ).then((r) => r.data),
  getAgentTaskStatus: (taskId: string) =>
    client.get<import('@/types').AgentTaskStatusResponse>(
      `/nl2sql/agent-tasks/${encodeURIComponent(taskId)}`
    ).then((r) => r.data),
  getAgentResultPage: (queryId: string, params?: { page?: number; per_page?: number }) =>
    client.get<{
      queryId: string;
      columns: string[];
      rows: Record<string, unknown>[];
      page: number;
      perPage: number;
      totalRows: number;
      hasMore: boolean;
    }>(`/nl2sql/agent-results/${encodeURIComponent(queryId)}`, { params }).then((r) => r.data),

  // ── Data Attribution ────────────────────────────────────────────────────────

  attributionAnalyzeAsync: (data: AttributionAnalyzeRequest) =>
    client.post<AttributionTaskStartResponse>(
      '/nl2sql/attribution/analyze-async',
      data
    ).then((r) => r.data),
  getAttributionTaskStatus: (taskId: string) =>
    client.get<AttributionTaskStatusResponse>(
      `/nl2sql/attribution/tasks/${encodeURIComponent(taskId)}`
    ).then((r) => r.data),
  cancelAttributionTask: (taskId: string) =>
    client.post<AttributionTaskStatusResponse>(
      `/nl2sql/attribution/tasks/${encodeURIComponent(taskId)}/cancel`,
      {},
    ).then((r) => r.data),
  listAttributionConversations: (params?: { page?: number; per_page?: number }) =>
    client.get<AttributionConversationListResponse>(
      '/nl2sql/attribution/conversations',
      { params },
    ).then((r) => r.data),
  getAttributionConversation: (conversationId: string) =>
    client.get<AttributionConversationDetailResponse>(
      `/nl2sql/attribution/conversations/${encodeURIComponent(conversationId)}`
    ).then((r) => r.data),
  deleteAttributionConversation: (conversationId: string) =>
    client.delete<{ deleted: boolean; id: string }>(
      `/nl2sql/attribution/conversations/${encodeURIComponent(conversationId)}`
    ).then((r) => r.data),

  // ── P3-Enterprise: Business Domains ───────────────────────────────────────────

  /** List all business domains across all datasources for the tenant. */
  listBusinessDomains: () =>
    client.get<import('@/types').ListBusinessDomainsResponse>(
      '/nl2sql/domains'
    ).then((r) => r.data),

  /** List domains + tables for a specific datasource. */
  listDomainsForDatasource: (datasourceId: string) =>
    client.get<import('@/types').ListDomainsForDatasourceResponse>(
      `/nl2sql/domains/${encodeURIComponent(datasourceId)}`
    ).then((r) => r.data),

  /** Re-run domain discovery via LLM for a datasource. */
  rediscoverDomains: (datasourceId: string) =>
    client.post<import('@/types').RediscoverDomainsResponse>(
      `/nl2sql/domains/${encodeURIComponent(datasourceId)}/rediscover`
    ).then((r) => r.data),

  /** Create a business domain manually. */
  createBusinessDomain: (
    datasourceId: string,
    data: import('@/types').CreateDomainRequest,
  ) =>
    client.post<import('@/types').CreateDomainResponse>(
      `/nl2sql/domains/${encodeURIComponent(datasourceId)}`,
      data,
    ).then((r) => r.data),

  /** Update domain name/description (marks as manual). */
  updateDomain: (datasourceId: string, domainId: string, data: import('@/types').UpdateDomainRequest) =>
    client.patch<{ success: boolean }>(
      `/nl2sql/domains/${encodeURIComponent(datasourceId)}/tables/${encodeURIComponent(domainId)}`,
      data
    ).then((r) => r.data),

  /** Delete a business domain. */
  deleteDomain: (datasourceId: string, domainId: string) =>
    client.delete(
      `/nl2sql/domains/${encodeURIComponent(datasourceId)}/tables/${encodeURIComponent(domainId)}`
    ).then((r) => r.data),

  /** List table-to-domain mappings for a specific domain. */
  listDomainTableMappings: (datasourceId: string, domainId: string) =>
    client.get<{ mappings: Array<{ id: number; tableName: string; datasourceId: string; domainId: number; confidenceScore: number }> }>(
      `/nl2sql/domains/${encodeURIComponent(datasourceId)}/tables/${encodeURIComponent(domainId)}/mappings`
    ).then((r) => r.data),

  /** Assign tables to a domain. */
  assignTablesToDomain: (datasourceId: string, domainId: string, tableNames: string[]) =>
    client.post<{ assignedCount: number }>(
      `/nl2sql/domains/${encodeURIComponent(datasourceId)}/tables/${encodeURIComponent(domainId)}/mappings`,
      { tableNames }
    ).then((r) => r.data),

  /** Remove tables from a domain. */
  unassignTablesFromDomain: (datasourceId: string, domainId: string, tableNames: string[]) =>
    client.delete<{ removedCount: number }>(
      `/nl2sql/domains/${encodeURIComponent(datasourceId)}/tables/${encodeURIComponent(domainId)}/mappings`,
      { data: { tableNames } }
    ).then((r) => r.data),

  // ── P3-Enterprise: Schema Change Notifications ────────────────────────────────

  /** List schema change notifications with optional status filter. */
  listSchemaChanges: (params?: { status?: string; page?: number; per_page?: number }) =>
    client.get<import('@/types').ListSchemaChangesResponse>(
      '/nl2sql/schema-changes', { params }
    ).then((r) => r.data),

  /** Get detail of a specific schema change, including affected queries. */
  getSchemaChangeDetail: (notificationId: number) =>
    client.get<import('@/types').SchemaChangeDetailResponse>(
      `/nl2sql/schema-changes/${notificationId}`
    ).then((r) => r.data),

  /** Approve a schema change (triggers reindex). */
  approveSchemaChange: (notificationId: number) =>
    client.post<{ success: boolean }>(
      `/nl2sql/schema-changes/${notificationId}/approve`
    ).then((r) => r.data),

  /** Reject a schema change. */
  rejectSchemaChange: (notificationId: number) =>
    client.post<{ success: boolean }>(
      `/nl2sql/schema-changes/${notificationId}/reject`
    ).then((r) => r.data),

  // ── P3-Enterprise: Time Patterns ───────────────────────────────────────────

  /** List all time expression patterns (default + tenant-specific). */
  listTimePatterns: () =>
    client.get<import('@/types').ListTimePatternsResponse>(
      '/nl2sql/time-patterns'
    ).then((r) => r.data),

  /** Create a new time pattern. */
  createTimePattern: (data: import('@/types').CreateTimePatternRequest) =>
    client.post<{ id: number }>(
      '/nl2sql/time-patterns', data
    ).then((r) => r.data),

  /** Update a time pattern (partial update). */
  updateTimePattern: (patternId: number, data: import('@/types').UpdateTimePatternRequest) =>
    client.patch<{ success: boolean }>(
      `/nl2sql/time-patterns/${patternId}`, data
    ).then((r) => r.data),

  /** Delete a time pattern. */
  deleteTimePattern: (patternId: number) =>
    client.delete(`/nl2sql/time-patterns/${patternId}`).then((r) => r.data),

  // ── P3-Enterprise: Validation Rules ───────────────────────────────────────────

  /** List validation rules for a datasource. */
  listValidationRules: (datasourceId: string) =>
    client.get<import('@/types').ListValidationRulesResponse>(
      `/nl2sql/validation-rules/${encodeURIComponent(datasourceId)}`
    ).then((r) => r.data),

  /** Create a new validation rule. */
  createValidationRule: (datasourceId: string, data: import('@/types').CreateValidationRuleRequest) =>
    client.post<{ id: number }>(
      `/nl2sql/validation-rules/${encodeURIComponent(datasourceId)}`, data
    ).then((r) => r.data),

  /** Update a validation rule (partial update). */
  updateValidationRule: (datasourceId: string, ruleId: number, data: import('@/types').UpdateValidationRuleRequest) =>
    client.patch<{ success: boolean }>(
      `/nl2sql/validation-rules/${encodeURIComponent(datasourceId)}/${ruleId}`, data
    ).then((r) => r.data),

  /** Delete a validation rule. */
  deleteValidationRule: (datasourceId: string, ruleId: number) =>
    client.delete(
      `/nl2sql/validation-rules/${encodeURIComponent(datasourceId)}/${ruleId}`
    ).then((r) => r.data),

  // ── R-7: Column Masking Rules (tenant-wide) ──────────────────────────────────

  /** List tenant column-masking rules. */
  listMaskingRules: () =>
    client.get<import('@/types').ListMaskingRulesResponse>(
      `/nl2sql/masking-rules`
    ).then((r) => r.data),

  /** Create a column-masking rule. */
  createMaskingRule: (data: import('@/types').CreateMaskingRuleRequest) =>
    client.post<{ id: number }>(`/nl2sql/masking-rules`, data).then((r) => r.data),

  /** Update a column-masking rule (partial). */
  updateMaskingRule: (id: number, data: import('@/types').UpdateMaskingRuleRequest) =>
    client.patch<{ success: boolean }>(`/nl2sql/masking-rules/${id}`, data).then((r) => r.data),

  /** Soft-delete a column-masking rule. */
  deleteMaskingRule: (id: number) =>
    client.delete<{ success: boolean }>(`/nl2sql/masking-rules/${id}`).then((r) => r.data),

  // ── P2-1: Synonyms ───────────────────────────────────────────────────────────

  /** List synonyms for a datasource (paginated). */
  listSynonyms: (datasourceId: string, page = 1, perPage = 20) =>
    client.get<import('@/types').PaginatedSynonymsResponse>(
      `/nl2sql/synonyms/${encodeURIComponent(datasourceId)}`,
      { params: { page, per_page: perPage } }
    ).then((r) => r.data),

  /** Create a new synonym mapping. */
  createSynonym: (datasourceId: string, data: import('@/types').CreateSynonymRequest) =>
    client.post<{ id: number }>(
      `/nl2sql/synonyms/${encodeURIComponent(datasourceId)}`, data
    ).then((r) => r.data),

  /** Bulk-create synonyms from CSV import. */
  bulkCreateSynonyms: (datasourceId: string, data: { synonyms: import('@/types').CreateSynonymRequest[] }) =>
    client.post<{ created: number; skipped: number }>(
      `/nl2sql/synonyms/${encodeURIComponent(datasourceId)}/bulk`, data
    ).then((r) => r.data),

  /** Update an existing synonym. */
  updateSynonym: (datasourceId: string, synonymId: number, data: import('@/types').UpdateSynonymRequest) =>
    client.patch<{ success: boolean }>(
      `/nl2sql/synonyms/${encodeURIComponent(datasourceId)}/${synonymId}`, data
    ).then((r) => r.data),

  /** Delete a synonym. */
  deleteSynonym: (datasourceId: string, synonymId: number) =>
    client.delete(
      `/nl2sql/synonyms/${encodeURIComponent(datasourceId)}/${synonymId}`
    ).then((r) => r.data),

  // ── P1-2: Metrics ───────────────────────────────────────────────────────────

  /** List metrics for a datasource. */
  listMetrics: (datasourceId: string) =>
    client.get<import('@/types').ListMetricsResponse>(
      `/nl2sql/metrics/${encodeURIComponent(datasourceId)}`
    ).then((r) => r.data),

  /** Create a new metric definition. */
  createMetric: (datasourceId: string, data: import('@/types').CreateMetricRequest) =>
    client.post<{ id: number }>(
      `/nl2sql/metrics/${encodeURIComponent(datasourceId)}`, data
    ).then((r) => r.data),

  /** Update an existing metric. */
  updateMetric: (datasourceId: string, metricId: number, data: import('@/types').UpdateMetricRequest) =>
    client.patch<{ success: boolean }>(
      `/nl2sql/metrics/${encodeURIComponent(datasourceId)}/${metricId}`, data
    ).then((r) => r.data),

  /** Delete a metric. */
  deleteMetric: (datasourceId: string, metricId: number) =>
    client.delete(
      `/nl2sql/metrics/${encodeURIComponent(datasourceId)}/${metricId}`
    ).then((r) => r.data),

  /** Transition metric status: submit_review | approve | reject | deprecate | restore. */
  updateMetricStatus: (datasourceId: string, metricId: number, action: string, comment?: string) =>
    client.post<{ status: string }>(
      `/nl2sql/metrics/${encodeURIComponent(datasourceId)}/${metricId}/status`,
      { action, comment }
    ).then((r) => r.data),

  /** Fuzzy-lookup a metric by natural language question. */
  metricLookup: (datasourceId: string, question: string) =>
    client.get<import('@/types').ListMetricsResponse>(
      `/nl2sql/metrics/${encodeURIComponent(datasourceId)}/lookup`,
      { params: { question } }
    ).then((r) => r.data),

  // ── P1-3: Join Paths ───────────────────────────────────────────────────────

  /** List join paths for a datasource. */
  listJoinPaths: (datasourceId: string) =>
    client.get<import('@/types').ListJoinPathsResponse>(
      `/nl2sql/join-paths/${encodeURIComponent(datasourceId)}`
    ).then((r) => r.data),

  /** Re-discover join paths for a datasource. */
  rediscoverJoinPaths: (datasourceId: string) =>
    client.post<import('@/types').RediscoverJoinPathsResponse>(
      `/nl2sql/join-paths/${encodeURIComponent(datasourceId)}/rediscover`, {}
    ).then((r) => r.data),

  /** Verify or un-verify a join path. */
  verifyJoinPath: (datasourceId: string, pathId: number, verified: boolean) =>
    client.patch<{ verified: boolean }>(
      `/nl2sql/join-paths/${encodeURIComponent(datasourceId)}/${pathId}/verify`,
      { verified }
    ).then((r) => r.data),

  /** Create a new join path for a datasource. */
  createJoinPath: (datasourceId: string, data: import('@/types').CreateJoinPathRequest) =>
    client.post<import('@/types').JoinPathItem>(
      `/nl2sql/join-paths/${encodeURIComponent(datasourceId)}`, data
    ).then((r) => r.data),

  /** Update an existing join path. */
  updateJoinPath: (datasourceId: string, pathId: number, data: import('@/types').UpdateJoinPathRequest) =>
    client.put<{ success: boolean }>(
      `/nl2sql/join-paths/${encodeURIComponent(datasourceId)}/${pathId}`, data
    ).then((r) => r.data),

  /** Delete a join path. */
  deleteJoinPath: (datasourceId: string, pathId: number) =>
    client.delete(
      `/nl2sql/join-paths/${encodeURIComponent(datasourceId)}/${pathId}`
    ).then((r) => r.data),

  // ── P2-2: Cross-Datasource Relations ──────────────────────────────────────

  /** List all cross-datasource relations for the tenant. */
  listCrossDSRelations: () =>
    client.get<import('@/types').ListCrossDSRelationsResponse>(
      '/nl2sql/cross-ds-relations'
    ).then((r) => r.data),

  /** Create a new cross-datasource relation. */
  createCrossDSRelation: (data: import('@/types').CreateCrossDSRelationRequest) =>
    client.post<import('@/types').CrossDSRelationItem>(
      '/nl2sql/cross-ds-relations', data
    ).then((r) => r.data),

  /** Update a cross-datasource relation. */
  updateCrossDSRelation: (relationId: number, data: import('@/types').UpdateCrossDSRelationRequest) =>
    client.patch<{ success: boolean }>(
      `/nl2sql/cross-ds-relations/${relationId}`, data
    ).then((r) => r.data),

  /** Delete a cross-datasource relation. */
  deleteCrossDSRelation: (relationId: number) =>
    client.delete(
      `/nl2sql/cross-ds-relations/${relationId}`
    ).then((r) => r.data),

  // ── P2-3: Cross-Domain Clusters ────────────────────────────────────────────

  /** List all cross-domain clusters for the tenant. */
  listCrossDomainClusters: () =>
    client.get<import('@/types').ListCrossDomainClustersResponse>(
      '/nl2sql/cross-domain-clusters'
    ).then((r) => r.data),

  /** Create a new cross-domain cluster. */
  createCrossDomainCluster: (data: import('@/types').CreateCrossDomainClusterRequest) =>
    client.post<import('@/types').CrossDomainClusterItem>(
      '/nl2sql/cross-domain-clusters', data
    ).then((r) => r.data),

  /** Update a cross-domain cluster. */
  updateCrossDomainCluster: (clusterId: number, data: import('@/types').UpdateCrossDomainClusterRequest) =>
    client.patch<{ success: boolean }>(
      `/nl2sql/cross-domain-clusters/${clusterId}`, data
    ).then((r) => r.data),

  /** Delete a cross-domain cluster. */
  deleteCrossDomainCluster: (clusterId: number) =>
    client.delete(
      `/nl2sql/cross-domain-clusters/${clusterId}`
    ).then((r) => r.data),

  /** Auto-discover cross-domain clusters. */
  autoDiscoverClusters: () =>
    client.post<{ clustersDiscovered: number }>(
      '/nl2sql/cross-domain-clusters/auto-discover', {}
    ).then((r) => r.data),

  // ── P3-1: Analytics ───────────────────────────────────────────────────────

  /** Get analytics overview. */
  analyticsOverview: (params?: { start_date?: string; end_date?: string }) =>
    client.get<import('@/types').AnalyticsOverview>(
      '/nl2sql/analytics/overview', { params }
    ).then((r) => r.data),

  /** Get routing analytics by method. */
  analyticsRouting: (params?: { start_date?: string; end_date?: string }) =>
    client.get<import('@/types').AnalyticsRouting>(
      '/nl2sql/analytics/routing', { params }
    ).then((r) => r.data),

  /** Get applied-rule hit analytics (coverage + top rules + daily trend). */
  analyticsRuleHits: (params?: { start_date?: string; end_date?: string }) =>
    client.get<import('@/types').AnalyticsRuleHits>(
      '/nl2sql/analytics/rule-hits', { params }
    ).then((r) => r.data),

  /** Get datasource-level health metrics (query volume, success, latency). */
  analyticsDatasourceHealth: (params?: { start_date?: string; end_date?: string }) =>
    client.get<import('@/types').AnalyticsDatasourceHealth>(
      '/nl2sql/analytics/datasource-health', { params }
    ).then((r) => r.data),

  /** Get semantic coverage analytics. */
  analyticsSemanticCoverage: () =>
    client.get<import('@/types').AnalyticsSemanticCoverage>(
      '/nl2sql/analytics/semantic-coverage'
    ).then((r) => r.data),

  /** Explain SQL query results in natural language.
   *  Calls the /nl2sql/explain endpoint to get an AI-generated interpretation
   *  of the query and its results. */
  explain: (data: { query_id: string; data_source_id?: string | null; sql?: string; language?: string }) =>
    client.post<import('@/types').Nl2sqlExplainResponse>(
      '/nl2sql/explain', data
    ).then((r) => r.data),

  /** Generate a natural language explanation of a SQL query (without query results).
   *  Calls the /nl2sql/explain-sql endpoint with SQL + schema to get a plain-English description.
   *  The response language is determined by the `language` field (e.g. "zh-CN" or "en-US"). */
  explainSql: (data: { sql: string; datasource_id: string; language: string }) =>
    client.post<{ explanation: string; summary: string; insights?: string[]; column_notes?: { column: string; observation: string }[] }>(
      '/nl2sql/explain-sql', data
    ).then((r) => r.data),

  /** Get daily trend analytics. */
  analyticsTrends: (params?: { start_date?: string; end_date?: string; granularity?: string }) =>
    client.get<import('@/types').AnalyticsTrends>(
      '/nl2sql/analytics/trends', { params }
    ).then((r) => r.data),

  // ── F-10: Query Permission Policies ────────────────────────────────────────────

  /** List query policies for the tenant. */
  listQueryPolicies: (params?: { page?: number; per_page?: number }) =>
    client.get<import('@/types').Nl2sqlQueryPolicyListResponse>(
      '/nl2sql/query-policies', { params }
    ).then((r) => r.data),

  /** Create a new query policy. */
  createQueryPolicy: (data: {
    datasource_id: string;
    user_id: string;
    allowed_tables: string[];
    denied_tables: string[];
    allowed_columns: string[];
    denied_columns: string[];
    row_filter_expr?: string;
    description?: string;
    enabled?: boolean;
  }) =>
    client.post<import('@/types').Nl2sqlQueryPolicy>(
      '/nl2sql/query-policies', data
    ).then((r) => r.data),

  /** Update an existing query policy. */
  updateQueryPolicy: (id: number, data: Partial<{
    user_id: string;
    allowed_tables: string[];
    denied_tables: string[];
    allowed_columns: string[];
    denied_columns: string[];
    row_filter_expr?: string;
    description?: string;
    enabled?: boolean;
  }>) =>
    client.patch<import('@/types').Nl2sqlQueryPolicy>(
      `/nl2sql/query-policies/${id}`, data
    ).then((r) => r.data),

  /** Delete a query policy. */
  deleteQueryPolicy: (id: number) =>
    client.delete(`/nl2sql/query-policies/${id}`).then((r) => r.data),

  // ── F-11: Query Performance Analysis ───────────────────────────────────────────

  /** Get slow queries with execution time percentiles. */
  slowQueries: (params?: { page?: number; per_page?: number; start_date?: string; end_date?: string }) =>
    client.get<SlowQueriesResponse>(
      '/nl2sql/analytics/slow-queries', { params }
    ).then((r) => r.data),

  /** Get per-user query statistics for the leaderboard. */
  analyticsUserLeaderboard: (params?: { page?: number; per_page?: number }) =>
    client.get<import('@/types').UserLeaderboardResponse>(
      '/nl2sql/analytics/user-leaderboard', { params }
    ).then((r) => r.data),

  // ── Feedback ────────────────────────────────────────────────────────────────

  submitFeedback: (data: {
    conversationId: string;
    queryId?: number;
    datasourceId: string;
    generatedSql: string;
    feedbackType: 'thumbs_up' | 'thumbs_down' | 'correction' | 'clarification_needed';
    correctedSql?: string;
    correctionNote?: string;
  }) => client.post<{ id: number }>('/nl2sql/feedback', data).then((r) => r.data),

  getFeedbackStats: (datasourceId: string) =>
    client.get<{
      ds_id: string;
      total_feedback: number;
      thumbs_up_count: number;
      thumbs_down_count: number;
      correction_count: number;
      satisfaction_rate: number | null;
    }>(`/nl2sql/feedback/stats/${encodeURIComponent(datasourceId)}`).then((r) => r.data),

  // ── Result Cache ─────────────────────────────────────────────────────────────

  clearResultCache: (datasourceId: string) =>
    client.delete(`/nl2sql/result-cache/${encodeURIComponent(datasourceId)}`).then((r) => r.data),
};

export function streamNl2sqlQueryTask(
  taskId: string,
  handlers: {
    onEvent?: (event: Nl2sqlQueryTaskEvent) => void;
    onError?: (error: string) => void;
    onDone?: (finalEvent: Nl2sqlQueryTaskEvent) => void;
  },
) {
  const token = localStorage.getItem('token');
  const tenantId = localStorage.getItem('tenant_id');
  const baseUrl = (client.defaults.baseURL ?? '/api/v1').replace('/api/v1', '');

  let aborted = false;
  let reader: ReadableStreamDefaultReader<Uint8Array> | null = null;
  let currentEvent = '';
  let currentData = '';

  fetch(`${baseUrl}/api/v1/nl2sql/query-tasks/${encodeURIComponent(taskId)}/events`, {
    method: 'GET',
    headers: {
      Authorization: `Bearer ${token}`,
      ...(tenantId ? { 'X-Tenant-ID': tenantId } : {}),
    },
  }).then(async (response) => {
    if (aborted) return;
    if (!response.ok) {
      const text = await response.text();
      if (aborted) return;
      handlers.onError?.(`请求失败: ${response.status} ${text}`);
      return;
    }

    const stream = response.body;
    if (!stream) {
      if (aborted) return;
      handlers.onError?.('无响应体');
      return;
    }

    reader = stream.getReader();
    const decoder = new TextDecoder();
    let buffer = '';

    const flush = () => {
      if (!currentEvent || !currentData) {
        currentEvent = '';
        currentData = '';
        return;
      }
      if (aborted) {
        currentEvent = '';
        currentData = '';
        return;
      }
      try {
        const payload = JSON.parse(currentData) as Nl2sqlQueryTaskEvent;
        if (currentEvent === 'task_event') {
          handlers.onEvent?.(payload);
          if (payload.status === 'completed' || payload.status === 'clarification_needed' || payload.status === 'failed') {
            handlers.onDone?.(payload);
          }
        }
      } catch {
        // Ignore malformed SSE records and continue with later events.
      }
      currentEvent = '';
      currentData = '';
    };

    while (true) {
      if (aborted) break;
      const { done, value } = await reader.read();
      if (done) {
        flush();
        break;
      }

      const chunk = decoder.decode(value, { stream: true });
      buffer += chunk;

      const lines = buffer.split('\n');
      buffer = lines.pop() ?? '';

      for (const raw of lines) {
        const trimmed = raw.trim();
        if (!trimmed) {
          flush();
          continue;
        }
        if (trimmed.startsWith('event:')) {
          flush();
          currentEvent = trimmed.slice(6).trim();
        } else if (trimmed.startsWith('data:')) {
          currentData = trimmed.slice(5).trim();
        } else if (currentData) {
          currentData += '\n' + trimmed;
        }
      }
    }
  }).catch((err) => {
    if (aborted) return;
    handlers.onError?.(err.message ?? 'stream error');
  });

  return () => {
    aborted = true;
    reader?.cancel();
  };
}

export function streamNl2sqlClarifyTask(
  taskId: string,
  handlers: {
    onEvent?: (event: import('@/types').ClarifyTaskEvent) => void;
    onError?: (error: string) => void;
    onDone?: (finalEvent: import('@/types').ClarifyTaskEvent) => void;
  },
) {
  const token = localStorage.getItem('token');
  const tenantId = localStorage.getItem('tenant_id');
  const baseUrl = (client.defaults.baseURL ?? '/api/v1').replace('/api/v1', '');

  let aborted = false;
  let reader: ReadableStreamDefaultReader<Uint8Array> | null = null;
  let currentEvent = '';
  let currentData = '';

  fetch(`${baseUrl}/api/v1/nl2sql/clarify-tasks/${encodeURIComponent(taskId)}/events`, {
    method: 'GET',
    headers: {
      Authorization: `Bearer ${token}`,
      ...(tenantId ? { 'X-Tenant-ID': tenantId } : {}),
    },
  }).then(async (response) => {
    if (aborted) return;
    if (!response.ok) {
      const text = await response.text();
      if (aborted) return;
      handlers.onError?.(`请求失败: ${response.status} ${text}`);
      return;
    }

    const stream = response.body;
    if (!stream) {
      if (aborted) return;
      handlers.onError?.('无响应体');
      return;
    }

    reader = stream.getReader();
    const decoder = new TextDecoder();
    let buffer = '';

    const flush = () => {
      if (!currentEvent || !currentData) {
        currentEvent = '';
        currentData = '';
        return;
      }
      if (aborted) {
        currentEvent = '';
        currentData = '';
        return;
      }
      try {
        const payload = JSON.parse(currentData) as import('@/types').ClarifyTaskEvent;
        if (currentEvent === 'task_event') {
          handlers.onEvent?.(payload);
          if (payload.status === 'completed' || payload.status === 'clarification_needed' || payload.status === 'failed') {
            handlers.onDone?.(payload);
          }
        }
      } catch {
        // Ignore malformed SSE records and continue with later events.
      }
      currentEvent = '';
      currentData = '';
    };

    while (true) {
      if (aborted) break;
      const { done, value } = await reader.read();
      if (done) {
        flush();
        break;
      }

      const chunk = decoder.decode(value, { stream: true });
      buffer += chunk;

      const lines = buffer.split('\n');
      buffer = lines.pop() ?? '';

      for (const raw of lines) {
        const trimmed = raw.trim();
        if (!trimmed) {
          flush();
          continue;
        }
        if (trimmed.startsWith('event:')) {
          flush();
          currentEvent = trimmed.slice(6).trim();
        } else if (trimmed.startsWith('data:')) {
          currentData = trimmed.slice(5).trim();
        } else if (currentData) {
          currentData += '\n' + trimmed;
        }
      }
    }
  }).catch((err) => {
    if (aborted) return;
    handlers.onError?.(err.message ?? 'stream error');
  });

  return () => {
    aborted = true;
    reader?.cancel();
  };
}

export function streamNl2sqlRouteTask(
  taskId: string,
  handlers: {
    onEvent?: (event: import('@/types').RouteTaskEvent) => void;
    onError?: (error: string) => void;
    onDone?: (finalEvent: import('@/types').RouteTaskEvent) => void;
  },
) {
  const token = localStorage.getItem('token');
  const tenantId = localStorage.getItem('tenant_id');
  const baseUrl = (client.defaults.baseURL ?? '/api/v1').replace('/api/v1', '');

  let aborted = false;
  let reader: ReadableStreamDefaultReader<Uint8Array> | null = null;
  let currentEvent = '';
  let currentData = '';
  let lastEvent: import('@/types').RouteTaskEvent | null = null;
  let doneEmitted = false;

  fetch(`${baseUrl}/api/v1/nl2sql/route-tasks/${encodeURIComponent(taskId)}/events`, {
    method: 'GET',
    headers: {
      Authorization: `Bearer ${token}`,
      ...(tenantId ? { 'X-Tenant-ID': tenantId } : {}),
    },
  }).then(async (response) => {
    if (!response.ok) {
      const text = await response.text();
      handlers.onError?.(`请求失败: ${response.status} ${text}`);
      return;
    }

    const stream = response.body;
    if (!stream) {
      handlers.onError?.('无响应体');
      return;
    }

    reader = stream.getReader();
    const decoder = new TextDecoder();
    let buffer = '';

    const flush = () => {
      if (!currentEvent || !currentData) {
        currentEvent = '';
        currentData = '';
        return;
      }
      if (aborted) {
        currentEvent = '';
        currentData = '';
        return;
      }
      try {
        const payload = JSON.parse(currentData) as import('@/types').RouteTaskEvent;
        if (currentEvent === 'task_event') {
          lastEvent = payload;
          handlers.onEvent?.(payload);
          if (payload.status === 'completed' || payload.status === 'clarification_needed' || payload.status === 'failed') {
            doneEmitted = true;
            handlers.onDone?.(payload);
          }
        }
      } catch {
        // Ignore malformed SSE records and continue with later events.
      }
      currentEvent = '';
      currentData = '';
    };

    while (true) {
      if (aborted) break;
      const { done, value } = await reader.read();
      if (done) {
        flush();
        if (!aborted && !doneEmitted && lastEvent) {
          doneEmitted = true;
          handlers.onDone?.(lastEvent);
        }
        break;
      }

      const chunk = decoder.decode(value, { stream: true });
      buffer += chunk;

      const lines = buffer.split('\n');
      buffer = lines.pop() ?? '';

      for (const raw of lines) {
        const trimmed = raw.trim();
        if (!trimmed) {
          flush();
          continue;
        }
        if (trimmed.startsWith('event:')) {
          flush();
          currentEvent = trimmed.slice(6).trim();
        } else if (trimmed.startsWith('data:')) {
          currentData = trimmed.slice(5).trim();
        } else if (currentData) {
          currentData += '\n' + trimmed;
        }
      }
    }
  }).catch((err) => {
    handlers.onError?.(err.message ?? 'stream error');
  });

  return () => {
    aborted = true;
    reader?.cancel();
  };
}

export function streamNl2sqlAgentTask(
  taskId: string,
  handlers: {
    onEvent?: (event: import('@/types').AgentTaskEvent) => void;
    onError?: (error: string) => void;
    onDone?: (finalEvent: import('@/types').AgentTaskEvent) => void;
  },
) {
  const token = localStorage.getItem('token');
  const tenantId = localStorage.getItem('tenant_id');
  const baseUrl = (client.defaults.baseURL ?? '/api/v1').replace('/api/v1', '');

  let aborted = false;
  let reader: ReadableStreamDefaultReader<Uint8Array> | null = null;
  let currentEvent = '';
  let currentData = '';

  fetch(`${baseUrl}/api/v1/nl2sql/agent-tasks/${encodeURIComponent(taskId)}/events`, {
    method: 'GET',
    headers: {
      Authorization: `Bearer ${token}`,
      ...(tenantId ? { 'X-Tenant-ID': tenantId } : {}),
    },
  }).then(async (response) => {
    if (!response.ok) {
      const text = await response.text();
      handlers.onError?.(`请求失败: ${response.status} ${text}`);
      return;
    }

    const stream = response.body;
    if (!stream) {
      handlers.onError?.('无响应体');
      return;
    }

    reader = stream.getReader();
    const decoder = new TextDecoder();
    let buffer = '';

    const flush = () => {
      if (!currentEvent || !currentData) {
        currentEvent = '';
        currentData = '';
        return;
      }
      if (aborted) {
        currentEvent = '';
        currentData = '';
        return;
      }
      try {
        const payload = JSON.parse(currentData) as import('@/types').AgentTaskEvent;
        if (currentEvent === 'task_event') {
          handlers.onEvent?.(payload);
          if (payload.status === 'completed' || payload.status === 'failed') {
            handlers.onDone?.(payload);
          }
        }
      } catch {
        // Ignore malformed SSE records and continue with later events.
      }
      currentEvent = '';
      currentData = '';
    };

    while (true) {
      if (aborted) break;
      const { done, value } = await reader.read();
      if (done) {
        flush();
        break;
      }

      const chunk = decoder.decode(value, { stream: true });
      buffer += chunk;

      const lines = buffer.split('\n');
      buffer = lines.pop() ?? '';

      for (const raw of lines) {
        const trimmed = raw.trim();
        if (!trimmed) {
          flush();
          continue;
        }
        if (trimmed.startsWith('event:')) {
          flush();
          currentEvent = trimmed.slice(6).trim();
        } else if (trimmed.startsWith('data:')) {
          currentData = trimmed.slice(5).trim();
        } else if (currentData) {
          currentData += '\n' + trimmed;
        }
      }
    }
  }).catch((err) => {
    handlers.onError?.(err.message ?? 'stream error');
  });

  return () => {
    aborted = true;
    reader?.cancel();
  };
}

export function streamNl2sqlAttributionTask(
  taskId: string,
  handlers: {
    onEvent?: (event: AttributionTaskEvent) => void;
    onError?: (error: string) => void;
    onDone?: (finalEvent: AttributionTaskEvent) => void;
  },
) {
  const token = localStorage.getItem('token');
  const tenantId = localStorage.getItem('tenant_id');
  const baseUrl = (client.defaults.baseURL ?? '/api/v1').replace('/api/v1', '');

  let aborted = false;
  let reader: ReadableStreamDefaultReader<Uint8Array> | null = null;
  let currentEvent = '';
  let currentData = '';

  fetch(`${baseUrl}/api/v1/nl2sql/attribution/tasks/${encodeURIComponent(taskId)}/events`, {
    method: 'GET',
    headers: {
      Authorization: `Bearer ${token}`,
      ...(tenantId ? { 'X-Tenant-ID': tenantId } : {}),
    },
  }).then(async (response) => {
    if (aborted) return;
    if (!response.ok) {
      const text = await response.text();
      if (aborted) return;
      handlers.onError?.(`请求失败: ${response.status} ${text}`);
      return;
    }

    const stream = response.body;
    if (!stream) {
      if (aborted) return;
      handlers.onError?.('无响应体');
      return;
    }

    reader = stream.getReader();
    const decoder = new TextDecoder();
    let buffer = '';

    const flush = () => {
      if (!currentEvent || !currentData) {
        currentEvent = '';
        currentData = '';
        return;
      }
      if (aborted) {
        currentEvent = '';
        currentData = '';
        return;
      }
      try {
        const payload = JSON.parse(currentData) as AttributionTaskEvent;
        if (currentEvent === 'task_event') {
          handlers.onEvent?.(payload);
          if (
            payload.status === 'completed' ||
            payload.status === 'clarification_needed' ||
            payload.status === 'no_data' ||
            payload.status === 'partial' ||
            payload.status === 'failed' ||
            payload.status === 'cancelled'
          ) {
            handlers.onDone?.(payload);
          }
        }
      } catch {
        // Ignore malformed SSE records and continue with later events.
      }
      currentEvent = '';
      currentData = '';
    };

    while (true) {
      if (aborted) break;
      const { done, value } = await reader.read();
      if (done) {
        flush();
        break;
      }

      const chunk = decoder.decode(value, { stream: true });
      buffer += chunk;

      const lines = buffer.split('\n');
      buffer = lines.pop() ?? '';

      for (const raw of lines) {
        const trimmed = raw.trim();
        if (!trimmed) {
          flush();
          continue;
        }
        if (trimmed.startsWith('event:')) {
          flush();
          currentEvent = trimmed.slice(6).trim();
        } else if (trimmed.startsWith('data:')) {
          currentData = trimmed.slice(5).trim();
        } else if (currentData) {
          currentData += '\n' + trimmed;
        }
      }
    }
  }).catch((err) => {
    if (aborted) return;
    handlers.onError?.(err.message ?? 'stream error');
  });

  return () => {
    aborted = true;
    reader?.cancel();
  };
}
