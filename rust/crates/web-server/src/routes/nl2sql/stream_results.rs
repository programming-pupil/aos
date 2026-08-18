//! SSE (Server-Sent Events) streaming for large query results.
//!
//! Streams MySQL results in batches as rows are fetched from the database.
//! Other engines fall back to the standard /execute endpoint.

use std::time::{Duration, Instant};

use axum::extract::{Extension, Json, State};
use axum::response::sse::{Event, Sse};
use futures_util::stream;

use crate::auth::Claims;
use crate::error::Result;
use crate::routes::data_sources::decrypt_config;
use crate::routes::nl2sql::sql_safety::SqlSafetyResult;
use crate::state::AppState;

const BATCH_SIZE: usize = 200;

/// SSE event types for streaming results.
#[derive(Debug, serde::Serialize)]
#[serde(tag = "type", content = "data")]
pub enum StreamEvent {
    Header {
        columns: Vec<StreamColumnMeta>,
    },
    RowBatch {
        rows: Vec<serde_json::Value>,
        batch_index: usize,
        total_returned: usize,
    },
    Done {
        total_rows: usize,
        execution_ms: u64,
    },
    Error {
        message: String,
    },
}

#[derive(Debug, serde::Serialize)]
pub struct StreamColumnMeta {
    pub name: String,
    #[serde(rename = "type")]
    pub type_: String,
}

#[derive(Debug, serde::Deserialize)]
pub struct StreamParams {
    pub sql: String,
    pub data_source_id: String,
    pub limit: Option<i64>,
    pub timeout_secs: Option<u32>,
}

fn make_event(event: &StreamEvent) -> Event {
    let data = serde_json::to_string(event).unwrap_or_default();
    Event::default().data(data)
}

