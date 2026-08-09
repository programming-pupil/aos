#!/usr/bin/env node
import { existsSync, readFileSync } from 'node:fs';
import { join, resolve } from 'node:path';

const repoRoot = resolve(process.cwd(), '..');
const webuiRoot = process.cwd();

const requiredFiles = [
  'src/pages/RdStudio.tsx',
  'src/pages/rdStudio/AgentTimeline.tsx',
  'src/pages/rdStudio/CodeChatPanel.tsx',
  'src/pages/rdStudio/CodeEditorPanel.tsx',
  'src/pages/rdStudio/CodeIntelPopover.tsx',
  'src/pages/rdStudio/CommandPalette.tsx',
  'src/pages/rdStudio/ContextCacheUsage.tsx',
  'src/pages/rdStudio/DefinitionCandidates.tsx',
  'src/pages/rdStudio/DiffInspector.tsx',
  'src/pages/rdStudio/FilePreview.tsx',
  'src/pages/rdStudio/LayoutPrimitives.tsx',
  'src/pages/rdStudio/PlanWorkbench.tsx',
  'src/pages/rdStudio/PreviewLogsPanel.tsx',
  'src/pages/rdStudio/PreviewPanel.tsx',
  'src/pages/rdStudio/QuickOpenPalette.tsx',
  'src/pages/rdStudio/PlanStageStepper.tsx',
  'src/pages/rdStudio/SpecEditor.tsx',
  'src/pages/rdStudio/TaskItemBoard.tsx',
  'src/pages/rdStudio/TerminalPanel.tsx',
  'src/pages/rdStudio/TestInspector.tsx',
  'src/pages/rdStudio/CodeWorkbench.tsx',
  'src/pages/rdStudio/WorkspaceSidebar.tsx',
  'src/pages/rdStudio/apiMapper.ts',
  'src/pages/rdStudio/reporting.ts',
  'src/pages/rdStudio/runtimeTimeline.ts',
  'src/pages/rdStudio/types.ts',
  'src/pages/rdStudio/utils.ts',
  'src/pages/rdStudio/hooks.ts',
  'src/api/rd.ts',
];

const requiredRepoFiles = [
  'rust/crates/web-server/sqlite-migrations/0001_baseline.sql',
  'rust/crates/web-server/src/routes/rd/code_intel.rs',
  'rust/crates/web-server/src/routes/rd/prompts.rs',
  'rust/crates/web-server/src/routes/rd/preview_sessions.rs',
  'rust/crates/web-server/src/routes/rd/specs.rs',
  'rust/crates/web-server/src/routes/rd/workbench.rs',
  'docs/AOS_CODE_STUDIO.md',
  'docs/AOS_CODE_STUDIO.zh-CN.md',
  'docs/CODE_STUDIO_SMOKE.md',
  'docs/CODE_STUDIO_SMOKE.zh-CN.md',
];

const rdStudioPlanSymbols = [
  'AgentTimeline',
  'CodeChatPanel',
  'DiffInspector',
  'FilePreview',
  'PlanWorkbench',
  'PreviewPanel',
  'PreviewLogsPanel',
  'QuickOpenPalette',
  'TerminalPanel',
  'TestInspector',
  'CodeWorkbench',
  'WorkspaceSidebar',
];

const rdApiSymbols = [
  'taskWorkbench',
  'createSpec',
  'generateSpec',
  'approveSpec',
  'generateDesign',
  'approveDesign',
  'generateTasks',
  'approveTasks',
  'implementSpecTask',
  'implementAllSpecTasks',
  'finalReportSpec',
  'codeIntelStatus',
  'codeIntelQuery',
  'codeIntelRestart',
  'createPreviewSession',
  'stopPreviewSession',
  'previewSessionLogs',
  'recordPreviewConsoleEvent',
];

const backendSpecSymbols = [
  'generate_spec',
  'approve_spec',
  'generate_design',
  'approve_design',
  'generate_tasks',
  'approve_tasks',
  'implement_task',
  'implement_all',
  'final_report',
];

const migrationSymbols = [
  'rd_spec_events',
  'rd_spec_task_links',
  'current_stage',
  'task_items_json',
  'implementation_summary_json',
];

const codeIntelSymbols = [
  'code_intel_status',
  'code_intel_query',
  'try_lsp_code_intel',
  'LSP_MANAGER',
  'LspSessionManager',
  'run_lsp_query_with_session',
  'restart_lsp_sessions_for_repository',
  'workspace/symbol',
  'LSP_LANGUAGE_ORDER',
  'textDocument/definition',
  'Content-Length',
  'source: "lsp"',
  'query_symbol_index',
  'run_rg_repository_search',
  'safe_join(&root, path)',
];

const previewSymbols = [
  'create_preview_session',
  'spawn_preview_runtime_command',
  'run_runtime_command',
  'request_cancel_runtime_session',
  'preview_proxy',
  'inject_preview_capture_script',
  'window.parent.postMessage',
  'screenshot.captured',
  'RuntimeArtifactWriteInput',
  'record_preview_event',
  'CAST(metadata_json AS TEXT) AS metadata_json',
  'try_get::<Option<u64>, _>("port")',
];

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

