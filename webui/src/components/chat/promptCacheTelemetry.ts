export interface PromptCacheTelemetrySample {
  inputTokens?: number | null;
  cacheReadInputTokens?: number | null;
  unexpected?: boolean | null;
  reason?: string | null;
}

export interface PromptCacheHitRate {
  cacheHitRate: number;
  cacheReadInputTokens: number;
  totalInputTokens: number;
  sampleCount: number;
  degradedSampleCount: number;
  reasons: string[];
}

function finiteNonNegative(value: unknown): number | null {
  return typeof value === 'number' && Number.isFinite(value) && value >= 0 ? value : null;
}

export function computePromptCacheHitRate(samples: PromptCacheTelemetrySample[]): PromptCacheHitRate {
  let cacheReadInputTokens = 0;
  let totalInputTokens = 0;
  let sampleCount = 0;
  let degradedSampleCount = 0;
  const reasons: string[] = [];

  for (const sample of samples) {
    if (sample.unexpected) {
      degradedSampleCount += 1;
      const reason = typeof sample.reason === 'string' && sample.reason.trim() ? sample.reason.trim() : 'unexpected prompt cache telemetry';
      reasons.push(reason);
      continue;
    }

    const inputTokens = finiteNonNegative(sample.inputTokens);
    const cacheTokens = finiteNonNegative(sample.cacheReadInputTokens);
    if (inputTokens === null) {
      degradedSampleCount += 1;
      reasons.push('missing total input tokens');
      continue;
    }

    sampleCount += 1;
    totalInputTokens += inputTokens;
    cacheReadInputTokens += Math.min(cacheTokens ?? 0, inputTokens);
  }

  return {
    cacheHitRate: totalInputTokens > 0 ? cacheReadInputTokens / totalInputTokens : 0,
    cacheReadInputTokens,
    totalInputTokens,
    sampleCount,
    degradedSampleCount,
    reasons,
  };
}
