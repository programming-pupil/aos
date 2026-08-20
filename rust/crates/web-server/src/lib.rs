//! Web Server — HTTP/WebSocket API layer for the enterprise `WebUI`.

mod auth;
mod auth_middleware;
mod config;
mod email;
mod error;
#[cfg(feature = "nl2sql")]
pub mod nl2sql;
mod routes;
mod semantic_kernel_store;
mod semantic_memory_worker;
mod state;
mod telemetry;

#[cfg(debug_assertions)]
use crate::semantic_kernel_store::process_fault_point;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use axum::{
    body::Body,
    http::{HeaderValue, Request, StatusCode, Uri},
    middleware::Next,
    response::Response,
    Router,
};
use tower_http::cors::{AllowOrigin, Any, CorsLayer};
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;

fn web_ui_service(web_dir: PathBuf) -> ServeDir<ServeFile> {
    let index_file = web_dir.join("index.html");
    if !index_file.is_file() {
        panic!("Web UI index file does not exist: {}", index_file.display());
    }
    ServeDir::new(web_dir).fallback(ServeFile::new(index_file))
}

pub(crate) fn sqlite_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

pub(crate) fn behavior_trace(case_id: &str) {
    if std::env::var("AOS_BEHAVIOR_TRACE_CASE").as_deref() == Ok(case_id) {
        static EMITTED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
        if EMITTED.set(()).is_ok() {
            eprintln!("AOS_PRODUCTION_TRACE\t{case_id}");
        }
    }
}

#[cfg(feature = "nl2sql")]
pub fn warm_local_embedding_model(cache_dir: PathBuf) -> anyhow::Result<()> {
    crate::nl2sql::embedding::configure_local_embedding_cache_dir(cache_dir)?;
    crate::nl2sql::embedding::warm_local_embedding_model()
}

#[cfg(feature = "nl2sql")]
pub fn shutdown_local_embedding_model() {
    crate::nl2sql::embedding::shutdown_local_embedding_model();
}

/// Rebuild the searchable Memory projection from canonical structured facts.
/// A projection may be deleted and regenerated without ever becoming an
/// authority. With `verify_hash`, every rebuilt user scope is checked against
/// its durable projection-state hash before the command succeeds.
pub async fn rebuild_memory_projection(
    data_dir: PathBuf,
    tenant_id: &str,
    user_id: Option<&str>,
    verify_hash: bool,
) -> anyhow::Result<usize> {
    let state = state::AppState::new(data_dir, Some("memory-projection-rebuild".into())).await?;
    let users = if let Some(user_id) = user_id.filter(|value| !value.trim().is_empty()) {
        vec![user_id.to_string()]
    } else {
        sqlx::query_scalar::<sqlx::Sqlite, String>(
            "SELECT DISTINCT user_id FROM structured_memory_facts
             WHERE tenant_id = ? ORDER BY user_id",
        )
        .bind(tenant_id)
        .fetch_all(&state.db)
        .await?
    };
    let mut rebuilt = 0usize;
    for user_id in users {
        let mut tx = state.db.begin().await?;
        crate::semantic_kernel_store::acquire_sqlite_write_lock(&mut tx).await?;
        let projection_hash =
            memory_engine::SqliteMemoryTransaction::rebuild_projection_in_transaction(
                &mut tx, tenant_id, &user_id,
            )
            .await
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        tx.commit().await?;
        if verify_hash {
            let stored_hash = sqlx::query_scalar::<sqlx::Sqlite, String>(
                "SELECT projection_hash FROM memory_projection_state
                 WHERE tenant_id = ? AND user_id = ?",
            )
            .bind(tenant_id)
            .bind(&user_id)
            .fetch_one(&state.db)
            .await?;
            anyhow::ensure!(
                stored_hash == projection_hash,
                "memory projection hash mismatch for tenant `{tenant_id}` user `{user_id}`"
            );
        }
        println!(
            "Memory projection rebuilt: tenant={tenant_id} user={user_id} hash={projection_hash}"
        );
        rebuilt += 1;
    }
    Ok(rebuilt)
}

/// Execute one black-box persistence TCK phase in a real `web-server`
/// process. This is deliberately available only to debug builds; the
/// integration test starts a fresh binary for `prepare`, observes its fault
/// exit, then starts another binary against the same data directory for
/// `recover`.
#[cfg(debug_assertions)]
fn process_tck_memory_draft(
    session: &str,
    tenant: &str,
    user: &str,
) -> memory_engine::MemoryFactDraft {
    memory_engine::MemoryFactDraft {
        fact_id: format!("{session}-fact"),
        projection_id: format!("{session}-projection"),
        tenant_id: tenant.into(),
        user_id: user.into(),
        scope: "session".into(),
        app: "chat".into(),
        session_id: Some(session.into()),
        channel: "long_term_memory".into(),
        kind: "fact".into(),
        subject: serde_json::json!({"entityType":"user","canonicalId":user}),
        predicate: "release_region".into(),
        value: serde_json::json!("APAC"),
        text: "The release region is APAC".into(),
        evidence_id: format!("{session}-evidence"),
        evidence_hash: "memory-source-hash".into(),
        valid_from: None,
        valid_until: None,
        confidence: 1.0,
        sensitivity: "internal".into(),
        lifecycle: memory_engine::FactLifecycle::Candidate,
        authority: vec!["user".into()],
        source_event_ids: vec![format!("{session}-event")],
        pollution_lineage: Vec::new(),
        memory_type: "fact".into(),
        source_type: "process_tck".into(),
        pinned: false,
        metadata: serde_json::json!({"case":"process-fault"}),
        stale_at: None,
        verified_at: None,
        embedding_model: None,
        embedding_dimensions: None,
        embedding_json: None,
    }
}