for (const file of requiredFiles) {
  assert(existsSync(join(webuiRoot, file)), `missing webui file: ${file}`);
}

for (const file of requiredRepoFiles) {
  assert(existsSync(join(repoRoot, file)), `missing repo file: ${file}`);
}

if (failures.length === 0) {
  const rdStudio = readWebui('src/pages/RdStudio.tsx');
  const lineCount = rdStudio.split('\n').length;
  assert(lineCount < 2600, `RdStudio.tsx should stay below 2600 lines after modularization, got ${lineCount}`);
  for (const symbol of rdStudioPlanSymbols) {
    assert(rdStudio.includes(symbol), `RdStudio.tsx does not compose ${symbol}`);
  }
  assert(!rdStudio.includes('function renderTimelineContent'), 'timeline rendering regressed back into RdStudio.tsx');
  assert(!rdStudio.includes('function renderAgentOpsTimelineBridge'), 'AgentOps bridge regressed back into RdStudio.tsx');

  const rdApi = readWebui('src/api/rd.ts');
  for (const symbol of rdApiSymbols) {
    assert(rdApi.includes(symbol), `rdApi missing ${symbol}`);
  }

  const specs = readRepo('rust/crates/web-server/src/routes/rd/specs.rs');
  for (const symbol of backendSpecSymbols) {
    assert(specs.includes(symbol), `rd/specs.rs missing ${symbol}`);
  }

  const migration = readRepo('rust/crates/web-server/sqlite-migrations/0001_baseline.sql');
  for (const symbol of migrationSymbols) {
    assert(migration.includes(symbol), `Plan Mode migration missing ${symbol}`);
  }

  const hybridMigration = migration;
  for (const symbol of ['rd_code_intel_sessions', 'rd_preview_sessions', 'rd_preview_events', '"port" INTEGER']) {
    assert(hybridMigration.includes(symbol), `Hybrid Code Studio migration missing ${symbol}`);
  }

  const codeIntel = readRepo('rust/crates/web-server/src/routes/rd/code_intel.rs');
  for (const symbol of codeIntelSymbols) {
    assert(codeIntel.includes(symbol), `Code Intel backend missing ${symbol}`);
  }

  const previewSessions = readRepo('rust/crates/web-server/src/routes/rd/preview_sessions.rs');
  for (const symbol of previewSymbols) {
    assert(previewSessions.includes(symbol), `Preview backend missing ${symbol}`);
  }
  assert(
    previewSessions.indexOf('spawn_preview_runtime_command') < previewSessions.indexOf('get_preview_session_inner'),
    'preview session creation should return after spawning background runtime command'
  );

  const filePreview = readWebui('src/pages/rdStudio/FilePreview.tsx');
  assert(filePreview.includes('CodeEditorPanel'), 'FilePreview should use Monaco CodeEditorPanel');

  const codeEditor = readWebui('src/pages/rdStudio/CodeEditorPanel.tsx');
  for (const symbol of ['@monaco-editor/react', 'codeIntelQuery', 'definition', 'references']) {
    assert(codeEditor.includes(symbol), `CodeEditorPanel missing ${symbol}`);
  }

  const shortcuts = readWebui('src/pages/rdStudio/hooks.ts');
  for (const symbol of ['onCommandPalette', "key === 'p'", "key === 'o'", "key === 'arrowleft'", "key === 'arrowright'"]) {
    assert(shortcuts.includes(symbol), `Code Studio shortcuts missing ${symbol}`);
  }

  const commandPalette = readWebui('src/pages/rdStudio/CommandPalette.tsx');
  for (const symbol of ['CommandPalette', 'commandQuickOpenFiles', 'commandQuickOpenSymbols', 'commandShowDiff', 'commandShowPreview']) {
    assert(commandPalette.includes(symbol), `CommandPalette missing ${symbol}`);
  }

  const previewPanel = readWebui('src/pages/rdStudio/PreviewPanel.tsx');
  for (const symbol of ['createPreviewSession', 'stopPreviewSession', 'authorizePreviewSession', 'iframe', 'recordPreviewConsoleEvent', 'aos-preview-event', 'previewEvidencePrompt']) {
    assert(previewPanel.includes(symbol), `PreviewPanel missing ${symbol}`);
  }

  const enDoc = readRepo('docs/AOS_CODE_STUDIO.md');
  const zhDoc = readRepo('docs/AOS_CODE_STUDIO.zh-CN.md');
  for (const phrase of ['Vibe Mode', 'Plan Mode', 'Diff-first', 'WatchDog']) {
    assert(enDoc.includes(phrase), `English Code Studio doc missing ${phrase}`);
  }
  for (const phrase of ['Vibe Mode', 'Plan Mode', 'Diff-first', '看门狗']) {
    assert(zhDoc.includes(phrase), `Chinese Code Studio doc missing ${phrase}`);
  }
}

if (failures.length > 0) {
  console.error('Code Studio plan check failed:');
  for (const failure of failures) {
    console.error(`- ${failure}`);
  }
  process.exit(1);
}

console.log('Code Studio plan check passed.');
