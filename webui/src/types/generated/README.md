/**
 * Backend-to-Frontend type mapping layer.
 *
 * This directory establishes a contractual mapping between Rust backend types
 * and TypeScript frontend types. It is the single source of truth for
 * cross-boundary type alignment.
 *
 * ## Naming conventions
 *
 * Backend types (Rust): `PascalCase`, e.g. `SessionDto`, `McpServerEntry`
 * Frontend types (TypeScript): `camelCase` with full-word names, e.g. `sessionId`, `sessionName`
 *
 * ## Mapping rules
 *
 * 1. **Always use explicit field names** — never rely on structural equality.
 *    The backend may rename a field; this layer captures the current mapping.
 * 2. **Nullable fields**: backend `Option<T>` maps to TypeScript `T | null | undefined`.
 * 3. **Date/time fields**: backend `chrono::DateTime` maps to ISO 8601 `string`.
 *    Never use `number` (Unix timestamp) for cross-boundary dates.
 * 4. **JSON fields**: backend `serde_json::Value` maps to `unknown` or `Record<string, unknown>`.
 * 5. **Lists**: backend `Vec<T>` maps to TypeScript `T[]`.
 *
 * ## Generated types
 *
 * Types in this directory are named after the Rust struct they map from,
 * prefixed with the domain scope. For example:
 *
 * | Backend (Rust)      | Frontend (TypeScript)          |
 * |---------------------|--------------------------------|
 * | `SessionDto`       | `McpSessionDto` (in mcp.ts)   |
 * | `ApiKeyRecord`      | `ApiKeyRecord` (in apiKeys.ts) |
 * | `GitlabProject`     | `GitlabProject` (in projects.ts)|
 *
 * ## Adding new types
 *
 * 1. Add the Rust struct definition (or OpenAPI schema) to `backend-types.md`.
 * 2. Create or extend the corresponding `*.ts` file in this directory.
 * 3. Re-export from `index.ts`.
 * 4. Update `backend-types.md` with the new mapping.
 *
 * @see /rust/crates/api/src/types.rs — backend type definitions
 * @see /rust/crates/web-server/src/routes/ — route handlers
 */
