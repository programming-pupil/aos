use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::providers::model_token_limit;

pub const MODEL_CAPABILITY_SCHEMA_VERSION: u32 = 2;
pub const MODEL_CAPABILITY_REGISTRY_VERSION: &str = "2026-08-06.1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ModelProtocol {
    #[serde(rename = "anthropic_messages")]
    AnthropicMessages,
    #[serde(rename = "openai_chat_completions", alias = "open_ai_chat_completions")]
    OpenAiChatCompletions,
    #[serde(rename = "openai_responses", alias = "open_ai_responses")]
    OpenAiResponses,
}

impl ModelProtocol {
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::AnthropicMessages => "anthropic_messages",
            Self::OpenAiChatCompletions => "openai_chat_completions",
            Self::OpenAiResponses => "openai_responses",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelFeatureProfile {
    pub streaming: Option<bool>,
    pub tools: Option<bool>,
    pub parallel_tools: Option<bool>,
    pub structured_output: Option<bool>,
    pub json_object: Option<bool>,
    pub json_schema: Option<bool>,
    pub strict_json_schema: Option<bool>,
    pub structured_output_verified: Option<bool>,
    pub structured_output_verified_mode: Option<String>,
    pub structured_output_verified_at: Option<String>,
    pub vision: Option<bool>,
    pub native_web_search: Option<bool>,
    pub reasoning_output: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReasoningProfile {
    pub transport: String,
    #[serde(default)]
    pub supported_values: Vec<String>,
    #[serde(default)]
    pub budget_map: BTreeMap<String, String>,
    pub default: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelProfile {
    #[serde(alias = "capabilitySchemaVersion")]
    pub schema_version: u32,
    pub registry_version: String,
    pub protocol: ModelProtocol,
    pub context_window_tokens: Option<u32>,
    pub max_output_tokens: Option<u32>,
    pub output_token_parameter: String,
    pub supports_temperature: Option<bool>,
    pub supports_top_p: Option<bool>,
    pub features: ModelFeatureProfile,
    pub reasoning: Option<ReasoningProfile>,
}

impl ModelProfile {
    #[must_use]
    pub fn runtime_capabilities(&self) -> Value {
        let mut value = json!({
            "capabilitySchemaVersion": self.schema_version,
            "registryVersion": self.registry_version,
            "protocol": self.protocol.as_str(),
            "outputTokenParameter": self.output_token_parameter,
            "features": self.features,
        });
        let object = value
            .as_object_mut()
            .expect("model profile JSON must be an object");
        if let Some(context) = self.context_window_tokens {
            object.insert("contextWindowTokens".to_string(), json!(context));
        }
        if let Some(max_output) = self.max_output_tokens {
            object.insert("maxOutputTokens".to_string(), json!(max_output));
        }
        if let Some(supported) = self.supports_temperature {
            object.insert("supportsTemperature".to_string(), json!(supported));
        }
        if let Some(supported) = self.supports_top_p {
            object.insert("supportsTopP".to_string(), json!(supported));
        }
        if self.output_token_parameter == "max_completion_tokens" {
            object.insert("useMaxCompletionTokens".to_string(), json!(true));
        }
        if let Some(reasoning) = &self.reasoning {
            object.insert("reasoning".to_string(), json!(reasoning));
            object.insert("reasoningTransport".to_string(), json!(reasoning.transport));
            object.insert(
                "reasoningEffortValues".to_string(),
                json!(reasoning.supported_values),
            );
            object.insert(
                "reasoningBudgetMap".to_string(),
                json!(reasoning.budget_map),
            );
            object.insert("reasoningEffort".to_string(), json!(true));
            if let Some(default) = &reasoning.default {
                object.insert("reasoningEffortDefault".to_string(), json!(default));
            }
            if self.features.reasoning_output == Some(true) {
                object.insert("includeReasoning".to_string(), json!(true));
            }
        }
        value
    }
}

fn normalized_model(model: &str) -> String {
    model
        .trim()
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
}

fn normalized_provider(provider: &str, base_url: Option<&str>, model: &str) -> String {
    let provider = provider.trim().to_ascii_lowercase();
    let base = base_url.unwrap_or_default().to_ascii_lowercase();
    let model = normalized_model(model);
    if provider == "anthropic" || base.contains("api.anthropic.com") || model.starts_with("claude")
    {
        return "anthropic".to_string();
    }
    if provider == "deepseek" || base.contains("api.deepseek.com") || model.starts_with("deepseek")
    {
        return "deepseek".to_string();
    }
    if provider == "kimi"
        || provider == "moonshot"
        || base.contains("api.moonshot.cn")
        || model.starts_with("kimi-")
    {
        return "kimi".to_string();
    }
    if provider == "glm"
        || provider == "zhipu"
        || base.contains("open.bigmodel.cn")
        || model.starts_with("glm-")
    {
        return "glm".to_string();
    }
    if provider == "gemini"
        || provider == "google"
        || base.contains("generativelanguage.googleapis.com")
        || model.starts_with("gemini-")
    {
        return "gemini".to_string();
    }
    if provider == "xai" || base.contains("api.x.ai") || model.starts_with("grok") {
        return "xai".to_string();
    }
    if base.contains("openrouter.ai") {
        return "openrouter".to_string();
    }
    provider
}

fn budget_map(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
    entries
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect()
}

#[must_use]
pub fn infer_model_profile(provider: &str, base_url: Option<&str>, model: &str) -> ModelProfile {
    let provider = normalized_provider(provider, base_url, model);
    let model_name = normalized_model(model);
    let limits = model_token_limit(model);
    let protocol = if provider == "anthropic" {
        ModelProtocol::AnthropicMessages
    } else {
        ModelProtocol::OpenAiChatCompletions
    };

    let is_openai_reasoning = model_name.starts_with("gpt-5")
        || model_name.starts_with("o1")
        || model_name.starts_with("o3")
        || model_name.starts_with("o4");
    let is_deepseek_v4 = model_name.starts_with("deepseek-v4");
    let is_anthropic_thinking = provider == "anthropic"
        && (model_name.contains("claude-3-7")
            || model_name.contains("claude-4")
            || model_name.contains("opus-4")
            || model_name.contains("sonnet-4")
            || model_name.contains("haiku-4"));
    let is_qwen_thinking = model_name.starts_with("qwq")
        || model_name.contains("qwen-qwq")
        || model_name.contains("thinking");
    let reasoning = if is_deepseek_v4 {
        Some(ReasoningProfile {
            transport: "reasoning_effort".to_string(),
            supported_values: ["low", "high", "max"].map(str::to_string).to_vec(),
            budget_map: budget_map(&[("fast", "low"), ("standard", "high"), ("deep", "max")]),
            default: Some("high".to_string()),
        })
    } else if is_openai_reasoning {
        let supports_xhigh = model_name.starts_with("gpt-5");
        let mut supported_values = ["low", "medium", "high"].map(str::to_string).to_vec();
        if supports_xhigh {
            supported_values.push("xhigh".to_string());
        }
        Some(ReasoningProfile {
            transport: "reasoning_effort".to_string(),
            supported_values,
            budget_map: budget_map(&[("fast", "low"), ("standard", "medium"), ("deep", "high")]),
            default: Some("medium".to_string()),
        })
    } else if is_anthropic_thinking {
        Some(ReasoningProfile {
            transport: "anthropic_thinking".to_string(),
            supported_values: ["low", "medium", "high"].map(str::to_string).to_vec(),
            budget_map: budget_map(&[("fast", "low"), ("standard", "medium"), ("deep", "high")]),
            default: Some("medium".to_string()),
        })
    } else if is_qwen_thinking {
        Some(ReasoningProfile {
            transport: "enable_thinking".to_string(),
            supported_values: ["disabled", "enabled"].map(str::to_string).to_vec(),
            budget_map: budget_map(&[
                ("fast", "disabled"),
                ("standard", "enabled"),
                ("deep", "enabled"),
            ]),
            default: Some("enabled".to_string()),
        })
    } else {
        None
    };

    let reasoning_model = reasoning.is_some();
    let output_token_parameter = if provider == "anthropic" {
        "max_tokens"
    } else if is_openai_reasoning {
        "max_completion_tokens"
    } else {
        "max_tokens"
    };
    let known_provider = matches!(
        provider.as_str(),
        "anthropic" | "openai" | "deepseek" | "kimi" | "glm" | "gemini" | "xai" | "openrouter"
    );

    ModelProfile {
        schema_version: MODEL_CAPABILITY_SCHEMA_VERSION,
        registry_version: MODEL_CAPABILITY_REGISTRY_VERSION.to_string(),
        protocol,
        context_window_tokens: limits.map(|value| value.context_window_tokens),
        max_output_tokens: limits.map(|value| value.max_output_tokens),
        output_token_parameter: output_token_parameter.to_string(),
        supports_temperature: Some(!reasoning_model),
        supports_top_p: Some(!reasoning_model),
        features: ModelFeatureProfile {
            streaming: Some(true),
            tools: known_provider.then_some(true),
            parallel_tools: (provider == "openai" || provider == "anthropic").then_some(true),
            structured_output: None,
            json_object: None,
            json_schema: None,
            strict_json_schema: None,
            structured_output_verified: None,
            structured_output_verified_mode: None,
            structured_output_verified_at: None,
            vision: None,
            native_web_search: (provider == "openai" && model_name.starts_with("gpt-"))
                .then_some(true),
            reasoning_output: reasoning_model.then_some(true),
        },
        reasoning,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deepseek_v4_uses_documented_reasoning_levels() {
        let profile = infer_model_profile(
            "custom",
            Some("https://api.deepseek.com/v1"),
            "deepseek-v4-pro",
        );
        let reasoning = profile.reasoning.expect("reasoning profile");
        assert_eq!(reasoning.supported_values, ["low", "high", "max"]);
        assert_eq!(
            reasoning.budget_map.get("deep").map(String::as_str),
            Some("max")
        );
    }

    #[test]
    fn gpt5_profile_records_xhigh_without_using_it_for_default_deep_turns() {
        let profile = infer_model_profile("openai", None, "gpt-5.4");
        let reasoning = profile.reasoning.expect("reasoning profile");
        assert!(reasoning
            .supported_values
            .iter()
            .any(|value| value == "xhigh"));
        assert_eq!(
            reasoning.budget_map.get("deep").map(String::as_str),
            Some("high")
        );
        assert_eq!(profile.output_token_parameter, "max_completion_tokens");
    }

    #[test]
    fn unknown_custom_model_is_conservative() {
        let profile =
            infer_model_profile("custom", Some("http://localhost:11434/v1"), "local-model");
        assert!(profile.reasoning.is_none());
        assert_eq!(profile.features.tools, None);
        assert_eq!(profile.context_window_tokens, None);
    }

    #[test]
    fn detected_native_search_support_does_not_enable_runtime_policy() {
        let profile = infer_model_profile("openai", None, "gpt-5.4");
        assert_eq!(profile.features.native_web_search, Some(true));
        assert!(profile
            .runtime_capabilities()
            .get("nativeWebSearch")
            .is_none());
    }

    #[test]
    fn openai_compatible_provider_presets_have_distinct_profiles() {
        for (provider, base_url, model) in [
            ("kimi", "https://api.moonshot.cn/v1", "kimi-k2.5"),
            ("glm", "https://open.bigmodel.cn/api/paas/v4", "glm-5"),
            (
                "gemini",
                "https://generativelanguage.googleapis.com/v1beta/openai",
                "gemini-2.5-pro",
            ),
            ("xai", "https://api.x.ai/v1", "grok-3"),
        ] {
            let profile = infer_model_profile(provider, Some(base_url), model);
            assert_eq!(profile.protocol, ModelProtocol::OpenAiChatCompletions);
            assert_eq!(profile.features.tools, Some(true));
            assert!(profile
                .runtime_capabilities()
                .get("nativeWebSearch")
                .is_none());
        }
    }
}
