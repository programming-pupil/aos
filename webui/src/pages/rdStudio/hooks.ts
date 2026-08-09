import { useEffect } from 'react';
import type { RdWorkspaceTabKey } from './types';

export function useRdStudioShortcuts({
  enabled,
  hasPendingChanges,
  onApply,
  onSelectWorkspaceTab,
  onSelectInspectorTab,
  onQuickOpenFiles,
  onQuickOpenSymbols,
  onCommandPalette,
  onNavigateBack,
  onNavigateForward,
}: {
  enabled: boolean;
  hasPendingChanges: boolean;
  onApply: () => void;
  onSelectWorkspaceTab: (tab: RdWorkspaceTabKey) => void;
  onSelectInspectorTab: (tab: RdWorkspaceTabKey) => void;
  onQuickOpenFiles?: () => void;
  onQuickOpenSymbols?: () => void;
  onCommandPalette?: () => void;
  onNavigateBack?: () => void;
  onNavigateForward?: () => void;
}) {
  useEffect(() => {
    if (!enabled) return undefined;
    function handleShortcut(event: KeyboardEvent) {
      const target = event.target as HTMLElement | null;
      const tagName = target?.tagName?.toLowerCase();
      const isTyping = !!target?.isContentEditable || tagName === 'input' || tagName === 'textarea';
      if (isTyping) return;
      const key = event.key.toLowerCase();
      if ((event.metaKey || event.ctrlKey) && event.key === 'Enter' && hasPendingChanges) {
        event.preventDefault();
        onApply();
        return;
      }
      if ((event.metaKey || event.ctrlKey) && event.shiftKey && key === 'p') {
        event.preventDefault();
        onCommandPalette?.();
        return;
      }
      if ((event.metaKey || event.ctrlKey) && event.shiftKey && key === 'o') {
        event.preventDefault();
        onQuickOpenSymbols?.();
        return;
      }
      if ((event.metaKey || event.ctrlKey) && !event.shiftKey && key === 'p') {
        event.preventDefault();
        onQuickOpenFiles?.();
        return;
      }
      if (event.altKey && key === 'arrowleft') {
        event.preventDefault();
        onNavigateBack?.();
        return;
      }
      if (event.altKey && key === 'arrowright') {
        event.preventDefault();
        onNavigateForward?.();
        return;
      }
      if (event.metaKey || event.ctrlKey || event.altKey) return;
      const nextWorkspaceTab: Record<string, RdWorkspaceTabKey> = {
        r: 'result',
        f: 'file',
        l: 'timeline',
        p: 'tokens',
      };
      const nextInspectorTab: Record<string, RdWorkspaceTabKey> = {
        d: 'diff',
        t: 'tests',
        c: 'context',
        e: 'references',
        v: 'preview',
      };
      const workspaceTab = nextWorkspaceTab[key];
      if (workspaceTab) {
        event.preventDefault();
        onSelectWorkspaceTab(workspaceTab);
        return;
      }
      const inspectorTab = nextInspectorTab[key];
      if (inspectorTab) {
        event.preventDefault();
        onSelectInspectorTab(inspectorTab);
      }
    }
    window.addEventListener('keydown', handleShortcut);
    return () => window.removeEventListener('keydown', handleShortcut);
  }, [
    enabled,
    hasPendingChanges,
    onApply,
    onCommandPalette,
    onNavigateBack,
    onNavigateForward,
    onQuickOpenFiles,
    onQuickOpenSymbols,
    onSelectInspectorTab,
    onSelectWorkspaceTab,
  ]);
}
