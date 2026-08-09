/* =============================================================================
   AOS — Mock Data for UI Development
   ============================================================================= */

export interface MockPipelineStage {
  id: string;
  name: string;
  icon: string;
  status: 'pending' | 'running' | 'success' | 'failed' | 'skipped';
  duration?: number;
  steps: MockPipelineStep[];
}

export interface MockPipelineStep {
  id: string;
  name: string;
  status: 'pending' | 'running' | 'success' | 'failed';
  duration?: number;
  log?: string[];
}

// ── Mock Pipeline Stages ────────────────────────────────────────────────────────

export const MOCK_PIPELINE_STAGES: MockPipelineStage[] = [
  {
    id: 'clone',
    name: 'Clone Repository',
    icon: '📦',
    status: 'success',
    duration: 3,
    steps: [
      { id: 'git-clone', name: 'git clone', status: 'success', duration: 2 },
      { id: 'git-checkout', name: 'git checkout branch', status: 'success', duration: 1 },
    ],
  },
  {
    id: 'analyze',
    name: 'Code Analysis',
    icon: '🔍',
    status: 'success',
    duration: 47,
    steps: [
      { id: 'analyze-user', name: 'Analyze user.ts', status: 'success', duration: 12 },
      { id: 'analyze-auth-routes', name: 'Analyze auth.ts routes', status: 'success', duration: 18 },
      { id: 'analyze-middleware', name: 'Analyze middleware/auth.ts', status: 'success', duration: 17 },
    ],
  },
  {
    id: 'write',
    name: 'Write Code',
    icon: '✏️',
    status: 'success',
    duration: 62,
    steps: [
      { id: 'modify-user', name: 'Modify models/user.ts', status: 'success', duration: 15 },
      { id: 'create-oauth2', name: 'Create services/oauth2.ts', status: 'success', duration: 25 },
      { id: 'modify-auth', name: 'Modify routes/auth.ts', status: 'success', duration: 22 },
    ],
  },
  {
    id: 'build',
    name: 'Build & Verify',
    icon: '🔨',
    status: 'success',
    duration: 28,
    steps: [
      { id: 'npm-build', name: 'npm run build', status: 'success', duration: 28 },
    ],
  },
  {
    id: 'test',
    name: 'Automated Testing',
    icon: '🧪',
    status: 'running',
    duration: undefined,
    steps: [
      { id: 'test-user', name: 'Unit test user.ts', status: 'running', duration: undefined },
      { id: 'test-oauth2', name: 'Unit test oauth2.ts', status: 'pending', duration: undefined },
      { id: 'test-auth', name: 'Integration test auth routes', status: 'pending', duration: undefined },
    ],
  },
  {
    id: 'commit',
    name: 'Git Commit',
    icon: '✅',
    status: 'pending',
    steps: [
      { id: 'git-add', name: 'git add', status: 'pending' },
      { id: 'git-commit', name: 'git commit', status: 'pending' },
      { id: 'git-push', name: 'git push', status: 'pending' },
    ],
  },
  {
    id: 'deploy',
    name: 'Trigger Deployment',
    icon: '🚀',
    status: 'pending',
    steps: [
      { id: 'trigger-ci', name: 'Trigger CI/CD Pipeline', status: 'pending' },
    ],
  },
];

// ── Mock Pipeline Logs ──────────────────────────────────────────────────────────

export const MOCK_PIPELINE_LOGS: Array<{ time: string; level: 'info' | 'success' | 'warn' | 'error'; msg: string }> = [
  { time: '14:32:01', level: 'info', msg: '🚀 开始 Stage: 拉取代码' },
  { time: '14:32:01', level: 'info', msg: '📦 git clone gitlab.company.com/backend/user-service' },
  { time: '14:32:02', level: 'success', msg: '✓ 代码仓库已克隆到 /workspace/user-service' },
  { time: '14:32:03', level: 'info', msg: '🔀 切换到分支: feature/oauth2-auth' },
  { time: '14:32:03', level: 'success', msg: '✅ Stage 1 完成 (3s)' },
  { time: '14:32:04', level: 'info', msg: '🚀 开始 Stage: 代码分析' },
  { time: '14:32:04', level: 'info', msg: '🔍 分析 user-service/src/models/user.ts' },
  { time: '14:32:05', level: 'info', msg: '🔍 分析 user-service/src/routes/auth.ts' },
  { time: '14:32:06', level: 'info', msg: '🔍 分析 user-service/src/middleware/auth.ts' },
  { time: '14:32:07', level: 'success', msg: '✓ 代码分析完成，发现 3 个待修改文件' },
  { time: '14:32:08', level: 'success', msg: '✅ Stage 2 完成 (47s)' },
  { time: '14:32:09', level: 'info', msg: '🚀 开始 Stage: 编写代码' },
  { time: '14:32:10', level: 'info', msg: '✏️ 修改 src/models/user.ts (+18 -3)' },
  { time: '14:32:15', level: 'info', msg: '✏️ 新建 src/services/oauth2.ts (+180)' },
  { time: '14:32:22', level: 'info', msg: '✏️ 修改 src/routes/auth.ts (+56 -12)' },
  { time: '14:32:30', level: 'success', msg: '✓ 5 个文件已修改/创建' },
  { time: '14:32:31', level: 'success', msg: '✅ Stage 3 完成 (62s)' },
  { time: '14:32:32', level: 'info', msg: '🚀 开始 Stage: 构建验证' },
  { time: '14:32:32', level: 'info', msg: '🔨 运行 npm run build...' },
  { time: '14:32:57', level: 'success', msg: '✓ 构建成功 (0 errors, 3 warnings)' },
  { time: '14:32:58', level: 'success', msg: '✅ Stage 4 完成 (28s)' },
  { time: '14:32:59', level: 'info', msg: '🚀 开始 Stage: 自动化测试' },
  { time: '14:32:59', level: 'info', msg: '🧪 运行单元测试...' },
  { time: '14:33:01', level: 'success', msg: '✓ user.ts 单元测试通过 (12/12)' },
  { time: '14:33:05', level: 'info', msg: '🧪 正在运行 oauth2.ts 单元测试...' },
];
