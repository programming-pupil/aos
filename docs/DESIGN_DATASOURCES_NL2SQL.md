# AOS Data Sources & NL2SQL 架构设计文档

**版本**: v1.0
**日期**: 2026-04-22
**状态**: Phase 3 — 设计文档（供 Apache 代码审核参考）
**项目**: Agent OS (AOS)

---

## 1. 整体系统架构

```
┌─────────────────────────────────────────────────────────────────────────────────┐
│                                   Frontend (React/TypeScript)                   │
│  ┌──────────────────────┐  ┌──────────────────────┐                            │
│  │   DataSources 页面    │  │    NL2sql 页面       │  Ant Design + TanStack Query │
│  │  /datasources        │  │  /nl2sql             │                            │
│  └──────────┬───────────┘  └──────────┬───────────┘                            │
└──────────────┼──────────────────────────┼────────────────────────────────────────┘
               │                          │
               │  REST API (JWT Auth)     │
               ▼                          ▼
┌─────────────────────────────────────────────────────────────────────────────────┐
│                                   API Layer (Axum)                              │
│  ┌──────────────────────────────────────────────────────────────────────────┐  │
│  │                    web-server /routes/mod.rs                              │  │
│  │  /api/v1/data-sources/*          │  /api/v1/nl2sql/*                     │  │
│  │    GET/POST/PATCH/DELETE          │    POST /query                       │  │
│  │    POST /{id}/test               │    POST /execute                      │  │
│  │    POST /{id}/discover           │    GET  /history                      │  │
│  └──────────────────────┬───────────┴──────────────────────┬──────────────────┘  │
│                          │                               │                        │
│                          ▼                               ▼                        │
│  ┌──────────────────────┴───────────────────────────────┴──────────────────┐    │
│  │                routes/data_sources.rs  │  routes/nl2sql.rs              │    │
│  │  · AES-256-GCM 配置加密              │  · LLM 调用 (ProviderClient)   │    │
│  │  · 租户 + 权限验证                  │  · SQL 安全性检查               │    │
│  │  · schema auto-discovery            │  · 查询历史持久化                │    │
│  └──────────────────────┬───────────────┬┴──────────────────────┬───────────┘    │
│                          │               │                       │                 │
└──────────────────────────┼───────────────┼───────────────────────┼─────────────────┘
                           │               │                       │
        ┌──────────────────┴──┐            │         ┌────────────┴──────────────────┐
        │   Dynamic Drivers   │            │         │    Dynamic Drivers             │
        │                    │            │         │                                │
        │  MySQL/TiDB ───────┼────────────┤         │  MySQL/TiDB ────────────────┤
        │  PostgreSQL        │            │         │  (query execution only)       │
        │  ClickHouse        │            │         │                                │
        │  HTTP API          │            │         └────────────────────────────────┘
        │  MCP Server        │            │
        └────────────────────┘            │
                                          ▼
                               ┌──────────────────────────┐
                               │  LLM Provider (Agent GW) │
                               │  Anthropic / OpenAI /    │
                               │  OpenRouter / Gemini...  │
                               └──────────────────────────┘
```

**核心设计原则**：
- **零信任架构**：每个 API 请求都经过 JWT 认证 + 租户隔离验证
- **最小权限原则**：SQL 执行只允许 SELECT，禁止所有 DDL/DML
- **敏感数据保护**：连接密码、API Token 等使用 AES-256-GCM 加密存储
- **优雅降级**：SMTP 未配置时邀请流程仍可工作（显示 URL）

---

## 2. 数据模型 ER 图

