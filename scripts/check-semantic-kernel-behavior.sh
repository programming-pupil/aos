#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
DATASET="$REPO_ROOT/eval/datasets/semantic-kernel-conformance.json"

REPO_ROOT="$REPO_ROOT" DATASET="$DATASET" node <<'NODE'
const fs = require('fs');
const path = require('path');
const { spawnSync } = require('child_process');

const root = process.env.REPO_ROOT;
const datasetPath = process.env.DATASET;
const dataset = JSON.parse(fs.readFileSync(datasetPath, 'utf8'));
if (!Array.isArray(dataset.cases) || dataset.cases.length === 0) {
  throw new Error('semantic-kernel conformance dataset is empty');
}

function parseRef(ref, label) {
  const split = ref.indexOf('::');
  if (split < 0) throw new Error(`invalid ${label} reference: ${ref}`);
  const file = ref.slice(0, split);
  const qualifiedSymbol = ref.slice(split + 2);
  const symbol = qualifiedSymbol.split('::').pop();
  if (!symbol) throw new Error(`missing ${label} symbol: ${ref}`);
  const absolute = path.join(root, file);
  if (!fs.existsSync(absolute)) throw new Error(`${label} file does not exist: ${file}`);
  return { file, symbol, qualifiedSymbol, source: fs.readFileSync(absolute, 'utf8') };
}

function findMatchingBrace(source, opening) {
  let depth = 0;
  let mode = 'code';
  let blockCommentDepth = 0;
  for (let i = opening; i < source.length; i += 1) {
    const char = source[i];
    const next = source[i + 1];
    if (mode === 'line-comment') {
      if (char === '\n') mode = 'code';
      continue;
    }
    if (mode === 'block-comment') {
      if (char === '/' && next === '*') { blockCommentDepth += 1; i += 1; continue; }
      if (char === '*' && next === '/') {
        blockCommentDepth -= 1;
        i += 1;
        if (blockCommentDepth === 0) mode = 'code';
      }
      continue;
    }
    if (mode === 'string' || mode === 'char') {
      if (char === '\\') { i += 1; continue; }
      if ((mode === 'string' && char === '"') || (mode === 'char' && char === "'")) mode = 'code';
      continue;
    }
    if (char === '/' && next === '/') { mode = 'line-comment'; i += 1; continue; }
    if (char === '/' && next === '*') {
      mode = 'block-comment';
      blockCommentDepth = 1;
      i += 1;
      continue;
    }
    if (char === '"') { mode = 'string'; continue; }
    if (char === "'" && /[A-Za-z_]/.test(next || '') && source[i + 2] !== "'") {
      continue; // Rust lifetime, not a character literal.
    }
    if (char === "'") { mode = 'char'; continue; }
    if (char === '{') depth += 1;
    if (char === '}') {
      depth -= 1;
      if (depth === 0) return i;
    }
  }
  throw new Error('unbalanced Rust braces while inspecting behavior case');
}

function functionCandidates(reference) {
  const escaped = reference.symbol.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  const declaration = new RegExp(`(?:pub(?:\\([^)]*\\))?\\s+)?(?:const\\s+)?(?:async\\s+)?fn\\s+${escaped}\\s*(?:<[^;{]*>)?\\s*\\(`, 'g');
  const candidates = [];
  for (const match of reference.source.matchAll(declaration)) {
    const opening = reference.source.indexOf('{', match.index + match[0].length);
    if (opening < 0) continue;
    const semicolon = reference.source.indexOf(';', match.index + match[0].length);
    if (semicolon >= 0 && semicolon < opening) continue;
    const closing = findMatchingBrace(reference.source, opening);
    candidates.push({
      start: match.index,
      end: closing + 1,
      body: reference.source.slice(opening + 1, closing),
      prefix: reference.source.slice(Math.max(0, match.index - 240), match.index),
    });
  }
  return candidates;
}

function testModuleRanges(source) {
  const ranges = [];
  const declaration = /#\s*\[\s*cfg\s*\(\s*test\s*\)\s*\][\s\S]{0,160}?\bmod\s+[A-Za-z_][A-Za-z0-9_]*\s*\{/g;
  for (const match of source.matchAll(declaration)) {
    const opening = source.indexOf('{', match.index);
    ranges.push([match.index, findMatchingBrace(source, opening) + 1]);
  }
  return ranges;
}

