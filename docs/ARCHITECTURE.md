# AOS — System Architecture

**Version**: 1.0
**Project**: Agent OS (AOS)

---

## 1. Overview

AOS is a cloud-native, multi-tenant AI agent platform built around a Web UI, Rust API server, and pluggable agent gateway. The system consists of three main layers:

```
┌─────────────────────────────────────────────────────────────┐
│                    Frontend (React + TypeScript)              │
│              SPA in /webui, served by the backend           │
└─────────────────────────────┬───────────────────────────────┘
                              │ HTTP/WebSocket
┌─────────────────────────────▼───────────────────────────────┐
│                  API Server (Rust / Axum)                   │
│              rust/crates/web-server/src/lib.rs              │
│                                                              │
│  Auth │ Users │ Tenants │ Dashboard │ MCP │ Skills │     │
│  Hooks │ API Keys │ Chat │ Agent │ Projects │ DataSources  │
│  NL2SQL │ Sessions │ Notifications │ Uploads                 │
└─────────────────────────────┬───────────────────────────────┘
                              │ SQLite + Local File System
┌─────────────────────────────▼───────────────────────────────┐
│                    Agent Gateway (Rust)                      │
│           rust/crates/agent-gateway/src/lib.rs              │
│                                                              │
│  LLM Provider Client │ MCP Manager │ Skills Loader │       │
│  Conversation Runtime │ Token Usage Tracker                 │
└─────────────────────────────────────────────────────────────┘
```

---

## 2. Frontend Architecture

### Tech Stack
- **Framework**: React 18 + TypeScript
- **Routing**: React Router v6
- **State**: Zustand (auth store, permissions store)
- **Data Fetching**: TanStack Query v5
- **UI Library**: Ant Design v5
- **Build Tool**: Vite
- **i18n**: i18next + react-i18next

### Key Files

| Path | Purpose |
|------|---------|
| `src/App.tsx` | Route definitions, auth guard |
| `src/components/Layout.tsx` | Sidebar nav, topbar, tenant switcher |
| `src/store/auth.ts` | Authentication state (JWT, user, tenant) |
| `src/store/permissions.ts` | Role → permission mapping |
| `src/api/index.ts` | All API clients (axios-based) |
| `src/api/queryKeys.ts` | TanStack Query cache keys |
| `src/i18n.ts` | Inline i18n translations (zh-CN + en-US) |

### Permission Model (Frontend)

```
Role: superadmin > admin > developer > viewer
        ↓
  Set<Permission> loaded from permissions store
        ↓
  Menu items filtered by permission
        ↓
  UI elements hidden/disabled by permission
```

Permissions are role-based (not resource-based). The actual enforcement lives in the backend.

---

## 3. Backend Architecture

### Tech Stack
- **Web Framework**: Axum 0.7
- **Database access**: SQLx with embedded SQLite for AOS platform data
- **Auth**: JWT (jsonwebtoken crate)
- **Serialization**: Serde
- **Async Runtime**: Tokio

### Module Map

| Module | File | Responsibility |
|--------|------|----------------|
| Auth | `src/auth.rs`, `src/auth_middleware.rs` | JWT creation, verification, middleware |
| Users | `src/routes/users.rs` | User CRUD, invites, role management |
| Tenants | `src/routes/tenants.rs` | Multi-tenant CRUD, quota management |
| Dashboard | `src/routes/dashboard.rs` | Token usage aggregation, alerts |
| MCP | `src/routes/mcp.rs` | MCP server registry, hot-reload |
| Skills | `src/routes/skills.rs` | Skills disk + DB management |
| Hooks | `src/routes/hooks.rs` | Pre/post tool-use hooks |
| API Keys | `src/routes/apikeys.rs` | Encrypted key management, failover |
| Chat | `src/routes/chat.rs` | LLM chat with streaming |
| Agent | `src/routes/agent.rs` | Full agent session management |
| Projects | `src/routes/projects.rs` | GitLab project registry |
| DataSources | `src/routes/data_sources.rs` | Multi-tenant data source registry |
| NL2SQL | `src/routes/nl2sql.rs` | Natural language to SQL |
| System Events | `src/routes/system_events.rs` | WebSocket broadcast channel |

### State Management

`AppState` is cloned and shared across all request handlers:

