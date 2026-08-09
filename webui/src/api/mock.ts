/**
 * Development-mode mock data.
 * Automatically used as fallback when the backend is unreachable.
 * Set VITE_USE_MOCK=true to force mock mode, or rely on auto-detection
 * (backend unreachable → mock mode).
 */
import type {
  DashboardOverview,
  ApiKeyRecord,
  SessionSummary,
  AgentSessionInfo,
  ChatSessionInfo,
  SkillInfo,
  McpServerInfo,
  ConfigOverview,
} from '@/types';

// ---------------------------------------------------------------------------
// Dashboard
// ---------------------------------------------------------------------------

export const MOCK_DASHBOARD_OVERVIEW: DashboardOverview = {
  token_stats: {
    total_input_tokens: 12_450_000,
    total_output_tokens: 8_230_000,
    total_cache_creation_tokens: 4_500_000,
    total_cache_read_tokens: 6_800_000,
    estimated_cost_usd: 128.45,
    session_count: 342,
    total_requests: 5_516,
    active_model_count: 5,
  },
  cache_stats: {
    total_cache_creation_tokens: 4_500_000,
    total_cache_read_tokens: 6_800_000,
    estimated_savings_usd: 67.30,
    cache_hit_rate: 60.3,
  },
  top_models: [
    { model: 'claude-opus-4-6', request_count: 1842, input_tokens: 6200000, output_tokens: 4100000, estimated_cost_usd: 82.40 },
    { model: 'claude-sonnet-4-7', request_count: 956, input_tokens: 3100000, output_tokens: 2050000, estimated_cost_usd: 31.20 },
    { model: 'claude-haiku-4-7', request_count: 2108, input_tokens: 2150000, output_tokens: 1530000, estimated_cost_usd: 12.15 },
    { model: 'gpt-4o', request_count: 412, input_tokens: 560000, output_tokens: 380000, estimated_cost_usd: 2.30 },
    { model: 'grok-2', request_count: 198, input_tokens: 230000, output_tokens: 160000, estimated_cost_usd: 0.40 },
  ],
  daily_trend: [
    { date: '2026-04-11', input_tokens: 420000, output_tokens: 280000, cache_creation_tokens: 150000, cache_read_tokens: 220000, estimated_cost_usd: 4.20 },
    { date: '2026-04-12', input_tokens: 480000, output_tokens: 310000, cache_creation_tokens: 170000, cache_read_tokens: 250000, estimated_cost_usd: 4.80 },
    { date: '2026-04-13', input_tokens: 510000, output_tokens: 340000, cache_creation_tokens: 190000, cache_read_tokens: 280000, estimated_cost_usd: 5.10 },
    { date: '2026-04-14', input_tokens: 390000, output_tokens: 260000, cache_creation_tokens: 130000, cache_read_tokens: 200000, estimated_cost_usd: 3.90 },
    { date: '2026-04-15', input_tokens: 620000, output_tokens: 410000, cache_creation_tokens: 220000, cache_read_tokens: 330000, estimated_cost_usd: 6.20 },
    { date: '2026-04-16', input_tokens: 580000, output_tokens: 380000, cache_creation_tokens: 200000, cache_read_tokens: 300000, estimated_cost_usd: 5.80 },
    { date: '2026-04-17', input_tokens: 550000, output_tokens: 350000, cache_creation_tokens: 180000, cache_read_tokens: 270000, estimated_cost_usd: 5.50 },
  ],
};

// ---------------------------------------------------------------------------
// API Keys
// ---------------------------------------------------------------------------

