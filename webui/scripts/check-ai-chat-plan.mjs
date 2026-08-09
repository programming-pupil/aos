#!/usr/bin/env node
import { existsSync, readFileSync } from 'node:fs';
import { join, resolve } from 'node:path';

const webuiRoot = process.cwd();
const repoRoot = resolve(webuiRoot, '..');
const failures = [];

function assert(condition, message) {
  if (!condition) failures.push(message);
}

function readWebui(relativePath) {
  return readFileSync(join(webuiRoot, relativePath), 'utf8');
}

function readRepo(relativePath) {
  return readFileSync(join(repoRoot, relativePath), 'utf8');
}

const requiredFiles = [
  'src/components/chat/ChatCore.tsx',
  'src/components/chat/MessageBubble.tsx',
  'src/api/agent.ts',
  'src/api/queryKeys.ts',
  'src/locales/en-US.json',
  'src/locales/zh-CN.json',
];

const requiredRepoFiles = [
  'rust/crates/web-server/src/routes/chat_capabilities.rs',
  'rust/crates/web-server/src/routes/search_orchestrator_runtime.rs',
  'rust/crates/web-server/src/routes/agent/agent_chat_turn_engine.rs',
  'rust/crates/web-server/src/routes/agent/agent_stream_session.rs',
  'rust/crates/web-server/src/routes/agent/agent_pm_task_api.rs',
  'rust/crates/agent-gateway/src/runtime_builder.rs',
  'rust/crates/tools/src/lib.rs',
];

for (const file of requiredFiles) {
  assert(existsSync(join(webuiRoot, file)), `missing webui file: ${file}`);
}

for (const file of requiredRepoFiles) {
  assert(existsSync(join(repoRoot, file)), `missing repo file: ${file}`);
}