```
┌──────────────────────────────────────────────────────────────────────────────────┐
│                                tenants (租户)                                     │
│  id (PK), name, slug, plan, max_users, max_tokens_monthly                     │
└────────────────────────────────┬─────────────────────────────────────────────────┘
                                 │
                                 │ 1:N
                                 ▼
┌──────────────────────────────────────────────────────────────────────────────────┐
│                              users (用户)                                          │
│  id (PK), tenant_id (FK), email, password_hash, role, permission_mode,         │
│  invite_token, is_active, created_at                                              │
└────────────────────────────────┬─────────────────────────────────────────────────┘
                                 │
                                 │
          ┌──────────────────────┼──────────────────────┐
          │ 1:N                 │ 1:N                  │ 1:N
          ▼                     ▼                      ▼
┌─────────────────────┐  ┌─────────────────────┐  ┌─────────────────────────────┐
│  data_sources       │  │  nl2sql_queries    │  │  api_keys                  │
│  (数据源注册表)      │  │  (查询历史)          │  │  (API Key 管理)            │
│                     │  │                     │  │                            │
│  id (PK)           │  │  id (PK)            │  │  id (PK)                   │
│  tenant_id (FK)    │  │  tenant_id (FK)     │  │  tenant_id (FK)            │
│  user_id (FK/NULL) │  │  user_id (FK)       │  │  key_hash                  │
│  name              │◄─┤  data_source_id (FK) │  │  provider                  │
│  description       │  │  question           │  └─────────────────────────────┘
│  db_type           │  │  generated_sql       │
│  visibility        │  │  executed            │
│  config (ENCRYPTED)│  │  rows_returned       │
│  schema_info (JSON)│  │  execution_ms        │
│  enabled          │  │  error_message       │
│  last_tested_at    │  │  created_at          │
│  last_error        │  └─────────────────────┘
│  created_by        │
└────────┬──────────┘
         │ 1:N (optional pre-cache)
         ▼
┌─────────────────────────────┐
│  data_source_schemas        │
│  (表结构预缓存)             │
│                             │
│  id (PK)                   │
│  data_source_id (FK)        │
│  table_name                │
│  columns (JSON)            │
│  row_count                 │
│  sampled_at                │
└─────────────────────────────┘
```

### 字段说明

| 表 | 字段 | 类型 | 说明 |
|---|---|---|---|
| `data_sources` | `user_id` | VARCHAR(64) NULL | NULL = 租户级共享；非NULL = 用户私有 |
| `data_sources` | `visibility` | VARCHAR(16) | `'tenant'` = 租户可见；`'private'` = 仅创建者 |
| `data_sources` | `config` | JSON | 加密后的连接配置（AES-256-GCM） |
| `data_sources` | `schema_info` | JSON | 缓存的 schema（表 + 列信息） |
| `data_source_schemas` | `columns` | JSON | `[{name, type, nullable, primary_key}]` |
| `nl2sql_queries` | `executed` | TINYINT(1) | 0=仅生成, 1=已执行 |
| `nl2sql_queries` | `data_source_id` | FK NULL | 允许跨数据源查询历史 |

### 索引策略

```
data_sources:         idx_ds_tenant, idx_ds_user, uk_tenant_user_name
data_source_schemas:  uk_ds_table (data_source_id, table_name), idx_ds_schema_ds
nl2sql_queries:       idx_nl2sql_tenant, idx_nl2sql_user, idx_nl2sql_ds, idx_nl2sql_created
```

---

## 3. NL2SQL LLM 调用流程时序图

