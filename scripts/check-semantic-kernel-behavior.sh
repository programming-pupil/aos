#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
DATASET="$REPO_ROOT/eval/datasets/semantic-kernel-conformance.json"
EVIDENCE_DIR="$REPO_ROOT/target/conformance"
EVIDENCE_FILE="$EVIDENCE_DIR/semantic-kernel-behavior.jsonl"
EVIDENCE_TMP="$EVIDENCE_FILE.tmp"
mkdir -p "$EVIDENCE_DIR"
: > "$EVIDENCE_TMP"
trap 'rm -f "$EVIDENCE_TMP"' EXIT

node - "$DATASET" "$REPO_ROOT" <<'NODE' | while IFS=$'\t' read -r case_id package test_filter production production_hash test_hash trace_anchor; do
const fs = require('fs');
const crypto = require('crypto');
const dataset = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
const root = process.argv[3];
const hash = value => crypto.createHash('sha256').update(value).digest('hex');
for (const item of dataset.cases) {
  const split = item.test.indexOf('::');
  if (split < 0) throw new Error(`invalid test reference: ${item.test}`);
  const path = item.test.slice(0, split);
  const symbol = item.test.slice(split + 2).split('::').pop();
  const match = /^rust\/crates\/([^/]+)\//.exec(path);
  if (!match) throw new Error(`test is outside a Rust crate: ${item.test}`);
  const productionPath = item.production.slice(0, item.production.indexOf('::'));
  const productionSymbol = item.production.split('::').pop();
  const productionSource = fs.readFileSync(`${root}/${productionPath}`, 'utf8');
  const testSource = fs.readFileSync(`${root}/${path}`, 'utf8');
  const anchor = item.traceAnchor || productionSymbol;
  process.stdout.write(`${item.id}\t${match[1]}\t${symbol}\t${item.production}\t${hash(productionSource)}\t${hash(testSource)}\t${anchor}\n`);
}
NODE
  echo "[semantic-kernel] $case_id -> $production -> $package::$test_filter"
  output="$(cargo test --manifest-path "$REPO_ROOT/rust/Cargo.toml" -p "$package" "$test_filter" -- --nocapture 2>&1)" || {
    printf '%s\n' "$output"
    exit 1
  }
  passed="$(printf '%s\n' "$output" | awk '/test result: ok/{sum += $4} END{print sum+0}')"
  if [[ "$passed" -lt 1 ]]; then
    printf '%s\n' "$output"
    echo "behavior case $case_id did not execute a matching test" >&2
    exit 1
  fi
  node - "$EVIDENCE_TMP" "$case_id" "$production" "$package::$test_filter" "$production_hash" "$test_hash" "$trace_anchor" "$passed" <<'NODE'
const fs = require('fs');
const [, , file, caseId, production, test, productionSourceHash, testSourceHash, traceAnchor, passed] = process.argv;
fs.appendFileSync(file, JSON.stringify({
  schemaVersion: 'semantic-kernel-behavior-evidence-v1',
  caseId,
  production,
  test,
  traceAnchor,
  productionSourceHash,
  testSourceHash,
  matchingTestsPassed: Number(passed),
  status: 'passed'
}) + '\n');
NODE
done

mv "$EVIDENCE_TMP" "$EVIDENCE_FILE"
trap - EXIT
echo "Semantic-kernel behavior conformance: all cases executed."
echo "Machine-readable evidence: $EVIDENCE_FILE"