#[cfg(debug_assertions)]
pub async fn run_semantic_kernel_process_tck(
    data_dir: PathBuf,
    case: String,
    mode: String,
) -> anyhow::Result<()> {
    use chrono::{Duration as ChronoDuration, Utc};
    use runtime::AgentExecutionKernel;

    crate::behavior_trace("FAULT-001");
    crate::behavior_trace("KEY-001");
    let prepare = mode == "prepare";
    if mode != "prepare" && mode != "recover" {
        anyhow::bail!("unknown semantic-kernel TCK mode `{mode}`");
    }
    // A recovery process must be able to open the database even when the
    // prepare process was killed after a commit boundary. Keep the original
    // point as test metadata, but never re-trigger it during startup.
    let expected_fault_point = std::env::var("AOS_PROCESS_FAULT_POINT").unwrap_or_default();
    if !prepare {
        std::env::remove_var("AOS_PROCESS_FAULT_POINT");
    }
    let state = state::AppState::new(data_dir.clone(), Some("semantic-kernel-tck".into())).await?;
    let db = state.db.clone();
    let tenant = "tck-tenant";
    let user = "tck-user";
    let session = format!("tck-{case}");
    let turn = format!("{session}-turn");
    let kernel = crate::semantic_kernel_store::RuntimeExecutionKernel::new(
        db.clone(),
        tenant,
        user,
        &session,
    );
    match case.as_str() {
        "migration" => {
            let migrations: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations")
                .fetch_one(&db)
                .await?;
            anyhow::ensure!(
                migrations >= 34,
                "migration ledger is incomplete: {migrations}"
            );
            if prepare {
                process_fault_point("migration.after_commit");
            }
            println!("AOS_PROCESS_RESTART_EVIDENCE\tmigration\t{migrations}");
        }
        "turn" => {
            if prepare {
                kernel
                    .start_turn(runtime::RuntimeTurnStart {
                        turn_id: turn.clone(),
                        user_input: "process fault turn input".into(),
                    })
                    .await?;
                let mut session_state = runtime::Session::new();
                session_state.session_id = session.clone();
                session_state.tenant_id = Some(tenant.into());
                session_state.user_id = Some(user.into());
                session_state.restore_turn(
                    turn.clone(),
                    "process fault turn input",
                    0,
                    None,
                    runtime::SessionTurnStatus::Completed,
                );
                session_state
                    .messages
                    .push(runtime::ConversationMessage::user_text(
                        "process fault turn input",
                    ));
                kernel
                    .finish_turn_with_checkpoint(
                        &turn,
                        runtime::RuntimeTurnTerminalStatus::Completed,
                        Some("process TCK"),
                        &session_state,
                    )
                    .await?;
            } else {
                kernel.recover().await?;
                let status: String = sqlx::query_scalar(
                    "SELECT status FROM agent_turns WHERE tenant_id = ? AND thread_id = ? AND id = ?",
                )
                .bind(tenant)
                .bind(&session)
                .bind(&turn)
                .fetch_one(&db)
                .await?;
                let checkpoints: i64 = sqlx::query_scalar(
                    "SELECT COUNT(*) FROM execution_checkpoints WHERE tenant_id = ? AND thread_id = ?",
                )
                .bind(tenant)
                .bind(&session)
                .fetch_one(&db)
                .await?;
                if checkpoints == 0 {
                    anyhow::ensure!(
                        status == "recovery_required",
                        "unexpected pre-commit turn status: {status}"
                    );
                } else {
                    anyhow::ensure!(
                        status == "completed",
                        "unexpected committed turn status: {status}"
                    );
                    anyhow::ensure!(
                        checkpoints == 1,
                        "committed terminal did not keep one checkpoint"
                    );
                }
                println!("AOS_PROCESS_RESTART_EVIDENCE\tturn\t{status}\t{checkpoints}");
            }
        }
        case if matches!(
            case,
            "interaction-approval"
                | "interaction-question"
                | "interaction-credential"
                | "interaction-oauth"
        ) || case == "interaction" =>
        {
            let interaction_kind = match case {
                "interaction-approval" => agent_protocol::InteractionKind::Approval,
                "interaction-credential" => agent_protocol::InteractionKind::CredentialRequest,
                "interaction-oauth" => agent_protocol::InteractionKind::ExternalAuthorization,
                _ => agent_protocol::InteractionKind::UserQuestion,
            };
            let invocation_id = format!("{case}-invocation");
            let interaction_id = if interaction_kind == agent_protocol::InteractionKind::Approval {
                crate::semantic_kernel_store::tenant_scoped_record_id(
                    "approval",
                    tenant,
                    &format!("{session}:{turn}:{invocation_id}"),
                )
            } else {
                format!("{session}-interaction")
            };
            if prepare {
                kernel
                    .start_turn(runtime::RuntimeTurnStart {
                        turn_id: turn.clone(),
                        user_input: "wait for durable answer".into(),
                    })
                    .await?;
                if interaction_kind == agent_protocol::InteractionKind::Approval {
                    sqlx::query(
                        "INSERT INTO capability_tokens
                            (id, tenant_id, user_id, session_id, tool_name,
                             resource_scope, action_scope, executor_scope,
                             expires_at, remaining_uses)
                         VALUES (?, ?, ?, ?, ?, ?, ?, 'native',
                                 datetime('now', '+15 minutes'), 1)
                         ON CONFLICT(id) DO NOTHING",
                    )
                    .bind(format!("{interaction_id}-capability"))
                    .bind(tenant)
                    .bind(user)
                    .bind(&session)
                    .bind("process_tck_tool")
                    .bind("process-tck-input")
                    .bind("execute")
                    .execute(&db)
                    .await?;
                    kernel
                        .request_approval(&runtime::RuntimeApprovalRequest {
                            turn_id: turn.clone(),
                            invocation_id: invocation_id.clone(),
                            tool_name: "process_tck_tool".into(),
                            input: "process-tck-input".into(),
                            iteration: 1,
                            request: runtime::PermissionRequest {
                                tool_name: "process_tck_tool".into(),
                                input: "process-tck-input".into(),
                                current_mode: runtime::PermissionMode::ReadOnly,
                                required_mode: runtime::PermissionMode::WorkspaceWrite,
                                reason: Some("process TCK approval".into()),
                            },
                            contract: runtime::RuntimeToolContract::test_read_only(
                                "process_tck_tool",
                            ),
                        })
                        .await?;
                } else {
                    kernel
                        .request_interaction(&runtime::RuntimeInteractionRequest {
                            interaction_id: interaction_id.clone(),
                            kind: interaction_kind,
                            turn_id: turn.clone(),
                            invocation_id,
                            owner_user_id: user.into(),
                            allowed_responder_ids: Vec::new(),
                            capability_requirement: None,
                            request_schema_hash: "tck-interaction-v1".into(),
                            choice_schema_hash: None,
                            display_projection: serde_json::json!({"title":"TCK interaction"}),
                            idempotency_key: format!("request-{interaction_id}"),
                            expected_turn_revision: 0,
                            expires_at: Some(Utc::now() + ChronoDuration::minutes(5)),
                        })
                        .await?;
                }
            } else {
                kernel.recover().await?;
                let pending = crate::semantic_kernel_store::list_runtime_interactions(
                    &db, tenant, user, &session,
                )
                .await?;
                let expected_pending =
                    usize::from(!expected_fault_point.ends_with(".before_commit"));
                anyhow::ensure!(
                    pending.len() == expected_pending,
                    "pending interaction recovery count mismatch: expected {expected_pending}, got {}",
                    pending.len()
                );
                if pending.is_empty() {
                    println!("AOS_PROCESS_RESTART_EVIDENCE\t{case}\trolled_back");
                    return Ok(());
                }
                let response_projection = match interaction_kind {
                    agent_protocol::InteractionKind::UserQuestion => {
                        Some(serde_json::json!({"answer":"confirmed after restart"}))
                    }
                    agent_protocol::InteractionKind::Approval => {
                        Some(serde_json::json!({"decision":"granted"}))
                    }
                    agent_protocol::InteractionKind::CredentialRequest => None,
                    agent_protocol::InteractionKind::ExternalAuthorization => {
                        Some(serde_json::json!({"authorization":"granted"}))
                    }
                };
                let response_state = match interaction_kind {
                    agent_protocol::InteractionKind::Approval => {
                        agent_protocol::InteractionState::Granted
                    }
                    _ => agent_protocol::InteractionState::Responded,
                };
                let encrypted_secret_ref = match interaction_kind {
                    agent_protocol::InteractionKind::CredentialRequest => {
                        Some("secret://process-tck/credential".to_string())
                    }
                    _ => None,
                };
                let resolution = runtime::RuntimeInteractionResolution {
                    interaction_id: interaction_id.clone(),
                    turn_id: turn.clone(),
                    responder_user_id: user.into(),
                    state: response_state,
                    response_projection,
                    encrypted_secret_ref,
                    idempotency_key: "answer-once".into(),
                };
                let answered =
                    runtime::AgentExecutionKernel::respond_interaction(&kernel, &resolution)
                        .await?;
                anyhow::ensure!(
                    answered.state == response_state,
                    "interaction response state was not persisted"
                );
                let consumed = runtime::AgentExecutionKernel::consume_interaction(
                    &kernel,
                    &interaction_id,
                    &turn,
                    "answer-once",
                )
                .await?;
                anyhow::ensure!(
                    consumed.state == agent_protocol::InteractionState::Consumed,
                    "interaction was not consumed after restart"
                );
                let duplicate = runtime::AgentExecutionKernel::consume_interaction(
                    &kernel,
                    &interaction_id,
                    &turn,
                    "different-replay",
                )
                .await;
                anyhow::ensure!(
                    duplicate.is_err(),
                    "duplicate interaction resume unexpectedly succeeded"
                );
                let state: String = sqlx::query_scalar(
                    "SELECT state FROM durable_interactions WHERE tenant_id = ? AND id = ?",
                )
                .bind(tenant)
                .bind(&interaction_id)
                .fetch_one(&db)
                .await?;
                anyhow::ensure!(
                    state == "consumed",
                    "interaction did not reach consumed: {state}"
                );
                println!("AOS_PROCESS_RESTART_EVIDENCE\t{case}\t{state}");
            }
        }
        "tool" => {
            let invocation_id = format!("{session}-tool");
            let intent = runtime::RuntimeToolIntent::new(
                &turn,
                &invocation_id,
                "read_file",
                "README.md",
                1,
                true,
                None,
            );
            if prepare {
                kernel
                    .start_turn(runtime::RuntimeTurnStart {
                        turn_id: turn.clone(),
                        user_input: "execute one governed tool".into(),
                    })
                    .await?;
                kernel.authorize_tool(&intent).await?;
                kernel.start_tool(&intent).await?;
                kernel
                    .finish_tool(runtime::RuntimeToolOutcome {
                        turn_id: turn.clone(),
                        invocation_id: invocation_id.clone(),
                        tool_name: "read_file".into(),
                        input: "README.md".into(),
                        output: "tool output ".repeat(20_000),
                        iteration: 1,
                        outcome: runtime::RuntimeToolOutcomeKind::Completed,
                    })
                    .await?;
            } else {
                kernel.recover().await?;
                let lifecycle: String = sqlx::query_scalar(
                    "SELECT lifecycle_state FROM tool_invocations
                     WHERE tenant_id = ? AND thread_id = ? AND turn_id = ?
                       AND tool_name = 'read_file'",
                )
                .bind(tenant)
                .bind(&session)
                .bind(&turn)
                .fetch_one(&db)
                .await?;
                let artifacts: i64 = sqlx::query_scalar(
                    "SELECT COUNT(*) FROM artifact_objects WHERE tenant_id = ? AND owner_scope = ?",
                )
                .bind(tenant)
                .bind(&session)
                .fetch_one(&db)
                .await?;
                if lifecycle == "outcome_unknown" {
                    anyhow::ensure!(artifacts == 0, "unknown tool outcome created an artifact");
                } else {
                    anyhow::ensure!(
                        lifecycle == "completed",
                        "unexpected committed tool state: {lifecycle}"
                    );
                    anyhow::ensure!(artifacts == 1, "committed tool outcome lost its artifact");
                }
                println!("AOS_PROCESS_RESTART_EVIDENCE\ttool\t{lifecycle}\t{artifacts}");
            }
        }
        "compaction" => {
            let archived = vec![
                runtime::ConversationMessage::user_text("The release region is APAC. ".repeat(100)),
                runtime::ConversationMessage::assistant(vec![runtime::ContentBlock::Text {
                    text: "The hard constraint is verify before release. ".repeat(100),
                }]),
            ];
            if prepare {
                kernel
                    .start_turn(runtime::RuntimeTurnStart {
                        turn_id: turn.clone(),
                        user_input: archived[0]
                            .blocks
                            .iter()
                            .find_map(|block| match block {
                                runtime::ContentBlock::Text { text } => Some(text.clone()),
                                _ => None,
                            })
                            .unwrap_or_default(),
                    })
                    .await?;
                kernel
                    .record_assistant_message(&turn, 1, &archived[1])
                    .await?;
                let packet = semantic_core::ContextPacket {
                    objective: "compaction TCK".into(),
                    envelope: semantic_core::ContextEnvelope::default(),
                    blocks: Vec::new(),
                    manifest: semantic_core::ContextManifest {
                        max_tokens: 1024,
                        used_tokens: 0,
                        blocks: Vec::new(),
                        snapshot_version: None,
                    },
                };
                kernel
                    .record_context_manifest(runtime::RuntimeContextManifestInput {
                        turn_id: turn.clone(),
                        iteration: 1,
                        budget_stage: runtime::RuntimeModelBudgetStage::General,
                        system_sections: Vec::new(),
                        messages: archived.clone(),
                        estimated_tokens: 2,
                        max_input_tokens: 1024,
                        model_version: Some("semantic-kernel-tck".into()),
                        active_tools: Vec::new(),
                        context_packet: packet,
                        prompt_manifest: None,
                        semantic_snapshot_version: None,
                    })
                    .await?;
                let mut session_state = runtime::Session::new();
                session_state.session_id = session.clone();
                session_state.tenant_id = Some(tenant.into());
                session_state.user_id = Some(user.into());
                session_state.messages = archived.clone();
                session_state
                    .messages
                    .push(runtime::ConversationMessage::user_text("retained tail"));
                let result = runtime::compact_session(
                    &session_state,
                    runtime::CompactionConfig {
                        preserve_recent_messages: 1,
                        max_estimated_tokens: 0,
                    },
                );
                let coverage = crate::semantic_kernel_store::ledger_coverage_for_archive(
                    &db, tenant, &session, &archived,
                )
                .await?;
                let transaction_id = crate::semantic_kernel_store::prepare_compaction_transaction(
                    &db,
                    tenant,
                    user,
                    &session,
                    "process-tck",
                    &coverage.event_sequences,
                    &coverage.message_event_ids,
                    &coverage.parent_compaction_ids,
                    &archived,
                    &[],
                    &result.summary,
                )
                .await?;
                crate::semantic_kernel_store::commit_compaction_transaction(
                    &db,
                    tenant,
                    user,
                    &session,
                    "chat",
                    &transaction_id,
                    "process-tck",
                    &result,
                )
                .await?;
            } else {
                kernel.recover().await?;
                let status: Option<String> = sqlx::query_scalar(
                    "SELECT status FROM compaction_transactions WHERE tenant_id = ? AND thread_id = ?",
                )
                .bind(tenant)
                .bind(&session)
                .fetch_optional(&db)
                .await?;
                // A prepare-before-commit fault rolls the insert back, so no
                // transaction row is expected after restart. Treat that
                // absence as the deterministic aborted state.
                let status = status.unwrap_or_else(|| "aborted".into());
                let artifacts: i64 = sqlx::query_scalar(
                    "SELECT COUNT(*) FROM artifact_objects WHERE tenant_id = ? AND owner_scope = ?",
                )
                .bind(tenant)
                .bind(&session)
                .fetch_one(&db)
                .await?;
                if status == "aborted" {
                    anyhow::ensure!(artifacts == 0, "faulted compaction published an artifact");
                } else {
                    anyhow::ensure!(
                        status == "committed",
                        "unexpected committed compaction state: {status}"
                    );
                    anyhow::ensure!(artifacts == 1, "committed compaction lost its artifact");
                }
                println!("AOS_PROCESS_RESTART_EVIDENCE\tcompaction\t{status}\t{artifacts}");
            }
        }
        "memory" => {
            let draft = process_tck_memory_draft(&session, tenant, user);
            if prepare {
                let mut repository = memory_engine::SqliteMemoryRepositoryAdapter::new(db.clone());
                memory_engine::MemoryRepository::upsert(&mut repository, draft.clone()).await?;
                process_fault_point("memory.repository.after_commit");
            }
            let facts: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM structured_memory_facts WHERE tenant_id = ? AND id = ?",
            )
            .bind(tenant)
            .bind(&draft.fact_id)
            .fetch_one(&db)
            .await?;
            let projections: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM agent_memory_items WHERE tenant_id = ? AND id = ?",
            )
            .bind(tenant)
            .bind(&draft.projection_id)
            .fetch_one(&db)
            .await?;
            anyhow::ensure!(
                facts == projections && (facts == 0 || facts == 1),
                "memory repository left a partial projection"
            );
            println!("AOS_PROCESS_RESTART_EVIDENCE\tmemory\t{facts}\t{projections}");
        }
        "memory-consolidation" => {
            let draft = process_tck_memory_draft(&session, tenant, user);
            if prepare {
                let mut repository = memory_engine::SqliteMemoryRepositoryAdapter::new(db.clone());
                memory_engine::MemoryRepository::upsert(&mut repository, draft.clone()).await?;
                crate::semantic_memory_worker::run_memory_maintenance_once(
                    &db,
                    "process-tck-memory-worker",
                )
                .await?;
            } else {
                // A before-commit crash leaves a claimed batch whose lease is
                // still owned by this deterministic worker ID. An after-commit
                // crash leaves the cursor advanced. Running one maintenance
                // iteration is correct and idempotent in both states.
                let _ = crate::semantic_memory_worker::run_memory_maintenance_once(
                    &db,
                    "process-tck-memory-worker",
                )
                .await?;
                let lifecycle: String = sqlx::query_scalar(
                    "SELECT lifecycle FROM structured_memory_facts
                     WHERE tenant_id = ? AND id = ?",
                )
                .bind(tenant)
                .bind(&draft.fact_id)
                .fetch_one(&db)
                .await?;
                let enabled: i64 = sqlx::query_scalar(
                    "SELECT enabled FROM agent_memory_items
                     WHERE tenant_id = ? AND id = ?",
                )
                .bind(tenant)
                .bind(&draft.projection_id)
                .fetch_one(&db)
                .await?;
                let batch_status: String = sqlx::query_scalar(
                    "SELECT status FROM memory_consolidation_batches
                     WHERE tenant_id = ? ORDER BY created_at DESC LIMIT 1",
                )
                .bind(tenant)
                .fetch_one(&db)
                .await?;
                let cursor: i64 = sqlx::query_scalar(
                    "SELECT cursor_sequence FROM memory_consolidation_leases
                     WHERE tenant_id = ?",
                )
                .bind(tenant)
                .fetch_one(&db)
                .await?;
                let max_sequence: i64 = sqlx::query_scalar(
                    "SELECT MAX(global_sequence) FROM memory_fact_events
                     WHERE tenant_id = ?",
                )
                .bind(tenant)
                .fetch_one(&db)
                .await?;
                anyhow::ensure!(
                    lifecycle == "confirmed" && enabled == 1,
                    "consolidation did not atomically publish the canonical fact and projection"
                );
                anyhow::ensure!(batch_status == "committed", "batch was not recovered");
                anyhow::ensure!(
                    cursor == max_sequence,
                    "consolidation cursor does not match the committed event sequence"
                );
                println!(
                    "AOS_PROCESS_RESTART_EVIDENCE\tmemory-consolidation\t{lifecycle}\t{batch_status}\t{cursor}"
                );
            }
        }
        "rotation" => {
            if prepare {
                std::env::set_var("ENCRYPTION_KEY", "11111111111111111111111111111111");
                std::env::set_var("ENCRYPTION_KEY_ID", "old-tck");
                std::env::remove_var("ENCRYPTION_KEY_RING");
                let old = agent_gateway::crypto::encrypt_scoped(
                    "rotation payload",
                    &agent_gateway::crypto::scoped_aad(
                        "context_manifest.raw",
                        tenant,
                        "rotation-manifest",
                    ),
                )?;
                std::env::set_var("ENCRYPTION_KEY", "22222222222222222222222222222222");
                std::env::set_var("ENCRYPTION_KEY_ID", "new-tck");
                std::env::set_var(
                    "ENCRYPTION_KEY_RING",
                    r#"{"old-tck":"11111111111111111111111111111111"}"#,
                );
                sqlx::query(
                    "INSERT INTO context_packet_manifests
                     (id, tenant_id, thread_id, manifest_hash, manifest_json,
                      raw_manifest_hash, raw_manifest_ciphertext, created_at)
                     VALUES ('rotation-manifest', ?, 'rotation-session', 'hash', '{}', 'raw', ?, CURRENT_TIMESTAMP)",
                )
                .bind(tenant)
                .bind(old)
                .execute(&db)
                .await?;
                crate::semantic_kernel_store::rotate_encrypted_payload_batch_with_data_dir(
                    &db, &data_dir, 20,
                )
                .await?;
            } else {
                let rotated =
                    crate::semantic_kernel_store::rotate_encrypted_payload_batch_with_data_dir(
                        &db, &data_dir, 20,
                    )
                    .await?;
                anyhow::ensure!(rotated >= 1, "rotation did not rewrite the stale payload");
                let certificate = crate::semantic_kernel_store::issue_key_retirement_certificate(
                    &db, &data_dir, "old-tck", true,
                )
                .await?;
                let stored: String = sqlx::query_scalar(
                    "SELECT registry_snapshot_hash FROM key_retirement_certificates WHERE key_id = 'old-tck'",
                )
                .fetch_one(&db)
                .await?;
                anyhow::ensure!(
                    stored == certificate,
                    "retirement certificate hash mismatch"
                );
                println!("AOS_PROCESS_RESTART_EVIDENCE\trotation\t{rotated}\t{certificate}");
            }
        }
        "rotation-negative" => {
            let without_backup = crate::semantic_kernel_store::issue_key_retirement_certificate(
                &db, &data_dir, "old-tck", false,
            )
            .await
            .expect_err("retirement without backup confirmation must be rejected");
            anyhow::ensure!(
                without_backup
                    .to_string()
                    .contains("backup-policy confirmation"),
                "wrong backup-policy rejection: {without_backup}"
            );
            let unknown = crate::semantic_kernel_store::issue_key_retirement_certificate(
                &db,
                &data_dir,
                "unknown-tck",
                true,
            )
            .await
            .expect_err("an unknown key must be rejected");
            anyhow::ensure!(
                unknown
                    .to_string()
                    .contains("not present in the configured key ring"),
                "wrong unknown-key rejection: {unknown}"
            );
            let active = crate::semantic_kernel_store::issue_key_retirement_certificate(
                &db, &data_dir, "new-tck", true,
            )
            .await
            .expect_err("the active key must be rejected");
            anyhow::ensure!(
                active.to_string().contains("active encryption key"),
                "wrong active-key rejection: {active}"
            );
            sqlx::query(
                "INSERT INTO context_packet_manifests
                    (id, tenant_id, thread_id, manifest_hash, manifest_json,
                     raw_manifest_hash, raw_manifest_ciphertext, created_at)
                 VALUES ('rotation-legacy', ?, 'rotation-session', 'hash', '{}',
                         'raw', 'legacy-ciphertext', CURRENT_TIMESTAMP)",
            )
            .bind(tenant)
            .execute(&db)
            .await?;
            let unknown_ciphertext =
                crate::semantic_kernel_store::issue_key_retirement_certificate(
                    &db, &data_dir, "old-tck", true,
                )
                .await
                .expect_err("unversioned ciphertext must block retirement");
            anyhow::ensure!(
                unknown_ciphertext
                    .to_string()
                    .contains("unknown or unversioned ciphertext"),
                "wrong unversioned-ciphertext rejection: {unknown_ciphertext}"
            );
            sqlx::query("DELETE FROM context_packet_manifests WHERE id = 'rotation-legacy'")
                .execute(&db)
                .await?;
            std::env::set_var("ENCRYPTION_KEY", "11111111111111111111111111111111");
            std::env::set_var("ENCRYPTION_KEY_ID", "old-tck");
            std::env::remove_var("ENCRYPTION_KEY_RING");
            let old_ciphertext = agent_gateway::crypto::encrypt_scoped(
                "retiring payload",
                &agent_gateway::crypto::scoped_aad(
                    "context_manifest.raw",
                    tenant,
                    "rotation-reference",
                ),
            )?;
            std::env::set_var("ENCRYPTION_KEY", "22222222222222222222222222222222");
            std::env::set_var("ENCRYPTION_KEY_ID", "new-tck");
            std::env::set_var(
                "ENCRYPTION_KEY_RING",
                r#"{"old-tck":"11111111111111111111111111111111"}"#,
            );
            sqlx::query(
                "INSERT INTO context_packet_manifests
                    (id, tenant_id, thread_id, manifest_hash, manifest_json,
                     raw_manifest_hash, raw_manifest_ciphertext, created_at)
                 VALUES ('rotation-reference', ?, 'rotation-session', 'hash', '{}',
                         'raw', ?, CURRENT_TIMESTAMP)",
            )
            .bind(tenant)
            .bind(old_ciphertext)
            .execute(&db)
            .await?;
            let referenced = crate::semantic_kernel_store::issue_key_retirement_certificate(
                &db, &data_dir, "old-tck", true,
            )
            .await
            .expect_err("a still-referenced old key must be rejected");
            anyhow::ensure!(
                referenced
                    .to_string()
                    .contains("still has ciphertext references"),
                "wrong referenced-key rejection: {referenced}"
            );
            sqlx::query("DELETE FROM context_packet_manifests WHERE id = 'rotation-reference'")
                .execute(&db)
                .await?;
            let wrong_aad_ciphertext = agent_gateway::crypto::encrypt_scoped(
                "misbound payload",
                &agent_gateway::crypto::scoped_aad("context_manifest.raw", tenant, "different-row"),
            )?;
            sqlx::query(
                "INSERT INTO context_packet_manifests
                    (id, tenant_id, thread_id, manifest_hash, manifest_json,
                     raw_manifest_hash, raw_manifest_ciphertext, created_at)
                 VALUES ('rotation-wrong-aad', ?, 'rotation-session', 'hash', '{}',
                         'raw', ?, CURRENT_TIMESTAMP)",
            )
            .bind(tenant)
            .bind(wrong_aad_ciphertext)
            .execute(&db)
            .await?;
            let misbound = crate::semantic_kernel_store::issue_key_retirement_certificate(
                &db, &data_dir, "old-tck", true,
            )
            .await
            .expect_err("wrong AAD must block retirement");
            anyhow::ensure!(
                misbound
                    .to_string()
                    .contains("failed sampled scoped decrypt"),
                "wrong sampled-decrypt rejection: {misbound}"
            );
            println!(
                "AOS_PROCESS_RESTART_EVIDENCE\trotation-negative\tbackup\tunknown\tactive\tlegacy\treferenced\taad"
            );
        }
        other => anyhow::bail!("unknown semantic-kernel TCK case `{other}`"),
    }
    Ok(())
}