export const MOCK_API_KEYS: ApiKeyRecord[] = [
  {
    id: 'key-001',
    name: 'Production - Anthropic',
    provider: 'anthropic',
    model: 'claude-opus-4-6',
    model_type: 'chat',
    base_url: undefined,
    key_hint: 'sk-ant-',
    daily_limit: 100000,
    monthly_limit: 2000000,
    enabled: true,
    created_at: new Date(Date.now() - 1000 * 60 * 60 * 24 * 30).toISOString(),
  },
  {
    id: 'key-002',
    name: 'Development - OpenAI',
    provider: 'openai',
    model: 'gpt-4o',
    model_type: 'chat',
    base_url: 'https://api.novita.ai/v1',
    key_hint: 'sk-opn-',
    daily_limit: 50000,
    monthly_limit: 1000000,
    enabled: true,
    created_at: new Date(Date.now() - 1000 * 60 * 60 * 24 * 14).toISOString(),
  },
  {
    id: 'key-003',
    name: 'Backup - xAI',
    provider: 'xai',
    model: 'grok-2',
    model_type: 'chat',
    key_hint: 'xai-',
    enabled: false,
    created_at: new Date(Date.now() - 1000 * 60 * 60 * 24 * 7).toISOString(),
  },
  {
    id: 'key-004',
    name: 'OpenRouter - Mixed',
    provider: 'openai',
    base_url: 'https://openrouter.ai/api/v1',
    model: undefined,
    model_type: 'chat',
    key_hint: 'sk-or-',
    enabled: true,
    created_at: new Date(Date.now() - 1000 * 60 * 60 * 24 * 3).toISOString(),
  },
];

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

export const MOCK_SESSIONS: SessionSummary[] = [
  {
    session_id: 'sess_oauth2_integration_001',
    path: '/workspace/user-service/.aos/sessions/sess_oauth2_integration_001.jsonl',
    message_count: 42,
    created_at: new Date(Date.now() - 1000 * 60 * 60 * 2).toISOString(),
    updated_at: new Date(Date.now() - 1000 * 60 * 15).toISOString(),
    model: 'claude-opus-4-6',
    compact_threshold: 100,
  },
  {
    session_id: 'sess_payment_bug_002',
    path: '/workspace/payment-api/.aos/sessions/sess_payment_bug_002.jsonl',
    message_count: 28,
    created_at: new Date(Date.now() - 1000 * 60 * 60 * 24).toISOString(),
    updated_at: new Date(Date.now() - 1000 * 60 * 60 * 5).toISOString(),
    model: 'claude-opus-4-6',
    compact_threshold: 100,
  },
  {
    session_id: 'sess_inventory_refactor_003',
    path: '/workspace/inventory-ms/.aos/sessions/sess_inventory_refactor_003.jsonl',
    message_count: 15,
    created_at: new Date(Date.now() - 1000 * 60 * 60 * 48).toISOString(),
    updated_at: new Date(Date.now() - 1000 * 60 * 60 * 24).toISOString(),
    model: 'claude-sonnet-4-7',
    compact_threshold: 100,
  },
  {
    session_id: 'sess_legacy_migrate_004',
    path: '/workspace/legacy-app/.aos/sessions/sess_legacy_migrate_004.jsonl',
    message_count: 156,
    created_at: new Date(Date.now() - 1000 * 60 * 60 * 24 * 7).toISOString(),
    updated_at: new Date(Date.now() - 1000 * 60 * 60 * 24 * 3).toISOString(),
    model: 'claude-opus-4-6',
    compact_threshold: 100,
  },
];

export const MOCK_AGENT_SESSIONS: AgentSessionInfo[] = [
  {
    session_id: 'agent_oauth_001',
    name: 'OAuth2 集成',
    state: 'idle',
    model: 'claude-opus-4-6',
    workspace: '/workspace/user-service',
    created_at: new Date(Date.now() - 1000 * 60 * 60 * 2).toISOString(),
    last_activity: new Date(Date.now() - 1000 * 60 * 15).toISOString(),
    is_pinned: true,
    source: 'agent',
  },
  {
    session_id: 'agent_payment_002',
    name: '支付模块重构',
    state: 'running',
    model: 'claude-opus-4-6',
    workspace: '/workspace/payment-api',
    created_at: new Date(Date.now() - 1000 * 60 * 60 * 24).toISOString(),
    last_activity: new Date(Date.now() - 1000 * 60 * 5).toISOString(),
    is_pinned: false,
    source: 'agent',
  },
];

// ---------------------------------------------------------------------------
// Chat Sessions
// ---------------------------------------------------------------------------

export const MOCK_CHAT_SESSIONS: ChatSessionInfo[] = [
  {
    sessionId: 'chat_001',
    messageCount: 12,
    lastUpdated: Date.now() - 1000 * 60 * 30,
  },
  {
    sessionId: 'chat_002',
    messageCount: 5,
    lastUpdated: Date.now() - 1000 * 60 * 60 * 3,
  },
];

