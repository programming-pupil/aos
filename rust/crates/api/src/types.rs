use runtime::{pricing_for_model, TokenUsage, UsageCostEstimate};
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct MessageRequest {
    pub model: String,
    pub max_tokens: u32,
    pub messages: Vec<InputMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolDefinition>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stream: bool,
    /// OpenAI-compatible tuning parameters. Optional — omitted from payload when None.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frequency_penalty: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub presence_penalty: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop: Option<Vec<String>>,
    /// Reasoning effort level for OpenAI-compatible reasoning models (e.g. `o4-mini`).
    /// Accepted values: `"low"`, `"medium"`, `"high"`. Omitted when `None`.
    /// Silently ignored by backends that do not support it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    /// Provider-specific generation capabilities declared by runtime config.
    ///
    /// These flags are capability-driven so open-source users can enable new
    /// models without waiting for AOS to hardcode model-name prefixes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub use_max_completion_tokens: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_reasoning: Option<bool>,
    /// Provider-specific request-body extensions declared by runtime capabilities.
    ///
    /// This is intentionally capability-driven instead of model-name-driven.
    /// Callers must sanitize keys before setting it so core fields such as
    /// `model`, `messages`, `stream`, `tools`, and `tool_choice` cannot be
    /// overwritten by operator config.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra_body: Option<serde_json::Map<String, serde_json::Value>>,
}

/// Canonical unary request for the provider `/responses/compact` endpoint.
///
/// This deliberately does not reuse [`MessageRequest`]. Remote compaction has
/// a different wire contract and returns provider-normalized response items,
/// not an assistant chat message. Keeping the types separate prevents a
/// summary-prompt chat call from being reported as provider-native
/// compaction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResponsesCompactRequest {
    pub model: String,
    pub input: Vec<Value>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub instructions: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Value>>,
    #[serde(default)]
    pub parallel_tool_calls: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_cache_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<Value>,
}

impl ResponsesCompactRequest {
    /// Apply the same outbound data-protection boundary used for ordinary
    /// model calls. Provider-normalized item shapes stay intact while their
    /// string/JSON payloads are protected recursively.
    #[must_use]
    pub fn protect_sensitive_content(
        &self,
        mode: runtime::DataProtectionMode,
    ) -> (Self, runtime::DataProtectionReport) {
        let mut request = self.clone();
        let mut report = runtime::DataProtectionReport::default();
        let (input, input_report) =
            runtime::protect_sensitive_json(&Value::Array(request.input.clone()), mode);
        if let Value::Array(input) = input {
            request.input = input;
        }
        report.merge(&input_report);
        let protected = runtime::protect_sensitive_text(&request.instructions, mode);
        request.instructions = protected.value;
        report.merge(&protected.report);
        if let Some(tools) = request.tools.as_mut() {
            let (protected, tools_report) =
                runtime::protect_sensitive_json(&Value::Array(tools.clone()), mode);
            if let Value::Array(protected) = protected {
                *tools = protected;
            }
            report.merge(&tools_report);
        }
        for value in [&mut request.reasoning, &mut request.text]
            .into_iter()
            .flatten()
        {
            let (protected, value_report) = runtime::protect_sensitive_json(value, mode);
            *value = protected;
            report.merge(&value_report);
        }
        (request, report)
    }
}

/// A validated provider-normalized item returned by `/responses/compact`.
/// `raw` is retained byte-semantically so opaque compaction metadata is never
/// flattened into prose or silently discarded.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResponsesCompactOutputItem {
    pub item_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    pub raw: Value,
}

/// Normalized result of the dedicated remote-compaction endpoint.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResponsesCompactResult {
    pub output: Vec<ResponsesCompactOutputItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

#[must_use]
pub(crate) fn is_reserved_extra_body_key(key: &str) -> bool {
    let normalized = key.trim().to_ascii_lowercase();
    matches!(
        normalized.as_str(),
        "model"
            | "messages"
            | "stream"
            | "tools"
            | "tool_choice"
            | "max_tokens"
            | "max_completion_tokens"
            | "system"
    )
}

