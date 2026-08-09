# AOS Code Studio Frontend Bug Demo

This tiny Vite/React project is intentionally broken:

- `src/metrics.ts` throws when cost is zero.
- `src/App.tsx` includes an organic channel with zero cost.
- Opening the app in Preview should show a console/runtime error.
- `src/metrics.test.ts` describes the expected fix.

Recommended AOS prompt:

```text
Fix the demo Vite/React console error. Inspect real files first, make the change only in the candidate workspace, run tests if available, and leave the main repository untouched until I apply the diff.
```

Expected flow:

1. Register this directory as a Code Studio repository.
2. Ask AOS to fix the preview error.
3. Review the candidate Diff.
4. Run `npm test`.
5. Apply selected hunks after review.
