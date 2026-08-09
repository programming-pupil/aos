#[cfg(feature = "agent")]
pub mod agent;
pub mod agent_ops;
pub mod agent_ops_adapters;
pub mod agent_ops_types;
pub mod agent_ops_watchdog_intent;
pub mod agent_runtime;
pub mod apikeys;
pub mod auth;
#[cfg(feature = "bot-agents")]
pub mod bot_agents;
#[cfg(feature = "bot-agents")]
pub mod bot_agents_inbound;
#[cfg(any(feature = "agent", feature = "bot-agents", feature = "pm"))]
pub mod bot_agents_outbound;
#[cfg(feature = "bot-agents")]
pub mod bot_agents_router;
#[cfg(feature = "bot-agents")]
pub mod bot_agents_types;
#[cfg(any(
    feature = "agent",
    feature = "pm",
    feature = "rd",
    feature = "bot-agents"
))]
pub mod builtin_skills;
pub mod chat;
pub mod chat_capabilities;
pub mod chat_intelligence;
pub mod config;
pub mod dashboard;
#[cfg(feature = "nl2sql")]
pub mod data_sources;
#[cfg(feature = "nl2sql")]
pub mod datasource_scheduler;
pub mod demo;
pub mod hooks;
pub mod hooks_validation;
pub mod mcp;
pub mod memory_continuity;
#[cfg(feature = "nl2sql")]
pub mod nl2sql;
pub mod notifications;
pub mod personal_workspace;
#[cfg(feature = "pm")]
pub mod pm;
#[cfg(feature = "pm")]
pub mod pm_scheduler;
#[cfg(feature = "projects")]
pub mod projects;
#[cfg(feature = "rd")]
pub mod rd;
pub mod search_orchestrator_runtime;
pub mod sessions;
pub mod setup;
pub mod skills;
pub mod super_assistant;
pub mod super_assistant_capabilities;
#[cfg(feature = "bot-agents")]
pub(crate) mod super_assistant_parent;
pub mod system_events;
pub mod task_control;
pub mod task_control_worker;
pub mod tenant_bootstrap;
pub mod tenants;
pub mod unified_workspace;
pub mod upload;
pub mod users;

use crate::auth::Claims;
use crate::error::Result;
use crate::state::AppState;
use serde::Deserialize;

// Re-export chat internals so sibling modules (e.g. nl2sql::routing) can use them.

#[derive(Debug, Default, Deserialize)]
pub struct PaginationParams {
    pub page: Option<u32>,
    pub per_page: Option<u32>,
}

impl PaginationParams {
    pub fn offset(&self) -> i64 {
        i64::from((self.page.unwrap_or(1).saturating_sub(1)) * self.per_page().unwrap_or(20))
    }
    pub fn limit(&self) -> i64 {
        i64::from(self.per_page().unwrap_or(20))
    }
    fn per_page(&self) -> Option<u32> {
        self.per_page.filter(|&p| p > 0 && p <= 100)
    }
}

#[expect(dead_code)]
pub async fn verify_token(state: &AppState, token: &str) -> Result<Claims> {
    crate::auth::verify_token(state, token).await
}
