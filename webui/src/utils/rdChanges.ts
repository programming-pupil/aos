import type { RdFileChange } from '@/types';

const APPLYABLE_CHANGE_TYPES = new Set(['modify', 'repair_diff', 'auto_repair', 'partial_remaining']);

function normalizeRepoPath(path?: string | null) {
  const raw = (path ?? '').trim().replaceAll('\\', '/').replace(/^['"`]+|['"`]+$/g, '');
  const withoutSide = raw.replace(/^a\//, '').replace(/^b\//, '');
  if (!withoutSide || withoutSide === '/dev/null') return '';
  return withoutSide
    .split('/')
    .filter((part) => part && part !== '.')
    .join('/');
}

function isInternalOrGeneratedPath(path?: string | null) {
  const normalized = normalizeRepoPath(path);
  if (!normalized) return true;
  if (
    normalized === '.git' ||
    normalized.startsWith('.git/') ||
    normalized === '.aos' ||
    normalized.startsWith('.aos/') ||
    normalized === '.aos-rd-candidates' ||
    normalized.startsWith('.aos-rd-candidates/')
  ) {
    return true;
  }
  return normalized.split('/').some((part) => (
    part === 'node_modules' ||
    part === 'target' ||
    part === 'dist' ||
    part === 'build' ||
    part === '.next' ||
    part === '.turbo' ||
    part === '.vite'
  ));
}

function diffMentionsInternalPath(diff?: string | null) {
  if (!diff) return false;
  for (const line of diff.split('\n')) {
    if (line.startsWith('diff --git ')) {
      const parts = line.slice('diff --git '.length).trim().split(/\s+/).slice(0, 2);
      if (parts.some(isInternalOrGeneratedPath)) return true;
    } else if (line.startsWith('--- ') || line.startsWith('+++ ')) {
      if (isInternalOrGeneratedPath(line.slice(4).trim())) return true;
    }
  }
  return false;
}

export function isRdFileChangeApplicable(change: RdFileChange) {
  return (
    !change.applied &&
    APPLYABLE_CHANGE_TYPES.has(change.changeType) &&
    !isInternalOrGeneratedPath(change.filePath) &&
    !diffMentionsInternalPath(change.diffPatch)
  );
}

export function rdFileChangeNotApplicableReason(change: RdFileChange) {
  if (change.applied) return null;
  if (!APPLYABLE_CHANGE_TYPES.has(change.changeType)) return 'change_type';
  if (isInternalOrGeneratedPath(change.filePath) || diffMentionsInternalPath(change.diffPatch)) {
    return 'internal_path';
  }
  return null;
}
