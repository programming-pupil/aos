/**
 * Centralized query/mutation key definitions for TanStack Query.
 * Using constants prevents key typos across the codebase.
 */

const BASE = {
  dashboard: ['dashboard'] as const,
  demo: ['demo'] as const,
  apiKeys: ['apiKeys'] as const,
  tenants: ['tenants'] as const,
  agentSessions: ['agentSessions'] as const,
  chatSessions: ['chatSessions'] as const,
  chatAdversarial: ['chatAdversarial'] as const,
  agentOps: ['agentOps'] as const,
  tasks: ['tasks'] as const,
  mcp: ['mcp'] as const,
  skills: ['skills'] as const,
  hooks: ['hooks'] as const,
  botAgents: ['botAgents'] as const,
  projects: ['projects'] as const,
  rd: ['rd'] as const,
  config: ['config'] as const,
  auth: ['auth'] as const,
  setup: ['setup'] as const,
  users: ['users'] as const,
  notifications: ['notifications'] as const,
  commands: ['commands'] as const,
  dataSources: ['dataSources'] as const,
  nl2sql: ['nl2sql'] as const,
  pm: ['pm'] as const,
};

export const queryKeys = {
  // Dashboard
  dashboard: {
    all: BASE.dashboard,
    configOverviewStats: () => [...BASE.dashboard, 'configOverviewStats'] as const,
    overview: (p?: { start_date?: string; end_date?: string }) =>
      [...BASE.dashboard, 'overview', p ?? {}] as const,
    dailyTrend: (p?: { start_date?: string; end_date?: string }) =>
      [...BASE.dashboard, 'dailyTrend', p ?? {}] as const,
    modelUsage: (p?: { start_date?: string; end_date?: string }) =>
      [...BASE.dashboard, 'modelUsage', p ?? {}] as const,
    moduleUsage: (p?: { start_date?: string; end_date?: string }) =>
      [...BASE.dashboard, 'moduleUsage', p ?? {}] as const,
    alerts: {
      all: [...BASE.dashboard, 'alerts'] as const,
      list: () => [...BASE.dashboard, 'alerts', 'list'] as const,
    },
  },

  demo: {
    all: BASE.demo,
    scenarios: () => [...BASE.demo, 'scenarios'] as const,
    scenario: (id: string) => [...BASE.demo, 'scenarios', id] as const,
  },

  // API Keys
  apiKeys: {
    all: BASE.apiKeys,
    list: () => [...BASE.apiKeys, 'list'] as const,
    stats: (keyId: string) => [...BASE.apiKeys, 'stats', keyId] as const,
  },

  // Tenants
  tenants: {
    all: BASE.tenants,
    list: (p?: { page?: number; per_page?: number }) =>
      [...BASE.tenants, 'list', p ?? {}] as const,
  },

  // Agent sessions
  agentSessions: {
    all: BASE.agentSessions,
    list: (source?: string) => [...BASE.agentSessions, 'list', source ?? 'all'] as const,
    detail: (sessionId: string) => [...BASE.agentSessions, 'detail', sessionId] as const,
    history: (sessionId: string) => [...BASE.agentSessions, 'history', sessionId] as const,
  },

  // Chat sessions
  chatSessions: {
    all: BASE.chatSessions,
    list: () => [...BASE.chatSessions, 'list'] as const,
    detail: (sessionId: string) => [...BASE.chatSessions, 'detail', sessionId] as const,
    capabilities: (model?: string) => [...BASE.chatSessions, 'capabilities', model ?? 'default'] as const,
  },

  chatAdversarial: {
    all: BASE.chatAdversarial,
    list: () => [...BASE.chatAdversarial, 'list'] as const,
    detail: (runId: string) => [...BASE.chatAdversarial, 'detail', runId] as const,
  },

  agentOps: {
    all: BASE.agentOps,
    summary: () => [...BASE.agentOps, 'summary'] as const,
    agents: () => [...BASE.agentOps, 'agents'] as const,
    tasks: (params?: {
      status?: string;
      attention_only?: boolean;
      capability_key?: string;
      source?: string;
      external_conversation_id?: string;
      linked_resource_type?: string;
      linked_resource_id?: string;
      page?: number;
      per_page?: number;
    }) =>
      [...BASE.agentOps, 'tasks', params ?? {}] as const,
    queue: (params?: {
      queueStatus?: string;
      capabilityKey?: string;
      workerId?: string;
      deadOnly?: boolean;
      staleOnly?: boolean;
      leaseTimeoutSecs?: number;
      page?: number;
      per_page?: number;
    }) => [...BASE.agentOps, 'queue', params ?? {}] as const,
    task: (id: string) => [...BASE.agentOps, 'tasks', id] as const,
    taskEvents: (id: string) => [...BASE.agentOps, 'tasks', id, 'events'] as const,
    runtimeProcesses: (sessionId: string) => [...BASE.agentOps, 'runtime', sessionId, 'processes'] as const,
    runtimeArtifacts: (sessionId: string) => [...BASE.agentOps, 'runtime', sessionId, 'artifacts'] as const,
    runtimeArtifact: (sessionId: string, artifactId: string) =>
      [...BASE.agentOps, 'runtime', sessionId, 'artifacts', artifactId] as const,
    capabilities: () => [...BASE.agentOps, 'capabilities'] as const,
  },

  tasks: {
    all: BASE.tasks,
    summary: (scope: 'own' | 'tenant' = 'own') => [...BASE.tasks, 'summary', scope] as const,
    list: (params?: Record<string, unknown>) => [...BASE.tasks, 'list', params ?? {}] as const,
    detail: (id: string) => [...BASE.tasks, 'detail', id] as const,
    events: (id: string) => [...BASE.tasks, 'events', id] as const,
    resources: (id: string) => [...BASE.tasks, 'resources', id] as const,
    artifacts: (id: string) => [...BASE.tasks, 'artifacts', id] as const,
    attempts: (id: string) => [...BASE.tasks, 'attempts', id] as const,
    commands: (id: string) => [...BASE.tasks, 'commands', id] as const,
    subscriptions: (id: string) => [...BASE.tasks, 'subscriptions', id] as const,
    watchRules: () => [...BASE.tasks, 'watchRules'] as const,
    deliveries: (params?: Record<string, unknown>) => [...BASE.tasks, 'deliveries', params ?? {}] as const,
    identities: () => [...BASE.tasks, 'identities'] as const,
  },

  // MCP
  mcp: {
    all: BASE.mcp,
    list: (params?: { page?: number; per_page?: number }) =>
      [...BASE.mcp, 'list', params ?? {}] as const,
    stats: () => [...BASE.mcp, 'stats'] as const,
  },

  // Skills
  skills: {
    all: BASE.skills,
    list: (params?: { page?: number; per_page?: number }) =>
      [...BASE.skills, 'list', params ?? {}] as const,
    detail: (name: string) => [...BASE.skills, 'detail', name] as const,
    readme: (name: string) => [...BASE.skills, 'readme', name] as const,
    marketReposRoot: () => [...BASE.skills, 'market', 'repos'] as const,
    marketRepos: (params?: { page?: number; per_page?: number }) =>
      [...BASE.skills, 'market', 'repos', params ?? {}] as const,
    marketSearchRoot: () => [...BASE.skills, 'market', 'search'] as const,
    marketSearch: (params?: { q?: string; page?: number; per_page?: number; limit?: number }) =>
      [...BASE.skills, 'market', 'search', params ?? {}] as const,
  },

  // Hooks
  hooks: {
    all: BASE.hooks,
    list: (params?: { page?: number; per_page?: number }) =>
      [...BASE.hooks, 'list', params ?? {}] as const,
    detail: (id: string) => [...BASE.hooks, 'detail', id] as const,
  },

  botAgents: {
    all: BASE.botAgents,
    list: (params?: { page?: number; per_page?: number }) =>
      [...BASE.botAgents, 'list', params ?? {}] as const,
    channels: (params?: { agent_id?: string; page?: number; per_page?: number }) =>
      [...BASE.botAgents, 'channels', params ?? {}] as const,
    logs: (params?: { agent_id?: string; channel_id?: string; page?: number; per_page?: number }) =>
      [...BASE.botAgents, 'logs', params ?? {}] as const,
    identities: () => [...BASE.botAgents, 'identities'] as const,
  },

  // Projects
  projects: {
    all: BASE.projects,
    list: () => [...BASE.projects, 'list'] as const,
    detail: (id: string) => [...BASE.projects, 'detail', id] as const,
  },

  rd: {
    all: BASE.rd,
    repositories: () => [...BASE.rd, 'repositories'] as const,
    quality: (params?: { days?: number; repositoryId?: string }) =>
      [...BASE.rd, 'quality', params ?? {}] as const,
    repositoryTree: (id: string) => [...BASE.rd, 'repositories', id, 'tree'] as const,
    repositoryFile: (id: string, path: string) =>
      [...BASE.rd, 'repositories', id, 'file', path] as const,
    repositorySearch: (id: string, q: string, limit?: number) =>
      [...BASE.rd, 'repositories', id, 'search', q, limit ?? 30] as const,
    repositoryFileSuggestions: (ids: string[], q: string, limit?: number) =>
      [...BASE.rd, 'repositories', 'fileSuggestions', ids.join(','), q, limit ?? 30] as const,
    repositoryWorktreeStatus: (id?: string) =>
      [...BASE.rd, 'repositories', id ?? 'none', 'worktreeStatus'] as const,
    repositorySymbols: (id: string, q?: string, limit?: number) =>
      [...BASE.rd, 'repositories', id, 'symbols', q ?? '', limit ?? 50] as const,
    repositoryImports: (id: string, q?: string, limit?: number) =>
      [...BASE.rd, 'repositories', id, 'imports', q ?? '', limit ?? 80] as const,
    codeIntelStatus: (id?: string) =>
      [...BASE.rd, 'repositories', id ?? 'none', 'codeIntel', 'status'] as const,
    codeIntelQuery: (id: string, action: string, path?: string, query?: string) =>
      [...BASE.rd, 'repositories', id, 'codeIntel', action, path ?? '', query ?? ''] as const,
    previewSession: (id?: string) =>
      [...BASE.rd, 'previewSessions', id ?? 'none'] as const,
    previewLogs: (id?: string) =>
      [...BASE.rd, 'previewSessions', id ?? 'none', 'logs'] as const,
    branches: (id: string) => [...BASE.rd, 'repositories', id, 'branches'] as const,
    tasks: (params?: { status?: string; repositoryId?: string; mode?: string; page?: number; perPage?: number }) =>
      [...BASE.rd, 'tasks', params ?? {}] as const,
    task: (id: string) => [...BASE.rd, 'tasks', id] as const,
    taskWorkbench: (id: string) => [...BASE.rd, 'tasks', id, 'workbench'] as const,
    taskEvents: (id: string, params?: { perPage?: number }) =>
      params ? [...BASE.rd, 'tasks', id, 'events', params] as const : [...BASE.rd, 'tasks', id, 'events'] as const,
    taskTokenDiagnostics: (id: string) => [...BASE.rd, 'tasks', id, 'tokenDiagnostics'] as const,
    taskChanges: (id: string) => [...BASE.rd, 'tasks', id, 'changes'] as const,
    taskTests: (id: string) => [...BASE.rd, 'tasks', id, 'tests'] as const,
    specs: () => [...BASE.rd, 'specs'] as const,
    spec: (id?: string) => [...BASE.rd, 'specs', id ?? 'none'] as const,
    specEvents: (id?: string) => [...BASE.rd, 'specs', id ?? 'none', 'events'] as const,
    agentProfiles: () => [...BASE.rd, 'agentProfiles'] as const,
    agentMarket: (params?: { q?: string; itemType?: string }) =>
      [...BASE.rd, 'agentMarket', params ?? {}] as const,
    agentWorkflows: () => [...BASE.rd, 'agentWorkflows'] as const,
    steeringRules: () => [...BASE.rd, 'steeringRules'] as const,
    integrations: () => [...BASE.rd, 'integrations'] as const,
    prDraft: (id: string, integrationId?: string) =>
      [...BASE.rd, 'tasks', id, 'prDraft', integrationId ?? ''] as const,
  },

  // Config
  config: {
    all: BASE.config,
    overview: () => [...BASE.config, 'overview'] as const,
    management: () => [...BASE.config, 'management'] as const,
  },

  // Auth
  auth: {
    me: () => ['auth', 'me'] as const,
  },

  // Setup
  setup: {
    status: () => ['setup', 'status'] as const,
  },

  // Users
  users: {
    all: BASE.users,
    list: (params?: { page?: number; per_page?: number }) =>
      [...BASE.users, 'list', params ?? {}] as const,
    detail: (id: string) => [...BASE.users, 'detail', id] as const,
    me: () => ['users', 'me'] as const,
  },

  // Notifications
  notifications: {
    all: BASE.notifications,
    list: (params?: { page?: number; per_page?: number; read?: string }) =>
      [...BASE.notifications, 'list', params ?? {}] as const,
  },

  pm: {
    all: BASE.pm,
    searchProviders: () => [...BASE.pm, 'searchProviders'] as const,
    searchDoctor: () => [...BASE.pm, 'searchDoctor'] as const,
  },

  // Commands (slash commands: builtin + skill-registered)
  commands: {
    all: BASE.commands,
    list: () => [...BASE.commands, 'list'] as const,
  },

  // Data Sources
  dataSources: {
    all: () => BASE.dataSources as unknown as string[],
    list: () => [...BASE.dataSources, 'list'] as const,
    detail: (id: string) => [...BASE.dataSources, 'detail', id] as const,
  },

  // NL2SQL
  nl2sql: {
    all: BASE.nl2sql,
    history: () => [...BASE.nl2sql, 'history'] as const,
    explain: (queryId: string) => [...BASE.nl2sql, 'explain', queryId] as const,
    queryPolicies: (page?: number, pageSize?: number) => [
      ...BASE.nl2sql, 'queryPolicies', page ?? 1, pageSize ?? 20,
    ] as const,
    schema: (dataSourceId: string) => [...BASE.nl2sql, 'schema', dataSourceId] as const,
    semantics: (dataSourceId: string) => [...BASE.nl2sql, 'semantics', dataSourceId] as const,
    embeddingConfig: () => [...BASE.nl2sql, 'embeddingConfig'] as const,
    embeddingHealth: () => [...BASE.nl2sql, 'embeddingHealth'] as const,
    // P3-Enterprise: Business Domains
    domains: {
      all: () => [...BASE.nl2sql, 'domains'] as const,
      list: (dsId?: string) => dsId
        ? [...BASE.nl2sql, 'domains', 'ds', dsId] as const
        : [...BASE.nl2sql, 'domains'] as const,
    },
    // P3-Enterprise: Schema Change Notifications
    schemaChanges: {
      all: () => [...BASE.nl2sql, 'schemaChanges'] as const,
      list: (params?: { status?: string; page?: number; per_page?: number }) =>
        [...BASE.nl2sql, 'schemaChanges', 'list', params ?? {}] as const,
    },
    // P3-Enterprise: Time Patterns
    timePatterns: {
      all: () => [...BASE.nl2sql, 'timePatterns'] as const,
    },
    // P3-Enterprise: Validation Rules
    validationRules: (dsId: string) =>
      [...BASE.nl2sql, 'validationRules', dsId] as const,
    // R-7: Column Masking Rules (tenant-wide, no datasource scoping in the key)
    maskingRules: () => [...BASE.nl2sql, 'maskingRules'] as const,
    // F-2: Manual Foreign Keys per datasource
    foreignKeys: (dsId: string) => [...BASE.nl2sql, 'foreignKeys', dsId] as const,
    // P3-Enterprise: Query Understanding cache
    quCache: (dsId: string) => [...BASE.nl2sql, 'quCache', dsId] as const,
    // P3-1: Multi-turn clarification
    clarification: {
      all: [...BASE.nl2sql, 'clarification'] as const,
      pending: (sessionId: string) => [...BASE.nl2sql, 'clarification', 'pending', sessionId] as const,
    },
    // P3-2: Conversation summary
    conversations: {
      all: [...BASE.nl2sql, 'conversations'] as const,
      list: (params?: { page?: number; per_page?: number }) =>
        [...BASE.nl2sql, 'conversations', 'list', params ?? {}] as const,
      detail: (id: string) => [...BASE.nl2sql, 'conversations', 'detail', id] as const,
    },
    attributionConversations: {
      all: [...BASE.nl2sql, 'attributionConversations'] as const,
      list: (params?: { page?: number; per_page?: number }) =>
        [...BASE.nl2sql, 'attributionConversations', 'list', params ?? {}] as const,
      detail: (id: string) => [...BASE.nl2sql, 'attributionConversations', 'detail', id] as const,
    },
    // P2-1: Synonyms
    synonyms: (dsId: string, page = 1, pageSize = 20) =>
      [...BASE.nl2sql, 'synonyms', dsId, { page, pageSize }] as const,
    // P1-2: Metrics
    metrics: (dsId: string) => [...BASE.nl2sql, 'metrics', dsId] as const,
    // P1-3: Join Paths
    joinPaths: (dsId: string) => [...BASE.nl2sql, 'joinPaths', dsId] as const,
    // P2-2: Cross-Datasource Relations
    crossDSRelations: {
      all: () => [...BASE.nl2sql, 'crossDSRelations'] as const,
    },
    // P2-3: Cross-Domain Clusters
    crossDomainClusters: {
      all: () => [...BASE.nl2sql, 'crossDomainClusters'] as const,
    },
    // F-09: Saved Views (consolidated key for cache invalidation)
    views: () => [...BASE.nl2sql, 'views'] as const,
    referencePacks: (dsId: string) => [...BASE.nl2sql, 'referencePacks', dsId] as const,
    sqlKnowledge: {
      all: () => [...BASE.nl2sql, 'sqlKnowledge'] as const,
      spaces: (params?: { datasourceId?: string; includeGlobal?: boolean }) =>
        [...BASE.nl2sql, 'sqlKnowledge', 'spaces', params ?? {}] as const,
      search: (params?: { datasourceId?: string; question?: string; limit?: number }) =>
        [...BASE.nl2sql, 'sqlKnowledge', 'search', params ?? {}] as const,
      file: (fileId: string, params?: { startLine?: number; endLine?: number }) =>
        [...BASE.nl2sql, 'sqlKnowledge', 'file', fileId, params ?? {}] as const,
      importTasks: (spaceId?: string | null) =>
        [...BASE.nl2sql, 'sqlKnowledge', 'importTasks', spaceId ?? 'none'] as const,
    },
    // P3-1: Analytics
    analytics: {
      overview: (p?: { start_date?: string; end_date?: string }) =>
        [...BASE.nl2sql, 'analytics', 'overview', p ?? {}] as const,
      routing: (p?: { start_date?: string; end_date?: string }) =>
        [...BASE.nl2sql, 'analytics', 'routing', p ?? {}] as const,
      ruleHits: (p?: { start_date?: string; end_date?: string }) =>
        [...BASE.nl2sql, 'analytics', 'ruleHits', p ?? {}] as const,
      datasourceHealth: (p?: { start_date?: string; end_date?: string }) =>
        [...BASE.nl2sql, 'analytics', 'datasourceHealth', p ?? {}] as const,
      semanticCoverage: () => [...BASE.nl2sql, 'analytics', 'semanticCoverage'] as const,
      trends: (p?: { start_date?: string; end_date?: string; granularity?: string }) =>
        [...BASE.nl2sql, 'analytics', 'trends', p ?? {}] as const,
      slowQueries: (p?: { start_date?: string; end_date?: string; page?: number; per_page?: number }) =>
        [...BASE.nl2sql, 'analytics', 'slowQueries', p ?? {}] as const,
    },
    feedback: (dsId: string) => [...BASE.nl2sql, 'feedback', dsId] as const,
  },
} as const;