/// Acquire SQLite's single-writer lock before a transaction reads state that it
/// will later update. This avoids deferred-transaction read-to-write upgrade
/// failures under concurrent requests; `busy_timeout` can then serialize the
/// short write transactions normally.
pub(crate) async fn acquire_sqlite_write_lock(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
) -> std::result::Result<(), sqlx::Error> {
    sqlx::query::<sqlx::Sqlite>("UPDATE aos_setup_lock SET lock_id = lock_id WHERE lock_id = 1")
        .execute(&mut **transaction)
        .await?;
    Ok(())
}

#[cfg(test)]
pub(crate) async fn test_sqlite_pool() -> sqlx::SqlitePool {
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

    let options = SqliteConnectOptions::new()
        .filename(":memory:")
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .expect("open SQLite test database");
    sqlx::migrate!("./sqlite-migrations")
        .run(&pool)
        .await
        .expect("apply SQLite test migrations");
    pool
}

#[cfg(test)]
pub(crate) async fn test_sqlite_file_pool() -> (sqlx::SqlitePool, PathBuf) {
    use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

    let database_path =
        std::env::temp_dir().join(format!("aos-sqlite-test-{}.db", uuid::Uuid::new_v4()));
    let options = SqliteConnectOptions::new()
        .filename(&database_path)
        .create_if_missing(true)
        .foreign_keys(true)
        .journal_mode(SqliteJournalMode::Wal)
        .busy_timeout(std::time::Duration::from_secs(10));
    let pool = SqlitePoolOptions::new()
        .max_connections(4)
        .connect_with(options)
        .await
        .expect("open file-backed SQLite test database");
    sqlx::migrate!("./sqlite-migrations")
        .run(&pool)
        .await
        .expect("apply SQLite test migrations");
    (pool, database_path)
}

