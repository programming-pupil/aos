/**
 * Backend-to-Frontend type mapping: MCP Server Management API.
 *
 * ## Backend source
 * - Route: `/api/v1/mcp`
 * - Handler: `rust/crates/web-server/src/routes/mcp.rs`
 * - DTO: `McpServerInfo` (line ~95)
 *
 * ## Mapping notes
 *
 * - Backend `McpServerInfo.id` is `Option<String>` — frontend maps to `string | undefined`
 * - Backend `created_at` / `updated_at` are `Option<String>` in ISO 8601 format.
 * - Backend `tools_count` is `u32`, frontend uses `number` (same semantics).
 * - Backend `status` defaults to `"unknown"` when absent.
 * - Backend `auth` contains the auth configuration. The `auth_token` value is **never**
 *   sent to the frontend — only `has_token: bool` is returned.
 *
 * ## Response envelope
 *
 * The API returns `{ servers: McpServerInfo[], total: number }`.
 * The `total` is a raw count from the database (i64 → number).
 */

/** Authentication configuration returned from the backend (token value is always redacted). */
export interface BackendMcpServerAuthInfo {
  /** Auth type: `"none"` | `"bearer_token"` | `"oauth"`. */
  auth_type: string;
  /** Whether an auth token is configured (true = token exists, false = no token). */
  has_token: boolean;
  /** Extra HTTP headers as a JSON object. Omitted when null. */
  extra_headers?: Record<string, string>;
  /** Request timeout in milliseconds. Omitted when null (defaults to 60000). */
  timeout_ms?: number;
}

export interface BackendMcpServerInfo {
  /** UUID from `mcp_server_registry.id`. Present only when read from DB. */
  id?: string;
  /** Unique server name, used as the primary key. */
  name: string;
  /** Transport type: `"stdio"` | `"http"` | `"sse"` | `"ws"`. */
  transport: string;
  /** Executable command (only for stdio transport). */
  command?: string;
  /** Command-line arguments as a JSON array (serialised to TEXT in MySQL). */
  args: string[];
  /** Server URL (only for http/sse/ws transports). */
  url?: string;
  /** Whether this server is enabled for the runtime. Default: false. */
  enabled: boolean;
  /** Number of tools exposed by this server. Populated after runtime discovery. */
  tools_count: number;
  /** Runtime health status: `"healthy"` | `"unhealthy"` | `"configured"` | `"unknown"`. */
  status: string;
  /** Last error message if the server failed to start or crashed. */
  last_error?: string;
  /** ISO 8601 timestamp of creation (DB only, not in settings.json). */
  created_at?: string;
  /** ISO 8601 timestamp of last update (DB only, not in settings.json). */
  updated_at?: string;
  /** Authentication configuration (token value is redacted). */
  auth: BackendMcpServerAuthInfo;
}

/** Response envelope for list and stats endpoints. */
export interface BackendMcpListResponse {
  servers: BackendMcpServerInfo[];
  total: number;
}

/** Request body for POST /api/v1/mcp */
export interface BackendAddMcpServerRequest {
  name: string;
  transport: string;
  command?: string;
  args?: string[];
  url?: string;
  auth_type?: string;
  auth_token?: string;
  extra_headers?: Record<string, string>;
  timeout_ms?: number;
}

/** Request body for PUT /api/v1/mcp/:name */
export interface BackendUpdateMcpServerRequest {
  transport?: string;
  command?: string;
  args?: string[];
  url?: string;
  enabled?: boolean;
  auth_type?: string;
  auth_token?: string;
  extra_headers?: Record<string, string>;
  timeout_ms?: number;
}

/** Request body for PUT /api/v1/mcp/:name/toggle */
export interface BackendToggleMcpServerRequest {
  enabled: boolean;
}

/** Response envelope for GET /api/v1/mcp/stats */
export interface BackendMcpStatsResponse {
  total_servers: number;
  healthy_servers: number;
  total_tools: number;
  transport_distribution: Record<string, number>;
}
