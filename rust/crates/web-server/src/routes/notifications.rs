//! Notifications API — read/mark notifications for the current user.
//!
//! ## Security
//! All routes are scoped to the authenticated user's `tenant_id` and `user_id`.

use axum::{
    extract::{Extension, Query, State},
    routing::{delete as routing_delete, get as routing_get, patch as routing_patch},
    Json, Router,
};
use serde::{Deserialize, Serialize};

use crate::auth::Claims;
use crate::error::Result;
use crate::state::AppState;

// ── DTOs ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct NotificationInfo {
    pub id: String,
    pub title: String,
    pub body: String,
    pub level: String,
    pub read: bool,
    pub created_at: String,
}

#[derive(Debug, Serialize)]
pub struct NotificationListResponse {
    pub notifications: Vec<NotificationInfo>,
    pub total: usize,
    pub unread_count: usize,
}

#[derive(Debug, Deserialize)]
pub struct MarkReadRequest {
    pub read: bool,
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn row_to_info(
    (id, title, body, level, read, created_at): (
        String,
        String,
        String,
        String,
        bool,
        chrono::DateTime<chrono::Utc>,
    ),
) -> NotificationInfo {
    NotificationInfo {
        id,
        title,
        body,
        level,
        read,
        created_at: created_at.to_rfc3339(),
    }
}

// ── Route handlers ───────────────────────────────────────────────────────────

/// GET /api/v1/notifications — list all notifications for the current user.
async fn list(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(params): Query<NotificationListParams>,
) -> Result<Json<NotificationListResponse>> {
    let page = params.page.unwrap_or(1).max(1);
    let per_page = params.per_page.unwrap_or(20).clamp(1, 100);
    let offset = i64::from(page.saturating_sub(1).saturating_mul(per_page));
    let limit = i64::from(per_page);

    let read_filter = match params.read.as_deref() {
        Some("true" | "1") => "WHERE effective_read = 1",
        Some("false" | "0") => "WHERE effective_read = 0",
        _ => "",
    };

    let visible_cte = format!(
        "WITH scoped AS (
           SELECT n.id, n.title, n.body, n.level, n.created_at,
                  CASE WHEN n.user_id IS NULL THEN COALESCE(r.`read`, 0) ELSE n.`read` END AS effective_read
           FROM notifications n
           LEFT JOIN notification_receipts r
             ON r.notification_id = n.id AND r.tenant_id = n.tenant_id AND r.user_id = ?
           WHERE n.tenant_id = ? AND (n.user_id = ? OR n.user_id IS NULL)
             AND (n.user_id IS NOT NULL OR r.deleted_at IS NULL)
         ), visible AS (
           SELECT scoped.*,
                  ROW_NUMBER() OVER (
                    PARTITION BY title, body, level, strftime('%Y-%m-%d %H:%M:%S', created_at)
                    ORDER BY effective_read ASC, created_at DESC, id DESC
                  ) AS duplicate_rank
           FROM scoped {read_filter}
         )"
    );
    let query = format!(
        "{visible_cte}
         SELECT id, title, body, level, effective_read, created_at
         FROM visible WHERE duplicate_rank = 1
         ORDER BY created_at DESC LIMIT ? OFFSET ?"
    );