if (failures.length === 0) {
  const chatCore = readWebui('src/components/chat/ChatCore.tsx');
  const messageBubble = readWebui('src/components/chat/MessageBubble.tsx');
  const agentApi = readWebui('src/api/agent.ts');
  const queryKeys = readWebui('src/api/queryKeys.ts');
  const en = JSON.parse(readWebui('src/locales/en-US.json'));
  const zh = JSON.parse(readWebui('src/locales/zh-CN.json'));
  const capabilities = readRepo('rust/crates/web-server/src/routes/chat_capabilities.rs');
  const searchOrchestrator = readRepo('rust/crates/web-server/src/routes/search_orchestrator_runtime.rs');
  const turnEngine = readRepo('rust/crates/web-server/src/routes/agent/agent_chat_turn_engine.rs');
  const streamSession = readRepo('rust/crates/web-server/src/routes/agent/agent_stream_session.rs');
  const pmTaskApi = readRepo('rust/crates/web-server/src/routes/agent/agent_pm_task_api.rs');
  const runtimeBuilder = readRepo('rust/crates/agent-gateway/src/runtime_builder.rs');
  const tools = readRepo('rust/crates/tools/src/lib.rs');

  assert(agentApi.includes('interface ChatTurnOptions'), 'agent API missing ChatTurnOptions');
  assert(agentApi.includes('searchEnabled?: boolean'), 'ChatTurnOptions missing searchEnabled');
  assert(agentApi.includes("mode: 'none' | 'selected' | 'all_attached'"), 'ChatTurnOptions missing file context modes');
  assert(agentApi.includes('strictGrounding?: boolean'), 'ChatTurnOptions missing strictGrounding');
  assert(agentApi.includes('getChatCapabilities'), 'agent API missing getChatCapabilities');
  assert(agentApi.includes("'/chat/capabilities'"), 'agent API does not call /chat/capabilities');
  assert(agentApi.includes('currentProvider?: string | null'), 'ChatCapabilityResponse missing currentProvider');

  assert(queryKeys.includes("capabilities: (model?: string)"), 'chat capabilities query key should include model');

  assert(chatCore.includes('const [searchMode, setSearchMode] = useState<"on" | "off">("off")'), 'web search mode should default off');
  assert(chatCore.includes('sessionSource === "chat" && !!searchCapability?.enabled'), 'web search availability must be capability-gated');
  assert(chatCore.includes('if (!webSearchAvailable && searchMode === "on")'), 'web search should reset off when provider is unavailable');
  assert(chatCore.includes('disabled={isStreaming}'), 'web search control must be disabled while a turn is running');
  assert(chatCore.includes('searchMode: webSearchAvailable ? searchMode : "off"'), 'turnOptions must never select search without a provider');
  assert(chatCore.includes('searchEnabled: webSearchAvailable && searchMode === "on"'), 'turnOptions must never enable search without a provider');
  assert(chatCore.includes('fileContext: {'), 'ChatCore must send fileContext turn options');
  assert(chatCore.includes('strictGrounding: false'), 'normal attached-file turns should not silently enable strict grounding');
  assert(chatCore.includes('chat.fileContextActive'), 'file context chip missing');
  assert(!/quick\s*\/\s*deep/i.test(chatCore), 'ChatCore should not expose quick/deep mode labels');

  for (const forbidden of ['快速问答', '深度问答', 'Quick Mode', 'Deep Mode']) {
    assert(!chatCore.includes(forbidden), `ChatCore exposes removed mode label: ${forbidden}`);
  }

  assert(chatCore.includes('function mapHistoryMessages'), 'history mapper missing');
  assert(chatCore.includes('if (msg.role === "tool") continue'), 'tool role messages must not render as standalone history messages');
  assert(chatCore.includes('queueOrAttachToolCalls'), 'historical tool calls must be attached to assistant messages');
  assert(chatCore.includes('withAssistantArtifacts'), 'assistant artifact merge helper missing');
  assert(chatCore.includes('evidenceSourcesFromTurn'), 'assistant response should collect web/file evidence sources');

  assert(messageBubble.includes('function MessageInsightFooter'), 'MessageBubble missing message insight footer');
  assert(messageBubble.includes('function SourcesSection'), 'MessageBubble missing evidence source rendering');
  assert(messageBubble.includes('sources={evidenceSources}'), 'sources must render under assistant messages');
  assert(messageBubble.includes('chat.sources'), 'source panel label missing');

  assert(capabilities.includes('.route("/capabilities"'), 'backend missing /chat/capabilities route');
  assert(capabilities.includes('.route("/search-providers"'), 'backend missing /chat/search-providers route');
  assert(capabilities.includes('current_provider'), 'backend capabilities missing current provider');
  assert(capabilities.includes('user_selectable: false'), 'reasoning mode must not be user-selectable');
  assert(capabilities.includes('provider.provider_type.clone()'), 'backend capabilities must expose configured provider types dynamically');
  for (const provider of ['brave', 'tavily', 'serper', 'exa', 'demo_search']) {
    assert(searchOrchestrator.includes(`"${provider}"`), `search orchestrator missing provider ${provider}`);
  }
  assert(capabilities.includes('mcp_search'), 'backend capabilities missing MCP search');

  assert(streamSession.includes('ChatTurnOptions'), 'stream session must parse ChatTurnOptions');
  assert(streamSession.includes('plan_chat_turn(ChatTurnEngineInput'), 'stream session must use the shared turn planner');
  assert(turnEngine.includes('resolve_effective_chat_search_mode'), 'turn planner must validate search availability');
  assert(turnEngine.includes('"WebSearch".to_string()'), 'search-off turns must block WebSearch');
  assert(turnEngine.includes('"WebFetch".to_string()'), 'search-off turns must block WebFetch');
  assert(turnEngine.includes('CHAT_BLOCK_MCP_SEARCH_TOOLS'), 'search-off turns must block MCP search/browser/fetch tools');
  assert(turnEngine.includes('chat_file_grounding_instruction'), 'turn planner missing file grounding instruction');
  assert(turnEngine.includes('Strict file grounding is enabled'), 'strict grounding instruction missing');
  assert(turnEngine.includes('chat_reasoning_budget'), 'adaptive internal reasoning budget missing');

  assert(runtimeBuilder.includes('CHAT_BLOCK_MCP_SEARCH_TOOLS'), 'runtime missing MCP search block sentinel');
  assert(runtimeBuilder.includes('is_mcp_search_like_tool'), 'runtime missing MCP search-like classifier');
  for (const marker of ['search', 'browser', 'browse', 'fetch', 'web', 'url', 'http']) {
    assert(runtimeBuilder.includes(`"${marker}"`), `MCP search classifier missing marker ${marker}`);
  }

  assert(pmTaskApi.includes('citationId'), 'document context diagnostics must include citationId');
  assert(pmTaskApi.includes('[file:{}]'), 'document context must instruct file citations');
  assert(pmTaskApi.includes('lineStart'), 'document context diagnostics missing lineStart');
  assert(pmTaskApi.includes('lineEnd'), 'document context diagnostics missing lineEnd');

  assert(tools.includes('AOSD_DEMO_SEARCH_ENABLED'), 'tools missing explicit demo search provider gate');
  assert(tools.includes('demo_search provider is disabled'), 'demo search must fail explicitly when disabled');
  assert(
    tools.includes('AOS built-in web search does not require an API key'),
    'built-in web search must remain available without provider keys',
  );

  for (const key of [
    ['chat', 'webSearch'],
    ['chat', 'webSearchTooltip'],
    ['chat', 'webSearchUnavailable'],
    ['chat', 'fileContextActive'],
    ['chat', 'sources'],
  ]) {
    const [scope, name] = key;
    assert(en?.[scope]?.[name] !== undefined, `en-US missing ${scope}.${name}`);
    assert(zh?.[scope]?.[name] !== undefined, `zh-CN missing ${scope}.${name}`);
  }
}

if (failures.length > 0) {
  console.error('AI Chat plan check failed:');
  for (const failure of failures) {
    console.error(`- ${failure}`);
  }
  process.exit(1);
}

console.log('AI Chat plan check passed');