function isTestOnly(reference, candidate) {
  if (/#\s*\[\s*cfg\s*\(\s*test\s*\)\s*\]\s*$/.test(candidate.prefix)) return true;
  return testModuleRanges(reference.source)
    .some(([start, end]) => candidate.start >= start && candidate.end <= end);
}

function selectProductionFunction(reference, caseId) {
  const marker = new RegExp(`(?:crate::)?behavior_trace\\(\\s*"${caseId}"\\s*\\)`);
  const candidates = functionCandidates(reference)
    .filter((candidate) => !isTestOnly(reference, candidate))
    .filter((candidate) => marker.test(candidate.body));
  if (candidates.length !== 1) {
    throw new Error(`${caseId}: expected one non-test production function containing its trace, got ${candidates.length}`);
  }
  return candidates[0];
}

function selectTestFunction(reference, caseId) {
  const candidates = functionCandidates(reference)
    .filter((candidate) => /#\s*\[\s*(?:tokio::)?test(?:\s*\([^\]]*\))?\s*\]/.test(candidate.prefix));
  if (candidates.length !== 1) {
    throw new Error(`${caseId}: expected exactly one declared test function, got ${candidates.length}`);
  }
  return candidates[0];
}

function testHasAssertions(candidate) {
  // Conformance tests may keep the actual assertion macros in a small helper
  // (for example assert_recovered/assert_fault_exit). Requiring only a direct
  // `assert!` would reject valid tests and make the gate unusable for process
  // and black-box fixtures. Still require an assertion-shaped call, rather
  // than treating `expect`/`unwrap` as proof of the expected behavior.
  return /(?:^|[^A-Za-z0-9_])assert(?:_eq|_ne|_matches|_anyhow|_err)?!\s*\(/.test(candidate.body)
    || /(?:^|[^A-Za-z0-9_])assert_[A-Za-z0-9_]*\s*\(/.test(candidate.body);
}

const seen = new Set();
for (const item of dataset.cases) {
  if (process.env.AOS_BEHAVIOR_TRACE_ONLY && process.env.AOS_BEHAVIOR_TRACE_ONLY !== item.id) continue;
  if (!item.id || seen.has(item.id)) throw new Error(`duplicate or missing case id: ${item.id}`);
  seen.add(item.id);
  const production = parseRef(item.production, 'production');
  const test = parseRef(item.test, 'test');
  selectProductionFunction(production, item.id);
  const testFunction = selectTestFunction(test, item.id);
  if (!testHasAssertions(testFunction)) throw new Error(`${item.id}: selected test function has no assertion`);

  const packageMatch = /^rust\/crates\/([^/]+)\//.exec(test.file);
  if (!packageMatch) throw new Error(`${item.id}: test is outside a Rust crate: ${item.test}`);
  const args = [
    'test', '--manifest-path', path.join(root, 'rust/Cargo.toml'),
    '-p', packageMatch[1], test.symbol, '--', '--nocapture',
  ];
  process.stdout.write(`[semantic-kernel] ${item.id} -> ${packageMatch[1]}::${test.symbol}\n`);
  const result = spawnSync('cargo', args, {
    cwd: root,
    env: { ...process.env, AOS_BEHAVIOR_TRACE_CASE: item.id },
    encoding: 'utf8',
    maxBuffer: 32 * 1024 * 1024,
  });
  const output = `${result.stdout || ''}${result.stderr || ''}`;
  process.stdout.write(output);
  if (result.error) throw new Error(`${item.id}: unable to execute cargo: ${result.error.message}`);
  if (result.status !== 0) throw new Error(`${item.id}: test exited with ${result.status}`);
  const trace = `AOS_PRODUCTION_TRACE\t${item.id}`;
  const traceCount = output.split(trace).length - 1;
  if (traceCount !== 1) throw new Error(`${item.id}: expected exactly one production trace, got ${traceCount}`);
  if (!/test result: ok\b/.test(output)) throw new Error(`${item.id}: test result was not successful`);
}
if (seen.size === 0) throw new Error('behavior case filter matched no dataset cases');
console.log(`Semantic-kernel behavior conformance: ${seen.size} cases executed with production traces.`);
NODE