```rust
pub struct AppState {
    pub data_dir: PathBuf,           // ~/.aos or configured
    pub db: SqlitePool,              // Local AOS platform database
    pub jwt_secret: Arc<RwLock<String>>, // Mutable JWT secret
    pub base_url: String,            // For invite URL generation
    pub default_model: String,       // Default LLM model
    pub usage_writer: Option<Arc<TokenUsageWriter>>,
    pub agent_manager: Option<Arc<AgentSessionManager>>,
    pub gitlab_manager: Option<Arc<GitlabProjectManager>>,
    pub config_registry: Option<Arc<TenantConfigRegistry>>,
}
```

---

## 4. Multi-Tenant Architecture

### Isolation Strategy: Row-Level Security

Every tenant-scoped table has a `tenant_id VARCHAR(36)` column. All queries bind `tenant_id` from the JWT claim:

```rust
// Every handler gets Claims from JWT middleware
let claims: Claims = request.extensions().get::<Claims>();
let tenant_id = &claims.tenant_id;

// Every query:
WHERE tenant_id = ?  -- always bound from JWT, never from user input
```

### Tenant Hierarchy

```
System Tenant (is_system=1)
  └── Created on first Setup
  └── Holds system-wide configuration

Regular Tenants
  └── Each has its own users, API keys, MCP servers, skills, etc.
  └── Quotas enforced at registration time (not runtime)
```

### Tenant Switch

Users with multi-tenant access see a tenant switcher in the topbar. Switching requires re-authentication with the target tenant's credentials, which issues a new JWT with the new `tenant_id`.

---

## 5. MCP Hot-Reload Architecture

MCP servers are configured in the database and hot-reloaded without restarting the server:

```
User adds MCP server via WebUI
         ↓
  POST /api/v1/mcp → DB write
         ↓
  WebUI calls agent_manager.reload_mcp_servers()
         ↓
  Agent Gateway reads all enabled MCP servers from DB
         ↓
  Spawns new MCP child processes / establishes new HTTP connections
         ↓
  Broadcasts mcp_added event via WebSocket
         ↓
  All connected clients refresh MCP status
```

This applies to: MCP server registration, updates, deletions, and enable/disable toggles.

---

## 6. Skill Hot-Reload Architecture

Skills are stored as files on disk with metadata in the database:

```
Skill Registry (DB)
  tenant_id, name, description, enabled, version, path

Skill File (Disk)
  $DATA_DIR/{tenant_id}/skills/{skill_name}/SKILL.md
  $DATA_DIR/{tenant_id}/skills/{skill_name}/commands/
```

Hot-reload triggers:
- Skill creation, update, deletion
- Skill enable/disable toggle
- On server startup (initial load)

---

## 7. Data Flow: Agent Request

```
User sends message in Agent Chat
         ↓
  POST /api/v1/agent/sessions/{id}/stream (SSE)
         ↓
  Middleware: JWT verify → extract tenant_id, user_id
         ↓
  Agent handler: load session, tenant config
         ↓
  Agent Gateway: resolve API key (tenant config → API key table)
         ↓
  Agent Gateway: load enabled MCP servers + skills for tenant
         ↓
  Conversation Runtime: build prompt with MCP tools + skill context
         ↓
  Provider Client: call LLM API (Anthropic/OpenAI/etc.)
         ↓
  Stream tokens back to client (SSE)
         ↓
  Token Usage Writer: record input/output/cache tokens to DB
         ↓
  On completion: broadcast usage stats via WebSocket
```

---

## 8. API Versioning Strategy

- Current version: **v1**
- All stable routes: `/api/v1/*`
- Breaking changes will increment to v2 with a migration period
- WebSocket endpoint: `/ws/system-events` (not versioned)

---

## 9. Security Model

### Authentication
- JWT Bearer tokens (HS256)
- 24-hour expiration
- Refresh via re-login (no refresh token — keep it simple)

### Authorization (Backend)
- Role-based: `superadmin`, `admin`, `developer`, `viewer`
- Tenant-scoped: all data filtered by `tenant_id` from JWT
- Route-level: admin/superadmin-only routes have explicit guards

### Data Protection
- API keys encrypted with AES-256-GCM before storage
- Data source connection configs encrypted with AES-256-GCM
- Encryption key stored in `$DATA_DIR/.encryption_key`
- SQL execution restricted to SELECT-only (no DDL/DML)

### Hook Security
- Hook code syntax validated before saving
- Security scan detects dangerous patterns (subprocess, eval, shell injection)
- Hooks run with configurable timeout (default 30s)
- `fail_fast` option stops pipeline on hook failure
