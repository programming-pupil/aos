import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';

const root = process.cwd();
const srcDir = path.join(root, 'src');
const localeFiles = ['zh-CN', 'en-US'].map((locale) => ({
  locale,
  file: path.join(srcDir, 'locales', `${locale}.json`),
}));

const sourceFiles = execFileSync('rg', ['--files', srcDir], { encoding: 'utf8' })
  .trim()
  .split('\n')
  .filter((file) => /\.(ts|tsx|js|jsx)$/.test(file))
  .filter((file) => !file.includes('/locales/'));

const keys = new Set();

for (const file of sourceFiles) {
  const source = fs.readFileSync(file, 'utf8');
  for (const match of source.matchAll(/\bt\(\s*['"]([^'"`$\n]+)['"]/g)) {
    keys.add(match[1]);
  }
  for (const match of source.matchAll(/\bi18nKey\s*=\s*['"]([^'"]+)['"]/g)) {
    keys.add(match[1]);
  }
}

function walkFiles(dir, predicate, files = []) {
  if (!fs.existsSync(dir)) return files;
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const entryPath = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      walkFiles(entryPath, predicate, files);
    } else if (entry.isFile() && predicate(entryPath)) {
      files.push(entryPath);
    }
  }
  return files;
}

function collectBackendNl2sqlStages() {
  const nl2sqlDir = path.join(root, '..', 'rust', 'crates', 'web-server', 'src', 'routes', 'nl2sql');
  const stages = new Set();
  const stagePatterns = [
    /\bemit_[a-z_]*stage\(\s*"([a-z0-9_]+)"/gs,
    /\.publish_stage\(\s*[^,]+,\s*"([a-z0-9_]+)"/gs,
    /stage:\s*Some\("([a-z0-9_]+)"\.to_string\(\)\)/g,
  ];
  for (const file of walkFiles(nl2sqlDir, (entryPath) => entryPath.endsWith('.rs'))) {
    const source = fs.readFileSync(file, 'utf8');
    for (const pattern of stagePatterns) {
      for (const match of source.matchAll(pattern)) {
        stages.add(match[1]);
      }
    }
  }
  return [...stages].sort();
}

const dynamicKeys = {
  'skills.source': ['uploaded', 'builtin', 'market', 'marketplace', 'repository'],
  'notifications.level': ['info', 'success', 'warning', 'error'],
  'dashboard.alerts.type': [
    'daily_budget',
    'monthly_budget',
    'per_key_limit',
    'system',
    'token',
    'cost',
    'latency',
    'error',
    'hook',
    'bot',
    'config',
  ],
  'dashboard.agentOps.health.attention': ['title', 'description', 'tag'],
  'dashboard.agentOps.health.busy': ['title', 'description', 'tag'],
  'dashboard.agentOps.health.healthy': ['title', 'description', 'tag'],
  'dashboard.agentOps.health.unknown': ['title', 'description', 'tag'],
  'hooks.eventType': [
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
  ],
  'hooks.language': ['python', 'shell'],
  'botAgents.inboundModes': ['auto', 'stream', 'socket', 'polling', 'webhook'],
  'botAgents.directions': ['inbound', 'outbound'],
  'botAgents.statuses': [
    'queued',
    'received',
    'processing',
    'replied',
    'sent',
    'failed',
    'ignored',
    'idle',
    'connecting',
    'connected',
    'polling',
    'webhook_only',
    'unsupported',
    'error',
    'running',
    'enabled',
    'disabled',
    'active',
    'inactive',
    'ok',
    'pending',
    'unknown',
  ],
  'botAgents.queueStatuses': ['none', 'queued', 'claimed', 'succeeded', 'failed', 'dead'],
  'chat.adversarialStatus': ['pending', 'running', 'completed', 'failed', 'cancelled'],
  'nl2sql.dbType': ['mysql', 'postgres', 'postgresql', 'clickhouse', 'trino'],
  'nl2sql.templates': ['sales', 'finance', 'operations', 'users', 'inventory', 'conversion'],
  'nl2sql.routeStages': [
    'ai_confirming',
    'cache_lookup',
    'clarification_gate',
    'done',
    'domain',
    'domain_classifying',
    'embedding',
    'execute_sql',
    'explain_preflight',
    'explain_sql',
    'failed',
    'fallback',
    'federated_workspace',
    'generate_sql',
    'global_search',
    'llm',
    'llm_routing',
    'load_context',
    'load_schema',
    'manual_continue',
    'persist_result',
    'policy_enforcement',
    'query_understanding',
    'ready',
    'request_validation',
    'route_selected',
    'rrfs_ranking',
    'search_candidates',
    'semantic_review',
    'sql_knowledge_probe',
    'start',
    'stats_boost',
    'text_match',
    'vector_matching',
  ],
  'management.timePatterns.resolvedTypeOptions': ['date_range', 'date', 'relative', 'period', 'none'],
  'management.maskingRules': [
    'maskTypeRedact',
    'maskTypeNull',
    'maskTypeConstant',
    'maskTypeHash',
    'maskTypeTokenize',
    'maskTypePartial',
  ],
  'rd.statuses': [
    'draft',
    'queued',
    'running',
    'waiting_approval',
    'completed',
    'failed',
    'cancelled',
    'passed',
    'timeout',
    'stale',
    'skipped',
  ],
  'rd.modes': ['ask', 'modify', 'explain', 'review'],
  'rd.riskLevels': ['low', 'medium', 'high', 'critical'],
  'rd.contextProfiles': ['overview', 'focused_ask', 'explain', 'modify', 'review', 'deep_review'],
  'rd.contextDepths': ['shallow', 'standard', 'deep'],
  'rd.retrievalSources': [
    'embedding_context',
    'embedding_summary',
    'embedding_symbol',
    'embedding_import',
    'embedding_task',
    'explicit_file',
    'file_summary',
    'symbol_index',
    'import_index',
    'dependency_graph',
    'retrieval_context',
  ],
  'rd.tokenStages': ['context_plan_llm', 'runtime_usage', 'runtime', 'summary'],
  'rd.planTaskStatuses': [
    'pending',
    'queued',
    'running',
    'waiting_approval',
    'completed',
    'failed',
    'cancelled',
    'skipped',
  ],
  'dataAttribution': [
    'status_completed',
    'status_partial',
    'status_no_data',
    'status_clarification_needed',
  ],
};

const backendNl2sqlStages = collectBackendNl2sqlStages();
if (backendNl2sqlStages.length > 0) {
  dynamicKeys['nl2sql.routeStages'] = [
    ...new Set([...dynamicKeys['nl2sql.routeStages'], ...backendNl2sqlStages]),
  ].sort();
}

for (const [prefix, values] of Object.entries(dynamicKeys)) {
  for (const value of values) {
    keys.add(`${prefix}.${value}`);
  }
}

function hasKey(obj, dottedKey) {
  let current = obj;
  for (const part of dottedKey.split('.')) {
    current = current?.[part];
    if (current === undefined) return false;
  }
  return true;
}

let hasMissing = false;
for (const { locale, file } of localeFiles) {
  const messages = JSON.parse(fs.readFileSync(file, 'utf8'));
  const missing = [...keys].sort().filter((key) => !hasKey(messages, key));
  console.log(`${locale}: ${missing.length} missing`);
  if (missing.length > 0) {
    hasMissing = true;
    for (const key of missing) {
      console.log(`  ${key}`);
    }
  }
}

if (hasMissing) {
  process.exitCode = 1;
}
