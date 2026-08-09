# Rust 后端代码规范

本文档定义 `web-server` crate 的代码规范，确保符合 Apache 开源标准。所有提交到此 crate 的代码均须遵循。

---

## 1. 错误处理

### 1.1 统一错误类型

使用 `thiserror` 定义自定义错误，禁止在生产路径中使用 `unwrap()` / `expect()` / `unwrap_or_default()`。

```rust
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("not found: {0}")]
    NotFound(String),

    #[error("forbidden")]
    Forbidden,

    #[error("validation error: {0}")]
    ValidationError(String),

    #[error("internal error: {0}")]
    Internal(String),

    #[error("unimplemented: {0}")]
    Unimplemented(String),
}
```

### 1.2 Handler 返回类型

所有 Handler 返回 `Result<Json<T>, AppError>`，由 `AppError` 统一转换：

```rust
async fn handler(...) -> Result<Json<Response>, AppError> {
    // ...business logic...
    Ok(Json(response))
}
```

### 1.3 禁止模式

```rust
// 禁止：unwrap/expect 在生产路径
let val = map.get("key").unwrap();

// 允许：传播错误
let val = map.get("key").ok_or_else(|| AppError::NotFound("key".into()))?;

// 禁止：unwrap_or_default
let val = map.get("key").unwrap_or_default();

// 允许：显式处理
let val = map.get("key").cloned().unwrap_or_else(|| default_value());
```

---

## 2. 数据库

### 2.1 查询接口

统一使用 `sqlx` 的 `query()` 和 `query_as()` 宏，禁止字符串拼接 SQL。

```rust
// 推荐：参数化查询
let row = sqlx::query("SELECT id, name FROM users WHERE tenant_id = ?")
    .bind(&claims.tenant_id)
    .fetch_optional(&state.db)
    .await?;

// 推荐：类型化查询
let user: (String, String) = sqlx::query_as("SELECT id, name FROM users WHERE id = ?")
    .bind(user_id)
    .fetch_one(&state.db)
    .await?;

// 推荐：类型化结构体
#[derive(sqlx::FromRow)]
struct UserRow { id: String, name: String }
let user: UserRow = sqlx::query_as("SELECT id, name FROM users WHERE id = ?")
    .bind(user_id)
    .fetch_one(&state.db)
    .await?;
```

### 2.2 禁止模式

```rust
// 禁止：字符串拼接 SQL
let sql = format!("SELECT * FROM users WHERE id = {}", user_id);

// 禁止：硬编码 SQL 字符串常量散落各处
// 应在数据访问层（DAL）封装
```

### 2.3 迁移

所有数据库迁移必须通过 SQL 文件（`migrations/` 目录）进行，禁止在代码中手动 `CREATE TABLE` / `ALTER TABLE`。

---

## 3. 异步

### 3.1 统一异步运行时

使用 `#[tokio::main]` 入口，统一 `async/await` 风格。

```rust
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // ...
    Ok(())
}
```

### 3.2 禁止混用同步/异步

禁止在 `async` 函数中使用阻塞的同步 IO（`std::fs::File`、`std::process::Command` 等），使用 `tokio::fs` 和 `tokio::process` 替代。

---

## 4. 日志

### 4.1 日志宏分级

使用 `tracing` 宏，遵循以下分级原则：

| 级别 | 场景 |
|------|------|
| `error!` | 请求失败、业务异常、不可恢复错误 |
| `warn!` | 可恢复异常、降级处理、权限拒绝 |
| `info!` | 重要业务流程节点（启动、关闭、关键操作） |
| `debug!` | 开发调试信息，生产环境默认关闭 |

### 4.2 结构化日志

```rust
// 推荐：结构化字段
tracing::info!(
    tenant_id = %claims.tenant_id,
    user_id = %claims.sub,
    data_source_id = %id,
    "data source accessed"
);

// 禁止：字符串拼接
tracing::info!("user {} accessed ds {}", claims.sub, id);
```

---

## 5. 安全

### 5.1 敏感信息加密

密码、Token、API Key 等敏感字段必须使用 AES-256-GCM 加密后存储（参见 `data_sources.rs` 中的 `encrypt_config` / `decrypt_config`）。

### 5.2 输入校验

所有用户输入在进入业务逻辑前必须校验：

```rust
// 推荐：参数校验
if req.name.trim().is_empty() {
    return Err(AppError::ValidationError("name is required".into()));
}

// 推荐：类型转换错误处理
let id: i64 = req.id.parse().map_err(|_| AppError::ValidationError("invalid id".into()))?;
```

### 5.3 权限检查

每个 Handler 必须显式检查租户隔离和用户权限：

```rust
if tenant_id != claims.tenant_id {
    return Err(AppError::Forbidden);
}
```

---

## 6. API 响应格式

### 6.1 统一包装

所有 API 响应使用统一的 `ApiResponse<T>` 结构（由 Axum 的 `Json<T>` 包装）。错误通过 `AppError` 的 `IntoResponse` 实现自动转换。

### 6.2 分页响应

列表类接口使用标准分页参数和响应：

```rust
#[derive(Debug, Deserialize)]
pub struct PaginationParams {
    pub offset: Option<usize>,
    pub limit: Option<usize>,
}
```

---

## 7. 测试

### 7.1 集成测试

每个 Handler 必须有对应的集成测试，测试用例放在 `tests/` 目录下。

### 7.2 测试覆盖

- 正常路径
- 参数校验失败
- 权限拒绝
- 资源不存在

---

## 8. 文档

### 8.1 Doc Comment

所有 public API 必须有 doc comment：

```rust
/// GET /api/v1/data-sources — list visible data sources for current user.
async fn list(...) -> Result<Json<DataSourceListResponse>> { ... }
```

### 8.2 `cargo doc`

运行 `cargo doc` 必须通过，无 broken links。

---

## 9. 依赖管理

### 9.1 依赖版本

- 所有依赖版本从 `Cargo.toml` 的 `workspace.dependencies` 引用。
- 新增依赖需指定版本范围，禁止使用 `*` 或不固定版本。

### 9.2 Feature 标志

可选依赖使用 feature 标志控制，避免不必要的编译开销。

---

## 10. 模块组织

```
web-server/src/
├── routes/           # API 路由（一个文件一个资源域）
│   ├── mod.rs
│   ├── data_sources.rs
│   ├── nl2sql.rs
│   └── ...
├── auth.rs           # JWT 认证
├── auth_middleware.rs # Axum 认证中间件
├── error.rs          # 统一错误类型定义
├── state.rs          # AppState 应用状态
├── config.rs         # 配置加载
└── main.rs           # 入口
```

原则：路由层只处理 HTTP 协议转换，业务逻辑下沉到独立模块。