impl MessageRequest {
    #[must_use]
    pub fn with_streaming(mut self) -> Self {
        self.stream = true;
        self
    }

    /// Redact credentials before a request crosses the model-provider boundary.
    /// Signed reasoning is preserved verbatim because changing it invalidates
    /// the provider signature; user text, tool I/O and provider extensions are
    /// always protected.
    #[must_use]
    pub fn protect_sensitive_content(
        &self,
        mode: runtime::DataProtectionMode,
    ) -> (Self, runtime::DataProtectionReport) {
        let mut request = self.clone();
        let mut report = runtime::DataProtectionReport::default();

        if let Some(system) = request.system.as_mut() {
            let protected = runtime::protect_sensitive_text(system, mode);
            *system = protected.value;
            report.merge(&protected.report);
        }
        for message in &mut request.messages {
            for block in &mut message.content {
                protect_input_content_block(block, mode, &mut report);
            }
        }
        if let Some(tools) = request.tools.as_mut() {
            for tool in tools {
                if let Some(description) = tool.description.as_mut() {
                    let protected = runtime::protect_sensitive_text(description, mode);
                    *description = protected.value;
                    report.merge(&protected.report);
                }
                let (protected, finding) =
                    runtime::protect_sensitive_json(&tool.input_schema, mode);
                tool.input_schema = protected;
                report.merge(&finding);
            }
        }
        if let Some(stop) = request.stop.as_mut() {
            for value in stop {
                let protected = runtime::protect_sensitive_text(value, mode);
                *value = protected.value;
                report.merge(&protected.report);
            }
        }
        if let Some(extra_body) = request.extra_body.as_mut() {
            let (protected, finding) =
                runtime::protect_sensitive_json(&Value::Object(extra_body.clone()), mode);
            if let Value::Object(protected) = protected {
                *extra_body = protected;
            }
            report.merge(&finding);
        }

        (request, report)
    }
}