// ---------------------------------------------------------------------------
// Skills
// ---------------------------------------------------------------------------

export const MOCK_SKILLS: SkillInfo[] = [
  {
    id: 'skill-001',
    name: 'using-superpowers',
    description: '查找和使用 Agent Skills 的最佳实践指南',
    path: '/skills/using-superpowers',
    source: 'builtin',
    tags: ['meta', 'skills', 'guide'],
    enabled: true,
    version: '1.0.0',
    commands_count: 3,
    created_at: new Date(Date.now() - 1000 * 60 * 60 * 24 * 30).toISOString(),
    updated_at: new Date(Date.now() - 1000 * 60 * 60 * 24 * 30).toISOString(),
  },
  {
    id: 'skill-002',
    name: 'web-dev',
    description: 'Web 前端开发的最佳实践 Skill',
    path: '/skills/web-dev',
    source: 'uploaded',
    tags: ['frontend', 'react', 'typescript'],
    enabled: true,
    version: '1.0.0',
    commands_count: 8,
    created_at: new Date(Date.now() - 1000 * 60 * 60 * 24 * 14).toISOString(),
    updated_at: new Date(Date.now() - 1000 * 60 * 60 * 24 * 14).toISOString(),
  },
  {
    id: 'skill-003',
    name: 'rust-dev',
    description: 'Rust 系统编程的 Skill',
    path: '/skills/rust-dev',
    source: 'uploaded',
    tags: ['backend', 'rust', 'systems'],
    enabled: false,
    version: '1.0.0',
    commands_count: 12,
    created_at: new Date(Date.now() - 1000 * 60 * 60 * 24 * 7).toISOString(),
    updated_at: new Date(Date.now() - 1000 * 60 * 60 * 24 * 7).toISOString(),
  },
];

// ---------------------------------------------------------------------------
// MCP
// ---------------------------------------------------------------------------

export const MOCK_MCP_SERVERS: McpServerInfo[] = [
  {
    name: 'filesystem-mcp',
    transport: 'stdio',
    command: 'npx',
    args: ['-y', '@modelcontextprotocol/server-filesystem', '/workspace'],
    enabled: true,
    tools_count: 6,
    status: 'healthy',
    connection_status: 'connected',
    auth: { auth_type: 'none', has_token: false },
    last_error: undefined,
  },
  {
    name: 'weather-mcp',
    transport: 'http',
    url: 'http://localhost:3000/mcp',
    args: [],
    enabled: true,
    tools_count: 3,
    status: 'healthy',
    connection_status: 'connected',
    auth: { auth_type: 'none', has_token: false },
    last_error: undefined,
  },
  {
    name: 'github-mcp',
    transport: 'stdio',
    command: 'npx',
    args: ['-y', '@modelcontextprotocol/server-github'],
    enabled: false,
    tools_count: 12,
    status: 'unhealthy',
    connection_status: 'error',
    auth: { auth_type: 'none', has_token: false },
    last_error: 'Process exited with code 1',
  },
];

// ---------------------------------------------------------------------------
// Projects
// ---------------------------------------------------------------------------

export const MOCK_PROJECTS: import('@/types').GitlabProject[] = [
  {
    id: 'proj-001',
    name: 'user-service',
    url: 'https://gitlab.example.com/team/user-service.git',
    branch: 'main',
    description: '用户认证和权限管理微服务，支持 OAuth2、JWT、CAS 等多种认证方式',
    is_cloned: true,
    clone_path: '/workspace/user-service',
    last_sync_at: new Date(Date.now() - 1000 * 60 * 30).toISOString(),
    created_at: new Date(Date.now() - 1000 * 60 * 60 * 24 * 7).toISOString(),
  },
  {
    id: 'proj-002',
    name: 'payment-api',
    url: 'https://gitlab.example.com/team/payment-api.git',
    branch: 'develop',
    description: '支付网关 API 服务，支持支付宝、微信、银联多通道支付接入',
    is_cloned: true,
    clone_path: '/workspace/payment-api',
    last_sync_at: new Date(Date.now() - 1000 * 60 * 60 * 2).toISOString(),
    created_at: new Date(Date.now() - 1000 * 60 * 60 * 24 * 3).toISOString(),
  },
  {
    id: 'proj-003',
    name: 'inventory-ms',
    url: 'https://gitlab.example.com/team/inventory-ms.git',
    branch: 'main',
    description: '库存管理微服务，处理商品的入库、出库和库存预警',
    is_cloned: false,
    created_at: new Date(Date.now() - 1000 * 60 * 60 * 24).toISOString(),
  },
];

