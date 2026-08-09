import type { RdCodeIntelLocation, RdRepository } from '@/types';
import { CodeEditorPanel } from './CodeEditorPanel';

export function FilePreview({
  repository,
  path,
  revealLine,
  revealColumn,
  onOpenPath,
  onReferences,
}: {
  repository?: RdRepository | null;
  path?: string | null;
  revealLine?: number;
  revealColumn?: number;
  onOpenPath?: (path: string) => void;
  onReferences?: (locations: RdCodeIntelLocation[]) => void;
}) {
  return (
    <CodeEditorPanel
      repository={repository}
      path={path}
      revealLine={revealLine}
      revealColumn={revealColumn}
      onOpenPath={onOpenPath}
      onReferences={onReferences}
    />
  );
}
