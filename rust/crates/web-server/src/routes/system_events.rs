//! System events WebSocket handler for real-time push notifications.
//!
//! Browser clients connect to `/ws/system-events` with the `aos-auth` WebSocket
//! subprotocol followed by a JWT. The server selects only the non-secret
//! `aos-auth` protocol, so the token never appears in the URL or response.
//! Each event includes `tenant_id` so clients can filter by tenant.

use std::time::Duration;

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Extension, State,
    },
    response::IntoResponse,
    Router,
};
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use tokio::sync::broadcast;
use tokio::time::interval;

use crate::auth::Claims;
use crate::state::AppState;

// ── Shared broadcast data types ────────────────────────────────────────────────

/// Minimal skill info sent over the WebSocket broadcast channel.
/// Matches the frontend `SkillBroadcastEntry` type.
#[derive(Debug, Clone, Serialize)]
pub struct SkillBroadcastEntry {
    pub name: String,
    pub description: String,
    pub source: String,
    pub tags: Vec<String>,
    pub enabled: bool,
}

// ── Shared broadcast channel ───────────────────────────────────────────────────

static BROADCAST_SENDER: std::sync::OnceLock<tokio::sync::broadcast::Sender<SystemEvent>> =
    std::sync::OnceLock::new();

pub fn init_broadcast_channel() {
    let _ = BROADCAST_SENDER.get_or_init(|| broadcast::channel(1024).0);
}

pub fn broadcast_event(event: SystemEvent) {
    if let Some(sender) = BROADCAST_SENDER.get() {
        let _ = sender.send(event);
    }
}

pub fn broadcast_mcp_added(tenant_id: &str, name: &str) {
    broadcast_event(SystemEvent::McpServerAdded {
        tenant_id: tenant_id.to_string(),
        name: name.to_string(),
    });
}

pub fn broadcast_mcp_removed(tenant_id: &str, name: &str) {
    broadcast_event(SystemEvent::McpServerRemoved {
        tenant_id: tenant_id.to_string(),
        name: name.to_string(),
    });
}

pub fn broadcast_mcp_toggled(tenant_id: &str, name: &str, enabled: bool) {
    broadcast_event(SystemEvent::McpServerToggled {
        tenant_id: tenant_id.to_string(),
        name: name.to_string(),
        enabled,
    });
}

pub fn broadcast_mcp_status_changed(
    tenant_id: &str,
    name: &str,
    status: &str,
    last_error: Option<&str>,
) {
    broadcast_event(SystemEvent::McpStatusChanged {
        tenant_id: tenant_id.to_string(),
        name: name.to_string(),
        status: status.to_string(),
        last_error: last_error.map(ToString::to_string),
    });
}

pub fn broadcast_mcp_updated(tenant_id: &str, name: &str) {
    broadcast_event(SystemEvent::McpServerUpdated {
        tenant_id: tenant_id.to_string(),
        name: name.to_string(),
    });
}

pub fn broadcast_skills_updated(tenant_id: &str, skills: &[SkillBroadcastEntry]) {
    broadcast_event(SystemEvent::SkillsUpdated {
        tenant_id: tenant_id.to_string(),
        skills: skills.to_vec(),
    });
}

pub fn broadcast_hooks_updated(tenant_id: &str) {
    broadcast_event(SystemEvent::HooksUpdated {
        tenant_id: tenant_id.to_string(),
    });
}

#[cfg(feature = "pm")]
pub fn broadcast_search_providers_updated(tenant_id: &str) {
    broadcast_event(SystemEvent::SearchProvidersUpdated {
        tenant_id: tenant_id.to_string(),
    });
}

// ── Event types ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[allow(dead_code)]
pub enum SystemEvent {
    McpServerAdded {
        tenant_id: String,
        name: String,
    },
    McpServerRemoved {
        tenant_id: String,
        name: String,
    },
    McpServerToggled {
        tenant_id: String,
        name: String,
        enabled: bool,
    },
    McpServerUpdated {
        tenant_id: String,
        name: String,
    },
    McpStatusChanged {
        tenant_id: String,
        name: String,
        status: String,
        last_error: Option<String>,
    },
    ModelSwitched {
        tenant_id: String,
        user_id: String,
        model: String,
    },
    TokenLimitWarning {
        tenant_id: String,
        user_id: String,
        percentage: u8,
        limit: i64,
        current: i64,
    },
    Heartbeat {
        server_time: String,
    },
    SkillsUpdated {
        tenant_id: String,
        skills: Vec<SkillBroadcastEntry>,
    },
    HooksUpdated {
        tenant_id: String,
    },
    SearchProvidersUpdated {
        tenant_id: String,
    },
}

