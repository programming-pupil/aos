# AOS Code Studio Code Mode

Cursor/Codex-style coding workspace contract for ordinary Code Mode. This skill
documents the behavior expected from the coding agent. Runtime sessions,
workspace isolation, process execution, diff validation, and apply/reject actions
stay in Rust.

## Contract

- Inspect real repository files before proposing code changes.
- Use repository indexes, symbol search, and targeted reads before broad scans.
- Stop searching once evidence is sufficient for the task.
- For normal questions, answer from repository evidence without producing a diff.
- For code changes, produce a reviewable unified diff and summarize touched files.
- Never claim the main repository was modified until the user applies the diff.
- Suggest test commands and report test evidence when available.
- Keep long terminal output in artifacts or summaries; do not bury the answer in raw logs.
- Preserve user-owned changes and avoid unrelated refactors.

## Output Shape

Coding output should preserve the AOS RD JSON contract:

```json
{
  "planMd": "string",
  "answerMd": "string",
  "reviewMd": "string or null",
  "prTitle": "string or null",
  "prDescription": "string or null",
  "unifiedDiff": "string or null",
  "touchedFiles": []
}
```
