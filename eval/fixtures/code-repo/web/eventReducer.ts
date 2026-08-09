export function appendEvent(items: string[], eventId: number, text: string): string[] {
  // Intentionally incomplete fixture: replayed event IDs are not deduplicated.
  return [...items, `${eventId}:${text}`];
}