```
┌────────┐       ┌──────────────┐      ┌────────────────┐     ┌─────────────┐
│ Client │       │  nl2sql.rs   │      │ ProviderClient │     │  LLM API    │
└────┬───┘       └───────┬──────┘      └───────┬────────┘     └──────┬──────┘
     │  POST /query       │                    │                     │
     │  {ds_id, question} │                    │                     │
     │───────────────────►│                    │                     │
     │                    │  validate_access() │                     │
     │                    │──────────────────► │                     │
     │                    │◄────────────────── │                     │
     │                    │                    │                     │
     │                    │  fetch schema_info  │                     │
     │                    │───────────────────► │                     │
     │                    │◄─────────────────── │                     │
     │                    │                    │                     │
     │                    │  build_nl2sql_prompt(schema)            │
     │                    │  ──────────────────┼─────────────────────│
     │                    │  System: 你是一个SQL专家...             │
     │                    │  User: 问题: {question}                 │
     │                    │  Generate SQL:                          │
     │                    │                    │ send_message()   │
     │                    │                    │─────────────────► │
     │                    │                    │◄───────────────── │
     │                    │                    │ {content: SQL}   │
     │                    │                    │                   │
     │                    │  parse SQL, trim markdown fences       │
     │                    │  ───────────────────────────────────────│
     │                    │  SQL = "SELECT ..."                    │
     │                    │                    │                   │
     │                    │  INSERT nl2sql_queries (executed=0)   │
     │                    │─────────────────────────────────────────│
     │                    │                    │                   │
     │  {sql, query_id}   │                    │                   │
     │◄───────────────────│                    │                   │
     │                    │                    │                   │
     │  POST /execute     │                    │                   │
     │  {query_id, sql}   │                    │                   │
     │───────────────────►│                    │                   │
     │                    │  is_safe_sql(sql)?  (SELECT only check) │
     │                    │──────────────────► │                   │
     │                    │◄────────────────── │ (ALLOW/DENY)      │
     │                    │                    │                   │
     │                    │  decrypt config (AES-256-GCM)           │
     │                    │  ───────────────────────────────────────│
     │                    │                    │                   │
     │                    │  sqlx::query(sql)                       │
     │                    │─────────────────────────────────────────►
     │                    │◄────────────────────────────────────────
     │                    │  rows, columns                         │
     │                    │                    │                   │
     │                    │  UPDATE nl2sql_queries (executed=1)   │
     │                    │─────────────────────────────────────────│
     │                    │                    │                   │
     │  {columns, rows}   │                    │                   │
     │◄───────────────────│                    │                   │
```

---

## 4. 多租户隔离策略

### 4.1 数据隔离模型

AOS 采用**行级租户隔离**（Row-Level Tenant Isolation）：

```
every query ALWAYS contains:  WHERE tenant_id = '{current_tenant}'
every INSERT always binds:    tenant_id = '{current_tenant}'
```

**JWT 声明结构**：
```json
{
  "sub": "user_uuid",
  "tenant_id": "tenant_uuid",
  "role": "admin|developer|viewer",
  "permission_mode": "workspace_write|workspace_read|full_access",
  "exp": 1234567890
}
```

### 4.2 隔离层级

| 层级 | 范围 | 访问控制 |
|---|---|---|
| Superadmin | 全系统 | `role == "superadmin"` — bypass tenant check |
| Admin | 租户内所有资源 | `role == "admin"` — 读写租户级数据源 |
| Developer | 私有 + 租户级读取 | `role == "developer"` — 读写私有数据源 |
| Viewer | 租户内只读 | `role == "viewer"` — 只读租户级数据源 |

### 4.3 租户切换机制

```
用户登录 → JWT 包含 tenant_id → 每个请求自动携带

切换租户：
  前端显示租户选择器 → 用户选择目标租户
  → 弹出 re-login 模态框 → 用户输入目标租户账号密码
  → 后端签发新 JWT (含目标 tenant_id)
  → 前端更新 auth store → 刷新页面
```

**设计决策**：不实现跨租户 token 刷新，而是要求重新认证。这是安全最佳实践，避免了 token 劫持攻击面。

### 4.4 数据源可见性

```
NL2SQL 查询时的数据源范围：
  = {租户级共享数据源 (user_id = NULL)} 
    ∪ {当前用户私有数据源 (user_id = current_user)}

DataSources 列表页：
  Tab「租户共享」: user_id = NULL, role ∈ {admin, superadmin} 可管理
  Tab「我的私有」: user_id = current_user
```

---

## 5. 安全模型

### 5.1 敏感信息加密

**加密范围**：`data_sources.config` JSON 中的所有敏感字段

**加密算法**：AES-256-GCM
- Nonce: 12 字节（每次加密随机生成）
- Key: 从 `~/.aos/.encryption_key` 文件读取 32 字节十六进制密钥
- Dev 模式：若密钥文件不存在，使用全零填充密钥（仅用于本地开发）

