#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
DATASET="$REPO_ROOT/eval/datasets/semantic-kernel-conformance.json"

node - "$DATASET" <<'NODE' | while IFS=$'\t' read -r case_id package test_filter; do
const fs = require('fs');
const dataset = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
for (const item of dataset.cases) {
  const split = item.test.indexOf('::');
  if (split < 0) throw new Error(`invalid test reference: ${item.test}`);
  const path = item.test.slice(0, split);
  const symbol = item.test.slice(split + 2).split('::').pop();
  const match = /^rust\/crates\/([^/]+)\//.exec(path);
  if (!match) throw new Error(`test is outside a Rust crate: ${item.test}`);
  process.stdout.write(`${item.id}\t${match[1]}\t${symbol}\n`);
}
NODE
  echo "[semantic-kernel] $case_id -> $package::$test_filter"
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
done

echo "Semantic-kernel behavior conformance: all cases executed."
