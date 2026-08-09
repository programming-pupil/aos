/**
 * System events WebSocket client.
 *
 * Connects to /ws/system-events with the JWT token from localStorage.
 * Provides a reactive hook for components to subscribe to system-wide events
 * (MCP status changes, model switches, token warnings, heartbeats).
 *
 * Usage:
 *   const { connected, events } = useSystemEvents();
 *   // or with TanStack Query:
 *   const { data: events } = useSystemEventsQuery();
 */

import { useCallback, useEffect, useRef, useState } from 'react';
import { useQueryClient } from '@tanstack/react-query';
import { queryKeys } from '@/api/queryKeys';
import { getStoredAuthToken, useAuthStore } from '@/store/auth';

// ── Event types ────────────────────────────────────────────────────────────────

export type SystemEventType =
  | 'connected'
  | 'mcp_server_added'
  | 'mcp_server_removed'
  | 'mcp_server_toggled'
  | 'mcp_server_updated'
  | 'mcp_status_changed'
  | 'model_switched'
  | 'token_limit_warning'
  | 'heartbeat'
  | 'skills_updated'
  | 'hooks_updated'
  | 'search_providers_updated';

export interface SystemEventBase {
  tenant_id: string;
}

export interface ConnectedEvent extends SystemEventBase {
  type: 'connected';
  user_id: string;
}

export interface McpServerAddedEvent extends SystemEventBase {
  type: 'mcp_server_added';
  name: string;
}

export interface McpServerRemovedEvent extends SystemEventBase {
  type: 'mcp_server_removed';
  name: string;
}

export interface McpServerToggledEvent extends SystemEventBase {
  type: 'mcp_server_toggled';
  name: string;
  enabled: boolean;
}

export interface McpServerUpdatedEvent extends SystemEventBase {
  type: 'mcp_server_updated';
  name: string;
}

export interface McpStatusChangedEvent extends SystemEventBase {
  type: 'mcp_status_changed';
  name: string;
  status: string;
  last_error?: string;
}

export interface ModelSwitchedEvent extends SystemEventBase {
  type: 'model_switched';
  user_id: string;
  model: string;
}

export interface TokenLimitWarningEvent extends SystemEventBase {
  type: 'token_limit_warning';
  user_id: string;
  percentage: number;
  limit: number;
  current: number;
}

export interface HeartbeatEvent {
  type: 'heartbeat';
  server_time: string;
}

export interface SkillBroadcastEntry {
  name: string;
  description: string;
  source: string;
  tags: string[];
  enabled: boolean;
}

export interface SkillsUpdatedEvent extends SystemEventBase {
  type: 'skills_updated';
  skills: SkillBroadcastEntry[];
}

export interface HooksUpdatedEvent extends SystemEventBase {
  type: 'hooks_updated';
}

export interface SearchProvidersUpdatedEvent extends SystemEventBase {
  type: 'search_providers_updated';
}

export type SystemEvent =
  | ConnectedEvent
  | McpServerAddedEvent
  | McpServerRemovedEvent
  | McpServerToggledEvent
  | McpServerUpdatedEvent
  | McpStatusChangedEvent
  | ModelSwitchedEvent
  | TokenLimitWarningEvent
  | HeartbeatEvent
  | SkillsUpdatedEvent
  | HooksUpdatedEvent
  | SearchProvidersUpdatedEvent;

// ── WebSocket singleton ────────────────────────────────────────────────────────

type WsState = 'connecting' | 'connected' | 'disconnected' | 'error';

interface WsClient {
  ws: WebSocket | null;
  state: WsState;
  listeners: Set<(event: SystemEvent) => void>;
  reconnectTimer: ReturnType<typeof setTimeout> | null;
  reconnectDelay: number;
}

const REINITIAL_DELAY_MS = 5_000;
const MAX_RECONNECT_DELAY_MS = 30_000;
const BACKOFF_MULTIPLIER = 1.5;

const client: WsClient = {
  ws: null,
  state: 'disconnected',
  listeners: new Set(),
  reconnectTimer: null,
  reconnectDelay: REINITIAL_DELAY_MS,
};

function getWsConnection(): { url: string; token: string } | null {
  const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
  const host = window.location.host;
  const token = getStoredAuthToken();
  if (!token) return null;
  return { url: `${protocol}//${host}/ws/system-events`, token };
}

