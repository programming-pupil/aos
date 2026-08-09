import { useCallback, useEffect, useRef, useState } from 'react';

export interface PromptQueueItem {
  id: string;
  question: string;
  source?: string;
  createdAt: number;
  optimisticUserTurnId?: string;
}

interface UsePromptQueueOptions {
  onProcess: (item: PromptQueueItem) => Promise<void> | void;
  paused?: boolean;
}

export function usePromptQueue({ onProcess, paused = false }: UsePromptQueueOptions) {
  const [items, setItems] = useState<PromptQueueItem[]>([]);
  const [processing, setProcessing] = useState(false);
  const [activeItemId, setActiveItemId] = useState<string | null>(null);
  const processingRef = useRef(false);
  const onProcessRef = useRef(onProcess);

  useEffect(() => {
    onProcessRef.current = onProcess;
  }, [onProcess]);

  const enqueue = useCallback((
    question: string,
    source: string = 'input',
    optimisticUserTurnId?: string,
  ) => {
    const normalized = question.trim();
    if (!normalized) return;
    setItems(prev => [...prev, {
      id: `queued-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`,
      question: normalized,
      source,
      createdAt: Date.now(),
      optimisticUserTurnId,
    }]);
  }, []);

  const updateItem = useCallback((id: string, question: string) => {
    setItems(prev => prev.map(item => item.id === id ? { ...item, question } : item));
  }, []);

  const removeItem = useCallback((id: string) => {
    setItems(prev => prev.filter(item => item.id !== id));
  }, []);

  const clear = useCallback(() => {
    setItems([]);
    if (!processingRef.current) {
      setProcessing(false);
      setActiveItemId(null);
    }
  }, []);

  const resume = useCallback(() => {
    setProcessing(false);
    setActiveItemId(null);
    processingRef.current = false;
  }, []);

  useEffect(() => {
    if (paused) return;
    if (processingRef.current) return;
    if (items.length === 0) return;

    const next = items[0];
    processingRef.current = true;
    setProcessing(true);
    setActiveItemId(next.id);

    void (async () => {
      try {
        if (next.question.trim()) {
          await onProcessRef.current(next);
        }
      } finally {
        setItems(prev => prev.filter(item => item.id !== next.id));
        setActiveItemId(null);
        setProcessing(false);
        processingRef.current = false;
      }
    })();
  }, [items, paused]);

  return {
    items,
    processing,
    activeItemId,
    enqueue,
    updateItem,
    removeItem,
    clear,
    resume,
  };
}