#[cfg(test)]
mod sqlite_baseline_tests {
    use sha2::Digest;
    use sqlx::sqlite::SqlitePoolOptions;
    use std::borrow::Cow;

    #[test]
    fn historical_semantic_kernel_migration_checksum_is_stable() {
        let checksum = hex::encode(sha2::Sha384::digest(include_bytes!(
            "../sqlite-migrations/0017_semantic_kernel_core.sql"
        )));
        assert_eq!(
            checksum,
            "58772fbbda2f10a4d1fb421caaf7eb3f55f20e06edb7c8cbcf9807992518676bd5f9e9a0db0ffe13889d079ef76e280f"
        );
    }

    async fn migrate_through(pool: &sqlx::SqlitePool, max_version: i64) {
        let full = sqlx::migrate!("./sqlite-migrations");
        let partial = sqlx::migrate::Migrator {
            migrations: Cow::Owned(
                full.iter()
                    .filter(|migration| migration.version <= max_version)
                    .cloned()
                    .collect(),
            ),
            ignore_missing: false,
            locking: true,
            no_tx: false,
        };
        partial
            .run(pool)
            .await
            .unwrap_or_else(|error| panic!("migrate through {max_version}: {error}"));
    }

    #[tokio::test]
    async fn baseline_migration_is_idempotent_and_seeds_setup_lock_once() {
        let pool = crate::test_sqlite_pool().await;
        sqlx::migrate!("./sqlite-migrations")
            .run(&pool)
            .await
            .expect("reapply SQLite migrations");

        let migration_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations")
            .fetch_one(&pool)
            .await
            .expect("count migration ledger");
        let setup_lock_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM aos_setup_lock WHERE lock_id = 1")
                .fetch_one(&pool)
                .await
                .expect("count setup lock seed");
        let repository_auto_sync_column_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pragma_table_info('rd_repository_settings') \
             WHERE name IN ('auto_sync_enabled','auto_sync_interval_minutes','last_auto_sync_at','last_sync_error')",
        )
        .fetch_one(&pool)
        .await
        .expect("count repository auto-sync columns");
        let agent_market_source_column_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pragma_table_info('rd_agent_profiles') \
             WHERE name IN ('source','source_item_id')",
        )
        .fetch_one(&pool)
        .await
        .expect("count agent market source columns");
        let model_profile_table_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type = 'table' AND name = 'model_capability_profiles'",
        )
        .fetch_one(&pool)
        .await
        .expect("count model capability profile table");
        let api_key_profile_column_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pragma_table_info('api_keys')
             WHERE name = 'model_profile_id'",
        )
        .fetch_one(&pool)
        .await
        .expect("count API key model profile column");
        let rd_spec_repository_ids_column_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pragma_table_info('rd_specs')
             WHERE name = 'repository_ids_json'",
        )
        .fetch_one(&pool)
        .await
        .expect("count Plan Mode repository selection column");

        assert!(migration_count >= 17);
        assert_eq!(setup_lock_count, 1);
        assert_eq!(repository_auto_sync_column_count, 4);
        assert_eq!(agent_market_source_column_count, 2);
        assert_eq!(model_profile_table_count, 1);
        assert_eq!(api_key_profile_column_count, 1);
        assert_eq!(rd_spec_repository_ids_column_count, 1);
        pool.close().await;
    }

    #[tokio::test]
    async fn memory_v3_migration_preserves_canonical_facts_and_quarantines_projection_only_rows() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("open Memory migration fixture");
        migrate_through(&pool, 33).await;
        for (id, content) in [
            ("canonical-projection", "owner confirmed timezone UTC"),
            ("projection-only", "unverified model guess"),
        ] {
            sqlx::query(
                "INSERT INTO agent_memory_items
                   (id, tenant_id, user_id, scope, app, session_key, memory_type,
                    content, content_hash, source_type, confidence, enabled,
                    embedding_model, embedding_dimensions, embedding_json)
                 VALUES (?, 'tenant-memory', 'user-memory', 'global', 'chat', '',
                         'preference', ?, ?, 'legacy', 0.9, 1,
                         'stale-model', 2, '[0.1,0.2]')",
            )
            .bind(id)
            .bind(content)
            .bind(format!("hash:{id}"))
            .execute(&pool)
            .await
            .expect("seed Memory projection");
        }
        sqlx::query(
            "INSERT INTO structured_memory_facts
               (id, tenant_id, user_id, scope, app, channel, kind, subject_json,
                predicate, value_json, text, evidence_id, evidence_hash,
                observed_at, confidence, sensitivity, current,
                projection_memory_id, candidate_json)
             VALUES ('canonical-fact', 'tenant-memory', 'user-memory', 'global',
                     'chat', 'continuity', 'preference', '{\"kind\":\"user\"}',
                     'user.timezone', '{\"value\":\"UTC\"}',
                     'owner confirmed timezone UTC', 'owner-answer',
                     'hash:canonical-projection', CURRENT_TIMESTAMP, 1.0,
                     'internal', 1, 'canonical-projection', '{}')",
        )
        .execute(&pool)
        .await
        .expect("seed canonical Memory fact");

        sqlx::migrate!("./sqlite-migrations")
            .run(&pool)
            .await
            .expect("upgrade Memory fixture to v3");

        let facts = sqlx::query_as::<_, (String, String, i64)>(
            "SELECT projection_memory_id, lifecycle, current
             FROM structured_memory_facts
             WHERE tenant_id = 'tenant-memory' ORDER BY projection_memory_id",
        )
        .fetch_all(&pool)
        .await
        .expect("read migrated facts");
        assert_eq!(
            facts,
            vec![
                ("canonical-projection".into(), "confirmed".into(), 1),
                ("projection-only".into(), "candidate".into(), 0),
            ]
        );
        let rows = sqlx::query_as::<_, (String, String, Option<String>, i64)>(
            "SELECT fact.lifecycle, fact.candidate_json, item.embedding_json,
                    EXISTS (
                      SELECT 1 FROM memory_embedding_rebuild_outbox AS rebuild
                      WHERE rebuild.fact_id = fact.id
                    )
             FROM structured_memory_facts AS fact
             INNER JOIN agent_memory_items AS item
               ON item.id = fact.projection_memory_id
             WHERE fact.tenant_id = 'tenant-memory'
             ORDER BY fact.projection_memory_id",
        )
        .fetch_all(&pool)
        .await
        .expect("read Memory migration effects");
        for (_, candidate_json, embedding_json, _) in &rows {
            serde_json::from_str::<memory_engine::MemoryFactDraft>(candidate_json)
                .expect("migration must emit a reducer-compatible canonical fact");
            assert!(
                embedding_json.is_none(),
                "unversioned vector must be invalidated"
            );
        }
        assert_eq!(
            rows[0].3, 1,
            "confirmed fact must be queued for re-embedding"
        );
        assert_eq!(
            rows[1].3, 0,
            "candidate fact must not enter retrieval indexing"
        );
        let events: Vec<(String, String)> = sqlx::query_as(
            "SELECT operation, lifecycle FROM memory_fact_events
             WHERE tenant_id = 'tenant-memory' ORDER BY global_sequence",
        )
        .fetch_all(&pool)
        .await
        .expect("read canonical migration events");
        assert!(events.contains(&("confirmed".into(), "confirmed".into())));
        assert!(events.contains(&("candidate_created".into(), "candidate".into())));
    }

    #[tokio::test]
    async fn semantic_contract_scope_migration_maps_only_unambiguous_legacy_rows() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("open legacy contract fixture");
        sqlx::raw_sql(
            "CREATE TABLE data_sources (id TEXT PRIMARY KEY, tenant_id TEXT NOT NULL);
             CREATE TABLE nl2sql_metrics (id INTEGER PRIMARY KEY);
             CREATE TABLE nl2sql_join_paths (id INTEGER PRIMARY KEY);
             CREATE TABLE metric_contracts (
               id TEXT NOT NULL, tenant_id TEXT NOT NULL, version INTEGER NOT NULL,
               status TEXT NOT NULL, contract_json TEXT NOT NULL, valid_from TEXT NOT NULL,
               valid_until TEXT, PRIMARY KEY(tenant_id, id, version));
             CREATE TABLE join_contracts (
               id TEXT NOT NULL, tenant_id TEXT NOT NULL, version INTEGER NOT NULL,
               status TEXT NOT NULL, contract_json TEXT NOT NULL,
               PRIMARY KEY(tenant_id, id, version));
             INSERT INTO data_sources VALUES
               ('single-ds', 'single-tenant'),
               ('multi-a', 'multi-tenant'),
               ('multi-b', 'multi-tenant');
             INSERT INTO metric_contracts VALUES
               ('orders', 'single-tenant', 1, 'published', '{}', '2026-01-01', NULL),
               ('roi', 'multi-tenant', 2, 'published', '{}', '2026-01-01', NULL);
             INSERT INTO join_contracts VALUES
               ('orders-users', 'single-tenant', 1, 'published', '{}'),
               ('revenue-cost', 'multi-tenant', 3, 'published', '{}');",
        )
        .execute(&pool)
        .await
        .expect("seed legacy contract fixture");

        sqlx::raw_sql(include_str!(
            "../sqlite-migrations/0030_semantic_contract_production_scope.sql"
        ))
        .execute(&pool)
        .await
        .expect("upgrade legacy semantic contracts");

        let single_metric: (String, String) = sqlx::query_as(
            "SELECT datasource_id, status FROM metric_contracts
             WHERE tenant_id = 'single-tenant' AND id = 'orders'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(single_metric, ("single-ds".into(), "published".into()));

        let ambiguous_metric: (String, String, String) = sqlx::query_as(
            "SELECT datasource_id, status, lineage_json FROM metric_contracts
             WHERE tenant_id = 'multi-tenant' AND id = 'roi'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(ambiguous_metric.0, "__legacy_unscoped__");
        assert_eq!(ambiguous_metric.1, "legacy_unscoped");
        assert!(ambiguous_metric.2.contains("blocked_ambiguous_datasource"));

        let join_rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM join_contracts")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(join_rows, 2);
    }

    #[tokio::test]
    async fn n_minus_one_and_two_snapshots_upgrade_without_semantic_data_loss() {
        for snapshot_version in [31_i64, 32_i64] {
            let pool = SqlitePoolOptions::new()
                .max_connections(1)
                .connect("sqlite::memory:")
                .await
                .expect("open upgrade fixture");
            migrate_through(&pool, snapshot_version).await;

            sqlx::query(
                "INSERT INTO metric_contracts
                    (id, tenant_id, datasource_id, source_metric_id, version, status,
                     contract_json, lineage_json, valid_from, valid_until)
                 VALUES ('metric:legacy', 'tenant-upgrade', 'ds-upgrade', 7, 3, 'active',
                         '{\"id\":\"metric:legacy\"}', '{\"source\":\"upgrade-fixture\"}',
                         '2026-01-01T00:00:00Z', NULL)",
            )
            .execute(&pool)
            .await
            .expect("seed metric contract");
            sqlx::query(
                "INSERT INTO join_contracts
                    (id, tenant_id, datasource_id, source_kind, source_id, version,
                     status, contract_json, lineage_json, valid_from, valid_until)
                 VALUES ('join:legacy', 'tenant-upgrade', 'ds-upgrade', 'join_path', 9, 2,
                         'active', '{\"id\":\"join:legacy\"}',
                         '{\"source\":\"upgrade-fixture\"}',
                         '2026-01-01T00:00:00Z', NULL)",
            )
            .execute(&pool)
            .await
            .expect("seed join contract");
            sqlx::query(
                "INSERT INTO capability_tokens
                    (id, tenant_id, user_id, session_id, tool_name, resource_scope,
                     action_scope, executor_scope, child_scope, expires_at, remaining_uses)
                 VALUES ('legacy-capability', 'tenant-upgrade', 'user-upgrade', 'session-upgrade',
                         'read_file', 'workspace', 'read', 'native', NULL,
                         '2099-01-01T00:00:00Z', 2)",
            )
            .execute(&pool)
            .await
            .expect("seed capability");

            let full = sqlx::migrate!("./sqlite-migrations");
            full.run(&pool).await.expect("upgrade snapshot to current");
            full.run(&pool)
                .await
                .expect("repeated startup must keep the migration ledger stable");

            let metric: (String, String, i64) = sqlx::query_as(
                "SELECT contract_json, lineage_json, version FROM metric_contracts
                 WHERE tenant_id = 'tenant-upgrade' AND datasource_id = 'ds-upgrade'
                   AND id = 'metric:legacy'",
            )
            .fetch_one(&pool)
            .await
            .expect("load upgraded metric contract");
            assert_eq!(
                metric,
                (
                    "{\"id\":\"metric:legacy\"}".into(),
                    "{\"source\":\"upgrade-fixture\"}".into(),
                    3,
                )
            );
            let join: (String, String, i64) = sqlx::query_as(
                "SELECT contract_json, lineage_json, version FROM join_contracts
                 WHERE tenant_id = 'tenant-upgrade' AND datasource_id = 'ds-upgrade'
                   AND id = 'join:legacy'",
            )
            .fetch_one(&pool)
            .await
            .expect("load upgraded join contract");
            assert_eq!(
                join,
                (
                    "{\"id\":\"join:legacy\"}".into(),
                    "{\"source\":\"upgrade-fixture\"}".into(),
                    2,
                )
            );
            let capability: (i64, String, Option<String>, Option<String>) = sqlx::query_as(
                "SELECT remaining_uses, policy_version, parent_token_id, revoked_at
                 FROM capability_tokens WHERE id = 'legacy-capability'",
            )
            .fetch_one(&pool)
            .await
            .expect("load upgraded capability");
            assert_eq!(capability, (2, "capability-policy-v1".into(), None, None));
            let migration_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations")
                .fetch_one(&pool)
                .await
                .expect("count current migration ledger");
            assert!(
                migration_count >= 40,
                "upgrade fixture must apply every current durable schema migration"
            );
            pool.close().await;
        }
    }

    #[tokio::test]
    async fn deep_research_budget_migration_only_updates_the_legacy_default() {
        let pool = crate::test_sqlite_pool().await;
        for (tenant_id, pipeline_timeout_secs) in [
            ("legacy-default", 1800_i64),
            ("tenant-customized", 1799_i64),
        ] {
            sqlx::query(
                "INSERT INTO pm_budget_profiles
                    (tenant_id, profile_key, display_name, enabled, is_default, priority,
                     pipeline_timeout_secs, max_attempts, retrieve_max_tool_calls,
                     max_calls_per_source, source_slot_search_secs, source_slot_browser_secs,
                     source_slot_api_fetch_secs, preflight_model_timeout_secs,
                     preflight_probe_timeout_secs, preflight_overall_timeout_secs,
                     retry_step_budget_secs, retry_total_budget_secs)
                 VALUES (?, 'normal', 'Normal', 1, 1, 100, ?, 4, 12, 3,
                         300, 300, 300, 30, 10, 120, 90, 420)",
            )
            .bind(tenant_id)
            .bind(pipeline_timeout_secs)
            .execute(&pool)
            .await
            .expect("insert budget profile fixture");
        }

        sqlx::query(include_str!(
            "../sqlite-migrations/0004_deep_research_runtime_budget.sql"
        ))
        .execute(&pool)
        .await
        .expect("reapply deep research budget migration statement");

        let migrated: (i64, i64, i64, i64) = sqlx::query_as(
            "SELECT pipeline_timeout_secs, source_slot_search_secs,
                    source_slot_browser_secs, retry_total_budget_secs
             FROM pm_budget_profiles WHERE tenant_id = 'legacy-default'",
        )
        .fetch_one(&pool)
        .await
        .expect("load migrated default budget");
        assert_eq!(migrated, (540, 90, 120, 240));

        let customized: (i64, i64, i64, i64) = sqlx::query_as(
            "SELECT pipeline_timeout_secs, source_slot_search_secs,
                    source_slot_browser_secs, retry_total_budget_secs
             FROM pm_budget_profiles WHERE tenant_id = 'tenant-customized'",
        )
        .fetch_one(&pool)
        .await
        .expect("load customized budget");
        assert_eq!(customized, (1799, 300, 300, 420));
        pool.close().await;
    }

    #[tokio::test]
    async fn bounded_research_migration_preserves_customized_profiles() {
        let pool = crate::test_sqlite_pool().await;
        for (tenant_id, pipeline_timeout_secs) in [
            ("bounded-default", 540_i64),
            ("bounded-customized", 541_i64),
        ] {
            sqlx::query(
                "INSERT INTO pm_budget_profiles
                    (tenant_id, profile_key, display_name, enabled, is_default, priority,
                     pipeline_timeout_secs, max_attempts, retrieve_max_tool_calls,
                     max_calls_per_source, source_slot_search_secs, source_slot_browser_secs,
                     source_slot_api_fetch_secs, preflight_model_timeout_secs,
                     preflight_probe_timeout_secs, preflight_overall_timeout_secs,
                     retry_step_budget_secs, retry_total_budget_secs)
                 VALUES (?, 'normal', 'Normal', 1, 1, 100, ?, 4, 12, 3,
                         90, 120, 90, 30, 10, 45, 75, 240)",
            )
            .bind(tenant_id)
            .bind(pipeline_timeout_secs)
            .execute(&pool)
            .await
            .expect("insert bounded budget fixture");
        }

        sqlx::query(include_str!(
            "../sqlite-migrations/0007_deep_research_bounded_execution.sql"
        ))
        .execute(&pool)
        .await
        .expect("reapply bounded research migration statement");

        let migrated: (i64, i64, i64, i64) = sqlx::query_as(
            "SELECT pipeline_timeout_secs, max_attempts, retrieve_max_tool_calls,
                    source_slot_search_secs
             FROM pm_budget_profiles WHERE tenant_id = 'bounded-default'",
        )
        .fetch_one(&pool)
        .await
        .expect("load bounded default");
        assert_eq!(migrated, (480, 3, 8, 110));

        let customized: (i64, i64, i64, i64) = sqlx::query_as(
            "SELECT pipeline_timeout_secs, max_attempts, retrieve_max_tool_calls,
                    source_slot_search_secs
             FROM pm_budget_profiles WHERE tenant_id = 'bounded-customized'",
        )
        .fetch_one(&pool)
        .await
        .expect("load bounded custom profile");
        assert_eq!(customized, (541, 4, 12, 90));
        pool.close().await;
    }

    #[tokio::test]
    async fn marginal_evidence_budget_migration_preserves_customized_profiles() {
        let pool = crate::test_sqlite_pool().await;
        for (tenant_id, pipeline_timeout_secs) in [
            ("marginal-default", 480_i64),
            ("marginal-customized", 481_i64),
        ] {
            sqlx::query(
                "INSERT INTO pm_budget_profiles
                    (tenant_id, profile_key, display_name, enabled, is_default, priority,
                     pipeline_timeout_secs, max_attempts, retrieve_max_tool_calls,
                     max_calls_per_source, source_slot_search_secs, source_slot_browser_secs,
                     source_slot_api_fetch_secs, preflight_model_timeout_secs,
                     preflight_probe_timeout_secs, preflight_overall_timeout_secs,
                     retry_step_budget_secs, retry_total_budget_secs)
                 VALUES (?, 'normal', 'Normal', 1, 1, 100, ?, 3, 8, 3,
                         110, 120, 90, 30, 10, 45, 75, 240)",
            )
            .bind(tenant_id)
            .bind(pipeline_timeout_secs)
            .execute(&pool)
            .await
            .expect("insert marginal evidence budget fixture");
        }

        sqlx::query(include_str!(
            "../sqlite-migrations/0010_deep_research_marginal_evidence_budget.sql"
        ))
        .execute(&pool)
        .await
        .expect("reapply marginal evidence budget migration statement");

        let migrated: (i64, i64, i64, i64, i64, i64) = sqlx::query_as(
            "SELECT pipeline_timeout_secs, max_attempts, retrieve_max_tool_calls,
                    source_slot_search_secs, source_slot_browser_secs, retry_total_budget_secs
             FROM pm_budget_profiles WHERE tenant_id = 'marginal-default'",
        )
        .fetch_one(&pool)
        .await
        .expect("load marginal default budget");
        assert_eq!(migrated, (390, 2, 6, 90, 100, 150));

        let customized: (i64, i64, i64, i64, i64, i64) = sqlx::query_as(
            "SELECT pipeline_timeout_secs, max_attempts, retrieve_max_tool_calls,
                    source_slot_search_secs, source_slot_browser_secs, retry_total_budget_secs
             FROM pm_budget_profiles WHERE tenant_id = 'marginal-customized'",
        )
        .fetch_one(&pool)
        .await
        .expect("load customized marginal budget");
        assert_eq!(customized, (481, 3, 8, 110, 120, 240));
        pool.close().await;
    }

    #[tokio::test]
    async fn experience_budget_migration_preserves_customized_profiles() {
        let pool = crate::test_sqlite_pool().await;
        for (tenant_id, pipeline_timeout_secs) in [
            ("experience-default", 390_i64),
            ("experience-customized", 391_i64),
        ] {
            sqlx::query(
                "INSERT INTO pm_budget_profiles
                    (tenant_id, profile_key, display_name, enabled, is_default, priority,
                     pipeline_timeout_secs, max_attempts, retrieve_max_tool_calls,
                     max_calls_per_source, source_slot_search_secs, source_slot_browser_secs,
                     source_slot_api_fetch_secs, preflight_model_timeout_secs,
                     preflight_probe_timeout_secs, preflight_overall_timeout_secs,
                     retry_step_budget_secs, retry_total_budget_secs)
                 VALUES (?, 'normal', 'Normal', 1, 1, 100, ?, 2, 6, 3,
                         90, 100, 75, 30, 10, 45, 60, 150)",
            )
            .bind(tenant_id)
            .bind(pipeline_timeout_secs)
            .execute(&pool)
            .await
            .expect("insert experience budget fixture");
        }

        sqlx::query(include_str!(
            "../sqlite-migrations/0013_deep_research_experience_budget.sql"
        ))
        .execute(&pool)
        .await
        .expect("reapply experience budget migration statement");

        let migrated: (i64, i64, i64, i64, i64) = sqlx::query_as(
            "SELECT pipeline_timeout_secs, source_slot_search_secs,
                    source_slot_browser_secs, source_slot_api_fetch_secs,
                    retry_total_budget_secs
             FROM pm_budget_profiles WHERE tenant_id = 'experience-default'",
        )
        .fetch_one(&pool)
        .await
        .expect("load experience default budget");
        assert_eq!(migrated, (360, 75, 90, 60, 120));

        let customized: (i64, i64, i64, i64, i64) = sqlx::query_as(
            "SELECT pipeline_timeout_secs, source_slot_search_secs,
                    source_slot_browser_secs, source_slot_api_fetch_secs,
                    retry_total_budget_secs
             FROM pm_budget_profiles WHERE tenant_id = 'experience-customized'",
        )
        .fetch_one(&pool)
        .await
        .expect("load customized experience budget");
        assert_eq!(customized, (391, 90, 100, 75, 150));
        pool.close().await;
    }
}
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

