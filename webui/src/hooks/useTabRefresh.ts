import { useCallback } from 'react';

/**
 * Generic helper: when users click the currently active tab again,
 * fire a refresh callback to re-fetch fresh data.
 */
export function useTabRefresh(activeKey: string, onRefreshActive: (key: string) => void) {
  return useCallback(
    (key: string) => {
      if (key === activeKey) {
        onRefreshActive(key);
      }
    },
    [activeKey, onRefreshActive],
  );
}

