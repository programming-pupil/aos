# Evaluation Datasets

Datasets in this directory are deterministic inputs for the `eval-harness`
crate.

Default reproducibility seed: `20260710`.

`codex-parity-gaps.seed.json` is a synthetic contract fixture. Its fixed values
verify harness wiring only; it intentionally contains no Codex baseline and
must not be reported as measured product quality. Versioned online A/B results
must use separate dataset files with raw evidence references and reviewer
methodology.

Each dataset file should keep stable case ids and avoid wall-clock dependent
inputs unless the timestamp is explicitly fixed in the case.
