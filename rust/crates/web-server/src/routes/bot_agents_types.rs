//! Bot Gateway DTOs and capability binding normalization.
//!
//! The route module focuses on persistence, queueing, and execution. This file
//! keeps API shapes and config normalization in one reviewable place.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Serialize)]
pub struct BotAgentInfo {
    pub id: String,
    pub tenant_id: String,
    pub name: String,
    pub description: Option<String>,
    pub enabled: bool,
    pub default_capability: String,
    pub persona_prompt: Option<String>,
    pub capabilities: Vec<BotAgentCapabilityInfo>,
    pub channels_count: u64,
    pub created_by: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct BotAgentCapabilityInfo {
    pub id: String,
    pub agent_id: String,
    pub capability_key: String,
    pub enabled: bool,
    pub config_json: Option<Value>,
}

#[derive(Debug, Serialize)]
pub struct BotAgentChannelInfo {
    pub id: String,
    pub agent_id: String,
    pub platform: String,
    pub name: String,
    pub enabled: bool,
    pub inbound_mode: String,
    pub inbound_secret_set: bool,
    pub inbound_status: String,
    pub inbound_error: Option<String>,
    pub inbound_last_seen_at: Option<String>,
    pub inbound_last_message_at: Option<String>,
    pub outbound_webhook_url: Option<String>,
    pub outbound_token_set: bool,
    pub signing_secret_set: bool,
    pub outbound_signing_secret_set: bool,
    pub config_json: Option<Value>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Serialize)]
pub struct BotMessageLogInfo {
    pub id: String,
    pub agent_id: Option<String>,
    pub channel_id: Option<String>,
    pub agent_task_id: Option<String>,
    pub direction: String,
    pub platform: String,
    pub external_user_id: Option<String>,
    pub external_conversation_id: Option<String>,
    pub message_type: String,
    pub content_json: Option<Value>,
    pub status: String,
    pub queue_status: String,
    pub attempt_count: i32,
    pub max_attempts: i32,
    pub claimed_by: Option<String>,
    pub claimed_at: Option<String>,
    pub last_error: Option<String>,
    pub finished_at: Option<String>,
    pub error_message: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Serialize)]
pub struct ListResponse<T> {
    pub items: Vec<T>,
    pub total: i64,
}