    let rows = sqlx::query_as::<
        _,
        (
            String,
            String,
            String,
            String,
            bool,
            chrono::DateTime<chrono::Utc>,
        ),
    >(&query)
    .bind(&claims.sub)
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.db)
    .await?;

    let notifications: Vec<NotificationInfo> = rows.into_iter().map(row_to_info).collect();

    let total: (i64,) = sqlx::query_as(&format!(
        "{visible_cte} SELECT COUNT(*) FROM visible WHERE duplicate_rank = 1"
    ))
    .bind(&claims.sub)
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .fetch_one(&state.db)
    .await?;

    let unread: (i64,) = sqlx::query_as(
        "WITH scoped AS (
           SELECT n.id, n.title, n.body, n.level, n.created_at,
                  CASE WHEN n.user_id IS NULL THEN COALESCE(r.`read`, 0) ELSE n.`read` END AS effective_read
           FROM notifications n
           LEFT JOIN notification_receipts r
             ON r.notification_id = n.id AND r.tenant_id = n.tenant_id AND r.user_id = ?
           WHERE n.tenant_id = ? AND (n.user_id = ? OR n.user_id IS NULL)
             AND (n.user_id IS NOT NULL OR r.deleted_at IS NULL)
         ), visible AS (
           SELECT scoped.*,
                  ROW_NUMBER() OVER (
                    PARTITION BY title, body, level, strftime('%Y-%m-%d %H:%M:%S', created_at)
                    ORDER BY effective_read ASC, created_at DESC, id DESC
                  ) AS duplicate_rank
           FROM scoped
         )
         SELECT COUNT(*) FROM visible WHERE duplicate_rank = 1 AND effective_read = 0",
    )
    .bind(&claims.sub)
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .fetch_one(&state.db)
    .await?;

    Ok(Json(NotificationListResponse {
        total: usize::try_from(total.0).unwrap_or(0),
        unread_count: usize::try_from(unread.0).unwrap_or(0),
        notifications,
    }))
}

/// PATCH /api/v1/notifications/:id/read — mark a notification as read/unread.
async fn mark_read(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(req): Json<MarkReadRequest>,
) -> Result<Json<serde_json::Value>> {
    let mut tx = state.db.begin().await?;
    sqlx::query(
        "UPDATE notifications SET `read` = ?
         WHERE tenant_id = ? AND user_id = ?
           AND EXISTS (
             SELECT 1 FROM notifications anchor
             WHERE anchor.id = ? AND anchor.tenant_id = notifications.tenant_id
               AND (anchor.user_id = ? OR anchor.user_id IS NULL)
               AND anchor.title = notifications.title
               AND anchor.body = notifications.body
               AND anchor.level = notifications.level
               AND strftime('%Y-%m-%d %H:%M:%S', anchor.created_at)
                   = strftime('%Y-%m-%d %H:%M:%S', notifications.created_at)
           )",
    )
    .bind(i32::from(req.read))
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .bind(&id)
    .bind(&claims.sub)
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        "INSERT INTO notification_receipts
           (notification_id, tenant_id, user_id, `read`, read_at, deleted_at, updated_at)
         SELECT n.id, n.tenant_id, ?, ?,
                CASE WHEN ? = 1 THEN CURRENT_TIMESTAMP ELSE NULL END,
                NULL, CURRENT_TIMESTAMP
         FROM notifications n
         INNER JOIN notifications anchor
           ON anchor.id = ? AND anchor.tenant_id = n.tenant_id
          AND anchor.title = n.title AND anchor.body = n.body AND anchor.level = n.level
          AND strftime('%Y-%m-%d %H:%M:%S', anchor.created_at)
              = strftime('%Y-%m-%d %H:%M:%S', n.created_at)
         WHERE n.tenant_id = ? AND n.user_id IS NULL
           AND (anchor.user_id = ? OR anchor.user_id IS NULL)
         ON CONFLICT(notification_id, user_id) DO UPDATE SET
           `read` = excluded.`read`, read_at = excluded.read_at,
           deleted_at = notification_receipts.deleted_at, updated_at = CURRENT_TIMESTAMP",
    )
    .bind(&claims.sub)
    .bind(i32::from(req.read))
    .bind(i32::from(req.read))
    .bind(&id)
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(Json(serde_json::json!({ "id": id, "read": req.read })))
}

