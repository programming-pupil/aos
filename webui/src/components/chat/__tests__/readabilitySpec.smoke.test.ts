import fs from 'node:fs';
import path from 'node:path';
import { describe, expect, it } from 'vitest';

describe('Readability spec document', () => {
  it('exists and contains the required structural rules', () => {
    const specPath = path.resolve(process.cwd(), '../docs/READABILITY_SPEC.md');
    const content = fs.readFileSync(specPath, 'utf8');

    expect(content).toContain('散文');
    expect(content).toContain('要点列表');
    expect(content).toContain('代码块');
    expect(content).toContain('表格');
    expect(content).toContain('标题层级');
    expect(content).toContain('简单问题');
    expect(content).toContain('克制');
  });
});
