import { useCallback, useState } from 'react';

export function useDismissibleNotice(key: string) {
  const [visible, setVisible] = useState(() => {
    try {
      return window.localStorage.getItem(key) !== 'dismissed';
    } catch {
      return true;
    }
  });

  const dismiss = useCallback(() => {
    setVisible(false);
    try {
      window.localStorage.setItem(key, 'dismissed');
    } catch {
      // Storage may be disabled; hiding for the current page is still useful.
    }
  }, [key]);

  return { visible, dismiss };
}