pub use error::{AppError, Result};
pub use state::AppState;

const DEFAULT_TOKIO_WORKER_STACK_SIZE_BYTES: usize = 16 * 1024 * 1024;

#[cfg(feature = "bot-agents")]
fn init_bot_gateway_tls_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

#[cfg(not(feature = "bot-agents"))]
fn init_bot_gateway_tls_provider() {}

fn init_tracing() {
    let _ = tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                "web_server=debug,agent_gateway=debug,agent_gateway::runtime_builder=debug,runtime=debug,tower_http=debug,billing=info".into()
            }),
        )
        .with(tracing_subscriber::fmt::layer())
        .try_init();
}

fn log_startup_phase(started: Instant, phase: &'static str) {
    tracing::info!(
        phase = %phase,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "startup phase completed"
    );
}

fn enabled_feature_list() -> String {
    let mut features = Vec::new();
    if cfg!(feature = "agent") {
        features.push("agent");
    }
    if cfg!(feature = "pm") {
        features.push("pm");
    }
    if cfg!(feature = "nl2sql") {
        features.push("nl2sql");
    }
    if cfg!(feature = "rd") {
        features.push("rd");
    }
    if cfg!(feature = "bot-agents") {
        features.push("bot-agents");
    }
    if cfg!(feature = "projects") {
        features.push("projects");
    }
    if features.is_empty() {
        "default".to_string()
    } else {
        features.join(",")
    }
}