/// POST /api/v1/notifications/mark-all-read — mark all notifications as read.
async fn mark_all_read(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> Result<Json<serde_json::Value>> {
    let mut tx = state.db.begin().await?;
    sqlx::query(
        "UPDATE notifications SET `read` = 1
         WHERE tenant_id = ? AND user_id = ? AND `read` = 0",
    )
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        "INSERT INTO notification_receipts
           (notification_id, tenant_id, user_id, `read`, read_at, updated_at)
         SELECT n.id, n.tenant_id, ?, 1, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP
         FROM notifications n
         LEFT JOIN notification_receipts existing
           ON existing.notification_id = n.id AND existing.user_id = ?
         WHERE n.tenant_id = ? AND n.user_id IS NULL AND existing.deleted_at IS NULL
         ON CONFLICT(notification_id, user_id) DO UPDATE SET
           `read` = 1, read_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP",
    )
    .bind(&claims.sub)
    .bind(&claims.sub)
    .bind(&claims.tenant_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(Json(serde_json::json!({ "success": true })))
}

/// DELETE /api/v1/notifications/:id — delete a notification.
async fn delete(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<serde_json::Value>> {
    let mut tx = state.db.begin().await?;
    // Materialize broadcast receipts before deleting a possibly-private anchor
    // row used to identify its collapsed duplicate group.
    sqlx::query(
        "INSERT INTO notification_receipts
           (notification_id, tenant_id, user_id, `read`, read_at, deleted_at, updated_at)
         SELECT n.id, n.tenant_id, ?, 1, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP
         FROM notifications n
         INNER JOIN notifications anchor
           ON anchor.id = ? AND anchor.tenant_id = n.tenant_id
          AND anchor.title = n.title AND anchor.body = n.body AND anchor.level = n.level
          AND strftime('%Y-%m-%d %H:%M:%S', anchor.created_at)
              = strftime('%Y-%m-%d %H:%M:%S', n.created_at)
         WHERE n.tenant_id = ? AND n.user_id IS NULL
           AND (anchor.user_id = ? OR anchor.user_id IS NULL)
         ON CONFLICT(notification_id, user_id) DO UPDATE SET
           `read` = 1, read_at = COALESCE(notification_receipts.read_at, CURRENT_TIMESTAMP),
           deleted_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP",
    )
    .bind(&claims.sub)
    .bind(&id)
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        "DELETE FROM notifications
         WHERE tenant_id = ? AND user_id = ?
           AND EXISTS (
             SELECT 1 FROM notifications anchor
             WHERE anchor.id = ? AND anchor.tenant_id = notifications.tenant_id
               AND (anchor.user_id = ? OR anchor.user_id IS NULL)
               AND anchor.title = notifications.title
               AND anchor.body = notifications.body
               AND anchor.level = notifications.level
               AND strftime('%Y-%m-%d %H:%M:%S', anchor.created_at)
                   = strftime('%Y-%m-%d %H:%M:%S', notifications.created_at)
           )",
    )
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .bind(&id)
    .bind(&claims.sub)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(Json(serde_json::json!({ "deleted": true, "id": id })))
}

// ── Router ──────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct NotificationListParams {
    pub page: Option<u32>,
    pub per_page: Option<u32>,
    pub read: Option<String>,
}