#[derive(Debug, Deserialize)]
pub struct CreateAgentRequest {
    pub name: String,
    pub description: Option<String>,
    pub enabled: Option<bool>,
    pub default_capability: Option<String>,
    pub persona_prompt: Option<String>,
    #[serde(default)]
    pub capabilities: Vec<CapabilityBindingInput>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateAgentRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub enabled: Option<bool>,
    pub default_capability: Option<String>,
    pub persona_prompt: Option<String>,
    pub capabilities: Option<Vec<CapabilityBindingInput>>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum CapabilityBindingInput {
    Key(String),
    Detailed {
        capability_key: String,
        #[serde(default)]
        trigger_prefixes: Vec<String>,
        #[serde(default)]
        require_mention: bool,
        #[serde(default)]
        fallback_when_no_prefix: bool,
        model: Option<String>,
        models: Option<Vec<String>>,
        #[serde(rename = "maxRounds")]
        max_rounds: Option<u32>,
        #[serde(rename = "repositoryId")]
        repository_id: Option<String>,
        #[serde(rename = "agentProfileId")]
        agent_profile_id: Option<String>,
        #[serde(rename = "workflowId")]
        workflow_id: Option<String>,
        #[serde(rename = "contextDepth")]
        context_depth: Option<String>,
        #[serde(rename = "shouldDeepScan")]
        should_deep_scan: Option<bool>,
        #[serde(rename = "dataSourceId")]
        data_source_id: Option<String>,
        #[serde(rename = "allowExecuteSql")]
        allow_execute_sql: Option<bool>,
        #[serde(rename = "deepAnalysis")]
        deep_analysis: Option<bool>,
        #[serde(rename = "watchdogScope")]
        watchdog_scope: Option<String>,
        #[serde(rename = "allowActions")]
        allow_actions: Option<Vec<String>>,
        #[serde(rename = "confidenceThreshold")]
        confidence_threshold: Option<f64>,
        #[serde(rename = "executionMode")]
        execution_mode: Option<String>,
        #[serde(rename = "syncTimeoutMs")]
        sync_timeout_ms: Option<u64>,
        #[serde(rename = "ackTimeoutMs")]
        ack_timeout_ms: Option<u64>,
    },
}

#[derive(Debug, Deserialize)]
pub struct CreateChannelRequest {
    pub agent_id: String,
    pub platform: String,
    pub name: String,
    pub enabled: Option<bool>,
    pub inbound_mode: Option<String>,
    pub inbound_secret: Option<String>,
    pub outbound_webhook_url: Option<String>,
    pub outbound_token: Option<String>,
    pub signing_secret: Option<String>,
    pub outbound_signing_secret: Option<String>,
    pub config_json: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateChannelRequest {
    pub platform: Option<String>,
    pub name: Option<String>,
    pub enabled: Option<bool>,
    pub inbound_mode: Option<String>,
    pub inbound_secret: Option<String>,
    pub outbound_webhook_url: Option<String>,
    pub outbound_token: Option<String>,
    pub signing_secret: Option<String>,
    pub outbound_signing_secret: Option<String>,
    pub config_json: Option<Value>,
}

#[derive(Debug, Deserialize, Default)]
pub struct ChannelListQuery {
    pub agent_id: Option<String>,
    pub page: Option<u32>,
    pub per_page: Option<u32>,
}

#[derive(Debug, Deserialize, Default)]
pub struct LogListQuery {
    pub channel_id: Option<String>,
    pub agent_id: Option<String>,
    pub page: Option<u32>,
    pub per_page: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct TestChannelRequest {
    pub title: Option<String>,
    pub text: Option<String>,
    pub external_conversation_id: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct WebhookQuery {
    pub secret: Option<String>,
    pub challenge: Option<String>,
    #[serde(rename = "hub.mode")]
    pub hub_mode: Option<String>,
    #[serde(rename = "hub.verify_token")]
    pub hub_verify_token: Option<String>,
    #[serde(rename = "hub.challenge")]
    pub hub_challenge: Option<String>,
}

pub fn clean_opt(value: Option<String>) -> Option<String> {
    value.and_then(|v| {
        let trimmed = v.trim().to_string();
        (!trimmed.is_empty()).then_some(trimmed)
    })
}

pub fn normalize_capability_key(key: &str) -> String {
    key.trim().to_ascii_lowercase()
}

pub fn normalize_trigger_prefixes(prefixes: &[String]) -> Vec<String> {
    let mut out = prefixes
        .iter()
        .map(|item| item.trim().to_string())
        .filter(|item| !item.is_empty())
        .collect::<Vec<_>>();
    out.sort();
    out.dedup();
    out
}

fn clean_string_value(value: &Option<String>) -> Option<String> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(ToOwned::to_owned)
}

fn clean_string_vec(value: &Option<Vec<String>>) -> Vec<String> {
    value
        .as_ref()
        .map(|items| normalize_trigger_prefixes(items))
        .unwrap_or_default()
}

pub fn capability_binding_parts(input: &CapabilityBindingInput) -> Option<(String, Option<Value>)> {
    match input {
        CapabilityBindingInput::Key(key) => {
            let key = normalize_capability_key(key);
            (!key.is_empty()).then_some((key, None))
        }
        CapabilityBindingInput::Detailed {
            capability_key,
            trigger_prefixes,
            require_mention,
            fallback_when_no_prefix,
            model,
            models,
            max_rounds,
            repository_id,
            agent_profile_id,
            workflow_id,
            context_depth,
            should_deep_scan,
            data_source_id,
            allow_execute_sql,
            deep_analysis,
            watchdog_scope,
            allow_actions,
            confidence_threshold,
            execution_mode,
            sync_timeout_ms,
            ack_timeout_ms,
        } => {
            let key = normalize_capability_key(capability_key);
            if key.is_empty() {
                return None;
            }
            let prefixes = normalize_trigger_prefixes(trigger_prefixes);
            let mut config = json!({
                "trigger_prefixes": prefixes,
                "require_mention": require_mention,
                "fallback_when_no_prefix": fallback_when_no_prefix
            });
            if let Some(object) = config.as_object_mut() {
                match key.as_str() {
                    "ai_chat" | "generic_ai" => {
                        if let Some(value) = clean_string_value(model) {
                            object.insert("model".to_string(), Value::String(value));
                        }
                    }
                    "super_adversarial" => {
                        let values = clean_string_vec(models);
                        if !values.is_empty() {
                            object.insert("models".to_string(), json!(values));
                        }
                        if let Some(value) = max_rounds {
                            object.insert("maxRounds".to_string(), json!(value));
                        }
                    }
                    "pm_assistant" => {
                        if let Some(value) = clean_string_value(model) {
                            object.insert("model".to_string(), Value::String(value));
                        }
                        if let Some(value) = deep_analysis {
                            object.insert("deepAnalysis".to_string(), json!(value));
                        }
                    }
                    "rd_agent" => {
                        if let Some(value) = clean_string_value(model) {
                            object.insert("model".to_string(), Value::String(value));
                        }
                        if let Some(value) = clean_string_value(repository_id) {
                            object.insert("repositoryId".to_string(), Value::String(value));
                        }
                        if let Some(value) = clean_string_value(agent_profile_id) {
                            object.insert("agentProfileId".to_string(), Value::String(value));
                        }
                        if let Some(value) = clean_string_value(workflow_id) {
                            object.insert("workflowId".to_string(), Value::String(value));
                        }
                        if let Some(value) = clean_string_value(context_depth) {
                            object.insert("contextDepth".to_string(), Value::String(value));
                        }
                        if let Some(value) = should_deep_scan {
                            object.insert("shouldDeepScan".to_string(), json!(value));
                        }
                    }
                    "nl2sql" => {
                        if let Some(value) = clean_string_value(data_source_id) {
                            object.insert("dataSourceId".to_string(), Value::String(value));
                        }
                        if let Some(value) = allow_execute_sql {
                            object.insert("allowExecuteSql".to_string(), json!(value));
                        }
                    }
                    "watchdog" => {
                        if let Some(value) = clean_string_value(watchdog_scope) {
                            object.insert("watchdogScope".to_string(), Value::String(value));
                        }
                        let values = clean_string_vec(allow_actions);
                        object.insert(
                            "allowActions".to_string(),
                            json!(if values.is_empty() {
                                vec!["open_task".to_string()]
                            } else {
                                values
                            }),
                        );
                    }
                    "aos_router" => {
                        if let Some(value) = confidence_threshold {
                            object.insert(
                                "confidenceThreshold".to_string(),
                                json!(value.clamp(0.50, 0.99)),
                            );
                        }
                    }
                    _ => {}
                }
                if let Some(value) = clean_string_value(execution_mode) {
                    object.insert("executionMode".to_string(), Value::String(value));
                }
                if let Some(value) = sync_timeout_ms {
                    object.insert("syncTimeoutMs".to_string(), json!(value));
                }
                if let Some(value) = ack_timeout_ms {
                    object.insert("ackTimeoutMs".to_string(), json!(value));
                }
            }
            Some((key, Some(config)))
        }
    }
}

pub fn first_capability_key(capabilities: &[CapabilityBindingInput]) -> Option<String> {
    let keys = capabilities
        .iter()
        .filter_map(|item| capability_binding_parts(item).map(|(key, _)| key))
        .collect::<Vec<_>>();
    keys.iter()
        .find(|key| key.as_str() == "aos_router")
        .cloned()
        .or_else(|| keys.into_iter().next())
}

pub fn default_capability_bindings() -> Vec<CapabilityBindingInput> {
    vec![CapabilityBindingInput::Detailed {
        capability_key: "aos_router".to_string(),
        trigger_prefixes: Vec::new(),
        require_mention: false,
        fallback_when_no_prefix: true,
        model: None,
        models: None,
        max_rounds: None,
        repository_id: None,
        agent_profile_id: None,
        workflow_id: None,
        context_depth: None,
        should_deep_scan: None,
        data_source_id: None,
        allow_execute_sql: None,
        deep_analysis: None,
        watchdog_scope: None,
        allow_actions: None,
        confidence_threshold: None,
        execution_mode: None,
        sync_timeout_ms: None,
        ack_timeout_ms: None,
    }]
}

#[cfg(test)]
mod tests {
    use super::{
        capability_binding_parts, default_capability_bindings, first_capability_key,
        CapabilityBindingInput,
    };