pub fn configured_tokio_worker_stack_size_bytes() -> usize {
    std::env::var("AOS_TOKIO_WORKER_STACK_SIZE_BYTES")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value >= 2 * 1024 * 1024)
        .unwrap_or(DEFAULT_TOKIO_WORKER_STACK_SIZE_BYTES)
}

fn log_startup_fingerprint() {
    let current_exe = std::env::current_exe()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|error| format!("unknown ({error})"));
    tracing::info!(
        current_exe = %current_exe,
        package_version = env!("CARGO_PKG_VERSION"),
        build_profile = if cfg!(debug_assertions) { "debug" } else { "release" },
        enabled_features = %enabled_feature_list(),
        tokio_worker_stack_size_bytes = configured_tokio_worker_stack_size_bytes(),
        rust_log = ?std::env::var("RUST_LOG").ok(),
        "web-server startup fingerprint"
    );
}

async fn log_http_errors(req: Request<Body>, next: Next) -> Response {
    let method = req.method().clone();
    let uri = crate::auth_middleware::sanitized_request_uri(req.uri());
    let started = Instant::now();
    let response = next.run(req).await;
    let status = response.status();
    if status.is_server_error() {
        tracing::error!(
            method = %method,
            uri = %uri,
            status = status.as_u16(),
            elapsed_ms = started.elapsed().as_millis() as u64,
            "http request returned 5xx"
        );
    } else if status == StatusCode::PRECONDITION_REQUIRED {
        tracing::warn!(
            method = %method,
            uri = %uri,
            status = status.as_u16(),
            elapsed_ms = started.elapsed().as_millis() as u64,
            "http request blocked before setup completed"
        );
    } else if status.is_client_error() {
        tracing::warn!(
            method = %method,
            uri = %uri,
            status = status.as_u16(),
            elapsed_ms = started.elapsed().as_millis() as u64,
            "http request returned 4xx"
        );
    }
    response
}

fn normalized_cors_origin(raw: &str) -> Option<HeaderValue> {
    let uri = raw.trim().parse::<Uri>().ok()?;
    let scheme = uri.scheme_str()?;
    let authority = uri.authority()?.as_str();
    HeaderValue::from_str(&format!("{scheme}://{authority}")).ok()
}

fn build_cors_layer(base_url: &str) -> CorsLayer {
    let configured = std::env::var("CORS_ALLOWED_ORIGINS").unwrap_or_default();
    let configured_origins = configured
        .split(',')
        .map(str::trim)
        .filter(|origin| !origin.is_empty())
        .collect::<Vec<_>>();

    if configured_origins.iter().any(|origin| *origin == "*") {
        tracing::warn!(
            "CORS_ALLOWED_ORIGINS explicitly allows every origin; use a fixed origin list outside isolated development"
        );
        return CorsLayer::new()
            .allow_origin(Any)
            .allow_methods(Any)
            .allow_headers(Any);
    }

    let raw_origins = if configured_origins.is_empty() {
        vec![base_url]
    } else {
        configured_origins
    };
    let mut origins = raw_origins
        .into_iter()
        .filter_map(normalized_cors_origin)
        .collect::<Vec<_>>();
    origins.sort_unstable_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
    origins.dedup();

    let layer = CorsLayer::new().allow_methods(Any).allow_headers(Any);
    if origins.is_empty() {
        tracing::warn!(
            base_url,
            "no valid CORS origins were configured; cross-origin browser requests are disabled"
        );
        layer
    } else {
        layer.allow_origin(AllowOrigin::list(origins))
    }
}

#[cfg(test)]
mod cors_tests {
    use super::normalized_cors_origin;

    #[test]
    fn cors_origin_keeps_only_scheme_and_authority() {
        let origin = normalized_cors_origin("https://aos.example.com/app/path?ignored=1")
            .expect("valid origin");
        assert_eq!(origin, "https://aos.example.com");
    }

    #[test]
    fn cors_origin_rejects_relative_or_malformed_values() {
        assert!(normalized_cors_origin("/relative").is_none());
        assert!(normalized_cors_origin("not a url").is_none());
    }
}

/// Build the Axum router with all routes mounted.
pub fn build_router(state: AppState) -> Router<()> {
    init_bot_gateway_tls_provider();

    let cors = build_cors_layer(&state.base_url);
    let app_state = if state.usage_writer.is_some() {
        state
    } else {
        let usage_writer = crate::routes::chat::TokenUsageWriter::new(state.db.clone());
        state.with_usage_writer(usage_writer)
    };

    #[allow(unused_mut)]
    let mut base: Router<AppState> = Router::new()
        .nest("/api/v1/auth", routes::auth::routes(app_state.clone()))
        .nest("/api/v1/setup", routes::setup::routes())
        .nest("/api/v1/users", routes::users::routes(app_state.clone()))
        .nest(
            "/api/v1/notifications",
            routes::notifications::routes(app_state.clone()),
        )
        .nest("/api/v1/dashboard", routes::dashboard::routes(&app_state))
        .nest("/api/v1/mcp", routes::mcp::routes(app_state.clone()))
        .nest(
            "/api/v1/memory",
            routes::memory_continuity::routes(app_state.clone()),
        )
        .nest(
            "/api/v1/workspace",
            routes::personal_workspace::routes(app_state.clone()),
        )
        .nest("/api/v1/skills", routes::skills::routes(app_state.clone()))
        .nest("/api/v1/hooks", routes::hooks::routes(app_state.clone()))
        .nest(
            "/api/v1/apikeys",
            routes::apikeys::routes(app_state.clone()),
        )
        .nest(
            "/api/v1/sessions",
            routes::sessions::routes(app_state.clone()),
        )
        .nest(
            "/api/v1/tenants",
            routes::tenants::routes(app_state.clone()),
        )
        .nest(
            "/api/v1/chat",
            routes::chat::routes(app_state.clone())
                .merge(routes::chat_capabilities::routes(app_state.clone())),
        )
        .nest(
            "/api/v1/chat",
            routes::chat_intelligence::routes(app_state.clone()),
        )
        .nest("/api/v1/uploads", routes::upload::routes(app_state.clone()))
        .nest("/api/v1/config", routes::config::routes(app_state.clone()))
        .nest("/api/v1/demo", routes::demo::routes(app_state.clone()))
        .nest(
            "/api/v1/agent-ops",
            routes::agent_ops::routes(app_state.clone()),
        )
        .nest(
            "/api/v1/agent-runtime",
            routes::agent_runtime::routes(app_state.clone()),
        )
        .nest(
            "/api/v1/tasks",
            routes::task_control::routes(app_state.clone()),
        )
        .nest(
            "/api/v1/bot-identities",
            routes::task_control::identity_routes(app_state.clone()),
        )
        // WebSocket endpoints (with auth middleware)
        .nest("/ws", routes::system_events::ws_routes(app_state.clone()));

    #[cfg(feature = "bot-agents")]
    {
        base = base.nest(
            "/api/v1/bot-agents",
            routes::bot_agents::routes(app_state.clone()),
        );
        base = base.nest(
            "/api/v1/super-assistant",
            routes::super_assistant::routes(app_state.clone()),
        );
    }
    #[cfg(feature = "agent")]
    {
        base = base.nest("/api/v1/agent", routes::agent::routes(app_state.clone()));
    }
    #[cfg(feature = "pm")]
    {
        base = base.nest("/api/v1/pm", routes::pm::routes(app_state.clone()));
    }
    #[cfg(feature = "projects")]
    {
        base = base.nest(
            "/api/v1/projects",
            routes::projects::routes(app_state.clone()),
        );
    }
    #[cfg(feature = "rd")]
    {
        base = base.nest("/api/v1/rd", routes::rd::routes(app_state.clone()));
    }
    #[cfg(feature = "nl2sql")]
    {
        base = base
            .nest(
                "/api/v1/data-sources",
                routes::data_sources::routes(app_state.clone()),
            )
            .nest("/api/v1/nl2sql", routes::nl2sql::routes(app_state.clone()));
    }

    base.layer(axum::middleware::from_fn_with_state(
        app_state.clone(),
        crate::auth_middleware::require_setup_initialized,
    ))
    .layer(cors)
    .layer(axum::middleware::from_fn(log_http_errors))
    .layer(TraceLayer::new_for_http())
    .with_state(app_state)
}

