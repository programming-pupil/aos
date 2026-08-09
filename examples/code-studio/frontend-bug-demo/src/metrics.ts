export function calculateRoi(revenueUsd: number, costUsd: number): number {
  // BUG FOR DEMO: organic traffic can have zero cost. This should not throw;
  // the Agent is expected to make ROI handling explicit and testable.
  if (costUsd === 0) {
    throw new Error('Cannot calculate ROI when cost is zero');
  }
  return revenueUsd / costUsd;
}

export function formatPercent(value: number): string {
  return `${(value * 100).toFixed(1)}%`;
}