    #[test]
    fn default_bot_uses_super_assistant_router() {
        let bindings = default_capability_bindings();
        let (key, config) = capability_binding_parts(&bindings[0]).expect("default binding");
        assert_eq!(key, "aos_router");
        let config = config.expect("default config");
        assert_eq!(config["require_mention"], false);
        assert_eq!(config["fallback_when_no_prefix"], true);
    }

    #[test]
    fn super_assistant_router_is_the_preferred_default_when_bound() {
        let bindings = vec![
            CapabilityBindingInput::Key("rd_agent".to_string()),
            CapabilityBindingInput::Key("aos_router".to_string()),
            CapabilityBindingInput::Key("nl2sql".to_string()),
        ];
        assert_eq!(
            first_capability_key(&bindings).as_deref(),
            Some("aos_router")
        );
    }

    #[test]
    fn aos_router_binding_persists_confidence_threshold() {
        let (_, config) = capability_binding_parts(&CapabilityBindingInput::Detailed {
            capability_key: "aos_router".to_string(),
            trigger_prefixes: Vec::new(),
            require_mention: false,
            fallback_when_no_prefix: true,
            model: None,
            models: None,
            max_rounds: None,
            repository_id: None,
            agent_profile_id: None,
            workflow_id: None,
            context_depth: None,
            should_deep_scan: None,
            data_source_id: None,
            allow_execute_sql: None,
            deep_analysis: None,
            watchdog_scope: None,
            allow_actions: None,
            confidence_threshold: Some(1.5),
            execution_mode: None,
            sync_timeout_ms: None,
            ack_timeout_ms: None,
        })
        .expect("router binding");
        let config = config.expect("config json");
        assert_eq!(
            config
                .get("confidenceThreshold")
                .and_then(serde_json::Value::as_f64),
            Some(0.99)
        );
    }
}