fn protect_input_content_block(
    block: &mut InputContentBlock,
    mode: runtime::DataProtectionMode,
    report: &mut runtime::DataProtectionReport,
) {
    match block {
        InputContentBlock::Text { text } => {
            let protected = runtime::protect_sensitive_text(text, mode);
            *text = protected.value;
            report.merge(&protected.report);
        }
        InputContentBlock::Thinking {
            thinking,
            signature,
        } => {
            if signature.is_none() {
                let protected = runtime::protect_sensitive_text(thinking, mode);
                *thinking = protected.value;
                report.merge(&protected.report);
            }
        }
        InputContentBlock::Image {
            source_type, data, ..
        }
        | InputContentBlock::Document {
            source_type, data, ..
        } => {
            if *source_type == ImageSourceType::Url {
                let protected = runtime::protect_sensitive_text(data, mode);
                *data = protected.value;
                report.merge(&protected.report);
            }
        }
        InputContentBlock::ToolUse { input, .. } => {
            let (protected, finding) = runtime::protect_sensitive_json(input, mode);
            *input = protected;
            report.merge(&finding);
        }
        InputContentBlock::ToolResult { content, .. } => {
            for item in content {
                match item {
                    ToolResultContentBlock::Text { text } => {
                        let protected = runtime::protect_sensitive_text(text, mode);
                        *text = protected.value;
                        report.merge(&protected.report);
                    }
                    ToolResultContentBlock::Json { value } => {
                        let (protected, finding) = runtime::protect_sensitive_json(value, mode);
                        *value = protected;
                        report.merge(&finding);
                    }
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InputMessage {
    pub role: String,
    pub content: Vec<InputContentBlock>,
}

impl InputMessage {
    #[must_use]
    pub fn user_text(text: impl Into<String>) -> Self {
        Self {
            role: "user".to_string(),
            content: vec![InputContentBlock::Text { text: text.into() }],
        }
    }

    #[must_use]
    pub fn user_image(
        media_type: impl Into<String>,
        source_type: ImageSourceType,
        data: impl Into<String>,
    ) -> Self {
        Self {
            role: "user".to_string(),
            content: vec![InputContentBlock::Image {
                media_type: media_type.into(),
                source_type,
                data: data.into(),
            }],
        }
    }

    #[must_use]
    pub fn user_document(
        media_type: impl Into<String>,
        source_type: ImageSourceType,
        data: impl Into<String>,
        name: Option<String>,
    ) -> Self {
        Self {
            role: "user".to_string(),
            content: vec![InputContentBlock::Document {
                media_type: media_type.into(),
                source_type,
                data: data.into(),
                name,
            }],
        }
    }

    #[must_use]
    pub fn user_tool_result(
        tool_use_id: impl Into<String>,
        content: impl Into<String>,
        is_error: bool,
    ) -> Self {
        Self {
            role: "user".to_string(),
            content: vec![InputContentBlock::ToolResult {
                tool_use_id: tool_use_id.into(),
                content: vec![ToolResultContentBlock::Text {
                    text: content.into(),
                }],
                is_error,
            }],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageSourceType {
    Base64,
    Url,
}

impl ImageSourceType {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Base64 => "base64",
            Self::Url => "url",
        }
    }
}

/// `InputContentBlock` enum with manual Serialize/Deserialize implementations.
/// Uses custom serialization format for Anthropic API compatibility.
#[derive(Debug, Clone, PartialEq)]
pub enum InputContentBlock {
    Text {
        text: String,
    },
    /// Extended reasoning ("thinking") block replayed back to the provider on
    /// multi-turn tool use. Anthropic requires prior thinking blocks (with
    /// their `signature`) to be echoed back to maintain reasoning continuity;
    /// OpenAI-compatible providers (DeepSeek/GLM/Qwen) require `reasoning_content`
    /// to be passed back or they reject the request.
    Thinking {
        thinking: String,
        signature: Option<String>,
    },
    Image {
        media_type: String,
        source_type: ImageSourceType,
        data: String,
    },
    Document {
        /// MIME type of the document (e.g. "application/pdf").
        media_type: String,
        /// Either "base64" or "url".
        source_type: ImageSourceType,
        data: String,
        /// Original filename, used by some providers for context.
        name: Option<String>,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        content: Vec<ToolResultContentBlock>,
        is_error: bool,
    },
}

/// Serializes an Image content block into the Anthropic API wire format:
/// { "type": "image", "source": { "type": "base64"|"url", "`media_type`": "...", "data": "..." } }
fn serialize_image<S>(block: &InputContentBlock, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    let InputContentBlock::Image {
        media_type,
        source_type,
        data,
    } = block
    else {
        return Err(serde::ser::Error::custom("expected Image block"));
    };
    let mut map = serializer.serialize_map(Some(2))?;
    map.serialize_entry("type", "image")?;
    map.serialize_entry(
        "source",
        &serde_json::json!({
            "type": source_type.as_str(),
            "media_type": media_type,
            "data": data,
        }),
    )?;
    map.end()
}

/// Serializes a Document content block into the Anthropic API wire format:
/// { "type": "document", "source": { "type": "base64"|"url", "`media_type`": "...", "data": "..." }, "name": "..." }
fn serialize_document<S>(block: &InputContentBlock, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    let InputContentBlock::Document {
        media_type,
        source_type,
        data,
        name,
    } = block
    else {
        return Err(serde::ser::Error::custom("expected Document block"));
    };
    let mut map = serializer.serialize_map(Some(if name.is_some() { 3 } else { 2 }))?;
    map.serialize_entry("type", "document")?;
    map.serialize_entry(
        "source",
        &serde_json::json!({
            "type": source_type.as_str(),
            "media_type": media_type,
            "data": data,
        }),
    )?;
    if let Some(n) = name {
        map.serialize_entry("name", &n)?;
    }
    map.end()
}

/// Custom Serialize for `InputContentBlock` to handle Anthropic API wire format.
impl Serialize for InputContentBlock {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            InputContentBlock::Text { text } => {
                let mut map = serializer.serialize_map(Some(2))?;
                map.serialize_entry("type", "text")?;
                map.serialize_entry("text", text)?;
                map.end()
            }
            InputContentBlock::Thinking {
                thinking,
                signature,
            } => {
                let map_size = if signature.is_some() { 3 } else { 2 };
                let mut map = serializer.serialize_map(Some(map_size))?;
                map.serialize_entry("type", "thinking")?;
                map.serialize_entry("thinking", thinking)?;
                if let Some(signature) = signature {
                    map.serialize_entry("signature", signature)?;
                }
                map.end()
            }
            InputContentBlock::Image { .. } => serialize_image(self, serializer),
            InputContentBlock::Document { .. } => serialize_document(self, serializer),
            InputContentBlock::ToolUse { id, name, input } => {
                let mut map = serializer.serialize_map(Some(4))?;
                map.serialize_entry("type", "tool_use")?;
                map.serialize_entry("id", id)?;
                map.serialize_entry("name", name)?;
                map.serialize_entry("input", input)?;
                map.end()
            }
            InputContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => {
                let map_size = if *is_error { 4 } else { 3 };
                let mut map = serializer.serialize_map(Some(map_size))?;
                map.serialize_entry("type", "tool_result")?;
                map.serialize_entry("tool_use_id", tool_use_id)?;
                map.serialize_entry("content", content)?;
                if *is_error {
                    map.serialize_entry("is_error", is_error)?;
                }
                map.end()
            }
        }
    }
}

/// Custom Deserialize for `InputContentBlock`.
/// Uses manual deserialization to handle the Anthropic wire format
/// where `Image` and `Document` have a nested `source` object.
impl<'de> Deserialize<'de> for InputContentBlock {
    #[allow(clippy::too_many_lines)]
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(serde::Deserialize)]
        #[serde(tag = "type", rename_all = "snake_case")]
        enum TagOnly {
            Text,
            Thinking,
            Image,
            Document,
            ToolUse,
            ToolResult,
        }

        let json = Value::deserialize(deserializer)?;
        let Some(tag) = json.get("type").and_then(|v| v.as_str()) else {
            return Err(serde::de::Error::custom(
                "missing `type` field in InputContentBlock",
            ));
        };

        match tag {
            "thinking" => Ok(InputContentBlock::Thinking {
                thinking: json
                    .get("thinking")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                signature: json
                    .get("signature")
                    .and_then(|v| v.as_str())
                    .map(ToString::to_string),
            }),
            "text" => {
                let inner = serde_json::from_value::<TagOnly>(json.clone())
                    .map_err(serde::de::Error::custom)?;
                if let TagOnly::Text = inner {
                    Ok(InputContentBlock::Text {
                        text: json
                            .get("text")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                    })
                } else {
                    unreachable!()
                }
            }
            "image" => {
                let source = json.get("source").ok_or_else(|| {
                    serde::de::Error::custom("missing `source` field in Image block")
                })?;
                let source_type = source
                    .get("type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("base64");
                let source_type = if source_type == "url" {
                    ImageSourceType::Url
                } else {
                    ImageSourceType::Base64
                };
                Ok(InputContentBlock::Image {
                    media_type: source
                        .get("media_type")
                        .and_then(|v| v.as_str())
                        .unwrap_or("image/png")
                        .to_string(),
                    source_type,
                    data: source
                        .get("data")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                })
            }
            "document" => {
                let source = json.get("source").ok_or_else(|| {
                    serde::de::Error::custom("missing `source` field in Document block")
                })?;
                let source_type = source
                    .get("type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("base64");
                let source_type = if source_type == "url" {
                    ImageSourceType::Url
                } else {
                    ImageSourceType::Base64
                };
                Ok(InputContentBlock::Document {
                    media_type: source
                        .get("media_type")
                        .and_then(|v| v.as_str())
                        .unwrap_or("application/octet-stream")
                        .to_string(),
                    source_type,
                    data: source
                        .get("data")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    name: json.get("name").and_then(|v| v.as_str()).map(String::from),
                })
            }
            "tool_use" => {
                #[derive(serde::Deserialize)]
                #[serde(tag = "type", rename_all = "snake_case")]
                enum ToolUseTag {
                    ToolUse {
                        id: String,
                        name: String,
                        input: Value,
                    },
                }
                let inner =
                    serde_json::from_value::<ToolUseTag>(json).map_err(serde::de::Error::custom)?;
                let ToolUseTag::ToolUse { id, name, input } = inner;
                Ok(InputContentBlock::ToolUse { id, name, input })
            }
            "tool_result" => {
                #[derive(serde::Deserialize)]
                #[serde(tag = "type", rename_all = "snake_case")]
                enum ToolResultTag {
                    ToolResult {
                        tool_use_id: String,
                        content: Vec<ToolResultContentBlock>,
                        #[serde(default)]
                        is_error: bool,
                    },
                }
                let inner = serde_json::from_value::<ToolResultTag>(json)
                    .map_err(serde::de::Error::custom)?;
                let ToolResultTag::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                } = inner;
                Ok(InputContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                })
            }
            _ => Err(serde::de::Error::custom(format!(
                "unknown InputContentBlock type: {tag}"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolResultContentBlock {
    Text { text: String },
    Json { value: Value },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub input_schema: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolChoice {
    Auto,
    Any,
    Tool { name: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessageResponse {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub role: String,
    pub content: Vec<OutputContentBlock>,
    pub model: String,
    #[serde(default)]
    pub stop_reason: Option<String>,
    #[serde(default)]
    pub stop_sequence: Option<String>,
    #[serde(default)]
    pub usage: Usage,
    #[serde(default)]
    pub request_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_metadata: Option<Value>,
}

impl MessageResponse {
    #[must_use]
    pub fn total_tokens(&self) -> u32 {
        self.usage.total_tokens()
    }

    #[must_use]
    pub fn protect_sensitive_content(
        &self,
        mode: runtime::DataProtectionMode,
    ) -> (Self, runtime::DataProtectionReport) {
        let mut response = self.clone();
        let mut report = runtime::DataProtectionReport::default();
        for block in &mut response.content {
            protect_output_content_block(block, mode, &mut report);
        }
        if let Some(metadata) = response.provider_metadata.as_mut() {
            let (protected, finding) = runtime::protect_sensitive_json(metadata, mode);
            *metadata = protected;
            report.merge(&finding);
        }
        (response, report)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OutputContentBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    Thinking {
        #[serde(default)]
        thinking: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
    RedactedThinking {
        data: Value,
    },
}

fn protect_output_content_block(
    block: &mut OutputContentBlock,
    mode: runtime::DataProtectionMode,
    report: &mut runtime::DataProtectionReport,
) {
    match block {
        OutputContentBlock::Text { text } => {
            let protected = runtime::protect_sensitive_text(text, mode);
            *text = protected.value;
            report.merge(&protected.report);
        }
        OutputContentBlock::ToolUse { input, .. } => {
            let (protected, finding) = runtime::protect_sensitive_json(input, mode);
            *input = protected;
            report.merge(&finding);
        }
        OutputContentBlock::Thinking {
            thinking,
            signature,
        } => {
            if signature.is_none() {
                let protected = runtime::protect_sensitive_text(thinking, mode);
                *thinking = protected.value;
                report.merge(&protected.report);
            }
        }
        OutputContentBlock::RedactedThinking { .. } => {}
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u32,
    #[serde(default)]
    pub cache_creation_input_tokens: u32,
    #[serde(default)]
    pub cache_read_input_tokens: u32,
    #[serde(default)]
    pub output_tokens: u32,
}

impl Usage {
    #[must_use]
    pub const fn total_tokens(&self) -> u32 {
        self.input_tokens
            + self.output_tokens
            + self.cache_creation_input_tokens
            + self.cache_read_input_tokens
    }

    #[must_use]
    pub const fn token_usage(&self) -> TokenUsage {
        TokenUsage {
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            cache_creation_input_tokens: self.cache_creation_input_tokens,
            cache_read_input_tokens: self.cache_read_input_tokens,
        }
    }

    #[must_use]
    pub fn estimated_cost_usd(&self, model: &str) -> UsageCostEstimate {
        let usage = self.token_usage();
        pricing_for_model(model).map_or_else(
            || usage.estimate_cost_usd(),
            |pricing| usage.estimate_cost_usd_with_pricing(pricing),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessageStartEvent {
    pub message: MessageResponse,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessageDeltaEvent {
    pub delta: MessageDelta,
    #[serde(default)]
    pub usage: Usage,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageDelta {
    #[serde(default)]
    pub stop_reason: Option<String>,
    #[serde(default)]
    pub stop_sequence: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContentBlockStartEvent {
    pub index: u32,
    pub content_block: OutputContentBlock,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContentBlockDeltaEvent {
    pub index: u32,
    pub delta: ContentBlockDelta,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlockDelta {
    TextDelta { text: String },
    InputJsonDelta { partial_json: String },
    ThinkingDelta { thinking: String },
    SignatureDelta { signature: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentBlockStopEvent {
    pub index: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageStopEvent {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEvent {
    MessageStart(MessageStartEvent),
    MessageDelta(MessageDeltaEvent),
    ContentBlockStart(ContentBlockStartEvent),
    ContentBlockDelta(ContentBlockDeltaEvent),
    ContentBlockStop(ContentBlockStopEvent),
    MessageStop(MessageStopEvent),
}

impl StreamEvent {
    #[must_use]
    pub fn protect_sensitive_content(
        &self,
        mode: runtime::DataProtectionMode,
    ) -> (Self, runtime::DataProtectionReport) {
        let mut event = self.clone();
        let mut report = runtime::DataProtectionReport::default();
        match &mut event {
            StreamEvent::MessageStart(start) => {
                let (protected, finding) = start.message.protect_sensitive_content(mode);
                start.message = protected;
                report.merge(&finding);
            }
            StreamEvent::ContentBlockStart(start) => {
                protect_output_content_block(&mut start.content_block, mode, &mut report);
            }
            StreamEvent::ContentBlockDelta(delta) => match &mut delta.delta {
                ContentBlockDelta::TextDelta { text }
                | ContentBlockDelta::InputJsonDelta { partial_json: text } => {
                    let protected = runtime::protect_sensitive_text(text, mode);
                    *text = protected.value;
                    report.merge(&protected.report);
                }
                ContentBlockDelta::ThinkingDelta { .. }
                | ContentBlockDelta::SignatureDelta { .. } => {}
            },
            StreamEvent::MessageDelta(_)
            | StreamEvent::ContentBlockStop(_)
            | StreamEvent::MessageStop(_) => {}
        }
        (event, report)
    }
}

#[cfg(test)]
mod tests {
    use runtime::format_usd;

    use super::{
        InputContentBlock, InputMessage, MessageRequest, MessageResponse, ToolResultContentBlock,
        Usage,
    };

    #[test]
    fn usage_total_tokens_includes_cache_tokens() {
        let usage = Usage {
            input_tokens: 10,
            cache_creation_input_tokens: 2,
            cache_read_input_tokens: 3,
            output_tokens: 4,
        };

        assert_eq!(usage.total_tokens(), 19);
        assert_eq!(usage.token_usage().total_tokens(), 19);
    }

    #[test]
    fn message_response_estimates_cost_from_model_usage() {
        let response = MessageResponse {
            id: "msg_cost".to_string(),
            kind: "message".to_string(),
            role: "assistant".to_string(),
            content: Vec::new(),
            model: "claude-sonnet-4-20250514".to_string(),
            stop_reason: Some("end_turn".to_string()),
            stop_sequence: None,
            usage: Usage {
                input_tokens: 1_000_000,
                cache_creation_input_tokens: 100_000,
                cache_read_input_tokens: 200_000,
                output_tokens: 500_000,
            },
            request_id: None,
            provider_metadata: None,
        };

        let cost = response.usage.estimated_cost_usd(&response.model);
        assert_eq!(format_usd(cost.total_cost_usd()), "$54.6750");
        assert_eq!(response.total_tokens(), 1_800_000);
    }

    #[test]
    fn provider_request_protection_covers_system_text_tool_input_and_result() {
        let request = MessageRequest {
            model: "test-model".to_string(),
            system: Some("Authorization: Bearer system-secret-token".to_string()),
            messages: vec![InputMessage {
                role: "user".to_string(),
                content: vec![
                    InputContentBlock::Text {
                        text: "api_key=sk-1234567890abcdef".to_string(),
                    },
                    InputContentBlock::ToolUse {
                        id: "call-1".to_string(),
                        name: "example".to_string(),
                        input: serde_json::json!({"password": "password=database-secret"}),
                    },
                    InputContentBlock::ToolResult {
                        tool_use_id: "call-1".to_string(),
                        content: vec![ToolResultContentBlock::Text {
                            text: "mysql://reader:database-secret@example.test/aos".to_string(),
                        }],
                        is_error: false,
                    },
                ],
            }],
            ..Default::default()
        };

        let (protected, report) =
            request.protect_sensitive_content(runtime::configured_data_protection_mode());
        let serialized = serde_json::to_string(&protected).unwrap();
        assert!(!serialized.contains("system-secret-token"));
        assert!(!serialized.contains("sk-1234567890abcdef"));
        assert!(!serialized.contains("database-secret"));
        assert!(report.finding_count >= 4);
    }

    #[test]
    fn provider_request_protection_preserves_signed_reasoning_protocol() {
        let request = MessageRequest {
            model: "test-model".to_string(),
            messages: vec![InputMessage {
                role: "assistant".to_string(),
                content: vec![InputContentBlock::Thinking {
                    thinking: "api_key=sk-1234567890abcdef".to_string(),
                    signature: Some("provider-signature".to_string()),
                }],
            }],
            ..Default::default()
        };

        let (protected, report) =
            request.protect_sensitive_content(runtime::configured_data_protection_mode());
        assert_eq!(protected, request);
        assert!(!report.redacted);
    }

    #[test]
    fn provider_response_protection_redacts_visible_text_and_tool_input() {
        let response = MessageResponse {
            id: "msg-secret".to_string(),
            kind: "message".to_string(),
            role: "assistant".to_string(),
            content: vec![
                super::OutputContentBlock::Text {
                    text: "api_key=sk-1234567890abcdef".to_string(),
                },
                super::OutputContentBlock::ToolUse {
                    id: "call-secret".to_string(),
                    name: "example".to_string(),
                    input: serde_json::json!({
                        "url": "https://example.test/?token=query-secret-value"
                    }),
                },
            ],
            model: "test-model".to_string(),
            stop_reason: None,
            stop_sequence: None,
            usage: Usage::default(),
            request_id: None,
            provider_metadata: None,
        };

        let (protected, report) =
            response.protect_sensitive_content(runtime::configured_data_protection_mode());
        let serialized = serde_json::to_string(&protected).unwrap();
        assert!(!serialized.contains("sk-1234567890abcdef"));
        assert!(!serialized.contains("query-secret-value"));
        assert!(report.finding_count >= 2);
    }

    #[test]
    fn streaming_response_event_redacts_visible_secret_delta() {
        let event = super::StreamEvent::ContentBlockDelta(super::ContentBlockDeltaEvent {
            index: 0,
            delta: super::ContentBlockDelta::TextDelta {
                text: "Authorization: Bearer opaque-token-123456".to_string(),
            },
        });
        let (protected, report) =
            event.protect_sensitive_content(runtime::configured_data_protection_mode());
        let serialized = serde_json::to_string(&protected).unwrap();
        assert!(!serialized.contains("opaque-token-123456"));
        assert!(report.redacted);
    }
}