export function connectWs(): void {
  if (client.ws && (client.ws.readyState === WebSocket.OPEN || client.ws.readyState === WebSocket.CONNECTING)) {
    return;
  }

  client.state = 'connecting';
  const connection = getWsConnection();
  if (!connection) {
    client.state = 'disconnected';
    return;
  }

  try {
    client.ws = new WebSocket(connection.url, ['aos-auth', connection.token]);

    client.ws.onopen = () => {
      client.state = 'connected';
      client.reconnectDelay = REINITIAL_DELAY_MS;
    };

    client.ws.onmessage = (ev) => {
      try {
        const event = JSON.parse(ev.data) as SystemEvent;
        client.listeners.forEach((fn) => fn(event));
      } catch {
        // Ignore malformed websocket records; subsequent events remain usable.
      }
    };

    client.ws.onerror = () => {
      client.state = 'error';
    };

    client.ws.onclose = () => {
      client.state = 'disconnected';
      client.ws = null;
      scheduleReconnect();
    };
  } catch (e) {
    client.state = 'error';
    scheduleReconnect();
  }
}

function scheduleReconnect(): void {
  if (client.reconnectTimer) return;
  client.reconnectTimer = setTimeout(() => {
    client.reconnectTimer = null;
    if (client.state !== 'connected') {
      connectWs();
    }
  }, client.reconnectDelay);
  client.reconnectDelay = Math.min(client.reconnectDelay * BACKOFF_MULTIPLIER, MAX_RECONNECT_DELAY_MS);
}

export function disconnectWs(): void {
  if (client.reconnectTimer) {
    clearTimeout(client.reconnectTimer);
    client.reconnectTimer = null;
  }
  if (client.ws) {
    client.ws.onclose = null;
    client.ws.close();
    client.ws = null;
  }
  client.state = 'disconnected';
  client.listeners.clear();
}

export function onSystemEvent(fn: (event: SystemEvent) => void): () => void {
  client.listeners.add(fn);
  return () => client.listeners.delete(fn);
}

// ── React hook ────────────────────────────────────────────────────────────────

export interface UseSystemEventsOptions {
  /** Automatically connect on mount. Default: true. */
  autoConnect?: boolean;
  /** Filter to only events for this tenant_id. Default: reads from localStorage token. */
  tenantId?: string;
  /** Called whenever hooks are updated (created, modified, or deleted). */
  onHooksUpdated?: () => void;
  /** Called whenever Skills are installed, updated, deleted, enabled, or disabled. */
  onSkillsUpdated?: () => void;
  /** Called whenever any MCP server is added, removed, toggled, updated, or changes status. */
  onMcpUpdated?: () => void;
  /** Called whenever Search Provider config or health changes. */
  onSearchProvidersUpdated?: () => void;
}

export interface UseSystemEventsResult {
  /** Current WebSocket connection state. */
  state: WsState;
  /** Whether the WebSocket is connected. */
  connected: boolean;
  /** All events received since the last reset. */
  events: SystemEvent[];
  /** Clear the events buffer. */
  clearEvents: () => void;
  /** Manually trigger a reconnection. */
  reconnect: () => void;
}