pub fn routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/", routing_get(list))
        .route("/mark-all-read", routing_patch(mark_all_read))
        .route("/{id}/read", routing_patch(mark_read))
        .route("/{id}", routing_delete(delete))
        .layer(axum::middleware::from_fn_with_state(
            state,
            crate::auth_middleware::require_auth,
        ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sqlite_test_state(db: sqlx::SqlitePool) -> AppState {
        AppState {
            data_dir: std::env::temp_dir(),
            platform_lifecycle: None,
            control_db: db.clone(),
            telemetry_db: db.clone(),
            #[cfg(feature = "pm")]
            pm_telemetry: crate::routes::agent::PmTelemetrySink::for_test(),
            db,
            jwt_secret: std::sync::Arc::new(tokio::sync::RwLock::new("test".repeat(8))),
            base_url: "http://localhost".to_string(),
            default_model: "test-model".to_string(),
            setup_initialized_cache: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            usage_writer: None,
            agent_manager: None,
            #[cfg(feature = "projects")]
            gitlab_manager: None,
            config_registry: None,
            #[cfg(feature = "nl2sql")]
            nl2sql_embedding_store: None,
            #[cfg(feature = "rd")]
            rd_embedding_store: None,
            #[cfg(feature = "nl2sql")]
            nl2sql_routing_engine: None,
            #[cfg(feature = "nl2sql")]
            nl2sql_pool_cache: std::sync::Arc::new(crate::nl2sql::datasource_pool::PoolCache::new()),
            #[cfg(feature = "nl2sql")]
            nl2sql_rate_limiter: std::sync::Arc::new(
                crate::nl2sql::rate_limiter::TenantRateLimiter::default(),
            ),
        }
    }

    async fn notification_fixture() -> (AppState, Claims, String) {
        let db = crate::test_sqlite_pool().await;
        let tenant_id = format!("tenant-{}", uuid::Uuid::new_v4());
        let user_id = format!("user-{}", uuid::Uuid::new_v4());
        sqlx::query("INSERT INTO tenants (id, name, slug, plan) VALUES (?, 'Test', ?, 'free')")
            .bind(&tenant_id)
            .bind(format!("test-{}", uuid::Uuid::new_v4()))
            .execute(&db)
            .await
            .expect("tenant fixture");
        sqlx::query(
            "INSERT INTO users (id, email, password_hash, tenant_id)
             VALUES (?, ?, 'not-used', ?)",
        )
        .bind(&user_id)
        .bind(format!("{user_id}@example.invalid"))
        .bind(&tenant_id)
        .execute(&db)
        .await
        .expect("user fixture");
        let claims = Claims::new(&user_id, "user@example.invalid", "user", &tenant_id);
        (sqlite_test_state(db), claims, user_id)
    }

    #[tokio::test]
    async fn duplicate_notifications_are_collapsed_and_mutated_as_one_group() {
        let (state, claims, user_id) = notification_fixture().await;
        for id in ["notice-a", "notice-b"] {
            sqlx::query(
                "INSERT INTO notifications
                   (id, tenant_id, user_id, title, body, level, `read`, created_at)
                 VALUES (?, ?, ?, 'Task complete', 'The same result', 'success', 0,
                         '2026-07-31 12:00:00.100')",
            )
            .bind(id)
            .bind(&claims.tenant_id)
            .bind(&user_id)
            .execute(&state.db)
            .await
            .expect("notification fixture");
        }

        let Json(first_page) = list(
            State(state.clone()),
            Extension(claims.clone()),
            Query(NotificationListParams {
                page: Some(1),
                per_page: Some(20),
                read: None,
            }),
        )
        .await
        .expect("list notifications");
        assert_eq!(first_page.total, 1);
        assert_eq!(first_page.unread_count, 1);
        assert_eq!(first_page.notifications.len(), 1);

        let visible_id = first_page.notifications[0].id.clone();
        let _ = mark_read(
            State(state.clone()),
            Extension(claims.clone()),
            axum::extract::Path(visible_id.clone()),
            Json(MarkReadRequest { read: true }),
        )
        .await
        .expect("mark duplicate group read");
        let unread: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM notifications WHERE tenant_id = ? AND user_id = ? AND `read` = 0",
        )
        .bind(&claims.tenant_id)
        .bind(&user_id)
        .fetch_one(&state.db)
        .await
        .expect("count unread");
        assert_eq!(unread, 0);

        let _ = delete(
            State(state.clone()),
            Extension(claims.clone()),
            axum::extract::Path(visible_id),
        )
        .await
        .expect("delete duplicate group");
        let remaining: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM notifications WHERE tenant_id = ? AND user_id = ?",
        )
        .bind(&claims.tenant_id)
        .bind(&user_id)
        .fetch_one(&state.db)
        .await
        .expect("count remaining");
        assert_eq!(remaining, 0);
    }

    #[tokio::test]
    async fn notification_mutations_cannot_cross_user_boundaries() {
        let (state, claims, _user_id) = notification_fixture().await;
        let other_user = format!("other-{}", uuid::Uuid::new_v4());
        sqlx::query(
            "INSERT INTO notifications (id, tenant_id, user_id, title, body, level)
             VALUES ('other-notice', ?, ?, 'Private', 'Other user', 'info')",
        )
        .bind(&claims.tenant_id)
        .bind(&other_user)
        .execute(&state.db)
        .await
        .expect("other notification fixture");

        let _ = delete(
            State(state.clone()),
            Extension(claims),
            axum::extract::Path("other-notice".to_string()),
        )
        .await
        .expect("unauthorized id stays a no-op");
        let exists: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM notifications WHERE id = 'other-notice' AND user_id = ?",
        )
        .bind(&other_user)
        .fetch_one(&state.db)
        .await
        .expect("count other notification");
        assert_eq!(exists, 1);
    }

    #[tokio::test]
    async fn broadcast_read_and_delete_state_is_independent_per_user() {
        let (state, first_claims, _) = notification_fixture().await;
        let second_user_id = format!("user-{}", uuid::Uuid::new_v4());
        sqlx::query(
            "INSERT INTO users (id, email, password_hash, tenant_id)
             VALUES (?, ?, 'not-used', ?)",
        )
        .bind(&second_user_id)
        .bind(format!("{second_user_id}@example.invalid"))
        .bind(&first_claims.tenant_id)
        .execute(&state.db)
        .await
        .expect("second user fixture");
        let second_claims = Claims::new(
            &second_user_id,
            "second@example.invalid",
            "user",
            &first_claims.tenant_id,
        );
        sqlx::query(
            "INSERT INTO notifications (id, tenant_id, user_id, title, body, level, `read`)
             VALUES ('tenant-broadcast', ?, NULL, 'Broadcast', 'For everyone', 'info', 0)",
        )
        .bind(&first_claims.tenant_id)
        .execute(&state.db)
        .await
        .expect("broadcast fixture");

        let _ = mark_read(
            State(state.clone()),
            Extension(first_claims.clone()),
            axum::extract::Path("tenant-broadcast".to_string()),
            Json(MarkReadRequest { read: true }),
        )
        .await
        .expect("first user marks broadcast read");

        let Json(first_list) = list(
            State(state.clone()),
            Extension(first_claims.clone()),
            Query(NotificationListParams {
                page: None,
                per_page: None,
                read: None,
            }),
        )
        .await
        .expect("first user list");
        let Json(second_list) = list(
            State(state.clone()),
            Extension(second_claims.clone()),
            Query(NotificationListParams {
                page: None,
                per_page: None,
                read: None,
            }),
        )
        .await
        .expect("second user list");
        assert_eq!(first_list.unread_count, 0);
        assert_eq!(second_list.unread_count, 1);
        assert!(!second_list.notifications[0].read);

        let master_read: i64 =
            sqlx::query_scalar("SELECT `read` FROM notifications WHERE id = 'tenant-broadcast'")
                .fetch_one(&state.db)
                .await
                .expect("broadcast master state");
        assert_eq!(master_read, 0);

        let _ = delete(
            State(state.clone()),
            Extension(first_claims.clone()),
            axum::extract::Path("tenant-broadcast".to_string()),
        )
        .await
        .expect("first user dismisses broadcast");
        let Json(first_after_delete) = list(
            State(state.clone()),
            Extension(first_claims),
            Query(NotificationListParams {
                page: None,
                per_page: None,
                read: None,
            }),
        )
        .await
        .expect("first user list after delete");
        assert_eq!(first_after_delete.total, 0);

        let Json(second_after_delete) = list(
            State(state),
            Extension(second_claims),
            Query(NotificationListParams {
                page: None,
                per_page: None,
                read: None,
            }),
        )
        .await
        .expect("second user still sees broadcast");
        assert_eq!(second_after_delete.total, 1);
        assert_eq!(second_after_delete.unread_count, 1);
    }
}