**加密后的 JSON 结构**：
```json
{
  "_encrypted": true,
  "nonce": "base64(nonce_bytes)",
  "data": "base64(nonce || ciphertext)"
}
```

**加密字段自动识别**：
```
password, auth_token, secret, api_key, token, private_key
```

### 5.2 SQL 注入防护

**第一层：语法限制**（`is_safe_sql` 函数）
```rust
fn is_safe_sql(sql: &str) -> bool {
    let sql = sql.trim().to_uppercase();
    !["INSERT", "UPDATE", "DELETE", "DROP", "TRUNCATE",
      "ALTER", "CREATE", "GRANT", "REVOKE"]
        .iter()
        .any(|kw| sql.contains(kw))
}
```

**第二层：执行时限制**
- 30 秒查询超时
- 最多 10,000 行结果限制
- 仅支持 `SELECT` 语句
- 每个数据源独立的连接池（隔离会话）

**第三层：schema 注入**
- LLM 仅能看到 `schema_info` 中已注册的表结构
- 无法推断数据库中其他表的存在

### 5.3 权限模型

**DataSources 权限**：
```
datasources:read   — 列出和查看数据源
datasources:write  — 创建和更新数据源
datasources:delete — 删除数据源（私有: 仅创建者; 租户级: admin）
```

**NL2SQL 权限**：
```
nl2sql:read  — 使用 NL2SQL 查询
nl2sql:write — 执行生成的 SQL
```

**角色权限映射**：
| 角色 | datasources | nl2sql |
|---|---|---|
| superadmin | 全部租户全部权限 | 全部 |
| admin | CRUD 租户级 + 私有读 | 读写 |
| developer | CRUD 私有 + 租户级读 | 读写 |
| viewer | 租户级只读 | 读 |

### 5.4 API 认证

```
所有 /api/v1/* 路由均需要：
  1. Authorization: Bearer <JWT>
  2. JWT 验证签名 + 检查 exp
  3. 从 JWT 提取 tenant_id 和 user_id

例外路由（无需认证）：
  POST /api/v1/auth/login
  POST /api/v1/auth/register
  POST /api/v1/auth/accept-invite  (使用 invite_token 而非 JWT)
```

### 5.5 SMTP 安全

- 连接支持 SMTPS (port 465) 和 STARTTLS (port 587)
- 凭证存储在环境变量，不写入配置文件
- 邮件内容不包含敏感操作细节

---

## 6. 扩展点

### 6.1 新增数据源类型

在 `db_type` 枚举中添加新类型后，需要修改：

**1. `routes/data_sources.rs` — `test_connection` 函数**
```rust
match db_type.as_str() {
    "mysql" | "tidb" => { /* MySQL 连接测试 */ }
    "postgres" => { /* PostgreSQL 连接测试 */ }
    "http_api" => { /* HTTP health check */ }
    "mcp" => { /* MCP ping */ }
    // + 新增类型
    _ => Err(AppError::ValidationError("unsupported db_type".into())),
}
```

**2. `routes/data_sources.rs` — `discover_schema` 函数**
```rust
match db_type.as_str() {
    "mysql" | "tidb" => { /* SHOW TABLES + DESCRIBE */ }
    "postgres" => { /* information_schema 查询 */ }
    // + 新增类型
    _ => Err(AppError::ValidationError("schema discovery not supported".into())),
}
```

**3. `routes/nl2sql.rs` — `execute` 函数**
```rust
match db_type.as_str() {
    "mysql" | "tidb" => { /* sqlx MySQL 执行 */ }
    "http_api" => { /* HTTP API 执行（通过 query_template）*/ }
    // + 新增类型
    _ => Err(AppError::ValidationError("execution not supported".into())),
}
```

### 6.2 新增 LLM Provider

NL2SQL 使用现有的 `api::ProviderClient` 抽象，新增 provider 只需：

```rust
// 在 api/crates/providers/ 中添加新的 ProviderKind
// NL2SQL 无需修改代码，自动支持所有 ProviderClient 支持的模型
```

### 6.3 新增权限