export interface MockFileNode {
  name: string;
  type: 'file' | 'dir';
  path: string;
  size?: number;
  children?: MockFileNode[];
  lang?: string;
}

export const MOCK_FILE_TREES: Record<string, MockFileNode[]> = {
  'proj-001': [
    {
      name: 'src', type: 'dir', path: 'src',
      children: [
        {
          name: 'controllers', type: 'dir', path: 'src/controllers',
          children: [
            { name: 'AuthController.ts', type: 'file', path: 'src/controllers/AuthController.ts', size: 4230, lang: 'typescript' },
            { name: 'UserController.ts', type: 'file', path: 'src/controllers/UserController.ts', size: 3810, lang: 'typescript' },
          ],
        },
        {
          name: 'services', type: 'dir', path: 'src/services',
          children: [
            { name: 'OAuth2Service.ts', type: 'file', path: 'src/services/OAuth2Service.ts', size: 8420, lang: 'typescript' },
            { name: 'JWTTokenService.ts', type: 'file', path: 'src/services/JWTTokenService.ts', size: 2960, lang: 'typescript' },
            { name: 'CasService.ts', type: 'file', path: 'src/services/CasService.ts', size: 5140, lang: 'typescript' },
          ],
        },
        {
          name: 'models', type: 'dir', path: 'src/models',
          children: [
            { name: 'User.ts', type: 'file', path: 'src/models/User.ts', size: 1200, lang: 'typescript' },
            { name: 'Role.ts', type: 'file', path: 'src/models/Role.ts', size: 880, lang: 'typescript' },
          ],
        },
        {
          name: 'middleware', type: 'dir', path: 'src/middleware',
          children: [
            { name: 'AuthMiddleware.ts', type: 'file', path: 'src/middleware/AuthMiddleware.ts', size: 2340, lang: 'typescript' },
          ],
        },
        { name: 'index.ts', type: 'file', path: 'src/index.ts', size: 640, lang: 'typescript' },
        { name: 'app.ts', type: 'file', path: 'src/app.ts', size: 1120, lang: 'typescript' },
      ],
    },
    {
      name: 'config', type: 'dir', path: 'config',
      children: [
        { name: 'oauth2.json', type: 'file', path: 'config/oauth2.json', size: 820, lang: 'json' },
        { name: 'jwt.json', type: 'file', path: 'config/jwt.json', size: 540, lang: 'json' },
      ],
    },
    { name: 'package.json', type: 'file', path: 'package.json', size: 1240, lang: 'json' },
    { name: 'tsconfig.json', type: 'file', path: 'tsconfig.json', size: 430, lang: 'json' },
    { name: '.env.example', type: 'file', path: '.env.example', size: 310 },
    { name: 'README.md', type: 'file', path: 'README.md', size: 2100, lang: 'markdown' },
    { name: 'Dockerfile', type: 'file', path: 'Dockerfile', size: 680 },
  ],
  'proj-002': [
    {
      name: 'src', type: 'dir', path: 'src',
      children: [
        {
          name: 'routes', type: 'dir', path: 'src/routes',
          children: [
            { name: 'alipay.ts', type: 'file', path: 'src/routes/alipay.ts', size: 5200, lang: 'typescript' },
            { name: 'wechat.ts', type: 'file', path: 'src/routes/wechat.ts', size: 4800, lang: 'typescript' },
            { name: 'unionpay.ts', type: 'file', path: 'src/routes/unionpay.ts', size: 3900, lang: 'typescript' },
          ],
        },
        {
          name: 'services', type: 'dir', path: 'src/services',
          children: [
            { name: 'PaymentService.ts', type: 'file', path: 'src/services/PaymentService.ts', size: 9600, lang: 'typescript' },
            { name: 'RefundService.ts', type: 'file', path: 'src/services/RefundService.ts', size: 4300, lang: 'typescript' },
          ],
        },
        { name: 'app.ts', type: 'file', path: 'src/app.ts', size: 980, lang: 'typescript' },
      ],
    },
    { name: 'package.json', type: 'file', path: 'package.json', size: 1380, lang: 'json' },
    { name: 'README.md', type: 'file', path: 'README.md', size: 1800, lang: 'markdown' },
    { name: 'docker-compose.yml', type: 'file', path: 'docker-compose.yml', size: 740 },
  ],
};