/// POST /api/v1/nl2sql/execute-stream — SSE streaming for large MySQL result sets.
pub async fn execute_stream(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(params): Json<StreamParams>,
) -> Result<Sse<impl stream::Stream<Item = std::result::Result<Event, std::convert::Infallible>>>> {
    use crate::routes::nl2sql::classify_sql;

    if let Err(e) =
        crate::routes::nl2sql::require_nl2sql_embedding_config(&state, &claims.tenant_id).await
    {
        let ev = make_event(&StreamEvent::Error {
            message: e.to_string(),
        });
        return Ok(Sse::new(stream::iter(vec![Ok(ev)])));
    }

    // Safety gate — only SELECT queries.
    if !matches!(classify_sql(&params.sql), SqlSafetyResult::Safe) {
        let ev = make_event(&StreamEvent::Error {
            message: "Only read-only SELECT statements are permitted.".to_string(),
        });
        return Ok(Sse::new(stream::iter(vec![Ok(ev)])));
    }

    // Validate datasource access.
    if let Err(e) = crate::routes::nl2sql::validate_data_source_access(
        &state,
        &claims.tenant_id,
        &claims.sub,
        &claims.role,
        &params.data_source_id,
    )
    .await
    {
        let ev = make_event(&StreamEvent::Error {
            message: e.to_string(),
        });
        return Ok(Sse::new(stream::iter(vec![Ok(ev)])));
    }

    // Load datasource config.
    let ds_row: Option<(String, serde_json::Value)> = sqlx::query_as(
        "SELECT db_type, config FROM data_sources WHERE id = ? AND tenant_id = ? AND deleted_at IS NULL",
    )
    .bind(&params.data_source_id)
    .bind(&claims.tenant_id)
    .fetch_optional(&state.db)
    .await
    .unwrap_or(None);

    let (db_type, config_json) = match ds_row {
        Some(r) => r,
        None => {
            let ev = make_event(&StreamEvent::Error {
                message: "Datasource not found.".to_string(),
            });
            return Ok(Sse::new(stream::iter(vec![Ok(ev)])));
        }
    };

    if db_type != "mysql" && db_type != "tidb" {
        let ev = make_event(&StreamEvent::Error {
            message: format!(
                "SSE streaming is only supported for MySQL/TiDB. Use /execute for {db_type}."
            ),
        });
        return Ok(Sse::new(stream::iter(vec![Ok(ev)])));
    }

    let timeout_secs = params.limit.map(|_| 60u32).unwrap_or(60);
    let hard_limit = params.limit.unwrap_or(10_000).min(100_000);

    // Build MySQL pool.
    let config_val = match decrypt_config(
        &config_json,
        &state.data_dir,
        &claims.tenant_id,
        &params.data_source_id,
    ) {
        Ok(v) => v,
        Err(e) => {
            let ev = make_event(&StreamEvent::Error {
                message: e.to_string(),
            });
            return Ok(Sse::new(stream::iter(vec![Ok(ev)])));
        }
    };

    #[derive(serde::Deserialize)]
    struct SqlConfig {
        host: String,
        port: u16,
        database: String,
        username: String,
        password: String,
    }
    let cfg: SqlConfig = match serde_json::from_value(config_val) {
        Ok(c) => c,
        Err(_) => {
            let ev = make_event(&StreamEvent::Error {
                message: "Invalid datasource config.".to_string(),
            });
            return Ok(Sse::new(stream::iter(vec![Ok(ev)])));
        }
    };

    let url = crate::routes::data_sources::build_mysql_url_parts(
        &cfg.username,
        &cfg.password,
        &cfg.host,
        cfg.port,
        &cfg.database,
    );

    // Fetch all rows with timeout, then stream them as SSE batches.
    // True cursor-based streaming requires sqlx fetch() which returns a non-Send stream
    // incompatible with Axum's SSE. We fetch all rows under timeout, then emit in batches.
    let hinted_sql = super::queries::inject_mysql_max_execution_time(
        &format!("{} LIMIT {}", params.sql.trim_end_matches(';'), hard_limit),
        (timeout_secs as u64).saturating_mul(1000),
    );

    let pool_result = state
        .nl2sql_pool_cache
        .get_mysql(&claims.tenant_id, &params.data_source_id, 0, &url)
        .await;

    let pool = match pool_result {
        Ok(p) => p,
        Err(e) => {
            let ev = make_event(&StreamEvent::Error {
                message: format!("Connection failed: {e}"),
            });
            return Ok(Sse::new(stream::iter(vec![Ok(ev)])));
        }
    };

    let start = Instant::now();
    let fetch_result = tokio::time::timeout(
        Duration::from_secs(timeout_secs as u64),
        sqlx::query(&hinted_sql).fetch_all(&pool),
    )
    .await;

    let sql_rows = match fetch_result {
        Ok(Ok(rows)) => rows,
        Ok(Err(e)) => {
            let ev = make_event(&StreamEvent::Error {
                message: e.to_string(),
            });
            return Ok(Sse::new(stream::iter(vec![Ok(ev)])));
        }
        Err(_) => {
            let ev = make_event(&StreamEvent::Error {
                message: "Query timed out.".to_string(),
            });
            return Ok(Sse::new(stream::iter(vec![Ok(ev)])));
        }
    };

    let execution_ms = start.elapsed().as_millis() as u64;

    // Build column metadata from first row.
    use sqlx::Column;
    let columns: Vec<StreamColumnMeta> = if sql_rows.is_empty() {
        vec![]
    } else {
        sql_rows[0]
            .columns()
            .iter()
            .map(|c| StreamColumnMeta {
                name: c.name().to_string(),
                type_: "text".to_string(),
            })
            .collect()
    };

    // Decode all rows.
    use sqlx::Row;
    let masking_rules = crate::routes::nl2sql::masking_rules::load_active_rules(
        &state.db,
        &claims.tenant_id,
        &params.data_source_id,
    )
    .await;

    let json_rows: Vec<serde_json::Value> = sql_rows
        .iter()
        .map(|row| {
            let mut map = serde_json::Map::new();
            for (i, col) in row.columns().iter().enumerate() {
                let raw = crate::routes::nl2sql::decode_mysql_cell(row, i);
                let masked = crate::routes::nl2sql::masking_rules::apply_rules_to_value(
                    &masking_rules,
                    &claims.tenant_id,
                    "",
                    col.name(),
                    &raw,
                );
                map.insert(col.name().to_string(), masked);
            }
            serde_json::Value::Object(map)
        })
        .collect();

    let total_rows = json_rows.len();

    // Build SSE event sequence: Header + RowBatches + Done.
    let mut events: Vec<std::result::Result<Event, std::convert::Infallible>> = Vec::new();
    events.push(Ok(make_event(&StreamEvent::Header { columns })));

    let mut total_returned = 0;
    for (batch_index, chunk) in json_rows.chunks(BATCH_SIZE).enumerate() {
        total_returned += chunk.len();
        events.push(Ok(make_event(&StreamEvent::RowBatch {
            rows: chunk.to_vec(),
            batch_index,
            total_returned,
        })));
    }
    events.push(Ok(make_event(&StreamEvent::Done {
        total_rows,
        execution_ms,
    })));

    Ok(Sse::new(stream::iter(events)))
}
