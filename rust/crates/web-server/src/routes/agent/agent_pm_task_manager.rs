use super::*;

impl PmResearchTaskManager {
    pub(super) fn new() -> Self {
        let config = PmResearchTaskConfig::from_env();
        let manager = Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            senders: Arc::new(Mutex::new(HashMap::new())),
            stream_senders: Arc::new(Mutex::new(HashMap::new())),
            run_slots: Arc::new(Semaphore::new(config.max_concurrent_running)),
            tenant_run_slots: Arc::new(Mutex::new(HashMap::new())),
            config,
        };
        manager.start_cleanup_loop();
        manager
    }

    pub(super) fn start_cleanup_loop(&self) {
        let manager = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(manager.config.cleanup_interval).await;
                let _ = manager.cleanup_expired().await;
            }
        });
    }

    pub(super) async fn ensure_sender(
        &self,
        task_id: &str,
    ) -> broadcast::Sender<PmResearchTaskEvent> {
        let mut map = self.senders.lock().await;
        if let Some(s) = map.get(task_id) {
            return s.clone();
        }
        let (tx, _) = broadcast::channel(self.config.event_channel_capacity);
        map.insert(task_id.to_string(), tx.clone());
        tx
    }

    pub(super) async fn ensure_stream_sender(
        &self,
        task_id: &str,
    ) -> broadcast::Sender<PmResearchTaskStreamEvent> {
        let mut map = self.stream_senders.lock().await;
        if let Some(s) = map.get(task_id) {
            return s.clone();
        }
        let (tx, _) = broadcast::channel(self.config.event_channel_capacity.max(256));
        map.insert(task_id.to_string(), tx.clone());
        tx
    }

    pub(super) async fn has_active_task(&self, task_id: &str) -> bool {
        self.inner
            .lock()
            .await
            .get(task_id)
            .map(|rec| rec.execution_active && !rec.done)
            .unwrap_or(false)
    }

    pub(super) async fn has_active_session_task(
        &self,
        session_id: &str,
        except_task_id: &str,
    ) -> bool {
        self.inner.lock().await.iter().any(|(task_id, rec)| {
            task_id != except_task_id
                && rec.session_id == session_id
                && rec.execution_active
                && !rec.done
        })
    }

    pub(super) async fn mark_execution_active(&self, task_id: &str, execution_active: bool) {
        let mut guard = self.inner.lock().await;
        if let Some(rec) = guard.get_mut(task_id) {
            rec.execution_active = execution_active && !rec.done;
            rec.last_update_at = Instant::now();
        }
    }

    pub(super) async fn active_task_diag(
        &self,
        task_id: &str,
    ) -> Option<(String, Option<String>, Option<usize>, u64, u64)> {
        let guard = self.inner.lock().await;
        let rec = guard.get(task_id)?;
        if !rec.execution_active || rec.done {
            return None;
        }
        Some((
            rec.last_event.status.clone(),
            rec.last_event.stage.clone(),
            rec.last_event.attempt,
            rec.last_event.elapsed_ms,
            rec.last_update_at.elapsed().as_millis() as u64,
        ))
    }

    pub(super) async fn restore_task_from_runtime_row(&self, row: &PmTaskRuntimeRow) {
        let mut inserted = false;
        let mut event_to_send: Option<PmResearchTaskEvent> = None;
        let now = Instant::now();
        {
            let mut guard = self.inner.lock().await;
            if let Some(existing) = guard.get_mut(&row.task_id) {
                existing.tenant_id = row.tenant_id.clone();
                existing.user_id = row.user_id.clone();
                existing.session_id = row.session_id.clone();
                existing.message = row.message.clone();
                existing.input_context = row.input_context.clone();
                existing.cancel_requested = row.cancel_requested;
                existing.done = pm_task_is_terminal_status(&row.status);
                existing.execution_active = false;
                existing.last_event.status = row.status.clone();
                existing.last_event.stage = row.stage.clone();
                existing.last_event.attempt = row.attempt;
                existing.last_event.elapsed_ms = row.elapsed_ms;
                existing.last_event.stage_elapsed_ms = row.stage_elapsed_ms;
                existing.last_event.detail = row.detail.clone();
                existing.last_event.response = row.response.clone();
                existing.last_event.error = row.error.clone();
                existing.event_seq = row.event_seq.max(existing.event_seq);
                existing.last_update_at = now;
            } else {
                if guard.len() >= self.config.max_tasks_in_memory {
                    tracing::warn!(
                        task_id = %row.task_id,
                        "pm task restore skipped: memory task pool reached limit"
                    );
                    return;
                }
                let rec = build_pm_task_record_from_runtime_row(row);
                event_to_send = Some(rec.last_event.clone());
                guard.insert(row.task_id.clone(), rec);
                inserted = true;
            }
        }
        if inserted {
            let tx = self.ensure_sender(&row.task_id).await;
            if let Some(evt) = event_to_send {
                let _ = tx.send(evt);
            }
        }
    }

    pub(super) async fn create_task(
        &self,
        db: &sqlx::SqlitePool,
        telemetry: &PmTelemetrySink,
        task_id: &str,
        session_id: &str,
        message: &str,
        input_context: Option<PmTaskInputContext>,
        tenant_id: &str,
        user_id: &str,
    ) -> Result<(), AppError> {
        self.cleanup_expired().await;
        let initial = PmResearchTaskEvent {
            task_id: task_id.to_string(),
            session_id: session_id.to_string(),
            status: "queued".to_string(),
            stage: Some("queued".to_string()),
            attempt: Some(1),
            message: Some("已加入后台研究队列".to_string()),
            elapsed_ms: 0,
            stage_elapsed_ms: Some(0),
            detail: None,
            response: None,
            error: None,
        };
        let now = Instant::now();
        let record = PmResearchTaskRecord {
            tenant_id: tenant_id.to_string(),
            user_id: user_id.to_string(),
            session_id: session_id.to_string(),
            message: message.to_string(),
            input_context,
            created_at: now,
            last_update_at: now,
            stage_started_at: now,
            completed_at: None,
            execution_active: false,
            done: false,
            cancel_requested: false,
            last_event: initial.clone(),
            event_seq: 1,
            answer_stream_seq: 0,
        };
        let persist_record = record.clone();
        {
            let mut guard = self.inner.lock().await;
            if guard.len() >= self.config.max_tasks_in_memory {
                return Err(AppError::TooManyRequests(format!(
                    "too many pm research tasks in memory (limit: {})",
                    self.config.max_tasks_in_memory
                )));
            }
            guard.insert(task_id.to_string(), record);
        }
        let tx = self.ensure_sender(task_id).await;
        let _ = tx.send(initial);
        persist_pm_task_record_and_event(
            db,
            telemetry,
            &persist_record,
            &persist_record.last_event,
        )
        .await;
        Ok(())
    }

    pub(super) async fn publish_stage(
        &self,
        db: &sqlx::SqlitePool,
        telemetry: &PmTelemetrySink,
        task_id: &str,
        stage: &str,
        status: &str,
        attempt: usize,
        message: Option<&str>,
        detail: Option<serde_json::Value>,
    ) {
        // Synthesis can produce a usable candidate while the deep loop is
        // still checking evidence.  Treat that as candidate-ready, not as a
        // completed stage; the canonical terminal event closes both stages in
        // one durable projection update.
        let mut normalized_status = status.to_string();
        let mut normalized_detail = detail;
        if stage == "synthesize" && status == "completed" {
            let deep_loop_running =
                crate::semantic_kernel_store::load_pm_stage_status(db, task_id, "deep_loop")
                    .await
                    .ok()
                    .flatten()
                    .is_some_and(|value| value == "running");
            if deep_loop_running {
                normalized_status = "running".to_string();
                let mut value = normalized_detail.unwrap_or_else(|| serde_json::json!({}));
                if let Some(object) = value.as_object_mut() {
                    object.insert(
                        "deliveryState".to_string(),
                        serde_json::Value::String("candidate_ready".to_string()),
                    );
                    object.insert(
                        "humanSummary".to_string(),
                        serde_json::Value::String(
                            "候选结论已生成，深度循环仍在完成证据校验。".to_string(),
                        ),
                    );
                }
                normalized_detail = Some(value);
            }
        }
        let evt_opt = {
            let mut guard = self.inner.lock().await;
            guard.get_mut(task_id).and_then(|rec| {
                if rec.done {
                    tracing::debug!(
                        task_id = %task_id,
                        stage,
                        status,
                        "skip late stage publish after task terminal completion"
                    );
                    return None;
                }
                let next_message = message.map(std::string::ToString::to_string);
                if rec.last_event.stage.as_deref() == Some(stage)
                    && rec.last_event.status == normalized_status
                    && rec.last_event.attempt == Some(attempt)
                    && rec.last_event.message == next_message
                    && rec.last_event.detail == normalized_detail
                {
                    return None;
                }
                rec.event_seq = rec.event_seq.saturating_add(1);
                let now_instant = Instant::now();
                let stage_changed = rec
                    .last_event
                    .stage
                    .as_deref()
                    .is_some_and(|last_stage| last_stage != stage);
                if stage_changed {
                    rec.stage_started_at = now_instant;
                }
                let now_elapsed = rec.created_at.elapsed().as_millis() as u64;
                let stage_elapsed =
                    now_instant.duration_since(rec.stage_started_at).as_millis() as u64;
                let evt = PmResearchTaskEvent {
                    task_id: task_id.to_string(),
                    session_id: rec.session_id.clone(),
                    status: normalized_status.clone(),
                    stage: Some(stage.to_string()),
                    attempt: Some(attempt),
                    message: next_message,
                    elapsed_ms: now_elapsed,
                    stage_elapsed_ms: Some(stage_elapsed),
                    detail: normalized_detail.clone(),
                    response: None,
                    error: None,
                };
                rec.last_event = evt.clone();
                rec.last_update_at = Instant::now();
                Some((evt, rec.clone()))
            })
        };
        if let Some((evt, rec)) = evt_opt {
            let tx = self.ensure_sender(task_id).await;
            let _ = tx.send(evt);
            persist_pm_task_record_and_event(db, telemetry, &rec, &rec.last_event).await;
        }
    }

    pub(super) async fn publish_heartbeat(&self, _db: &sqlx::SqlitePool, task_id: &str) {
        let evt_opt = {
            let mut guard = self.inner.lock().await;
            guard.get_mut(task_id).and_then(|rec| {
                if rec.done || rec.cancel_requested {
                    return None;
                }
                let stage = rec
                    .last_event
                    .stage
                    .clone()
                    .unwrap_or_else(|| "running".to_string());
                let status = if rec.last_event.status == "queued" {
                    "running".to_string()
                } else {
                    rec.last_event.status.clone()
                };
                let now_elapsed = rec.created_at.elapsed().as_millis() as u64;
                let stage_elapsed = rec.stage_started_at.elapsed().as_millis() as u64;
                let evt = PmResearchTaskEvent {
                    task_id: task_id.to_string(),
                    session_id: rec.session_id.clone(),
                    status,
                    stage: Some(stage),
                    attempt: rec.last_event.attempt,
                    message: rec.last_event.message.clone(),
                    elapsed_ms: now_elapsed,
                    stage_elapsed_ms: Some(stage_elapsed),
                    detail: Some(serde_json::json!({
                        "event": "pm.task.heartbeat",
                        "heartbeat": true,
                        "lastStage": rec.last_event.stage,
                        "lastStatus": rec.last_event.status,
                    })),
                    response: None,
                    error: None,
                };
                rec.last_update_at = Instant::now();
                Some(evt)
            })
        };
        if let Some(evt) = evt_opt {
            let tx = self.ensure_sender(task_id).await;
            let _ = tx.send(evt);
        }
    }

    pub(super) async fn publish_answer_delta(
        &self,
        telemetry: &PmTelemetrySink,
        task_id: &str,
        stage: &str,
        delta: String,
    ) {
        if delta.is_empty() {
            return;
        }
        let evt_opt = {
            let mut guard = self.inner.lock().await;
            guard.get_mut(task_id).and_then(|rec| {
                if rec.done || rec.cancel_requested {
                    return None;
                }
                rec.answer_stream_seq = rec.answer_stream_seq.saturating_add(1);
                rec.last_update_at = Instant::now();
                let evt = PmResearchTaskStreamEvent {
                    task_id: task_id.to_string(),
                    session_id: rec.session_id.clone(),
                    stage: stage.to_string(),
                    sequence: rec.answer_stream_seq,
                    delta,
                };
                Some((evt, rec.tenant_id.clone(), rec.user_id.clone()))
            })
        };
        if let Some((evt, tenant_id, user_id)) = evt_opt {
            let tx = self.ensure_stream_sender(task_id).await;
            let _ = tx.send(evt.clone());
            telemetry
                .enqueue(PmTelemetryEvent::AnswerDelta {
                    tenant_id,
                    user_id,
                    event: evt,
                })
                .await;
        }
    }

    pub(super) async fn publish_completed(
        &self,
        db: &sqlx::SqlitePool,
        telemetry: &PmTelemetrySink,
        task_id: &str,
        response: RunTurnResponse,
    ) {
        self.publish_terminal_result(
            db,
            telemetry,
            task_id,
            "completed",
            Some("后台研究执行完成"),
            Some(response),
            None,
        )
        .await;
    }

    pub(super) async fn publish_terminal_result(
        &self,
        db: &sqlx::SqlitePool,
        telemetry: &PmTelemetrySink,
        task_id: &str,
        status: &str,
        message: Option<&str>,
        response: Option<RunTurnResponse>,
        error: Option<String>,
    ) {
        let normalized_status = match status.trim().to_ascii_lowercase().as_str() {
            "failed" => "failed",
            "cancelled" => "cancelled",
            _ => "completed",
        };
        let terminal_stage = match normalized_status {
            "failed" => "failed",
            "cancelled" => "cancelled",
            _ => "done",
        };
        let evt_opt = {
            let mut guard = self.inner.lock().await;
            guard.get_mut(task_id).map(|rec| {
                rec.event_seq = rec.event_seq.saturating_add(1);
                rec.stage_started_at = Instant::now();
                let now_elapsed = rec.created_at.elapsed().as_millis() as u64;
                let stage_elapsed = rec.stage_started_at.elapsed().as_millis() as u64;
                let evt = PmResearchTaskEvent {
                    task_id: task_id.to_string(),
                    session_id: rec.session_id.clone(),
                    status: normalized_status.to_string(),
                    stage: Some(terminal_stage.to_string()),
                    attempt: rec.last_event.attempt,
                    message: message.map(std::string::ToString::to_string),
                    elapsed_ms: now_elapsed,
                    stage_elapsed_ms: Some(stage_elapsed),
                    detail: None,
                    response: response.and_then(|value| serde_json::to_value(value).ok()),
                    error,
                };
                rec.last_event = evt.clone();
                rec.execution_active = false;
                rec.done = true;
                rec.completed_at = Some(Instant::now());
                rec.last_update_at = Instant::now();
                (evt, rec.clone())
            })
        };
        if let Some((evt, rec)) = evt_opt {
            let tx = self.ensure_sender(task_id).await;
            let _ = tx.send(evt);
            let response_json = rec.last_event.response.clone();
            persist_pm_task_record_and_event(db, telemetry, &rec, &rec.last_event).await;
            if let Err(error) = crate::semantic_kernel_store::persist_pm_final_delivery(
                db,
                &rec.tenant_id,
                &rec.user_id,
                &rec.session_id,
                task_id,
                normalized_status,
                response_json.as_ref(),
            )
            .await
            {
                tracing::warn!(
                    task_id = %task_id,
                    error = %error,
                    "failed to persist PM final delivery artifact"
                );
            }
        }
    }

    pub(super) async fn publish_cancelled(
        &self,
        db: &sqlx::SqlitePool,
        telemetry: &PmTelemetrySink,
        task_id: &str,
    ) {
        let evt_opt = {
            let mut guard = self.inner.lock().await;
            guard.get_mut(task_id).map(|rec| {
                rec.event_seq = rec.event_seq.saturating_add(1);
                rec.stage_started_at = Instant::now();
                let now_elapsed = rec.created_at.elapsed().as_millis() as u64;
                let stage_elapsed = rec.stage_started_at.elapsed().as_millis() as u64;
                let evt = PmResearchTaskEvent {
                    task_id: task_id.to_string(),
                    session_id: rec.session_id.clone(),
                    status: "cancelled".to_string(),
                    stage: Some("cancelled".to_string()),
                    attempt: rec.last_event.attempt,
                    message: Some("后台研究已取消".to_string()),
                    elapsed_ms: now_elapsed,
                    stage_elapsed_ms: Some(stage_elapsed),
                    detail: None,
                    response: None,
                    error: None,
                };
                rec.last_event = evt.clone();
                rec.execution_active = false;
                rec.done = true;
                rec.completed_at = Some(Instant::now());
                rec.last_update_at = Instant::now();
                (evt, rec.clone())
            })
        };
        if let Some((evt, rec)) = evt_opt {
            let tx = self.ensure_sender(task_id).await;
            let _ = tx.send(evt);
            persist_pm_task_record_and_event(db, telemetry, &rec, &rec.last_event).await;
            if let Err(error) = crate::semantic_kernel_store::persist_pm_final_delivery(
                db,
                &rec.tenant_id,
                &rec.user_id,
                &rec.session_id,
                task_id,
                "cancelled",
                None,
            )
            .await
            {
                tracing::warn!(
                    task_id = %task_id,
                    error = %error,
                    "failed to persist cancelled PM delivery artifact"
                );
            }
        }
    }

    pub(super) async fn mark_cancel_requested(
        &self,
        db: &sqlx::SqlitePool,
        telemetry: &PmTelemetrySink,
        task_id: &str,
        tenant_id: &str,
        user_id: &str,
    ) -> Result<bool, AppError> {
        let maybe_evt = {
            let mut guard = self.inner.lock().await;
            match guard.get_mut(task_id) {
                Some(rec) => {
                    if rec.tenant_id != tenant_id || rec.user_id != user_id {
                        return Err(AppError::NotFound("pm research task not found".to_string()));
                    }
                    if rec.done {
                        return Ok(false);
                    }
                    rec.cancel_requested = true;
                    rec.event_seq = rec.event_seq.saturating_add(1);
                    rec.stage_started_at = Instant::now();
                    let now_elapsed = rec.created_at.elapsed().as_millis() as u64;
                    let stage_elapsed = rec.stage_started_at.elapsed().as_millis() as u64;
                    let evt = PmResearchTaskEvent {
                        task_id: task_id.to_string(),
                        session_id: rec.session_id.clone(),
                        status: "cancelling".to_string(),
                        stage: Some("cancelling".to_string()),
                        attempt: rec.last_event.attempt,
                        message: Some("已请求取消，将在当前阶段结束后停止".to_string()),
                        elapsed_ms: now_elapsed,
                        stage_elapsed_ms: Some(stage_elapsed),
                        detail: None,
                        response: None,
                        error: None,
                    };
                    rec.last_event = evt.clone();
                    rec.last_update_at = Instant::now();
                    Some((evt, rec.clone()))
                }
                None => None,
            }
        };
        if let Some((evt, rec)) = maybe_evt {
            let tx = self.ensure_sender(task_id).await;
            let _ = tx.send(evt);
            persist_pm_task_record_and_event(db, telemetry, &rec, &rec.last_event).await;
            return Ok(true);
        }

        let result = sqlx::query(
            "UPDATE pm_research_tasks
             SET cancel_requested = 1,
                 status = CASE
                     WHEN status IN ('completed','failed','cancelled') THEN status
                     ELSE 'cancelling'
                 END,
                 updated_at = CURRENT_TIMESTAMP
             WHERE task_id = ?
               AND tenant_id = ?
               AND user_id = ?
               AND status NOT IN ('completed','failed','cancelled')",
        )
        .bind(task_id)
        .bind(tenant_id)
        .bind(user_id)
        .execute(db)
        .await?;

        Ok(result.rows_affected() > 0)
    }

    pub(super) async fn is_cancel_requested(&self, task_id: &str) -> bool {
        self.inner
            .lock()
            .await
            .get(task_id)
            .map(|rec| rec.cancel_requested)
            .unwrap_or(false)
    }

    pub(super) async fn snapshot(
        &self,
        task_id: &str,
        tenant_id: &str,
        user_id: &str,
    ) -> Option<(PmResearchTaskEvent, bool)> {
        self.inner
            .lock()
            .await
            .get(task_id)
            .filter(|rec| rec.tenant_id == tenant_id && rec.user_id == user_id)
            .map(|rec| (rec.last_event.clone(), rec.cancel_requested))
    }

    pub(super) async fn subscribe(
        &self,
        task_id: &str,
        tenant_id: &str,
        user_id: &str,
    ) -> Option<broadcast::Receiver<PmResearchTaskEvent>> {
        let owner_ok = self
            .inner
            .lock()
            .await
            .get(task_id)
            .map(|rec| rec.tenant_id == tenant_id && rec.user_id == user_id)
            .unwrap_or(false);
        if !owner_ok {
            return None;
        }
        let tx = self.ensure_sender(task_id).await;
        Some(tx.subscribe())
    }

    pub(super) async fn subscribe_answer_stream(
        &self,
        task_id: &str,
        tenant_id: &str,
        user_id: &str,
    ) -> Option<broadcast::Receiver<PmResearchTaskStreamEvent>> {
        let owner_ok = self
            .inner
            .lock()
            .await
            .get(task_id)
            .map(|rec| rec.tenant_id == tenant_id && rec.user_id == user_id)
            .unwrap_or(false);
        if !owner_ok {
            return None;
        }
        let tx = self.ensure_stream_sender(task_id).await;
        Some(tx.subscribe())
    }

    pub(super) async fn try_acquire_run_slot(
        &self,
        tenant_id: &str,
    ) -> Result<PmResearchRunPermit, AppError> {
        let global = self.run_slots.clone().try_acquire_owned().map_err(|_| {
            AppError::TooManyRequests(format!(
                "too many concurrent pm research tasks (limit: {})",
                self.config.max_concurrent_running
            ))
        })?;
        let tenant_slots = {
            let mut slots = self.tenant_run_slots.lock().await;
            slots
                .entry(tenant_id.to_string())
                .or_insert_with(|| {
                    Arc::new(Semaphore::new(
                        self.config
                            .max_concurrent_per_tenant
                            .min(self.config.max_concurrent_running)
                            .max(1),
                    ))
                })
                .clone()
        };
        let tenant = tenant_slots.try_acquire_owned().map_err(|_| {
            AppError::TooManyRequests(format!(
                "tenant PM research concurrency limit reached (limit: {})",
                self.config.max_concurrent_per_tenant
            ))
        })?;
        Ok(PmResearchRunPermit {
            _global: global,
            _tenant: tenant,
        })
    }

    pub(super) async fn resume_payload(
        &self,
        task_id: &str,
        tenant_id: &str,
        user_id: &str,
    ) -> Result<(String, String, Option<PmTaskInputContext>), AppError> {
        let guard = self.inner.lock().await;
        let rec = guard
            .get(task_id)
            .ok_or_else(|| AppError::NotFound("pm research task not found".to_string()))?;
        if rec.tenant_id != tenant_id || rec.user_id != user_id {
            return Err(AppError::NotFound("pm research task not found".to_string()));
        }
        if !(rec.done
            && (rec.last_event.status == "failed" || rec.last_event.status == "cancelled"))
        {
            return Err(AppError::ValidationError(
                "only failed or cancelled task can be resumed".to_string(),
            ));
        }
        Ok((
            rec.session_id.clone(),
            rec.message.clone(),
            rec.input_context.clone(),
        ))
    }

    pub(super) async fn cleanup_expired(&self) -> usize {
        let now = Instant::now();
        let ttl = self.config.task_ttl;
        let mut expired_ids: Vec<String> = Vec::new();
        {
            let mut guard = self.inner.lock().await;
            guard.retain(|task_id, rec| {
                let expired = rec.done
                    && rec
                        .completed_at
                        .map(|done_at| now.duration_since(done_at) >= ttl)
                        .unwrap_or(false);
                if expired {
                    expired_ids.push(task_id.clone());
                    return false;
                }
                true
            });
        }
        if expired_ids.is_empty() {
            return 0;
        }
        let mut senders = self.senders.lock().await;
        for task_id in &expired_ids {
            senders.remove(task_id);
        }
        let mut stream_senders = self.stream_senders.lock().await;
        for task_id in &expired_ids {
            stream_senders.remove(task_id);
        }
        expired_ids.len()
    }
}

pub(super) fn pm_research_task_manager() -> &'static PmResearchTaskManager {
    static MANAGER: OnceLock<PmResearchTaskManager> = OnceLock::new();
    MANAGER.get_or_init(PmResearchTaskManager::new)
}