/// Build the agent session manager.
///
/// Injects the Super_Assistant "extract → persist → compact" hook factory so
/// that every runtime the manager builds runs the extract → persist → pinned
/// closure at its real auto-compaction trigger, persisting key info to
/// Unified_Memory *before* compaction commits (先持久化再压缩, Req 4.1 / 4.3 /
/// 4.9). The factory captures only a cheap DB pool handle (not `AppState`), so
/// the hook — held per session by the manager — never forms a reference cycle
/// back to the manager stored inside `AppState`.
pub fn build_agent_manager(
    db: &sqlx::SqlitePool,
    data_dir: std::path::PathBuf,
    config_home: std::path::PathBuf,
    config_registry: Arc<agent_gateway::TenantConfigRegistry>,
) -> std::result::Result<Arc<agent_gateway::AgentSessionManager>, agent_gateway::GatewayError> {
    let hook_config_registry = config_registry.clone();
    let compaction_hook_factory: agent_gateway::CompactionHookFactory =
        Arc::new(move |ctx: agent_gateway::CompactionHookContext| {
            let tenant_id = ctx.tenant_id.clone();
            let user_id = ctx.user_id.clone();
            let session_id = ctx.session_id.clone();
            let hook = crate::routes::super_assistant::RuntimeCompactionHook::new(
                ctx.db.clone(),
                tenant_id.clone(),
                user_id.clone(),
                session_id.clone(),
                ctx.app,
            )
            .with_config_registry(hook_config_registry.clone(), ctx.model.clone())
            .with_execution_kernel(Arc::new(
                crate::semantic_kernel_store::RuntimeExecutionKernel::new(
                    ctx.db, tenant_id, user_id, session_id,
                ),
            ));
            Arc::new(hook) as Arc<dyn runtime::CompactionHook>
        });
    agent_gateway::build_session_manager_with_registry(
        db,
        data_dir,
        config_home,
        config_registry,
        Some(compaction_hook_factory),
    )
}

/// Build the minimal application state needed by a standalone PM worker
/// process. It intentionally skips HTTP-only background services.
#[cfg(all(feature = "pm", feature = "agent"))]
pub async fn init_pm_worker_state(
    data_dir: PathBuf,
    default_model: Option<String>,
) -> anyhow::Result<AppState> {
    init_tracing();
    log_startup_fingerprint();
    let started = Instant::now();
    let mut state = AppState::new(data_dir.clone(), default_model).await?;
    let usage_writer = crate::routes::chat::TokenUsageWriter::new(state.db.clone());
    state = state.with_usage_writer(usage_writer);
    let config_registry = Arc::new(agent_gateway::TenantConfigRegistry::new(state.db.clone()));
    state.config_registry = Some(config_registry.clone());
    let config_home = data_dir.clone();
    let agent_manager = build_agent_manager(&state.db, data_dir, config_home, config_registry)?;
    state.agent_manager = Some(agent_manager);
    log_startup_phase(started, "pm_worker_state");
    Ok(state)
}

#[cfg(all(feature = "pm", feature = "agent"))]
pub async fn run_pm_worker_loop(state: AppState) {
    use std::time::Duration;
    use tokio::time::{interval, MissedTickBehavior};

    let runtime_interval_secs = std::env::var("PM_RESEARCH_TASK_RUNTIME_POLL_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(5)
        .max(1);
    tracing::info!(
        runtime_interval_secs,
        "standalone PM worker process started"
    );

    let mut runtime_ticker = interval(Duration::from_secs(runtime_interval_secs));
    runtime_ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    runtime_ticker.tick().await;

    loop {
        tokio::select! {
            _ = runtime_ticker.tick() => {
                if let Err(error) = crate::routes::agent::run_pm_background_runtime_cycle(&state).await {
                    tracing::warn!(
                        error = %error,
                        error_debug = ?error,
                        "standalone PM worker runtime cycle failed"
                    );
                }
            }
            _ = pm_worker_shutdown_signal() => {
                tracing::info!("standalone PM worker shutdown received");
                break;
            }
        }
    }
}

#[cfg(all(feature = "pm", feature = "agent"))]
async fn pm_worker_shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut sig) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            sig.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
}

async fn web_server_shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut sig) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            sig.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
}

/// Start the HTTP server.
pub async fn serve(addr: SocketAddr, data_dir: PathBuf) {
    serve_with_options(addr, data_dir, None, None, None).await;
}

