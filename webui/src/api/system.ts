import { client } from './client';
import type { ApiKeyRecord, ApiKeyStats } from '@/types';

type ApiKeyCapabilities = NonNullable<ApiKeyRecord['capabilities_json']>;

export interface ModelCapabilityResolution {
  profile: ApiKeyCapabilities;
  source: string;
  confidence: string;
  requiresProbe: boolean;
}

export interface DiscoveredApiModel {
  id: string;
  displayName?: string;
  createdAt?: string;
  profile: ApiKeyCapabilities;
  source: string;
  confidence: string;
}

export const apiKeysApi = {
  list: () => client.get<{ keys: ApiKeyRecord[]; total: number }>('/apikeys').then((r) => r.data),

  create: (data: {
    name: string;
    provider: string;
    base_url?: string;
    model?: string;
    dimensions?: number;
    audio_generate_path?: string;
    audio_query_path?: string;
    model_type?: string;
    key_value: string;
    daily_limit?: number;
    monthly_limit?: number;
    priority?: number;
    input_price_per_million?: number;
    output_price_per_million?: number;
    /** Scenario tags for routing this key to specific modules (chat / nl2sql / rd / pm). */
    scenarios?: string[];
    capabilities_json?: ApiKeyCapabilities | null;
  }) =>
    client.post<{ id: string; name: string; provider: string; key_hint: string }>('/apikeys', data).then((r) => r.data),

  update: (id: string, data: {
    name?: string;
    base_url?: string;
    model?: string;
    dimensions?: number;
    audio_generate_path?: string;
    audio_query_path?: string;
    model_type?: string;
    key_value?: string;
    daily_limit?: number;
    monthly_limit?: number;
    enabled?: boolean;
    priority?: number;
    is_primary?: boolean;
    input_price_per_million?: number;
    output_price_per_million?: number;
    expires_at?: string | null;
    /** Scenario tags for routing this key. Omit to leave unchanged. */
    scenarios?: string[] | null;
    capabilities_json?: ApiKeyCapabilities | null;
  }) =>
    client.put<ApiKeyRecord>(`/apikeys/${encodeURIComponent(id)}`, data).then((r) => r.data),

  delete: (id: string) =>
    client.delete(`/apikeys/${encodeURIComponent(id)}`).then((r) => r.data),

  stats: (keyId: string) =>
    client.get<ApiKeyStats>(`/apikeys/${encodeURIComponent(keyId)}/stats`).then((r) => r.data),

  testHealth: (id: string) =>
    client.post<{ ok: boolean; latency_ms: number; error?: string }>(`/apikeys/${encodeURIComponent(id)}/test`).then((r) => r.data),
  resolveModel: (data: {
    provider: string;
    baseUrl?: string;
    model: string;
    modelType?: string;
  }) =>
    client.post<ModelCapabilityResolution>('/apikeys/models/resolve', data).then((r) => r.data),

  discoverModels: (data: {
    provider: string;
    baseUrl?: string;
    apiKey?: string;
    existingKeyId?: string;
    modelType?: string;
  }) =>
    client.post<{ models: DiscoveredApiModel[]; endpoint: string }>(
      '/apikeys/models/discover',
      data
    ).then((r) => r.data),

  probeModel: (data: {
    provider: string;
    baseUrl?: string;
    apiKey?: string;
    existingKeyId?: string;
    model: string;
    modelType?: string;
    full?: boolean;
  }) =>
    client.post<{
      ok: boolean;
      latencyMs: number;
      endpoint: string;
      profile: ApiKeyCapabilities;
      source: string;
      confidence: string;
      checks: string[];
      warning?: string;
    }>('/apikeys/models/probe', data).then((r) => r.data),

  acceptModelProfile: (data: {
    provider: string;
    baseUrl?: string;
    model: string;
    modelType?: string;
    profile: ApiKeyCapabilities;
    source: string;
    confidence: string;
  }) =>
    client.post<ModelCapabilityResolution>('/apikeys/models/accept', data).then((r) => r.data),
};
