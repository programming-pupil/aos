import type { RdTaskEvent } from '@/types';
import type { RdTimelineEvent } from './types';

export function groupTimelineEventsByStage(timelineEvents: RdTimelineEvent[]) {
  const groups = new Map<string, RdTimelineEvent[]>();
  for (const event of timelineEvents) {
    const key = event.stage || 'unknown';
    const group = groups.get(key) ?? [];
    group.push(event);
    groups.set(key, group);
  }
  return Array.from(groups.entries()).map(([stage, items]) => ({
    stage,
    events: items,
    latest: items[0],
  }));
}

export function latestContextCacheEvent(events: RdTaskEvent[]) {
  return events.find((event) => event.stage === 'context_cache_usage')
    ?? events.find((event) => event.stage === 'context_retrieval_evidence');
}