```rust
// webui/src/store/permissions.ts
export type Permission =
  | 'datasources:read'
  | 'datasources:write'
  | 'datasources:delete'
  | 'nl2sql:read'
  | 'nl2sql:write'
  | 'pipeline:read'
  | 'pipeline:write';

// 已在 ROLE_PERMISSIONS 中为 admin/developer/viewer/superadmin 分配
```

---

## 7. 配置参考

### 7.1 SMTP 环境变量

```bash
SMTP_HOST=smtp.example.com
SMTP_PORT=465
SMTP_USE_TLS=true          # true = SMTPS/STARTTLS, false = plain TCP
SMTP_USERNAME=noreply@aos.ai
SMTP_PASSWORD=secret
SMTP_FROM=AOS <noreply@aos.ai>
```

### 7.2 加密密钥

```bash
# 生成 32 字节十六进制密钥
openssl rand -hex 32 > ~/.aos/.encryption_key
chmod 600 ~/.aos/.encryption_key
```

### 7.3 默认模型

```bash
DEFAULT_MODEL=claude-sonnet-4-20250514
```

---

## 8. 实施状态

| 阶段 | 任务 | 状态 |
|---|---|---|
| **Phase 1** | 菜单修复（Pipeline/NL2SQL/DataSources） | ✅ 完成 |
| **Phase 1** | permission_mode 硬编码修复 | ✅ 完成 |
| **Phase 1** | invite URL 格式修复 | ✅ 完成 |
| **Phase 1** | accept_invite 后端实现 | ✅ 完成 |
| **Phase 1** | SMTP 邮件发送接入 | ✅ 完成 |
| **Phase 2** | Migration 022（data_sources + schemas） | ✅ 完成 |
| **Phase 2** | Migration 023（nl2sql_queries） | ✅ 完成 |
| **Phase 2** | 后端 DataSources 路由 | ✅ 完成 |
| **Phase 2** | 后端 NL2SQL 路由 | ✅ 完成 |
| **Phase 2** | 前端 DataSources 页面 | ✅ 完成 |
| **Phase 2** | 前端 NL2SQL 页面 | ✅ 完成 |
| **Phase 3** | 设计文档（本文档） | ✅ 完成 |

---

## 9. 文件清单

### 后端新增文件

| 文件 | 说明 |
|---|---|
| `routes/data_sources.rs` | 数据源 CRUD + 连接测试 + Schema 发现 |
| `routes/nl2sql.rs` | NL2SQL 查询生成 + 执行 + 历史 |
| `email.rs` | SMTP 邮件发送（邀请邮件） |
| `config/email.rs` | SMTP 配置加载 |
| `migrations/022_data_sources.sql` | data_sources + data_source_schemas 表 |
| `migrations/023_nl2sql_queries.sql` | nl2sql_queries 表 |

### 前端新增/修改文件

| 文件 | 说明 |
|---|---|
| `pages/DataSources.tsx` | 数据源管理页面（完整重写） |
| `pages/Nl2sql.tsx` | NL2SQL 交互页面（完整重写） |
| `components/Layout.tsx` | 添加 Pipeline/NL2SQL/DataSources 菜单 |
| `store/auth.ts` | 新增 switchTenant 方法 |
| `store/permissions.ts` | 新增 datasources/nl2sql/pipeline 权限 |
| `api/index.ts` | 新增 dataSourcesApi + nl2sqlApi |
| `api/queryKeys.ts` | 新增 query key 定义 |
| `types/index.ts` | 新增 DataSource + NL2SQL 类型定义 |
| `i18n.ts` | 新增所有相关 i18n key |

### 后端修改文件

| 文件 | 说明 |
|---|---|
| `routes/users.rs` | 修复 permission_mode + invite URL + accept_invite |
| `routes/auth/mod.rs` | 新增 accept-invite 路由 |
| `lib.rs` | 注册新路由模块 |
| `Cargo.toml` | 新增 lettre 依赖 |
| `web-server/Cargo.toml` | 新增 lettre 依赖 |