impl SystemEvent {
    fn to_json_string(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// Returns JSON string only if the event belongs to the given tenant.
    /// Returns None for Heartbeat (broadcast to all) or tenant-mismatched events.
    fn to_json_string_for_tenant(&self, tenant_id: &str) -> Option<String> {
        match self {
            // Heartbeat is broadcast to all authenticated clients
            SystemEvent::Heartbeat { .. } => Some(self.to_json_string()),
            // All other events carry tenant_id — filter by tenant
            SystemEvent::McpServerAdded {
                tenant_id: event_tenant,
                ..
            }
            | SystemEvent::McpServerRemoved {
                tenant_id: event_tenant,
                ..
            }
            | SystemEvent::McpServerToggled {
                tenant_id: event_tenant,
                ..
            }
            | SystemEvent::McpServerUpdated {
                tenant_id: event_tenant,
                ..
            }
            | SystemEvent::McpStatusChanged {
                tenant_id: event_tenant,
                ..
            }
            | SystemEvent::ModelSwitched {
                tenant_id: event_tenant,
                ..
            }
            | SystemEvent::TokenLimitWarning {
                tenant_id: event_tenant,
                ..
            }
            | SystemEvent::SkillsUpdated {
                tenant_id: event_tenant,
                ..
            }
            | SystemEvent::HooksUpdated {
                tenant_id: event_tenant,
                ..
            }
            | SystemEvent::SearchProvidersUpdated {
                tenant_id: event_tenant,
            } => {
                if event_tenant == tenant_id {
                    Some(self.to_json_string())
                } else {
                    None
                }
            }
        }
    }
}

// ── WebSocket handler ─────────────────────────────────────────────────────────

async fn ws_handler(
    State(_state): State<AppState>,
    Extension(claims): Extension<Claims>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    ws.protocols(["aos-auth"])
        .on_upgrade(|socket| handle_socket(socket, claims))
}

async fn handle_socket(socket: WebSocket, claims: Claims) {
    let (mut ws_sender, mut ws_receiver) = socket.split();
    let tenant_id = claims.tenant_id.clone();
    let user_id = claims.sub.clone();

    let ack = serde_json::json!({
        "type": "connected",
        "tenant_id": tenant_id,
        "user_id": user_id,
    });
    let _ = ws_sender.send(Message::Text(ack.to_string().into())).await;

    let tx = BROADCAST_SENDER
        .get()
        .cloned()
        .unwrap_or_else(|| broadcast::channel(1024).0);
    let mut broadcast_rx = tx.subscribe();
    let mut heartbeat = interval(Duration::from_secs(30));

    loop {
        tokio::select! {
            event = broadcast_rx.recv() => {
                match event {
                    Ok(ev) => {
                        if let Some(json) = ev.to_json_string_for_tenant(&tenant_id) {
                            if ws_sender.send(Message::Text(json.into())).await.is_err() {
                                break;
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                }
            }
            _ = heartbeat.tick() => {
                let beat = SystemEvent::Heartbeat {
                    server_time: chrono::Utc::now().to_rfc3339(),
                };
                if ws_sender.send(Message::Text(beat.to_json_string().into())).await.is_err() {
                    break;
                }
            }
            msg = ws_receiver.next() => {
                match msg {
                    Some(Ok(Message::Ping(data))) => {
                        let _ = ws_sender.send(Message::Pong(data)).await;
                    }
                    Some(Ok(Message::Close(..))) | None => break,
                    Some(Err(e)) => {
                        tracing::debug!("ws receive error: {e}");
                        break;
                    }
                    _ => {}
                }
            }
        }
    }

    drop(broadcast_rx);
}

// ── Route ─────────────────────────────────────────────────────────────────────

pub fn ws_routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/system-events", axum::routing::get(ws_handler))
        .layer(axum::middleware::from_fn_with_state(
            state,
            crate::auth_middleware::require_auth,
        ))
}