export function useSystemEvents(options: UseSystemEventsOptions = {}): UseSystemEventsResult {
  const {
    autoConnect = true,
    onHooksUpdated,
    onSkillsUpdated,
    onMcpUpdated,
    onSearchProvidersUpdated,
  } = options;
  const [events, setEvents] = useState<SystemEvent[]>([]);
  const [state, setState] = useState<WsState>(client.state);
  const storeTenantId = useAuthStore((s) => s.tenantId);
  const tenantId = options.tenantId ?? storeTenantId;
  const tenantIdRef = useRef(tenantId);
  tenantIdRef.current = tenantId;
  const onHooksUpdatedRef = useRef(onHooksUpdated);
  onHooksUpdatedRef.current = onHooksUpdated;
  const onSkillsUpdatedRef = useRef(onSkillsUpdated);
  onSkillsUpdatedRef.current = onSkillsUpdated;
  const onMcpUpdatedRef = useRef(onMcpUpdated);
  onMcpUpdatedRef.current = onMcpUpdated;
  const onSearchProvidersUpdatedRef = useRef(onSearchProvidersUpdated);
  onSearchProvidersUpdatedRef.current = onSearchProvidersUpdated;

  useEffect(() => {
    if (!autoConnect) return;

    const updateState = () => setState(client.state);

    const handleEvent = (event: SystemEvent) => {
      if (event.type === 'connected') return;
      const tid = tenantIdRef.current;
      if (tid && 'tenant_id' in event && event.tenant_id !== tid) return;

      if (event.type === 'hooks_updated') {
        onHooksUpdatedRef.current?.();
      }

      if (isSkillsEvent(event)) {
        onSkillsUpdatedRef.current?.();
      }

      if (isMcpEvent(event)) {
        onMcpUpdatedRef.current?.();
      }

      if (isSearchProviderEvent(event)) {
        onSearchProvidersUpdatedRef.current?.();
      }

      setEvents((prev) => [...prev.slice(-49), event]);
    };

    const unsub = onSystemEvent(handleEvent);
    connectWs();

    const interval = setInterval(updateState, 1000);

    return () => {
      unsub();
      clearInterval(interval);
    };
  }, [autoConnect]);

  const clearEvents = useCallback(() => setEvents([]), []);

  const reconnect = useCallback(() => {
    disconnectWs();
    setTimeout(connectWs, 100);
  }, []);

  return {
    state,
    connected: state === 'connected',
    events,
    clearEvents,
    reconnect,
  };
}

// ── MCP-specific helpers ──────────────────────────────────────────────────────

function isMcpEvent(event: SystemEvent): boolean {
  return (
    event.type === 'mcp_server_added' ||
    event.type === 'mcp_server_removed' ||
    event.type === 'mcp_server_toggled' ||
    event.type === 'mcp_server_updated' ||
    event.type === 'mcp_status_changed'
  );
}

function isSkillsEvent(event: SystemEvent): boolean {
  return event.type === 'skills_updated';
}

function isHooksEvent(event: SystemEvent): boolean {
  return event.type === 'hooks_updated';
}

function isSearchProviderEvent(event: SystemEvent): boolean {
  return event.type === 'search_providers_updated';
}

// ── TanStack Query integration ────────────────────────────────────────────────

/**
 * Returns the current WebSocket connection state as a TanStack Query result.
 * Re-fetches every 2s so components using this as a query get reactive updates.
 */
export function useSystemEventsQuery() {
  const queryClient = useQueryClient();

  const { state, connected } = useSystemEvents({ autoConnect: true });

  useEffect(() => {
    if (connected) {
      const unsub = onSystemEvent((event) => {
        if (isMcpEvent(event)) {
          switch (event.type) {
            case 'mcp_server_added':
            case 'mcp_server_updated':
              queryClient.invalidateQueries({ queryKey: queryKeys.mcp.list() });
              queryClient.invalidateQueries({ queryKey: queryKeys.mcp.stats() });
              break;
            case 'mcp_server_removed':
              queryClient.invalidateQueries({ queryKey: queryKeys.mcp.list() });
              queryClient.invalidateQueries({ queryKey: queryKeys.mcp.stats() });
              break;
            case 'mcp_server_toggled':
              queryClient.invalidateQueries({ queryKey: queryKeys.mcp.list() });
              break;
            case 'mcp_status_changed':
              queryClient.invalidateQueries({ queryKey: queryKeys.mcp.list() });
              break;
          }
        } else if (isSkillsEvent(event)) {
          queryClient.invalidateQueries({ queryKey: queryKeys.skills.all });
          queryClient.invalidateQueries({ queryKey: queryKeys.commands.all });
          queryClient.invalidateQueries({ queryKey: queryKeys.chatSessions.all });
        } else if (isHooksEvent(event)) {
          queryClient.invalidateQueries({ queryKey: queryKeys.hooks.all });
        } else if (isSearchProviderEvent(event)) {
          queryClient.invalidateQueries({ queryKey: queryKeys.pm.searchProviders() });
          queryClient.invalidateQueries({ queryKey: queryKeys.pm.searchDoctor() });
          queryClient.invalidateQueries({ queryKey: queryKeys.chatSessions.all });
        }
      });
      return unsub;
    }
  }, [connected, queryClient]);

  return { data: state, isConnected: connected };
}