// ---------------------------------------------------------------------------
// Skills (local page variant — realistic English tags)
// ---------------------------------------------------------------------------

export const MOCK_SKILLS_LOCAL: SkillInfo[] = [
  {
    id: 'local-001',
    name: 'plan',
    description: '制定详细的项目开发计划，包含任务拆解、优先级排序和进度追踪策略',
    path: '/home/user/.claude/skills/plan',
    source: 'uploaded',
    tags: ['planning', 'task-management'],
    enabled: true,
    version: '1.0.0',
    commands_count: 3,
    created_at: new Date(Date.now() - 1000 * 60 * 60 * 24 * 20).toISOString(),
    updated_at: new Date(Date.now() - 1000 * 60 * 60 * 24 * 20).toISOString(),
  },
  {
    id: 'local-002',
    name: 'trace',
    description: '追踪代码执行路径，分析函数调用链和变量状态变化',
    path: '/home/user/.claude/skills/trace',
    source: 'uploaded',
    tags: ['debugging', 'analysis'],
    enabled: true,
    version: '1.0.0',
    commands_count: 5,
    created_at: new Date(Date.now() - 1000 * 60 * 60 * 24 * 18).toISOString(),
    updated_at: new Date(Date.now() - 1000 * 60 * 60 * 24 * 18).toISOString(),
  },
  {
    id: 'local-003',
    name: 'test',
    description: '自动生成单元测试和集成测试用例，支持主流测试框架',
    path: '/home/user/.claude/skills/test',
    source: 'uploaded',
    tags: ['testing', 'quality'],
    enabled: false,
    version: '1.0.0',
    commands_count: 4,
    created_at: new Date(Date.now() - 1000 * 60 * 60 * 24 * 15).toISOString(),
    updated_at: new Date(Date.now() - 1000 * 60 * 60 * 24 * 15).toISOString(),
  },
  {
    id: 'local-004',
    name: 'hud',
    description: '实时显示开发状态面板，包括代码覆盖率、构建状态和任务进度',
    path: '/home/user/.agents/skills/hud',
    source: 'uploaded',
    tags: ['visualization', 'status'],
    enabled: true,
    version: '1.0.0',
    commands_count: 2,
    created_at: new Date(Date.now() - 1000 * 60 * 60 * 24 * 10).toISOString(),
    updated_at: new Date(Date.now() - 1000 * 60 * 60 * 24 * 10).toISOString(),
  },
  {
    id: 'local-005',
    name: 'doc',
    description: '自动生成代码文档，支持 Markdown 和 OpenAPI 格式输出',
    path: '/home/user/.claude/skills/doc',
    source: 'uploaded',
    tags: ['documentation', 'automation'],
    enabled: true,
    version: '1.0.0',
    commands_count: 3,
    created_at: new Date(Date.now() - 1000 * 60 * 60 * 24 * 5).toISOString(),
    updated_at: new Date(Date.now() - 1000 * 60 * 60 * 24 * 5).toISOString(),
  },
];

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

export const MOCK_CONFIG: ConfigOverview = {
  configs: [
    {
      path: '/workspace/.claude/settings.json',
      source: 'workspace',
      content: {
        model: 'claude-opus-4-6',
        max_tokens: 8192,
        temperature: 0.7,
      },
    },
  ],
  current_model: 'claude-opus-4-6',
  active_plugins: ['git-manager', 'docker-helper'],
  active_mcp_servers: ['filesystem-mcp', 'weather-mcp'],
};
