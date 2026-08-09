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

for (const file of [
  'src/components/chat/PmTaskQueuePanel.tsx',
  'src/components/chat/PmFinalDeliveryPanel.tsx',
  'src/components/chat/PmInlineEvidencePanel.tsx',
  'src/components/chat/PmInlineNarrativePanel.tsx',
  'src/components/chat/PmExecutionDrawer.tsx',
  'src/components/chat/pmTerminalReconciler.ts',
  'src/components/chat/ChatCore.tsx',
  'src/api/pmStream.ts',
  'src/pages/OperationsCopilot.tsx',
]) {
  assert(existsSync(join(webuiRoot, file)), `missing webui file: ${file}`);
}

for (const file of [
  'rust/crates/web-server/src/routes/agent/agent_pm_task_runtime.rs',
]) {
  assert(existsSync(join(repoRoot, file)), `missing repo file: ${file}`);
}

if (failures.length === 0) {
  const chatCore = readWebui('src/components/chat/ChatCore.tsx');
  const queuePanel = readWebui('src/components/chat/PmTaskQueuePanel.tsx');
  const finalDeliveryPanel = readWebui('src/components/chat/PmFinalDeliveryPanel.tsx');
  const inlineEvidencePanel = readWebui('src/components/chat/PmInlineEvidencePanel.tsx');
  const inlineNarrativePanel = readWebui('src/components/chat/PmInlineNarrativePanel.tsx');
  const executionDrawer = readWebui('src/components/chat/PmExecutionDrawer.tsx');
  const reconciler = readWebui('src/components/chat/pmTerminalReconciler.ts');
  const pmStream = readWebui('src/api/pmStream.ts');
  const pmRuntime = readRepo('rust/crates/web-server/src/routes/agent/agent_pm_task_runtime.rs');

  assert(chatCore.includes('PmTaskQueuePanel'), 'ChatCore should compose PmTaskQueuePanel');
  assert(chatCore.includes('PmFinalDeliveryPanel'), 'ChatCore should compose PmFinalDeliveryPanel');
  assert(chatCore.includes('PmInlineEvidencePanel'), 'ChatCore should compose PmInlineEvidencePanel');
  assert(chatCore.includes('PmInlineNarrativePanel'), 'ChatCore should compose PmInlineNarrativePanel');
  assert(chatCore.includes('PmExecutionDrawer'), 'ChatCore should compose PmExecutionDrawer');
  assert(!chatCore.includes('operations.pmQueueMore", {'), 'ChatCore should not inline PM queue item rendering');
  assert(!chatCore.includes('false &&'), 'ChatCore should not keep permanently disabled PM JSX');
  assert(!chatCore.includes('<Card'), 'ChatCore should not inline PM research status cards');
  assert(!chatCore.includes('operations.pmSubtaskRuntime'), 'ChatCore should not inline PM subtask status rendering');
  assert(!chatCore.includes('operations.pmEvidenceTree'), 'ChatCore should not inline PM evidence tree rendering');
  assert(chatCore.includes('reconcilePmHistoryTerminalAssistant'), 'ChatCore should use PM terminal reconciler');
  assert(finalDeliveryPanel.includes('operations.pmFinalDeliveryHighlights'), 'final delivery panel missing highlights rendering');
  assert(inlineEvidencePanel.includes('operations.pmClaimEvidenceMap'), 'inline evidence panel missing claim-evidence rendering');
  assert(inlineNarrativePanel.includes('operations.pmInlinePhaseTrail'), 'inline narrative panel missing stage trail rendering');
  assert(executionDrawer.includes('operations.pmStageDetails'), 'execution drawer missing stage details rendering');
  assert(reconciler.includes('compactPmDuplicateTaskReplies'), 'PM reconciler missing duplicate task compaction');
  assert(reconciler.includes('userMessageId'), 'PM reconciler should support user-message anchoring');
  assert(queuePanel.includes('operations.pmCancelCooperativeHint'), 'queue panel missing cooperative cancel tooltip');
  assert(queuePanel.includes('operations.pmCancelCooperativeInline'), 'queue panel missing inline cancelling explanation');
  assert(pmStream.includes('isPmResearchTaskTerminalEvent(payload) && !doneEmitted'), 'PM SSE terminal done must be idempotent');
  assert(pmRuntime.includes('pm_history_any_shadow_assistant_after_user'), 'backend missing shadow assistant history guard');
  assert(pmRuntime.includes('reconcile_pm_task_history_turns_drops_shadow_assistants_after_queue_rebuild'), 'backend missing PM queue history regression test');
}

if (failures.length > 0) {
  console.error('PM Assistant check failed:');
  for (const failure of failures) {
    console.error(`- ${failure}`);
  }
  process.exit(1);
}

console.log('PM Assistant check passed');