/// Start the HTTP server with optional telemetry, model, and Web UI overrides.
pub async fn serve_with_options(
    addr: SocketAddr,
    data_dir: PathBuf,
    telemetry_dir: Option<PathBuf>,
    default_model: Option<String>,
    web_dir: Option<PathBuf>,
) {
    #[cfg(feature = "nl2sql")]
    crate::nl2sql::embedding::configure_local_embedding_cache_for_data_dir(&data_dir)
        .expect("failed to configure local embedding cache");

    let startup_started = Instant::now();
    init_tracing();
    log_startup_fingerprint();
    log_startup_phase(startup_started, "tracing_init");

    // Initialize system events broadcast channel
    routes::system_events::init_broadcast_channel();

    let phase_started = Instant::now();
    let mut state = AppState::new(data_dir.clone(), default_model)
        .await
        .expect("failed to init app state");
    let usage_writer = crate::routes::chat::TokenUsageWriter::new(state.db.clone());
    state = state.with_usage_writer(usage_writer);
    log_startup_phase(phase_started, "app_state");

    #[cfg(feature = "nl2sql")]
    let embed_store = {
        // Initialize the registry of physically isolated tenant/profile stores.
        let phase_started = Instant::now();
        let embed_store = match crate::nl2sql::embedding::EmbeddingStoreRegistry::open(
            data_dir.join("nl2sql").join("embedding-profiles"),
        ) {
            Ok(store) => {
                log_startup_phase(phase_started, "nl2sql_embedding_store_open");
                let store = Arc::new(store);
                tracing::info!("NL2SQL embedding profile registry initialized");
                Some(store)
            }
            Err(e) => {
                log_startup_phase(phase_started, "nl2sql_embedding_store_open");
                tracing::warn!(
                    "failed to init NL2SQL embedding store: {e}. Semantic routing disabled."
                );
                None
            }
        };
        state.nl2sql_embedding_store = embed_store.clone();
        embed_store
    };

    #[cfg(feature = "nl2sql")]
    if let Some(registry) = embed_store.clone() {
        crate::nl2sql::embedding_reindex_worker::start(state.db.clone(), registry);
        tracing::info!("NL2SQL embedding shadow-index worker started");
    }

    // Initialize RD embedding store (SQLite-backed, best-effort). The store is
    // only used for semantic context ranking; repository/task indexing is
    // scheduled asynchronously so Code 开发 main flows never wait on it.
    #[cfg(feature = "rd")]
    {
        let phase_started = Instant::now();
        state.rd_embedding_store = match crate::routes::rd::embedding::RdEmbeddingStore::open(
            &data_dir.join("rd").join("embeddings.db"),
        ) {
            Ok(store) => {
                log_startup_phase(phase_started, "rd_embedding_store_open");
                tracing::info!("RD embedding store initialized");
                Some(Arc::new(store))
            }
            Err(e) => {
                log_startup_phase(phase_started, "rd_embedding_store_open");
                tracing::warn!(
                    "failed to init RD embedding store: {e}. RD semantic retrieval disabled."
                );
                None
            }
        };
    }

    // Initialize NL2SQL routing engine if store is available
    #[cfg(feature = "nl2sql")]
    {
        let routing_engine = embed_store.as_ref().and_then(|registry| {
            let embed_url = std::env::var("EMBEDDING_BASE_URL").ok();
            let embed_api_key =
                runtime::explicit_env_opt_in_enabled("AOS_ALLOW_TENANT_EMBEDDING_ENV_FALLBACK")
                    .then(|| {
                        std::env::var("OPENAI_API_KEY")
                            .ok()
                            .or_else(|| std::env::var("ANTHROPIC_API_KEY").ok())
                    })
                    .flatten();
            let config = crate::nl2sql::local_embedding_config_for_runtime();
            let profile_id = config.profile_id("__runtime_default__");
            registry
                .profile_store(
                    "__runtime_default__",
                    &profile_id,
                    std::env::var("EMBEDDING_MODEL")
                        .ok()
                        .as_deref()
                        .unwrap_or(crate::nl2sql::LOCAL_EMBEDDING_MODEL),
                    embed_url.clone(),
                )
                .ok()
                .map(|store| {
                    Arc::new(crate::nl2sql::routing::RoutingEngine::new(
                        store,
                        std::env::var("EMBEDDING_MODEL")
                            .ok()
                            .as_deref()
                            .or(Some(crate::nl2sql::LOCAL_EMBEDDING_MODEL)),
                        embed_url,
                        embed_api_key,
                    ))
                })
        });
        state.nl2sql_routing_engine = routing_engine;
    }

    let phase_started = Instant::now();
    let consumer_dir = telemetry_dir.unwrap_or_else(|| data_dir.clone());
    telemetry::start_telemetry_consumer(consumer_dir, state.db.clone());
    log_startup_phase(phase_started, "telemetry_consumer_start");

    let config_registry = Arc::new(agent_gateway::TenantConfigRegistry::new(state.db.clone()));
    state.config_registry = Some(config_registry.clone());
    #[cfg(feature = "nl2sql")]
    routes::nl2sql::reference::start_sql_knowledge_import_worker(state.clone());

    #[cfg(feature = "agent")]
    {
        let phase_started = Instant::now();
        let config_home = data_dir.clone();
        let agent_manager = build_agent_manager(
            &state.db,
            data_dir.clone(),
            config_home.clone(),
            config_registry.clone(),
        )
        .expect("failed to build agent session manager");
        state.agent_manager = Some(agent_manager.clone());
        log_startup_phase(phase_started, "agent_manager_build");
    }

    #[cfg(feature = "projects")]
    {
        let phase_started = Instant::now();
        let gitlab_manager = Arc::new(agent_gateway::GitlabProjectManager::new(
            state.db.clone(),
            data_dir.clone(),
        ));
        state.gitlab_manager = Some(gitlab_manager);
        log_startup_phase(phase_started, "gitlab_manager_build");
    }

    // Start background periodic MCP server health checks
    let phase_started = Instant::now();
    routes::mcp::start_periodic_mcp_checker(state.clone());
    log_startup_phase(phase_started, "mcp_checker_start");

    let phase_started = Instant::now();
    routes::task_control_worker::ensure_task_control_schema(&state)
        .await
        .expect("WatchDog control-plane schema health check failed");
    routes::task_control_worker::start_task_control_workers(state.clone());
    log_startup_phase(phase_started, "task_control_workers_start");

    // On startup, mark any tasks that were left in 'running'/'pending' by a
    // previous crash as 'failed'. This prevents them from blocking future
    // refreshes indefinitely. This is safe because no worker is executing
    // them — the process that owned them is gone.
    #[cfg(feature = "nl2sql")]
    {
        let db = state.db.clone();
        tokio::spawn(async move {
            let phase_started = Instant::now();
            let result = sqlx::query(
                "UPDATE nl2sql_refresh_tasks \
                 SET status = 'failed', \
                     error_message = 'server restarted while task was in progress' \
                 WHERE status IN ('running', 'pending')",
            )
            .execute(&db)
            .await;
            match result {
                Ok(r) if r.rows_affected() > 0 => tracing::warn!(
                    count = r.rows_affected(),
                    "startup: marked orphaned refresh tasks as failed"
                ),
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "startup: failed to clean up orphaned tasks"),
            }
            log_startup_phase(phase_started, "nl2sql_orphan_refresh_cleanup_background");
        });
    }

    // Start background periodic schema refresh + semantic re-indexing. Keep
    // the handle + shutdown sender so a Ctrl-C can stop the cycle cleanly
    // instead of having the task abort mid-refresh.
    #[cfg(feature = "nl2sql")]
    let (scheduler_shutdown, scheduler_handle) =
        routes::datasource_scheduler::start_periodic_schema_refresh(state.clone());
    #[cfg(feature = "pm")]
    let (pm_scheduler_shutdown, pm_scheduler_handle) =
        routes::pm_scheduler::start_periodic_pm_scheduler(state.clone());
    #[cfg(feature = "rd")]
    if let Err(error) = routes::rd::recover_interrupted_plan_generations(&state.db).await {
        tracing::warn!(%error, "failed to recover interrupted RD plan generation states");
    }
    #[cfg(feature = "nl2sql")]
    match routes::nl2sql::attribution::recover_interrupted_attribution_tasks(&state.db).await {
        Ok(count) if count > 0 => tracing::warn!(
            count,
            "archived interrupted data-attribution tasks while preserving durable progress"
        ),
        Ok(_) => {}
        Err(error) => {
            tracing::warn!(%error, "failed to recover interrupted data-attribution tasks")
        }
    }
    #[cfg(feature = "rd")]
    let (rd_repository_scheduler_shutdown, rd_repository_scheduler_handle) =
        routes::rd::start_periodic_repository_sync(state.clone());
    #[cfg(feature = "bot-agents")]
    let phase_started = Instant::now();
    #[cfg(feature = "bot-agents")]
    routes::bot_agents_inbound::start_bot_agent_inbound_runtime(state.clone());
    #[cfg(feature = "bot-agents")]
    log_startup_phase(phase_started, "bot_agent_inbound_runtime_start");
    #[cfg(feature = "bot-agents")]
    {
        let phase_started = Instant::now();
        routes::bot_agents::start_bot_gateway_queue_worker(state.clone());
        log_startup_phase(phase_started, "bot_gateway_queue_worker_start");
    }
    #[cfg(feature = "nl2sql")]
    let (ann_shutdown, ann_handle) = if let Some(store) = embed_store.clone() {
        let (tx, mut rx) = tokio::sync::watch::channel(false);
        let interval_secs = std::env::var("NL2SQL_ANN_SNAPSHOT_INTERVAL_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(30)
            .max(5);
        let handle = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(std::time::Duration::from_secs(interval_secs));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            ticker.tick().await; // skip the immediate first tick during server startup
            loop {
                tokio::select! {
                    _ = ticker.tick() => {
                        let store = Arc::clone(&store);
                        match tokio::task::spawn_blocking(move || store.persist_ann_snapshots_if_dirty()).await {
                            Ok(Ok(count)) if count > 0 => tracing::info!(count, "ANN profile snapshots persisted to disk"),
                            Ok(Ok(_)) => {}
                            Ok(Err(e)) => tracing::warn!(error = %e, "ANN snapshot persist failed"),
                            Err(e) => tracing::warn!(error = %e, "ANN snapshot worker join failed"),
                        }
                    }
                    changed = rx.changed() => {
                        if changed.is_err() || *rx.borrow() {
                            break;
                        }
                    }
                }
            }
        });
        (Some(tx), Some(handle))
    } else {
        (None, None)
    };

    let shutdown_state = state.clone();
    let phase_started = Instant::now();
    let api_router = build_router(state);
    let router = if let Some(web_dir) = web_dir {
        tracing::info!(web_dir = %web_dir.display(), "serving built Web UI");
        Router::new()
            .merge(api_router)
            .fallback_service(web_ui_service(web_dir))
    } else {
        api_router
    };
    log_startup_phase(phase_started, "router_build");

    let phase_started = Instant::now();
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("failed to bind port");
    log_startup_phase(phase_started, "tcp_bind");
    tracing::info!(
        addr = %addr,
        startup_elapsed_ms = startup_started.elapsed().as_millis() as u64,
        "listening on {addr}"
    );

    // Keep graceful shutdown bounded. Long-lived SSE/WebSocket connections can
    // otherwise keep Axum alive indefinitely after SIGTERM.
    let (shutdown_tx, mut shutdown_observer) = tokio::sync::watch::channel(false);
    let mut server_shutdown = shutdown_tx.subscribe();
    tokio::spawn(async move {
        web_server_shutdown_signal().await;
        tracing::info!("shutdown signal received, stopping server");
        let _ = shutdown_tx.send(true);
    });
    let server = std::future::IntoFuture::into_future(
        axum::serve(listener, router).with_graceful_shutdown(async move {
            let _ = server_shutdown.changed().await;
        }),
    );
    tokio::pin!(server);
    let server_result = tokio::select! {
        result = &mut server => result,
        changed = shutdown_observer.changed() => {
            if changed.is_err() {
                (&mut server).await
            } else {
                match tokio::time::timeout(std::time::Duration::from_secs(5), &mut server).await {
                    Ok(result) => result,
                    Err(_) => {
                        tracing::warn!("HTTP connections did not drain within 5s; forcing server close");
                        Ok(())
                    }
                }
            }
        }
    };
    if let Err(e) = server_result {
        tracing::error!("server error: {e}");
    }

    // Tell the scheduler to stop and wait for its current cycle.
    #[cfg(feature = "nl2sql")]
    let _ = scheduler_shutdown.send(true);
    #[cfg(feature = "pm")]
    let _ = pm_scheduler_shutdown.send(true);
    #[cfg(feature = "rd")]
    let _ = rd_repository_scheduler_shutdown.send(true);
    #[cfg(feature = "nl2sql")]
    if let Some(tx) = ann_shutdown {
        let _ = tx.send(true);
    }
    // Use one shared deadline instead of waiting for each worker serially. A
    // timed-out task is explicitly aborted so shutdown duration stays bounded.
    let mut shutdown_handles: Vec<(&'static str, tokio::task::JoinHandle<()>)> = Vec::new();
    #[cfg(feature = "nl2sql")]
    shutdown_handles.push(("scheduler", scheduler_handle));
    #[cfg(feature = "pm")]
    shutdown_handles.push(("pm scheduler", pm_scheduler_handle));
    #[cfg(feature = "rd")]
    shutdown_handles.push(("repository scheduler", rd_repository_scheduler_handle));
    #[cfg(feature = "nl2sql")]
    if let Some(handle) = ann_handle {
        shutdown_handles.push(("ANN snapshot worker", handle));
    }
    let shutdown_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    for (name, mut handle) in shutdown_handles {
        match tokio::time::timeout_at(shutdown_deadline, &mut handle).await {
            Ok(Ok(())) => tracing::info!(task = name, "background task exited cleanly"),
            Ok(Err(error)) => {
                tracing::warn!(task = name, %error, "background task panicked on exit");
            }
            Err(_) => {
                tracing::warn!(
                    task = name,
                    "background task missed shutdown deadline; aborting"
                );
                handle.abort();
                let _ = handle.await;
            }
        }
    }
    if let Err(error) = shutdown_state.mark_clean_shutdown().await {
        tracing::warn!(%error, "failed to checkpoint SQLite or clear the unclean marker");
    }
}

#[cfg(test)]
mod web_ui_service_tests {
    use super::*;
    use axum::body::to_bytes;
    use tower::ServiceExt;

    #[tokio::test]
    async fn agent_manager_reuses_web_config_registry() {
        let pool = crate::test_sqlite_pool().await;
        let registry = Arc::new(agent_gateway::TenantConfigRegistry::new(pool.clone()));
        let data_dir = std::env::temp_dir().join(format!("aos-agent-{}", uuid::Uuid::new_v4()));
        let manager = build_agent_manager(&pool, data_dir.clone(), data_dir, registry.clone())
            .expect("build agent manager");

        assert!(Arc::ptr_eq(&registry, &manager.config_registry()));
        pool.close().await;
    }

    #[tokio::test]
    async fn spa_routes_fall_back_to_index_with_success_status() {
        let web_dir = std::env::temp_dir().join(format!("aos-web-ui-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&web_dir).expect("create temporary Web UI directory");
        std::fs::write(web_dir.join("index.html"), "<html>AOS WebUI</html>")
            .expect("write temporary index");

        let response = web_ui_service(web_dir.clone())
            .oneshot(
                Request::builder()
                    .uri("/dashboard")
                    .body(Body::empty())
                    .expect("build request"),
            )
            .await
            .expect("serve SPA route");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(Body::new(response.into_body()), 1024)
            .await
            .expect("read response body");
        assert_eq!(&body[..], b"<html>AOS WebUI</html>");

        std::fs::remove_dir_all(web_dir).expect("remove temporary Web UI directory");
    }
}
