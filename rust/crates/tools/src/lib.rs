use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use api::{
    max_tokens_for_model, resolve_model_alias, ApiError, ContentBlockDelta, InputContentBlock,
    InputMessage, MessageRequest, MessageResponse, OutputContentBlock, ProviderClient,
    StreamEvent as ApiStreamEvent, ToolChoice, ToolDefinition, ToolResultContentBlock,
};
use plugins::PluginTool;
use reqwest::blocking::Client;
use runtime::{
    check_freshness, dedupe_superseded_commit_events, edit_file, execute_bash, glob_search,
    grep_search, load_system_prompt,
    lsp_client::LspRegistry,
    mcp_tool_bridge::McpToolRegistry,
    permission_enforcer::{EnforcementResult, PermissionEnforcer},
    read_file,
    summary_compression::compress_summary_text,
    task_registry::TaskRegistry,
    team_cron_registry::{CronRegistry, TeamRegistry},
    worker_boot::{WorkerReadySnapshot, WorkerRegistry, WorkerTaskReceipt},
    write_file, ApiClient, ApiRequest, AssistantEvent, BashCommandInput, BashCommandOutput,
    BranchFreshness, ConfigLoader, ContentBlock, ConversationMessage, ConversationRuntime,
    GrepSearchInput, LaneCommitProvenance, LaneEvent, LaneEventBlocker, LaneEventName,
    LaneEventStatus, LaneFailureClass, McpDegradedReport, MessageRole, PermissionMode,
    PermissionPolicy, PromptCacheEvent, ProviderFallbackConfig, RuntimeError, Session, TaskPacket,
    ToolError, ToolExecutor,
};
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Global task registry shared across tool invocations within a session.
fn global_lsp_registry() -> &'static LspRegistry {
    use std::sync::OnceLock;
    static REGISTRY: OnceLock<LspRegistry> = OnceLock::new();
    REGISTRY.get_or_init(LspRegistry::new)
}

fn global_mcp_registry() -> &'static McpToolRegistry {
    use std::sync::OnceLock;
    static REGISTRY: OnceLock<McpToolRegistry> = OnceLock::new();
    REGISTRY.get_or_init(McpToolRegistry::new)
}

fn global_team_registry() -> &'static TeamRegistry {
    use std::sync::OnceLock;
    static REGISTRY: OnceLock<TeamRegistry> = OnceLock::new();
    REGISTRY.get_or_init(TeamRegistry::new)
}

fn global_cron_registry() -> &'static CronRegistry {
    use std::sync::OnceLock;
    static REGISTRY: OnceLock<CronRegistry> = OnceLock::new();
    REGISTRY.get_or_init(CronRegistry::new)
}

fn global_task_registry() -> &'static TaskRegistry {
    use std::sync::OnceLock;
    static REGISTRY: OnceLock<TaskRegistry> = OnceLock::new();
    REGISTRY.get_or_init(TaskRegistry::new)
}

fn global_worker_registry() -> &'static WorkerRegistry {
    use std::sync::OnceLock;
    static REGISTRY: OnceLock<WorkerRegistry> = OnceLock::new();
    REGISTRY.get_or_init(WorkerRegistry::new)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolManifestEntry {
    pub name: String,
    pub source: ToolSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolSource {
    Base,
    Conditional,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolRegistry {
    entries: Vec<ToolManifestEntry>,
}

impl ToolRegistry {
    #[must_use]
    pub fn new(entries: Vec<ToolManifestEntry>) -> Self {
        Self { entries }
    }

    #[must_use]
    pub fn entries(&self) -> &[ToolManifestEntry] {
        &self.entries
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: Value,
    pub required_permission: PermissionMode,
}

#[derive(Debug, Clone)]
pub struct GlobalToolRegistry {
    plugin_tools: Vec<PluginTool>,
    runtime_tools: Vec<RuntimeToolDefinition>,
    enforcer: Option<PermissionEnforcer>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeToolDefinition {
    pub name: String,
    pub description: Option<String>,
    pub input_schema: Value,
    pub required_permission: PermissionMode,
}

impl GlobalToolRegistry {
    #[must_use]
    pub fn builtin() -> Self {
        Self {
            plugin_tools: Vec::new(),
            runtime_tools: Vec::new(),
            enforcer: None,
        }
    }

    pub fn with_plugin_tools(plugin_tools: Vec<PluginTool>) -> Result<Self, String> {
        let builtin_names = mvp_tool_specs()
            .into_iter()
            .map(|spec| spec.name.to_string())
            .collect::<BTreeSet<_>>();
        let mut seen_plugin_names = BTreeSet::new();

        for tool in &plugin_tools {
            let name = tool.definition().name.clone();
            if builtin_names.contains(&name) {
                return Err(format!(
                    "plugin tool `{name}` conflicts with a built-in tool name"
                ));
            }
            if !seen_plugin_names.insert(name.clone()) {
                return Err(format!("duplicate plugin tool name `{name}`"));
            }
        }

        Ok(Self {
            plugin_tools,
            runtime_tools: Vec::new(),
            enforcer: None,
        })
    }

    pub fn with_runtime_tools(
        mut self,
        runtime_tools: Vec<RuntimeToolDefinition>,
    ) -> Result<Self, String> {
        let mut seen_names = mvp_tool_specs()
            .into_iter()
            .map(|spec| spec.name.to_string())
            .chain(
                self.plugin_tools
                    .iter()
                    .map(|tool| tool.definition().name.clone()),
            )
            .chain(self.runtime_tools.iter().map(|tool| tool.name.clone()))
            .collect::<BTreeSet<_>>();

        for tool in &runtime_tools {
            if !seen_names.insert(tool.name.clone()) {
                return Err(format!(
                    "runtime tool `{}` conflicts with an existing tool name",
                    tool.name
                ));
            }
        }

        self.runtime_tools.extend(runtime_tools);
        Ok(self)
    }

    #[must_use]
    pub fn with_enforcer(mut self, enforcer: PermissionEnforcer) -> Self {
        self.set_enforcer(enforcer);
        self
    }

    pub fn normalize_allowed_tools(
        &self,
        values: &[String],
    ) -> Result<Option<BTreeSet<String>>, String> {
        if values.is_empty() {
            return Ok(None);
        }

        let actual_names = self.actual_tool_names();
        let canonical_names = self.canonical_allowed_tool_names();
        let canonical_name_set = canonical_names.iter().cloned().collect::<BTreeSet<_>>();
        let mut name_map = BTreeMap::new();
        for actual in &actual_names {
            let canonical = canonical_allowed_tool_name(actual);
            name_map.insert(allowed_tool_lookup_key(actual), canonical.clone());
            name_map.insert(allowed_tool_lookup_key(&canonical), canonical);
        }

        for (alias, canonical) in self.allowed_tool_aliases() {
            if canonical_name_set.contains(&canonical) {
                name_map.insert(allowed_tool_lookup_key(&alias), canonical);
            }
        }

        let mut allowed = BTreeSet::new();
        for value in values {
            for token in value
                .split(|ch: char| ch == ',' || ch.is_whitespace())
                .filter(|token| !token.is_empty())
            {
                let canonical = name_map.get(&allowed_tool_lookup_key(token)).ok_or_else(|| {
                    format!(
                        "invalid_tool_name: unsupported tool in --allowedTools: {token}\nAvailable: {}\nAliases: {}\nHint: Use canonical snake_case tool names from Available or aliases from Aliases.",
                        canonical_names.join(", "),
                        format_allowed_tool_aliases(&self.allowed_tool_aliases())
                    )
                })?;
                allowed.insert(canonical.clone());
            }
        }

        if allowed.is_empty() {
            return Err(format!(
                "--allowedTools was provided with no usable tool names (got `{}`). Omit the flag to allow all tools.",
                values.join(" ")
            ));
        }

        Ok(Some(allowed))
    }

    #[must_use]
    pub fn definitions(&self, allowed_tools: Option<&BTreeSet<String>>) -> Vec<ToolDefinition> {
        let builtin = mvp_tool_specs()
            .into_iter()
            .filter(|spec| {
                allowed_tools
                    .is_none_or(|allowed| allowed.contains(&canonical_allowed_tool_name(spec.name)))
            })
            .map(|spec| ToolDefinition {
                name: spec.name.to_string(),
                description: Some(spec.description.to_string()),
                input_schema: spec.input_schema,
            });
        let runtime = self
            .runtime_tools
            .iter()
            .filter(|tool| {
                allowed_tools.is_none_or(|allowed| {
                    allowed.contains(&canonical_allowed_tool_name(&tool.name))
                })
            })
            .map(|tool| ToolDefinition {
                name: tool.name.clone(),
                description: tool.description.clone(),
                input_schema: tool.input_schema.clone(),
            });
        let plugin = self
            .plugin_tools
            .iter()
            .filter(|tool| {
                allowed_tools.is_none_or(|allowed| {
                    allowed.contains(&canonical_allowed_tool_name(
                        tool.definition().name.as_str(),
                    ))
                })
            })
            .map(|tool| ToolDefinition {
                name: tool.definition().name.clone(),
                description: tool.definition().description.clone(),
                input_schema: tool.definition().input_schema.clone(),
            });
        builtin.chain(runtime).chain(plugin).collect()
    }

    pub fn permission_specs(
        &self,
        allowed_tools: Option<&BTreeSet<String>>,
    ) -> Result<Vec<(String, PermissionMode)>, String> {
        let builtin = mvp_tool_specs()
            .into_iter()
            .filter(|spec| {
                allowed_tools
                    .is_none_or(|allowed| allowed.contains(&canonical_allowed_tool_name(spec.name)))
            })
            .map(|spec| (spec.name.to_string(), spec.required_permission));
        let runtime = self
            .runtime_tools
            .iter()
            .filter(|tool| {
                allowed_tools.is_none_or(|allowed| {
                    allowed.contains(&canonical_allowed_tool_name(&tool.name))
                })
            })
            .map(|tool| (tool.name.clone(), tool.required_permission));
        let plugin = self
            .plugin_tools
            .iter()
            .filter(|tool| {
                allowed_tools.is_none_or(|allowed| {
                    allowed.contains(&canonical_allowed_tool_name(
                        tool.definition().name.as_str(),
                    ))
                })
            })
            .map(|tool| {
                permission_mode_from_plugin(tool.required_permission())
                    .map(|permission| (tool.definition().name.clone(), permission))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(builtin.chain(runtime).chain(plugin).collect())
    }

    #[must_use]
    pub fn actual_tool_names(&self) -> Vec<String> {
        mvp_tool_specs()
            .iter()
            .map(|spec| spec.name.to_string())
            .chain(
                self.plugin_tools
                    .iter()
                    .map(|tool| tool.definition().name.clone()),
            )
            .chain(self.runtime_tools.iter().map(|tool| tool.name.clone()))
            .collect()
    }

    #[must_use]
    pub fn canonical_allowed_tool_names(&self) -> Vec<String> {
        self.actual_tool_names()
            .into_iter()
            .map(|name| canonical_allowed_tool_name(&name))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    #[must_use]
    pub fn allowed_tool_aliases(&self) -> BTreeMap<String, String> {
        let mut aliases = BTreeMap::from([
            ("read".to_string(), "read_file".to_string()),
            ("Read".to_string(), "read_file".to_string()),
            ("write".to_string(), "write_file".to_string()),
            ("Write".to_string(), "write_file".to_string()),
            ("edit".to_string(), "edit_file".to_string()),
            ("Edit".to_string(), "edit_file".to_string()),
            ("glob".to_string(), "glob_search".to_string()),
            ("Glob".to_string(), "glob_search".to_string()),
            ("grep".to_string(), "grep_search".to_string()),
            ("Grep".to_string(), "grep_search".to_string()),
        ]);
        for actual in self.actual_tool_names() {
            let canonical = canonical_allowed_tool_name(&actual);
            if actual != canonical {
                aliases.insert(actual, canonical);
            }
        }
        aliases
    }

    #[must_use]
    pub fn has_runtime_tool(&self, name: &str) -> bool {
        self.runtime_tools.iter().any(|tool| tool.name == name)
    }

    #[must_use]
    pub fn search(
        &self,
        query: &str,
        max_results: usize,
        pending_mcp_servers: Option<Vec<String>>,
        mcp_degraded: Option<McpDegradedReport>,
    ) -> ToolSearchOutput {
        let query = query.trim().to_string();
        let normalized_query = normalize_tool_search_query(&query);
        let matches = search_tool_specs(&query, max_results.max(1), &self.searchable_tool_specs());

        ToolSearchOutput {
            matches,
            query,
            normalized_query,
            total_deferred_tools: self.searchable_tool_specs().len(),
            pending_mcp_servers,
            mcp_degraded,
        }
    }

    pub fn set_enforcer(&mut self, enforcer: PermissionEnforcer) {
        self.enforcer = Some(enforcer);
    }

    pub fn execute(&self, name: &str, input: &Value) -> Result<String, String> {
        if mvp_tool_specs().iter().any(|spec| spec.name == name) {
            return execute_tool_with_enforcer(self.enforcer.as_ref(), name, input);
        }
        if let Some(plugin) = self
            .plugin_tools
            .iter()
            .find(|tool| tool.definition().name == name)
        {
            return plugin.execute(input).map_err(|error| error.to_string());
        }
        if let Some(canonical) = canonical_builtin_execution_name(name) {
            return execute_tool_with_enforcer(self.enforcer.as_ref(), canonical, input);
        }
        Err(format!("unsupported tool: {name}"))
    }

    fn searchable_tool_specs(&self) -> Vec<SearchableToolSpec> {
        let builtin = deferred_tool_specs()
            .into_iter()
            .map(|spec| SearchableToolSpec {
                name: spec.name.to_string(),
                description: spec.description.to_string(),
            });
        let runtime = self.runtime_tools.iter().map(|tool| SearchableToolSpec {
            name: tool.name.clone(),
            description: tool.description.clone().unwrap_or_default(),
        });
        let plugin = self.plugin_tools.iter().map(|tool| SearchableToolSpec {
            name: tool.definition().name.clone(),
            description: tool.definition().description.clone().unwrap_or_default(),
        });
        builtin.chain(runtime).chain(plugin).collect()
    }
}

pub fn canonical_allowed_tool_name(value: &str) -> String {
    let trimmed = value.trim().replace('-', "_");
    let mut output = String::new();
    let chars = trimmed.chars().collect::<Vec<_>>();
    for (index, ch) in chars.iter().copied().enumerate() {
        if ch == '_' || ch.is_whitespace() {
            output.push('_');
            continue;
        }
        let previous = index.checked_sub(1).and_then(|i| chars.get(i)).copied();
        let next = chars.get(index + 1).copied();
        if ch.is_ascii_uppercase()
            && index > 0
            && !output.ends_with('_')
            && (previous.is_some_and(|p| p.is_ascii_lowercase() || p.is_ascii_digit())
                || next.is_some_and(|n| n.is_ascii_lowercase()))
        {
            output.push('_');
        }
        output.push(ch.to_ascii_lowercase());
    }
    output.trim_matches('_').to_string()
}

fn canonical_builtin_execution_name(value: &str) -> Option<&'static str> {
    let canonical = canonical_allowed_tool_name(value);
    mvp_tool_specs()
        .into_iter()
        .find(|spec| canonical_allowed_tool_name(spec.name) == canonical)
        .map(|spec| spec.name)
}

fn allowed_tool_lookup_key(value: &str) -> String {
    canonical_allowed_tool_name(value).replace('_', "")
}

fn format_allowed_tool_aliases(aliases: &BTreeMap<String, String>) -> String {
    aliases
        .iter()
        .map(|(alias, canonical)| format!("{alias}={canonical}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn permission_mode_from_plugin(value: &str) -> Result<PermissionMode, String> {
    match value {
        "read-only" => Ok(PermissionMode::ReadOnly),
        "workspace-write" => Ok(PermissionMode::WorkspaceWrite),
        "danger-full-access" => Ok(PermissionMode::DangerFullAccess),
        other => Err(format!("unsupported plugin permission: {other}")),
    }
}

#[must_use]
#[allow(clippy::too_many_lines)]
pub fn mvp_tool_specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "bash",
            description: "Execute a shell command in the current workspace.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string" },
                    "timeout": { "type": "integer", "minimum": 1 },
                    "description": { "type": "string" },
                    "run_in_background": { "type": "boolean" },
                    "dangerouslyDisableSandbox": { "type": "boolean" },
                    "namespaceRestrictions": { "type": "boolean" },
                    "isolateNetwork": { "type": "boolean" },
                    "filesystemMode": { "type": "string", "enum": ["off", "workspace-only", "allow-list"] },
                    "allowedMounts": { "type": "array", "items": { "type": "string" } }
                },
                "required": ["command"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::DangerFullAccess,
        },
        ToolSpec {
            name: "read_file",
            description: "Read a text file from the workspace.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "offset": { "type": "integer", "minimum": 0 },
                    "limit": { "type": "integer", "minimum": 1 }
                },
                "required": ["path"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::ReadOnly,
        },
        ToolSpec {
            name: "write_file",
            description: "Write a text file in the workspace.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "content": { "type": "string" }
                },
                "required": ["path", "content"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::WorkspaceWrite,
        },
        ToolSpec {
            name: "edit_file",
            description: "Replace text in a workspace file.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "old_string": { "type": "string" },
                    "new_string": { "type": "string" },
                    "replace_all": { "type": "boolean" }
                },
                "required": ["path", "old_string", "new_string"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::WorkspaceWrite,
        },
        ToolSpec {
            name: "glob_search",
            description: "Find files by glob pattern.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string" },
                    "path": { "type": "string" }
                },
                "required": ["pattern"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::ReadOnly,
        },
        ToolSpec {
            name: "grep_search",
            description: "Search file contents with a regex pattern.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string" },
                    "path": { "type": "string" },
                    "glob": { "type": "string" },
                    "output_mode": { "type": "string" },
                    "-B": { "type": "integer", "minimum": 0 },
                    "-A": { "type": "integer", "minimum": 0 },
                    "-C": { "type": "integer", "minimum": 0 },
                    "context": { "type": "integer", "minimum": 0 },
                    "-n": { "type": "boolean" },
                    "-i": { "type": "boolean" },
                    "type": { "type": "string" },
                    "head_limit": { "type": "integer", "minimum": 1 },
                    "offset": { "type": "integer", "minimum": 0 },
                    "multiline": { "type": "boolean" }
                },
                "required": ["pattern"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::ReadOnly,
        },
        ToolSpec {
            name: "WebFetch",
            description:
                "Fetch a URL, convert it into readable text, and answer a prompt about it.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "url": { "type": "string", "format": "uri" },
                    "prompt": { "type": "string" }
                },
                "required": ["url", "prompt"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::ReadOnly,
        },
        ToolSpec {
            name: "WebSearch",
            description: "Search the web for current information and return cited results.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "minLength": 2 },
                    "allowed_domains": {
                        "type": "array",
                        "items": { "type": "string" }
                    },
                    "blocked_domains": {
                        "type": "array",
                        "items": { "type": "string" }
                    }
                },
                "required": ["query"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::ReadOnly,
        },
        ToolSpec {
            name: "TodoWrite",
            description: "Update the structured task list for the current session.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "todos": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "content": { "type": "string" },
                                "activeForm": { "type": "string" },
                                "status": {
                                    "type": "string",
                                    "enum": ["pending", "in_progress", "completed"]
                                }
                            },
                            "required": ["content", "activeForm", "status"],
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["todos"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::WorkspaceWrite,
        },
        ToolSpec {
            name: "Skill",
            description: "Load a local skill definition and its instructions.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "skill": { "type": "string" },
                    "args": { "type": "string" }
                },
                "required": ["skill"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::ReadOnly,
        },
        ToolSpec {
            name: "Agent",
            description: "Launch a specialized agent task and persist its handoff metadata.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "description": { "type": "string" },
                    "prompt": { "type": "string" },
                    "subagent_type": { "type": "string" },
                    "name": { "type": "string" },
                    "model": { "type": "string" }
                },
                "required": ["description", "prompt"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::DangerFullAccess,
        },
        ToolSpec {
            name: "ToolSearch",
            description: "Search for deferred or specialized tools by exact name or keywords.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "max_results": { "type": "integer", "minimum": 1 }
                },
                "required": ["query"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::ReadOnly,
        },
        ToolSpec {
            name: "NotebookEdit",
            description: "Replace, insert, or delete a cell in a Jupyter notebook.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "notebook_path": { "type": "string" },
                    "cell_id": { "type": "string" },
                    "new_source": { "type": "string" },
                    "cell_type": { "type": "string", "enum": ["code", "markdown"] },
                    "edit_mode": { "type": "string", "enum": ["replace", "insert", "delete"] }
                },
                "required": ["notebook_path"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::WorkspaceWrite,
        },
        ToolSpec {
            name: "Sleep",
            description: "Wait for a specified duration without holding a shell process.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "duration_ms": { "type": "integer", "minimum": 0 }
                },
                "required": ["duration_ms"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::ReadOnly,
        },
        ToolSpec {
            name: "SendUserMessage",
            description: "Send a message to the user.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "message": { "type": "string" },
                    "attachments": {
                        "type": "array",
                        "items": { "type": "string" }
                    },
                    "status": {
                        "type": "string",
                        "enum": ["normal", "proactive"]
                    }
                },
                "required": ["message", "status"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::ReadOnly,
        },
        ToolSpec {
            name: "Config",
            description: "Get or set AOS settings.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "setting": { "type": "string" },
                    "value": {
                        "type": ["string", "boolean", "number"]
                    }
                },
                "required": ["setting"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::WorkspaceWrite,
        },
        ToolSpec {
            name: "EnterPlanMode",
            description: "Enable a worktree-local planning mode override and remember the previous local setting for ExitPlanMode.",
            input_schema: json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
            required_permission: PermissionMode::WorkspaceWrite,
        },
        ToolSpec {
            name: "ExitPlanMode",
            description: "Restore or clear the worktree-local planning mode override created by EnterPlanMode.",
            input_schema: json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
            required_permission: PermissionMode::WorkspaceWrite,
        },
        ToolSpec {
            name: "StructuredOutput",
            description: "Return structured output in the requested format.",
            input_schema: json!({
                "type": "object",
                "additionalProperties": true
            }),
            required_permission: PermissionMode::ReadOnly,
        },
        ToolSpec {
            name: "REPL",
            description: "Execute code in a REPL-like subprocess.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "code": { "type": "string" },
                    "language": { "type": "string" },
                    "timeout_ms": { "type": "integer", "minimum": 1 }
                },
                "required": ["code", "language"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::DangerFullAccess,
        },
        ToolSpec {
            name: "PowerShell",
            description: "Execute a PowerShell command with optional timeout.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string" },
                    "timeout": { "type": "integer", "minimum": 1 },
                    "description": { "type": "string" },
                    "run_in_background": { "type": "boolean" }
                },
                "required": ["command"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::DangerFullAccess,
        },
        ToolSpec {
            name: "AskUserQuestion",
            description: "Ask the user a question and wait for their response.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "question": { "type": "string" },
                    "options": {
                        "type": "array",
                        "items": { "type": "string" }
                    }
                },
                "required": ["question"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::ReadOnly,
        },
        ToolSpec {
            name: "TaskCreate",
            description: "Create a background task that runs in a separate subprocess.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "prompt": { "type": "string" },
                    "description": { "type": "string" }
                },
                "required": ["prompt"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::DangerFullAccess,
        },
        ToolSpec {
            name: "RunTaskPacket",
            description: "Create a background task from a structured task packet.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "objective": { "type": "string" },
                    "scope": { "type": "string" },
                    "repo": { "type": "string" },
                    "branch_policy": { "type": "string" },
                    "acceptance_tests": {
                        "type": "array",
                        "items": { "type": "string" }
                    },
                    "commit_policy": { "type": "string" },
                    "reporting_contract": { "type": "string" },
                    "escalation_policy": { "type": "string" }
                },
                "required": [
                    "objective",
                    "scope",
                    "repo",
                    "branch_policy",
                    "acceptance_tests",
                    "commit_policy",
                    "reporting_contract",
                    "escalation_policy"
                ],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::DangerFullAccess,
        },
        ToolSpec {
            name: "TaskGet",
            description: "Get the status and details of a background task by ID.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "task_id": { "type": "string" }
                },
                "required": ["task_id"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::ReadOnly,
        },
        ToolSpec {
            name: "TaskList",
            description: "List all background tasks and their current status.",
            input_schema: json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
            required_permission: PermissionMode::ReadOnly,
        },
        ToolSpec {
            name: "TaskStop",
            description: "Stop a running background task by ID.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "task_id": { "type": "string" }
                },
                "required": ["task_id"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::DangerFullAccess,
        },
        ToolSpec {
            name: "TaskUpdate",
            description: "Send a message or update to a running background task.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "task_id": { "type": "string" },
                    "message": { "type": "string" }
                },
                "required": ["task_id", "message"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::DangerFullAccess,
        },
        ToolSpec {
            name: "TaskOutput",
            description: "Retrieve the output produced by a background task.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "task_id": { "type": "string" }
                },
                "required": ["task_id"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::ReadOnly,
        },
        ToolSpec {
            name: "WorkerCreate",
            description: "Create a coding worker boot session with trust-gate and prompt-delivery guards.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "cwd": { "type": "string" },
                    "trusted_roots": {
                        "type": "array",
                        "items": { "type": "string" }
                    },
                    "auto_recover_prompt_misdelivery": { "type": "boolean" }
                },
                "required": ["cwd"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::DangerFullAccess,
        },
        ToolSpec {
            name: "WorkerGet",
            description: "Fetch the current worker boot state, last error, and event history.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "worker_id": { "type": "string" }
                },
                "required": ["worker_id"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::ReadOnly,
        },
        ToolSpec {
            name: "WorkerObserve",
            description: "Feed a terminal snapshot into worker boot detection to resolve trust gates, ready handshakes, and prompt misdelivery.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "worker_id": { "type": "string" },
                    "screen_text": { "type": "string" }
                },
                "required": ["worker_id", "screen_text"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::ReadOnly,
        },
        ToolSpec {
            name: "WorkerResolveTrust",
            description: "Resolve a detected trust prompt so worker boot can continue.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "worker_id": { "type": "string" }
                },
                "required": ["worker_id"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::DangerFullAccess,
        },
        ToolSpec {
            name: "WorkerAwaitReady",
            description: "Return the current ready-handshake verdict for a coding worker.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "worker_id": { "type": "string" }
                },
                "required": ["worker_id"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::ReadOnly,
        },
        ToolSpec {
            name: "WorkerSendPrompt",
            description: "Send a task prompt only after the worker reaches ready_for_prompt; can replay a recovered prompt.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "worker_id": { "type": "string" },
                    "prompt": { "type": "string" },
                    "task_receipt": {
                        "type": "object",
                        "properties": {
                            "repo": { "type": "string" },
                            "task_kind": { "type": "string" },
                            "source_surface": { "type": "string" },
                            "expected_artifacts": {
                                "type": "array",
                                "items": { "type": "string" }
                            },
                            "objective_preview": { "type": "string" }
                        },
                        "required": ["repo", "task_kind", "source_surface", "objective_preview"],
                        "additionalProperties": false
                    }
                },
                "required": ["worker_id"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::DangerFullAccess,
        },
        ToolSpec {
            name: "WorkerRestart",
            description: "Restart worker boot state after a failed or stale startup.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "worker_id": { "type": "string" }
                },
                "required": ["worker_id"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::DangerFullAccess,
        },
        ToolSpec {
            name: "WorkerTerminate",
            description: "Terminate a worker and mark the lane finished from the control plane.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "worker_id": { "type": "string" }
                },
                "required": ["worker_id"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::DangerFullAccess,
        },
        ToolSpec {
            name: "WorkerObserveCompletion",
            description: "Report session completion to the worker, classifying finish_reason into Finished or Failed (provider-degraded). Use after the opencode session completes to advance the worker to its terminal state.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "worker_id": { "type": "string" },
                    "finish_reason": { "type": "string" },
                    "tokens_output": { "type": "integer", "minimum": 0 }
                },
                "required": ["worker_id", "finish_reason", "tokens_output"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::DangerFullAccess,
        },
        ToolSpec {
            name: "TeamCreate",
            description: "Create a team of sub-agents for parallel task execution.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string" },
                    "tasks": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "prompt": { "type": "string" },
                                "description": { "type": "string" }
                            },
                            "required": ["prompt"]
                        }
                    }
                },
                "required": ["name", "tasks"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::DangerFullAccess,
        },
        ToolSpec {
            name: "TeamDelete",
            description: "Delete a team and stop all its running tasks.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "team_id": { "type": "string" }
                },
                "required": ["team_id"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::DangerFullAccess,
        },
        ToolSpec {
            name: "CronCreate",
            description: "Create a scheduled recurring task.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "schedule": { "type": "string" },
                    "prompt": { "type": "string" },
                    "description": { "type": "string" }
                },
                "required": ["schedule", "prompt"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::DangerFullAccess,
        },
        ToolSpec {
            name: "CronDelete",
            description: "Delete a scheduled recurring task by ID.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "cron_id": { "type": "string" }
                },
                "required": ["cron_id"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::DangerFullAccess,
        },
        ToolSpec {
            name: "CronList",
            description: "List all scheduled recurring tasks.",
            input_schema: json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
            required_permission: PermissionMode::ReadOnly,
        },
        ToolSpec {
            name: "LSP",
            description: "Query Language Server Protocol for code intelligence (symbols, references, diagnostics).",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["symbols", "references", "diagnostics", "definition", "hover"] },
                    "path": { "type": "string" },
                    "line": { "type": "integer", "minimum": 0 },
                    "character": { "type": "integer", "minimum": 0 },
                    "query": { "type": "string" }
                },
                "required": ["action"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::ReadOnly,
        },
        ToolSpec {
            name: "ListMcpResources",
            description: "List available resources from connected MCP servers.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "server": { "type": "string" }
                },
                "additionalProperties": false
            }),
            required_permission: PermissionMode::ReadOnly,
        },
        ToolSpec {
            name: "ReadMcpResource",
            description: "Read a specific resource from an MCP server by URI.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "server": { "type": "string" },
                    "uri": { "type": "string" }
                },
                "required": ["uri"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::ReadOnly,
        },
        ToolSpec {
            name: "McpAuth",
            description: "Authenticate with an MCP server that requires OAuth or credentials.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "server": { "type": "string" }
                },
                "required": ["server"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::DangerFullAccess,
        },
        ToolSpec {
            name: "RemoteTrigger",
            description: "Trigger a remote action or webhook endpoint.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "url": { "type": "string" },
                    "method": { "type": "string", "enum": ["GET", "POST", "PUT", "DELETE"] },
                    "headers": { "type": "object" },
                    "body": { "type": "string" }
                },
                "required": ["url"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::DangerFullAccess,
        },
        ToolSpec {
            name: "MCP",
            description: "Execute a tool provided by a connected MCP server.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "server": { "type": "string" },
                    "tool": { "type": "string" },
                    "arguments": { "type": "object" }
                },
                "required": ["server", "tool"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::DangerFullAccess,
        },
        ToolSpec {
            name: "TestingPermission",
            description: "Test-only tool for verifying permission enforcement behavior.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string" }
                },
                "required": ["action"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::DangerFullAccess,
        },
        ToolSpec {
            name: "GitStatus",
            description: "Show the working tree status (branch, staged, unstaged, untracked). Equivalent to 'git status --short --branch'. Use this instead of running git status via bash to get structured, parseable output.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "short": { "type": "boolean" }
                },
                "additionalProperties": false
            }),
            required_permission: PermissionMode::ReadOnly,
        },
        ToolSpec {
            name: "GitDiff",
            description: "Show changes between commits, the index, and the working tree. Supports staged changes ('git diff --cached'), specific paths, commit ranges, and comparing two commits. Use this instead of running git diff via bash to get structured output.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "staged": { "type": "boolean" },
                    "commit": { "type": "string" },
                    "commit2": { "type": "string" }
                },
                "additionalProperties": false
            }),
            required_permission: PermissionMode::ReadOnly,
        },
        ToolSpec {
            name: "GitLog",
            description: "Show commit history. Supports limiting count, filtering by author/date/path, and oneline format. Defaults to the last 20 commits. Use this instead of running git log via bash to get structured output.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "count": { "type": "integer", "minimum": 1 },
                    "oneline": { "type": "boolean" },
                    "author": { "type": "string" },
                    "since": { "type": "string" },
                    "until": { "type": "string" }
                },
                "additionalProperties": false
            }),
            required_permission: PermissionMode::ReadOnly,
        },
        ToolSpec {
            name: "GitShow",
            description: "Show a commit, tag, or tree object. Use format to control output: patch (default) shows the full diff, stat shows a diffstat summary, and metadata shows commit info without the diff. Supports showing a specific file at a commit (commit:path) for patch/stat output. Use this instead of running git show via bash to get structured output.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "commit": { "type": "string" },
                    "path": { "type": "string" },
                    "stat": { "type": "boolean" },
                    "format": { "type": "string", "enum": ["patch", "stat", "metadata"] }
                },
                "required": ["commit"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::ReadOnly,
        },
        ToolSpec {
            name: "GitBlame",
            description: "Show what revision and author last modified each line of a file. Supports line range filtering (start_line, end_line). Use this instead of running git blame via bash to get structured output.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "start_line": { "type": "integer", "minimum": 1 },
                    "end_line": { "type": "integer", "minimum": 1 }
                },
                "required": ["path"],
                "additionalProperties": false
            }),
            required_permission: PermissionMode::ReadOnly,
        },
    ]
}

/// Check permission before executing a tool. Returns Err with denial reason if blocked.
pub fn enforce_permission_check(
    enforcer: &PermissionEnforcer,
    tool_name: &str,
    input: &Value,
) -> Result<(), String> {
    let input_str = serde_json::to_string(input).unwrap_or_default();
    let result = enforcer.check(tool_name, &input_str);

    match result {
        EnforcementResult::Allowed => Ok(()),
        EnforcementResult::Denied { reason, .. } => Err(reason),
    }
}

pub fn execute_tool(name: &str, input: &Value) -> Result<String, String> {
    execute_tool_with_enforcer(None, name, input)
}

#[derive(Debug, Clone)]
struct WebSearchProviderOverride {
    providers: Vec<WebSearchProviderConfig>,
    include_builtin_fallback: bool,
}

thread_local! {
    static WEB_SEARCH_PROVIDER_OVERRIDE: RefCell<Option<WebSearchProviderOverride>> = const { RefCell::new(None) };
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WebSearchProviderType {
    Builtin,
    Brave,
    Tavily,
    Serper,
    Exa,
    Searxng,
    GenericJson,
    InternalHttp,
    DemoSearch,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebSearchProviderConfig {
    pub id: String,
    pub name: String,
    pub provider_type: WebSearchProviderType,
    pub enabled: bool,
    pub priority: i32,
    pub base_url: Option<String>,
    pub method: Option<String>,
    pub auth_type: Option<String>,
    pub auth_secret: Option<String>,
    pub headers_json: Option<Value>,
    pub query_template_json: Option<Value>,
    pub response_mapping_json: Option<Value>,
    pub timeout_secs: Option<u64>,
    pub max_results: Option<usize>,
    pub fetch_content_enabled: Option<bool>,
    pub content_extract_mode: Option<String>,
    pub domain_allowlist: Option<Vec<String>>,
    pub domain_blocklist: Option<Vec<String>>,
    pub rate_limit_json: Option<Value>,
}

impl WebSearchProviderConfig {
    #[must_use]
    pub fn builtin(max_results: Option<usize>) -> Self {
        Self {
            id: "aos_builtin_web_search".to_string(),
            name: "AOS Built-in Web Search".to_string(),
            provider_type: WebSearchProviderType::Builtin,
            enabled: true,
            priority: i32::MAX,
            base_url: None,
            method: Some("GET".to_string()),
            auth_type: Some("none".to_string()),
            auth_secret: None,
            headers_json: None,
            query_template_json: None,
            response_mapping_json: None,
            timeout_secs: None,
            max_results,
            fetch_content_enabled: Some(true),
            content_extract_mode: Some("auto".to_string()),
            domain_allowlist: None,
            domain_blocklist: None,
            rate_limit_json: None,
        }
    }
}

pub fn with_web_search_provider_override<T>(
    providers: Vec<WebSearchProviderConfig>,
    f: impl FnOnce() -> T,
) -> T {
    with_web_search_provider_override_mode(providers, true, f)
}

pub fn with_strict_web_search_provider_override<T>(
    providers: Vec<WebSearchProviderConfig>,
    f: impl FnOnce() -> T,
) -> T {
    with_web_search_provider_override_mode(providers, false, f)
}

fn with_web_search_provider_override_mode<T>(
    providers: Vec<WebSearchProviderConfig>,
    include_builtin_fallback: bool,
    f: impl FnOnce() -> T,
) -> T {
    struct ResetGuard(Option<WebSearchProviderOverride>);
    impl Drop for ResetGuard {
        fn drop(&mut self) {
            let previous = self.0.take();
            WEB_SEARCH_PROVIDER_OVERRIDE.with(|slot| {
                *slot.borrow_mut() = previous;
            });
        }
    }

    let previous = WEB_SEARCH_PROVIDER_OVERRIDE.with(|slot| {
        slot.replace(Some(WebSearchProviderOverride {
            providers,
            include_builtin_fallback,
        }))
    });
    let _guard = ResetGuard(previous);
    f()
}

fn execute_tool_with_enforcer(
    enforcer: Option<&PermissionEnforcer>,
    name: &str,
    input: &Value,
) -> Result<String, String> {
    let name = canonical_builtin_execution_name(name).unwrap_or(name);
    match name {
        "bash" => {
            // Parse input to get the command for permission classification
            let bash_input: BashCommandInput = from_value(input)?;
            let classified_mode = classify_bash_permission(&bash_input.command);
            maybe_enforce_permission_check_with_mode(enforcer, name, input, classified_mode)?;
            run_bash(bash_input)
        }
        "read_file" => {
            let file_input: ReadFileInput = from_value(input)?;
            let required_mode = classify_read_path_permission(&file_input.path, false);
            maybe_enforce_permission_check_with_mode(enforcer, name, input, required_mode)?;
            run_read_file(file_input)
        }
        "write_file" => {
            let file_input: WriteFileInput = from_value(input)?;
            let required_mode = classify_file_path_permission(&file_input.path, true);
            maybe_enforce_permission_check_with_mode(enforcer, name, input, required_mode)?;
            run_write_file(file_input)
        }
        "edit_file" => {
            let file_input: EditFileInput = from_value(input)?;
            let required_mode = classify_file_path_permission(&file_input.path, false);
            maybe_enforce_permission_check_with_mode(enforcer, name, input, required_mode)?;
            run_edit_file(file_input)
        }
        "glob_search" => {
            let glob_input: GlobSearchInputValue = from_value(input)?;
            let required_mode = classify_glob_permission(&glob_input);
            maybe_enforce_permission_check_with_mode(enforcer, name, input, required_mode)?;
            run_glob_search(glob_input)
        }
        "grep_search" => {
            let grep_input: GrepSearchInput = from_value(input)?;
            let required_mode = classify_grep_permission(&grep_input);
            maybe_enforce_permission_check_with_mode(enforcer, name, input, required_mode)?;
            run_grep_search(grep_input)
        }
        "WebFetch" => {
            let web_input = from_value::<WebFetchInput>(input)?;
            maybe_enforce_permission_check_with_mode(
                enforcer,
                name,
                input,
                PermissionMode::DangerFullAccess,
            )?;
            run_web_fetch(web_input)
        }
        "WebSearch" => {
            let web_input = from_value::<WebSearchInput>(input)?;
            maybe_enforce_permission_check_with_mode(
                enforcer,
                name,
                input,
                PermissionMode::DangerFullAccess,
            )?;
            run_web_search(web_input)
        }
        "TodoWrite" => from_value::<TodoWriteInput>(input).and_then(run_todo_write),
        "Skill" => from_value::<SkillInput>(input).and_then(run_skill),
        "Agent" => from_value::<AgentInput>(input).and_then(run_agent),
        "ToolSearch" => from_value::<ToolSearchInput>(input).and_then(run_tool_search),
        "NotebookEdit" => from_value::<NotebookEditInput>(input).and_then(run_notebook_edit),
        "Sleep" => from_value::<SleepInput>(input).and_then(run_sleep),
        "SendUserMessage" | "Brief" => from_value::<BriefInput>(input).and_then(run_brief),
        "Config" => from_value::<ConfigInput>(input).and_then(run_config),
        "EnterPlanMode" => from_value::<EnterPlanModeInput>(input).and_then(run_enter_plan_mode),
        "ExitPlanMode" => from_value::<ExitPlanModeInput>(input).and_then(run_exit_plan_mode),
        "StructuredOutput" => {
            from_value::<StructuredOutputInput>(input).and_then(run_structured_output)
        }
        "REPL" => from_value::<ReplInput>(input).and_then(run_repl),
        "PowerShell" => {
            // Parse input to get the command for permission classification
            let ps_input: PowerShellInput = from_value(input)?;
            let classified_mode = classify_powershell_permission(&ps_input.command);
            maybe_enforce_permission_check_with_mode(enforcer, name, input, classified_mode)?;
            run_powershell(ps_input)
        }
        "AskUserQuestion" => {
            from_value::<AskUserQuestionInput>(input).and_then(run_ask_user_question)
        }
        "TaskCreate" => from_value::<TaskCreateInput>(input).and_then(run_task_create),
        "RunTaskPacket" => from_value::<TaskPacket>(input).and_then(run_task_packet),
        "TaskGet" => from_value::<TaskIdInput>(input).and_then(run_task_get),
        "TaskList" => run_task_list(input.clone()),
        "TaskStop" => from_value::<TaskIdInput>(input).and_then(run_task_stop),
        "TaskUpdate" => from_value::<TaskUpdateInput>(input).and_then(run_task_update),
        "TaskOutput" => from_value::<TaskIdInput>(input).and_then(run_task_output),
        "WorkerCreate" => from_value::<WorkerCreateInput>(input).and_then(run_worker_create),
        "WorkerGet" => from_value::<WorkerIdInput>(input).and_then(run_worker_get),
        "WorkerObserve" => from_value::<WorkerObserveInput>(input).and_then(run_worker_observe),
        "WorkerResolveTrust" => {
            from_value::<WorkerIdInput>(input).and_then(run_worker_resolve_trust)
        }
        "WorkerAwaitReady" => from_value::<WorkerIdInput>(input).and_then(run_worker_await_ready),
        "WorkerSendPrompt" => {
            from_value::<WorkerSendPromptInput>(input).and_then(run_worker_send_prompt)
        }
        "WorkerRestart" => from_value::<WorkerIdInput>(input).and_then(run_worker_restart),
        "WorkerTerminate" => from_value::<WorkerIdInput>(input).and_then(run_worker_terminate),
        "WorkerObserveCompletion" => from_value::<WorkerObserveCompletionInput>(input)
            .and_then(run_worker_observe_completion),
        "TeamCreate" => from_value::<TeamCreateInput>(input).and_then(run_team_create),
        "TeamDelete" => from_value::<TeamDeleteInput>(input).and_then(run_team_delete),
        "CronCreate" => from_value::<CronCreateInput>(input).and_then(run_cron_create),
        "CronDelete" => from_value::<CronDeleteInput>(input).and_then(run_cron_delete),
        "CronList" => run_cron_list(input.clone()),
        "LSP" => from_value::<LspInput>(input).and_then(run_lsp),
        "ListMcpResources" => {
            from_value::<McpResourceInput>(input).and_then(run_list_mcp_resources)
        }
        "ReadMcpResource" => from_value::<McpResourceInput>(input).and_then(run_read_mcp_resource),
        "McpAuth" => from_value::<McpAuthInput>(input).and_then(run_mcp_auth),
        "RemoteTrigger" => from_value::<RemoteTriggerInput>(input).and_then(run_remote_trigger),
        "MCP" => from_value::<McpToolInput>(input).and_then(run_mcp_tool),
        "TestingPermission" => {
            from_value::<TestingPermissionInput>(input).and_then(run_testing_permission)
        }
        "GitStatus" => from_value::<GitStatusInput>(input).and_then(run_git_status),
        "GitDiff" => from_value::<GitDiffInput>(input).and_then(run_git_diff),
        "GitLog" => from_value::<GitLogInput>(input).and_then(run_git_log),
        "GitShow" => from_value::<GitShowInput>(input).and_then(run_git_show),
        "GitBlame" => from_value::<GitBlameInput>(input).and_then(run_git_blame),
        _ => Err(format!("unsupported tool: {name}")),
    }
}

/// Enforce permission check with a dynamically classified permission mode.
/// Used for tools like bash and `PowerShell` where the required permission
/// depends on the actual command being executed.
fn maybe_enforce_permission_check_with_mode(
    enforcer: Option<&PermissionEnforcer>,
    tool_name: &str,
    input: &Value,
    required_mode: PermissionMode,
) -> Result<(), String> {
    if let Some(enforcer) = enforcer {
        let input_str = serde_json::to_string(input).unwrap_or_default();
        let result = enforcer.check_with_required_mode(tool_name, &input_str, required_mode);

        match result {
            EnforcementResult::Allowed => Ok(()),
            EnforcementResult::Denied { reason, .. } => Err(reason),
        }
    } else {
        Ok(())
    }
}

#[allow(clippy::needless_pass_by_value)]
fn run_ask_user_question(input: AskUserQuestionInput) -> Result<String, String> {
    use std::io::{self, BufRead, Write};

    // Display the question to the user via stdout
    let stdout = io::stdout();
    let stdin = io::stdin();
    let mut out = stdout.lock();

    writeln!(out, "\n[Question] {}", input.question).map_err(|e| e.to_string())?;

    if let Some(ref options) = input.options {
        for (i, option) in options.iter().enumerate() {
            writeln!(out, "  {}. {}", i + 1, option).map_err(|e| e.to_string())?;
        }
        write!(out, "Enter choice (1-{}): ", options.len()).map_err(|e| e.to_string())?;
    } else {
        write!(out, "Your answer: ").map_err(|e| e.to_string())?;
    }
    out.flush().map_err(|e| e.to_string())?;

    // Read user response from stdin
    let mut response = String::new();
    stdin
        .lock()
        .read_line(&mut response)
        .map_err(|e| e.to_string())?;
    let response = response.trim().to_string();

    // If options were provided, resolve the numeric choice
    let answer = if let Some(ref options) = input.options {
        if let Ok(idx) = response.parse::<usize>() {
            if idx >= 1 && idx <= options.len() {
                options[idx - 1].clone()
            } else {
                response.clone()
            }
        } else {
            response.clone()
        }
    } else {
        response.clone()
    };

    to_pretty_json(json!({
        "question": input.question,
        "answer": answer,
        "status": "answered"
    }))
}

#[allow(clippy::needless_pass_by_value)]
fn run_task_create(input: TaskCreateInput) -> Result<String, String> {
    let registry = global_task_registry();
    let task = registry.create(&input.prompt, input.description.as_deref());
    to_pretty_json(json!({
        "task_id": task.task_id,
        "status": task.status,
        "prompt": task.prompt,
        "description": task.description,
        "task_packet": task.task_packet,
        "created_at": task.created_at
    }))
}

#[allow(clippy::needless_pass_by_value)]
fn run_task_packet(input: TaskPacket) -> Result<String, String> {
    let registry = global_task_registry();
    let task = registry
        .create_from_packet(input)
        .map_err(|error| error.to_string())?;

    to_pretty_json(json!({
        "task_id": task.task_id,
        "status": task.status,
        "prompt": task.prompt,
        "description": task.description,
        "task_packet": task.task_packet,
        "created_at": task.created_at
    }))
}

#[allow(clippy::needless_pass_by_value)]
fn run_task_get(input: TaskIdInput) -> Result<String, String> {
    let registry = global_task_registry();
    match registry.get(&input.task_id) {
        Some(task) => to_pretty_json(json!({
            "task_id": task.task_id,
            "status": task.status,
            "prompt": task.prompt,
            "description": task.description,
            "task_packet": task.task_packet,
            "created_at": task.created_at,
            "updated_at": task.updated_at,
            "messages": task.messages,
            "team_id": task.team_id
        })),
        None => Err(format!("task not found: {}", input.task_id)),
    }
}

fn run_task_list(_input: Value) -> Result<String, String> {
    let registry = global_task_registry();
    let tasks: Vec<_> = registry
        .list(None)
        .into_iter()
        .map(|t| {
            json!({
                "task_id": t.task_id,
                "status": t.status,
                "prompt": t.prompt,
                "description": t.description,
                "task_packet": t.task_packet,
                "created_at": t.created_at,
                "updated_at": t.updated_at,
                "team_id": t.team_id
            })
        })
        .collect();
    to_pretty_json(json!({
        "tasks": tasks,
        "count": tasks.len()
    }))
}

#[allow(clippy::needless_pass_by_value)]
fn run_task_stop(input: TaskIdInput) -> Result<String, String> {
    let registry = global_task_registry();
    match registry.stop(&input.task_id) {
        Ok(task) => to_pretty_json(json!({
            "task_id": task.task_id,
            "status": task.status,
            "message": "Task stopped"
        })),
        Err(e) => Err(e),
    }
}

#[allow(clippy::needless_pass_by_value)]
fn run_task_update(input: TaskUpdateInput) -> Result<String, String> {
    let registry = global_task_registry();
    match registry.update(&input.task_id, &input.message) {
        Ok(task) => to_pretty_json(json!({
            "task_id": task.task_id,
            "status": task.status,
            "message_count": task.messages.len(),
            "last_message": input.message
        })),
        Err(e) => Err(e),
    }
}

#[allow(clippy::needless_pass_by_value)]
fn run_task_output(input: TaskIdInput) -> Result<String, String> {
    let registry = global_task_registry();
    match registry.output(&input.task_id) {
        Ok(output) => to_pretty_json(json!({
            "task_id": input.task_id,
            "output": output,
            "has_output": !output.is_empty()
        })),
        Err(e) => Err(e),
    }
}

#[allow(clippy::needless_pass_by_value)]
fn run_worker_create(input: WorkerCreateInput) -> Result<String, String> {
    // Merge config-level trusted_roots with per-call overrides.
    // Config provides the default allowlist; per-call roots add on top.
    let config_roots: Vec<String> = ConfigLoader::default_for(&input.cwd)
        .load()
        .ok()
        .map(|c| c.trusted_roots().to_vec())
        .unwrap_or_default();
    let merged_roots: Vec<String> = config_roots
        .into_iter()
        .chain(input.trusted_roots.iter().cloned())
        .collect();
    let worker = global_worker_registry().create(
        &input.cwd,
        &merged_roots,
        input.auto_recover_prompt_misdelivery,
    );
    to_pretty_json(worker)
}

#[allow(clippy::needless_pass_by_value)]
fn run_worker_get(input: WorkerIdInput) -> Result<String, String> {
    global_worker_registry().get(&input.worker_id).map_or_else(
        || Err(format!("worker not found: {}", input.worker_id)),
        to_pretty_json,
    )
}

#[allow(clippy::needless_pass_by_value)]
fn run_worker_observe(input: WorkerObserveInput) -> Result<String, String> {
    let worker = global_worker_registry().observe(&input.worker_id, &input.screen_text)?;
    to_pretty_json(worker)
}

#[allow(clippy::needless_pass_by_value)]
fn run_worker_resolve_trust(input: WorkerIdInput) -> Result<String, String> {
    let worker = global_worker_registry().resolve_trust(&input.worker_id)?;
    to_pretty_json(worker)
}

#[allow(clippy::needless_pass_by_value)]
fn run_worker_await_ready(input: WorkerIdInput) -> Result<String, String> {
    let snapshot: WorkerReadySnapshot = global_worker_registry().await_ready(&input.worker_id)?;
    to_pretty_json(snapshot)
}

#[allow(clippy::needless_pass_by_value)]
fn run_worker_send_prompt(input: WorkerSendPromptInput) -> Result<String, String> {
    let worker = global_worker_registry().send_prompt(
        &input.worker_id,
        input.prompt.as_deref(),
        input.task_receipt,
    )?;
    to_pretty_json(worker)
}

#[allow(clippy::needless_pass_by_value)]
fn run_worker_restart(input: WorkerIdInput) -> Result<String, String> {
    let worker = global_worker_registry().restart(&input.worker_id)?;
    to_pretty_json(worker)
}

#[allow(clippy::needless_pass_by_value)]
fn run_worker_terminate(input: WorkerIdInput) -> Result<String, String> {
    let worker = global_worker_registry().terminate(&input.worker_id)?;
    to_pretty_json(worker)
}

#[allow(clippy::needless_pass_by_value)]
fn run_worker_observe_completion(input: WorkerObserveCompletionInput) -> Result<String, String> {
    let worker = global_worker_registry().observe_completion(
        &input.worker_id,
        &input.finish_reason,
        input.tokens_output,
    )?;
    to_pretty_json(worker)
}

#[allow(clippy::needless_pass_by_value)]
fn run_team_create(input: TeamCreateInput) -> Result<String, String> {
    let task_ids: Vec<String> = input
        .tasks
        .iter()
        .filter_map(|t| t.get("task_id").and_then(|v| v.as_str()).map(str::to_owned))
        .collect();
    let team = global_team_registry().create(&input.name, task_ids);
    // Register team assignment on each task
    for task_id in &team.task_ids {
        let _ = global_task_registry().assign_team(task_id, &team.team_id);
    }
    to_pretty_json(json!({
        "team_id": team.team_id,
        "name": team.name,
        "task_count": team.task_ids.len(),
        "task_ids": team.task_ids,
        "status": team.status,
        "created_at": team.created_at
    }))
}

#[allow(clippy::needless_pass_by_value)]
fn run_team_delete(input: TeamDeleteInput) -> Result<String, String> {
    match global_team_registry().delete(&input.team_id) {
        Ok(team) => to_pretty_json(json!({
            "team_id": team.team_id,
            "name": team.name,
            "status": team.status,
            "message": "Team deleted"
        })),
        Err(e) => Err(e),
    }
}

#[allow(clippy::needless_pass_by_value)]
fn run_cron_create(input: CronCreateInput) -> Result<String, String> {
    let entry =
        global_cron_registry().create(&input.schedule, &input.prompt, input.description.as_deref());
    to_pretty_json(json!({
        "cron_id": entry.cron_id,
        "schedule": entry.schedule,
        "prompt": entry.prompt,
        "description": entry.description,
        "enabled": entry.enabled,
        "created_at": entry.created_at
    }))
}

#[allow(clippy::needless_pass_by_value)]
fn run_cron_delete(input: CronDeleteInput) -> Result<String, String> {
    match global_cron_registry().delete(&input.cron_id) {
        Ok(entry) => to_pretty_json(json!({
            "cron_id": entry.cron_id,
            "schedule": entry.schedule,
            "status": "deleted",
            "message": "Cron entry removed"
        })),
        Err(e) => Err(e),
    }
}

fn run_cron_list(_input: Value) -> Result<String, String> {
    let entries: Vec<_> = global_cron_registry()
        .list(false)
        .into_iter()
        .map(|e| {
            json!({
                "cron_id": e.cron_id,
                "schedule": e.schedule,
                "prompt": e.prompt,
                "description": e.description,
                "enabled": e.enabled,
                "run_count": e.run_count,
                "last_run_at": e.last_run_at,
                "created_at": e.created_at
            })
        })
        .collect();
    to_pretty_json(json!({
        "crons": entries,
        "count": entries.len()
    }))
}

#[allow(clippy::needless_pass_by_value)]
fn run_lsp(input: LspInput) -> Result<String, String> {
    let registry = global_lsp_registry();
    let action = &input.action;
    let path = input.path.as_deref();
    let line = input.line;
    let character = input.character;
    let query = input.query.as_deref();

    match registry.dispatch(action, path, line, character, query) {
        Ok(result) => to_pretty_json(result),
        Err(e) => to_pretty_json(json!({
            "action": action,
            "error": e,
            "status": "error"
        })),
    }
}

#[allow(clippy::needless_pass_by_value)]
fn run_list_mcp_resources(input: McpResourceInput) -> Result<String, String> {
    let registry = global_mcp_registry();
    let server = input.server.as_deref().unwrap_or("default");
    match registry.list_resources(server) {
        Ok(resources) => {
            let items: Vec<_> = resources
                .iter()
                .map(|r| {
                    json!({
                        "uri": r.uri,
                        "name": r.name,
                        "description": r.description,
                        "mime_type": r.mime_type,
                    })
                })
                .collect();
            to_pretty_json(json!({
                "server": server,
                "resources": items,
                "count": items.len()
            }))
        }
        Err(e) => to_pretty_json(json!({
            "server": server,
            "resources": [],
            "error": e
        })),
    }
}

#[allow(clippy::needless_pass_by_value)]
fn run_read_mcp_resource(input: McpResourceInput) -> Result<String, String> {
    let registry = global_mcp_registry();
    let uri = input.uri.as_deref().unwrap_or("");
    let server = input.server.as_deref().unwrap_or("default");
    match registry.read_resource(server, uri) {
        Ok(resource) => to_pretty_json(json!({
            "server": server,
            "uri": resource.uri,
            "name": resource.name,
            "description": resource.description,
            "mime_type": resource.mime_type
        })),
        Err(e) => to_pretty_json(json!({
            "server": server,
            "uri": uri,
            "error": e
        })),
    }
}

#[allow(clippy::needless_pass_by_value)]
fn run_mcp_auth(input: McpAuthInput) -> Result<String, String> {
    let registry = global_mcp_registry();
    match registry.get_server(&input.server) {
        Some(state) => to_pretty_json(json!({
            "server": input.server,
            "status": state.status,
            "server_info": state.server_info,
            "tool_count": state.tools.len(),
            "resource_count": state.resources.len()
        })),
        None => to_pretty_json(json!({
            "server": input.server,
            "status": "disconnected",
            "message": "Server not registered. Use MCP tool to connect first."
        })),
    }
}

#[allow(clippy::needless_pass_by_value)]
fn run_remote_trigger(input: RemoteTriggerInput) -> Result<String, String> {
    let method = input.method.unwrap_or_else(|| "GET".to_string());
    let client = Client::new();

    let mut request = match method.to_uppercase().as_str() {
        "GET" => client.get(&input.url),
        "POST" => client.post(&input.url),
        "PUT" => client.put(&input.url),
        "DELETE" => client.delete(&input.url),
        "PATCH" => client.patch(&input.url),
        "HEAD" => client.head(&input.url),
        other => return Err(format!("unsupported HTTP method: {other}")),
    };

    // Apply custom headers
    if let Some(ref headers) = input.headers {
        if let Some(obj) = headers.as_object() {
            for (key, value) in obj {
                if let Some(val) = value.as_str() {
                    request = request.header(key.as_str(), val);
                }
            }
        }
    }

    // Apply body
    if let Some(ref body) = input.body {
        request = request.body(body.clone());
    }

    // Execute with a 30-second timeout
    let request = request.timeout(Duration::from_secs(30));

    match request.send() {
        Ok(response) => {
            let status = response.status().as_u16();
            let body = response.text().unwrap_or_default();
            let truncated_body = if body.len() > 8192 {
                format!(
                    "{}\n\n[response truncated — {} bytes total]",
                    &body[..8192],
                    body.len()
                )
            } else {
                body
            };
            to_pretty_json(json!({
                "url": input.url,
                "method": method,
                "status_code": status,
                "body": truncated_body,
                "success": (200..300).contains(&status)
            }))
        }
        Err(e) => to_pretty_json(json!({
            "url": input.url,
            "method": method,
            "error": e.to_string(),
            "success": false
        })),
    }
}

#[allow(clippy::needless_pass_by_value)]
fn run_mcp_tool(input: McpToolInput) -> Result<String, String> {
    let registry = global_mcp_registry();
    let args = input.arguments.unwrap_or(serde_json::json!({}));
    match registry.call_tool(&input.server, &input.tool, &args) {
        Ok(result) => to_pretty_json(json!({
            "server": input.server,
            "tool": input.tool,
            "result": result,
            "status": "success"
        })),
        Err(e) => to_pretty_json(json!({
            "server": input.server,
            "tool": input.tool,
            "error": e,
            "status": "error"
        })),
    }
}

#[allow(clippy::needless_pass_by_value)]
fn run_testing_permission(input: TestingPermissionInput) -> Result<String, String> {
    to_pretty_json(json!({
        "action": input.action,
        "permitted": true,
        "message": "Testing permission tool stub"
    }))
}

/// Execute `git status --short --branch` and return structured JSON output.
/// Falls back to full `git status` if `short` is explicitly set to false.
#[allow(clippy::needless_pass_by_value)]
fn run_git_status(input: GitStatusInput) -> Result<String, String> {
    let mut args: Vec<&str> = vec!["status"];
    if input.short.unwrap_or(true) {
        args.push("--short");
        args.push("--branch");
    }
    match git_stdout(&args) {
        Some(output) => to_pretty_json(json!({ "output": output })),
        None => Err(
            "git status failed. Ensure the current directory is inside a git repository."
                .to_string(),
        ),
    }
}

/// Execute `git diff` with optional --cached, commit, and path filters.
#[allow(clippy::needless_pass_by_value)]
fn run_git_diff(input: GitDiffInput) -> Result<String, String> {
    let mut args: Vec<String> = vec!["diff".to_string()];
    if input.staged.unwrap_or(false) {
        args.push("--cached".to_string());
    }
    if let Some(ref commit) = input.commit {
        if let Some(ref commit2) = input.commit2 {
            args.push(format!("{commit}...{commit2}"));
        } else {
            args.push(commit.clone());
        }
    }
    if let Some(ref path) = input.path {
        args.push("--".to_string());
        args.push(path.clone());
    }
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let output = Command::new("git")
        .args(&arg_refs)
        .output()
        .map_err(|error| format!("git diff failed to start: {error}"))?;
    if output.status.success() {
        to_pretty_json(json!({ "output": String::from_utf8_lossy(&output.stdout).to_string() }))
    } else {
        Err(format!(
            "git diff failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

/// Execute `git log` with count, author, date, and path filters.
#[allow(clippy::needless_pass_by_value)]
fn run_git_log(input: GitLogInput) -> Result<String, String> {
    let mut args: Vec<String> = vec!["log".to_string()];
    let count = input.count.unwrap_or(20);
    args.push(format!("-n{count}"));
    if input.oneline.unwrap_or(false) {
        args.push("--oneline".to_string());
    }
    if let Some(ref author) = input.author {
        args.push(format!("--author={author}"));
    }
    if let Some(ref since) = input.since {
        args.push(format!("--since={since}"));
    }
    if let Some(ref until) = input.until {
        args.push(format!("--until={until}"));
    }
    if let Some(ref path) = input.path {
        args.push("--".to_string());
        args.push(path.clone());
    }
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    match git_stdout(&arg_refs) {
        Some(output) => to_pretty_json(json!({ "output": output })),
        None => Err(
            "git log failed. Ensure the current directory is inside a git repository.".to_string(),
        ),
    }
}

/// Execute `git show` for a commit, tag, or tree object.
#[allow(clippy::needless_pass_by_value)]
fn run_git_show(input: GitShowInput) -> Result<String, String> {
    let mut args: Vec<String> = vec!["show".to_string()];

    match input.format.as_deref() {
        Some("metadata") if input.path.is_some() => {
            return Err(
                "GitShow format \"metadata\" cannot be combined with path; metadata describes a commit, not a blob. Use format \"patch\" or \"stat\" with path, or omit path."
                    .to_string(),
            );
        }
        Some("metadata") => {
            args.push("--format=medium".to_string());
            args.push("--no-patch".to_string());
        }
        Some("stat") => args.push("--stat".to_string()),
        Some("patch") | None => {
            if input.format.is_none() && input.stat.unwrap_or(false) {
                args.push("--stat".to_string());
            }
        }
        Some(other) => {
            return Err(format!(
                "unknown GitShow format: \"{other}\". Supported values: \"patch\" (default), \"stat\", \"metadata\"."
            ));
        }
    }

    if let Some(ref path) = input.path {
        args.push(format!("{}:{}", input.commit, path));
    } else {
        args.push(input.commit.clone());
    }
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    match git_stdout(&arg_refs) {
        Some(output) => to_pretty_json(json!({ "output": output })),
        None => Err(format!(
            "git show {} failed. Ensure the commit exists.",
            input.commit
        )),
    }
}

/// Execute `git blame` on a file, optionally restricted to a line range.
#[allow(clippy::needless_pass_by_value)]
fn run_git_blame(input: GitBlameInput) -> Result<String, String> {
    let mut args: Vec<String> = vec!["blame".to_string()];
    if let (Some(start), Some(end)) = (input.start_line, input.end_line) {
        args.push(format!("-L{start},{end}"));
    }
    args.push(input.path.clone());
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    match git_stdout(&arg_refs) {
        Some(output) => to_pretty_json(json!({ "output": output })),
        None => Err(format!("git blame {} failed. Ensure the file exists and the directory is inside a git repository.", input.path)),
    }
}

fn from_value<T: for<'de> Deserialize<'de>>(input: &Value) -> Result<T, String> {
    serde_json::from_value(input.clone()).map_err(|error| error.to_string())
}

/// Classify bash command permission based on command type and path.
/// ROADMAP #50: Read-only commands targeting CWD paths get `WorkspaceWrite`,
/// all others remain `DangerFullAccess`.
fn classify_bash_permission(command: &str) -> PermissionMode {
    // Read-only commands that are safe when targeting workspace paths
    const READ_ONLY_COMMANDS: &[&str] = &[
        "cat", "head", "tail", "less", "more", "ls", "ll", "dir", "find", "test", "[", "[[",
        "grep", "rg", "awk", "sed", "file", "stat", "readlink", "wc", "sort", "uniq", "cut", "tr",
        "pwd", "echo", "printf",
    ];

    // Get the base command (first word before any args or pipes)
    let base_cmd = command.split_whitespace().next().unwrap_or("");
    let base_cmd = base_cmd.split('|').next().unwrap_or("").trim();
    let base_cmd = base_cmd.split(';').next().unwrap_or("").trim();
    let base_cmd = base_cmd.split('>').next().unwrap_or("").trim();
    let base_cmd = base_cmd.split('<').next().unwrap_or("").trim();

    // Check if it's a read-only command
    let cmd_name = base_cmd.split('/').next_back().unwrap_or(base_cmd);
    let is_read_only = READ_ONLY_COMMANDS.contains(&cmd_name);

    if !is_read_only {
        return PermissionMode::DangerFullAccess;
    }

    // Check if any path argument is outside workspace
    // Simple heuristic: check for absolute paths not starting with CWD
    if has_dangerous_paths(command) {
        return PermissionMode::DangerFullAccess;
    }

    PermissionMode::WorkspaceWrite
}

/// Check if command has dangerous paths (outside workspace).
fn has_dangerous_paths(command: &str) -> bool {
    // Look for absolute paths
    let tokens: Vec<&str> = command.split_whitespace().collect();

    for token in tokens {
        // Skip flags/options
        if token.starts_with('-') {
            continue;
        }

        // Check for absolute paths
        if token.starts_with('/') || token.starts_with("~/") {
            // Check if it's within CWD
            let path =
                PathBuf::from(token.replace('~', &std::env::var("HOME").unwrap_or_default()));
            if let Ok(cwd) = std::env::current_dir() {
                if !path.starts_with(&cwd) {
                    return true; // Path outside workspace
                }
            }
        }

        // Check for parent directory traversal that escapes workspace
        if token.contains("../..") || token.starts_with("../") && !token.starts_with("./") {
            return true;
        }
    }

    false
}

fn run_bash(input: BashCommandInput) -> Result<String, String> {
    if let Some(output) = workspace_test_branch_preflight(&input.command) {
        return serde_json::to_string_pretty(&output).map_err(|error| error.to_string());
    }
    serde_json::to_string_pretty(&execute_bash(input).map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())
}

fn workspace_test_branch_preflight(command: &str) -> Option<BashCommandOutput> {
    if !is_workspace_test_command(command) {
        return None;
    }

    let branch = current_git_branch()?;
    let main_ref = resolve_main_ref(&branch)?;
    let freshness = check_freshness(&branch, &main_ref);
    match freshness {
        BranchFreshness::Fresh => None,
        BranchFreshness::Stale {
            commits_behind,
            missing_fixes,
        } => Some(branch_divergence_output(
            command,
            &branch,
            &main_ref,
            commits_behind,
            None,
            &missing_fixes,
        )),
        BranchFreshness::Diverged {
            ahead,
            behind,
            missing_fixes,
        } => Some(branch_divergence_output(
            command,
            &branch,
            &main_ref,
            behind,
            Some(ahead),
            &missing_fixes,
        )),
    }
}

fn is_workspace_test_command(command: &str) -> bool {
    let normalized = normalize_shell_command(command);
    [
        "cargo test --workspace",
        "cargo test --all",
        "cargo nextest run --workspace",
        "cargo nextest run --all",
    ]
    .iter()
    .any(|needle| normalized.contains(needle))
}

fn normalize_shell_command(command: &str) -> String {
    command
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn resolve_main_ref(branch: &str) -> Option<String> {
    let has_local_main = git_ref_exists("main");
    let has_remote_main = git_ref_exists("origin/main");

    if branch == "main" && has_remote_main {
        Some("origin/main".to_string())
    } else if has_local_main {
        Some("main".to_string())
    } else if has_remote_main {
        Some("origin/main".to_string())
    } else {
        None
    }
}

fn git_ref_exists(reference: &str) -> bool {
    Command::new("git")
        .args(["rev-parse", "--verify", "--quiet", reference])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn git_stdout(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!stdout.is_empty()).then_some(stdout)
}

fn branch_divergence_output(
    command: &str,
    branch: &str,
    main_ref: &str,
    commits_behind: usize,
    commits_ahead: Option<usize>,
    missing_fixes: &[String],
) -> BashCommandOutput {
    let relation = commits_ahead.map_or_else(
        || format!("is {commits_behind} commit(s) behind"),
        |ahead| format!("has diverged ({ahead} ahead, {commits_behind} behind)"),
    );
    let missing_summary = if missing_fixes.is_empty() {
        "(none surfaced)".to_string()
    } else {
        missing_fixes.join("; ")
    };
    let stderr = format!(
        "branch divergence detected before workspace tests: `{branch}` {relation} `{main_ref}`. Missing commits: {missing_summary}. Merge or rebase `{main_ref}` before re-running `{command}`."
    );

    BashCommandOutput {
        stdout: String::new(),
        stderr: stderr.clone(),
        raw_output_path: None,
        interrupted: false,
        is_image: None,
        background_task_id: None,
        backgrounded_by_user: None,
        assistant_auto_backgrounded: None,
        dangerously_disable_sandbox: None,
        return_code_interpretation: Some("preflight_blocked:branch_divergence".to_string()),
        no_output_expected: Some(false),
        structured_content: Some(vec![serde_json::to_value(
            LaneEvent::new(
                LaneEventName::BranchStaleAgainstMain,
                LaneEventStatus::Blocked,
                iso8601_now(),
            )
            .with_failure_class(LaneFailureClass::BranchDivergence)
            .with_detail(stderr.clone())
            .with_data(json!({
                "branch": branch,
                "mainRef": main_ref,
                "commitsBehind": commits_behind,
                "commitsAhead": commits_ahead,
                "missingCommits": missing_fixes,
                "blockedCommand": command,
                "recommendedAction": format!("merge or rebase {main_ref} before workspace tests")
            })),
        )
        .expect("lane event should serialize")]),
        persisted_output_path: None,
        persisted_output_size: None,
        sandbox_status: None,
    }
}

#[allow(clippy::needless_pass_by_value)]
fn run_read_file(input: ReadFileInput) -> Result<String, String> {
    to_pretty_json(read_file(&input.path, input.offset, input.limit).map_err(io_to_string)?)
}

#[allow(clippy::needless_pass_by_value)]
fn run_write_file(input: WriteFileInput) -> Result<String, String> {
    to_pretty_json(write_file(&input.path, &input.content).map_err(io_to_string)?)
}

#[allow(clippy::needless_pass_by_value)]
fn run_edit_file(input: EditFileInput) -> Result<String, String> {
    to_pretty_json(
        edit_file(
            &input.path,
            &input.old_string,
            &input.new_string,
            input.replace_all.unwrap_or(false),
        )
        .map_err(io_to_string)?,
    )
}

#[allow(clippy::needless_pass_by_value)]
fn run_glob_search(input: GlobSearchInputValue) -> Result<String, String> {
    to_pretty_json(glob_search(&input.pattern, input.path.as_deref()).map_err(io_to_string)?)
}

#[allow(clippy::needless_pass_by_value)]
fn run_grep_search(input: GrepSearchInput) -> Result<String, String> {
    to_pretty_json(grep_search(&input).map_err(io_to_string)?)
}

#[allow(clippy::needless_pass_by_value)]
fn run_web_fetch(input: WebFetchInput) -> Result<String, String> {
    to_pretty_json(execute_web_fetch(&input)?)
}

#[allow(clippy::needless_pass_by_value)]
fn run_web_search(input: WebSearchInput) -> Result<String, String> {
    to_pretty_json(execute_web_search(&input)?)
}

fn run_todo_write(input: TodoWriteInput) -> Result<String, String> {
    to_pretty_json(execute_todo_write(input)?)
}

fn run_skill(input: SkillInput) -> Result<String, String> {
    to_pretty_json(execute_skill(input)?)
}

fn run_agent(input: AgentInput) -> Result<String, String> {
    to_pretty_json(execute_agent(input)?)
}

fn run_tool_search(input: ToolSearchInput) -> Result<String, String> {
    to_pretty_json(execute_tool_search(input))
}

fn run_notebook_edit(input: NotebookEditInput) -> Result<String, String> {
    to_pretty_json(execute_notebook_edit(input)?)
}

fn run_sleep(input: SleepInput) -> Result<String, String> {
    to_pretty_json(execute_sleep(input)?)
}

fn run_brief(input: BriefInput) -> Result<String, String> {
    to_pretty_json(execute_brief(input)?)
}

fn run_config(input: ConfigInput) -> Result<String, String> {
    to_pretty_json(execute_config(input)?)
}

fn run_enter_plan_mode(input: EnterPlanModeInput) -> Result<String, String> {
    to_pretty_json(execute_enter_plan_mode(input)?)
}

fn run_exit_plan_mode(input: ExitPlanModeInput) -> Result<String, String> {
    to_pretty_json(execute_exit_plan_mode(input)?)
}

fn run_structured_output(input: StructuredOutputInput) -> Result<String, String> {
    to_pretty_json(execute_structured_output(input)?)
}

fn run_repl(input: ReplInput) -> Result<String, String> {
    to_pretty_json(execute_repl(input)?)
}

/// Classify `PowerShell` command permission based on command type and path.
/// ROADMAP #50: Read-only commands targeting CWD paths get `WorkspaceWrite`,
/// all others remain `DangerFullAccess`.
fn classify_file_path_permission(path: &str, allow_missing: bool) -> PermissionMode {
    if path_within_current_workspace(path, allow_missing) {
        PermissionMode::WorkspaceWrite
    } else {
        PermissionMode::DangerFullAccess
    }
}

fn classify_read_path_permission(path: &str, allow_missing: bool) -> PermissionMode {
    if path_within_current_workspace(path, allow_missing) {
        PermissionMode::ReadOnly
    } else {
        PermissionMode::DangerFullAccess
    }
}

fn classify_glob_permission(input: &GlobSearchInputValue) -> PermissionMode {
    let base_allowed = input
        .path
        .as_deref()
        .is_none_or(|path| path_within_current_workspace(path, false));
    let pattern_allowed = path_within_current_workspace(&input.pattern, true);
    if base_allowed && pattern_allowed {
        PermissionMode::ReadOnly
    } else {
        PermissionMode::DangerFullAccess
    }
}

fn classify_grep_permission(input: &GrepSearchInput) -> PermissionMode {
    if input
        .path
        .as_deref()
        .is_none_or(|path| path_within_current_workspace(path, false))
    {
        PermissionMode::ReadOnly
    } else {
        PermissionMode::DangerFullAccess
    }
}

/// Returns true when `path` resolves to a location inside the current working
/// directory (the active workspace). Operations on in-workspace paths only need
/// the tool's baseline permission; anything resolving outside the workspace is
/// escalated to `DangerFullAccess` so the user is prompted before the runtime
/// touches files beyond the project boundary.
fn path_within_current_workspace(path: &str, allow_missing: bool) -> bool {
    let trimmed = path.trim_matches(|ch: char| {
        matches!(
            ch,
            '"' | '\'' | '`' | ',' | ';' | ')' | '(' | '[' | ']' | '{' | '}'
        )
    });
    if looks_like_windows_absolute_path(trimmed) {
        return false;
    }

    let Ok(cwd) = std::env::current_dir() else {
        return false;
    };
    let cwd = cwd.canonicalize().unwrap_or(cwd);
    let candidate = PathBuf::from(trimmed);
    let absolute = if candidate.is_absolute() {
        candidate
    } else {
        cwd.join(candidate)
    };

    let resolved = if allow_missing {
        absolute
            .parent()
            .and_then(|parent| parent.canonicalize().ok())
            .map(|parent| parent.join(absolute.file_name().unwrap_or_default()))
            .unwrap_or(absolute)
    } else {
        match absolute.canonicalize() {
            Ok(path) => path,
            Err(_) => absolute,
        }
    };

    resolved.starts_with(cwd)
}

fn looks_like_windows_absolute_path(token: &str) -> bool {
    let bytes = token.as_bytes();
    (bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'/' | b'\\'))
        || token.starts_with(r"\\")
}

fn classify_powershell_permission(command: &str) -> PermissionMode {
    // Read-only commands that are safe when targeting workspace paths
    const READ_ONLY_COMMANDS: &[&str] = &[
        "Get-Content",
        "Get-ChildItem",
        "Test-Path",
        "Get-Item",
        "Get-ItemProperty",
        "Get-FileHash",
        "Select-String",
    ];

    // Check if command starts with a read-only cmdlet
    let cmd_lower = command.trim().to_lowercase();
    let is_read_only_cmd = READ_ONLY_COMMANDS
        .iter()
        .any(|cmd| cmd_lower.starts_with(&cmd.to_lowercase()));

    if !is_read_only_cmd {
        return PermissionMode::DangerFullAccess;
    }

    // Check if the path is within workspace (CWD or subdirectory)
    // Extract path from command - look for -Path or positional parameter
    let path = extract_powershell_path(command);
    match path {
        Some(p) if is_within_workspace(&p) => PermissionMode::WorkspaceWrite,
        _ => PermissionMode::DangerFullAccess,
    }
}

/// Extract the path argument from a `PowerShell` command.
fn extract_powershell_path(command: &str) -> Option<String> {
    // Look for -Path parameter
    if let Some(idx) = command.to_lowercase().find("-path") {
        let after_path = &command[idx + 5..];
        let path = after_path.split_whitespace().next()?;
        return Some(path.trim_matches('"').trim_matches('\'').to_string());
    }

    // Look for positional path parameter (after command name)
    let parts: Vec<&str> = command.split_whitespace().collect();
    if parts.len() >= 2 {
        // Skip the cmdlet name and take the first argument
        let first_arg = parts[1];
        // Check if it looks like a path (contains \, /, or .)
        if first_arg.contains(['\\', '/', '.']) {
            return Some(first_arg.trim_matches('"').trim_matches('\'').to_string());
        }
    }

    None
}

/// Check if a path is within the current workspace.
fn is_within_workspace(path: &str) -> bool {
    let path = PathBuf::from(path);

    // If path is absolute, check if it starts with CWD
    if path.is_absolute() {
        if let Ok(cwd) = std::env::current_dir() {
            return path.starts_with(&cwd);
        }
    }

    // Relative paths are assumed to be within workspace
    !path.starts_with("/") && !path.starts_with("\\") && !path.starts_with("..")
}

fn run_powershell(input: PowerShellInput) -> Result<String, String> {
    to_pretty_json(execute_powershell(input).map_err(|error| error.to_string())?)
}

fn to_pretty_json<T: serde::Serialize>(value: T) -> Result<String, String> {
    serde_json::to_string_pretty(&value).map_err(|error| error.to_string())
}

#[allow(clippy::needless_pass_by_value)]
fn io_to_string(error: std::io::Error) -> String {
    error.to_string()
}

#[derive(Debug, Deserialize)]
struct ReadFileInput {
    path: String,
    offset: Option<usize>,
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct WriteFileInput {
    path: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct EditFileInput {
    path: String,
    old_string: String,
    new_string: String,
    replace_all: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct GlobSearchInputValue {
    pattern: String,
    path: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WebFetchInput {
    url: String,
    prompt: String,
}

#[derive(Debug, Clone, Deserialize)]
struct WebSearchInput {
    query: String,
    allowed_domains: Option<Vec<String>>,
    blocked_domains: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct TodoWriteInput {
    todos: Vec<TodoItem>,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
struct TodoItem {
    content: String,
    #[serde(rename = "activeForm")]
    active_form: String,
    status: TodoStatus,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Debug, Deserialize)]
struct SkillInput {
    skill: String,
    args: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AgentInput {
    description: String,
    prompt: String,
    subagent_type: Option<String>,
    name: Option<String>,
    model: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ToolSearchInput {
    query: String,
    max_results: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct NotebookEditInput {
    notebook_path: String,
    cell_id: Option<String>,
    new_source: Option<String>,
    cell_type: Option<NotebookCellType>,
    edit_mode: Option<NotebookEditMode>,
}

#[derive(Debug, Deserialize, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum NotebookCellType {
    Code,
    Markdown,
}

#[derive(Debug, Deserialize, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum NotebookEditMode {
    Replace,
    Insert,
    Delete,
}

#[derive(Debug, Deserialize)]
struct SleepInput {
    duration_ms: u64,
}

#[derive(Debug, Deserialize)]
struct BriefInput {
    message: String,
    attachments: Option<Vec<String>>,
    status: BriefStatus,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum BriefStatus {
    Normal,
    Proactive,
}

#[derive(Debug, Deserialize)]
struct ConfigInput {
    setting: String,
    value: Option<ConfigValue>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct EnterPlanModeInput {}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ExitPlanModeInput {}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ConfigValue {
    String(String),
    Bool(bool),
    Number(f64),
}

#[derive(Debug, Deserialize)]
#[serde(transparent)]
struct StructuredOutputInput(BTreeMap<String, Value>);

#[derive(Debug, Deserialize)]
struct ReplInput {
    code: String,
    language: String,
    timeout_ms: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct PowerShellInput {
    command: String,
    timeout: Option<u64>,
    description: Option<String>,
    run_in_background: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct AskUserQuestionInput {
    question: String,
    #[serde(default)]
    options: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct TaskCreateInput {
    prompt: String,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TaskIdInput {
    task_id: String,
}

#[derive(Debug, Deserialize)]
struct TaskUpdateInput {
    task_id: String,
    message: String,
}

#[derive(Debug, Deserialize)]
struct WorkerCreateInput {
    cwd: String,
    #[serde(default)]
    trusted_roots: Vec<String>,
    #[serde(default = "default_auto_recover_prompt_misdelivery")]
    auto_recover_prompt_misdelivery: bool,
}

#[derive(Debug, Deserialize)]
struct WorkerIdInput {
    worker_id: String,
}

#[derive(Debug, Deserialize)]
struct WorkerObserveCompletionInput {
    worker_id: String,
    finish_reason: String,
    tokens_output: u64,
}

#[derive(Debug, Deserialize)]
struct WorkerObserveInput {
    worker_id: String,
    screen_text: String,
}

#[derive(Debug, Deserialize)]
struct WorkerSendPromptInput {
    worker_id: String,
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    task_receipt: Option<WorkerTaskReceipt>,
}

const fn default_auto_recover_prompt_misdelivery() -> bool {
    true
}

#[derive(Debug, Deserialize)]
struct TeamCreateInput {
    name: String,
    tasks: Vec<Value>,
}

#[derive(Debug, Deserialize)]
struct TeamDeleteInput {
    team_id: String,
}

#[derive(Debug, Deserialize)]
struct CronCreateInput {
    schedule: String,
    prompt: String,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CronDeleteInput {
    cron_id: String,
}

#[derive(Debug, Deserialize)]
struct LspInput {
    action: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    line: Option<u32>,
    #[serde(default)]
    character: Option<u32>,
    #[serde(default)]
    query: Option<String>,
}

#[derive(Debug, Deserialize)]
struct McpResourceInput {
    #[serde(default)]
    server: Option<String>,
    #[serde(default)]
    uri: Option<String>,
}

#[derive(Debug, Deserialize)]
struct McpAuthInput {
    server: String,
}

#[derive(Debug, Deserialize)]
struct RemoteTriggerInput {
    url: String,
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    headers: Option<Value>,
    #[serde(default)]
    body: Option<String>,
}

#[derive(Debug, Deserialize)]
struct McpToolInput {
    server: String,
    tool: String,
    #[serde(default)]
    arguments: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct TestingPermissionInput {
    action: String,
}

/// Input for the `GitStatus` tool. Defaults to `--short --branch`.
#[derive(Debug, Deserialize)]
struct GitStatusInput {
    #[serde(default)]
    short: Option<bool>,
}

/// Input for the `GitDiff` tool. No options is equivalent to `git diff`.
#[derive(Debug, Deserialize)]
struct GitDiffInput {
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    staged: Option<bool>,
    #[serde(default)]
    commit: Option<String>,
    #[serde(default)]
    commit2: Option<String>,
}

/// Input for the `GitLog` tool. Defaults to the last 20 commits.
#[derive(Debug, Deserialize)]
struct GitLogInput {
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    count: Option<usize>,
    #[serde(default)]
    oneline: Option<bool>,
    #[serde(default)]
    author: Option<String>,
    #[serde(default)]
    since: Option<String>,
    #[serde(default)]
    until: Option<String>,
}

/// Input for the `GitShow` tool.
#[derive(Debug, Deserialize)]
struct GitShowInput {
    commit: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    stat: Option<bool>,
    #[serde(default)]
    format: Option<String>,
}

/// Input for the `GitBlame` tool.
#[derive(Debug, Deserialize)]
struct GitBlameInput {
    path: String,
    #[serde(rename = "start_line")]
    #[serde(default)]
    start_line: Option<usize>,
    #[serde(rename = "end_line")]
    #[serde(default)]
    end_line: Option<usize>,
}

#[derive(Debug, Serialize)]
struct WebFetchOutput {
    bytes: usize,
    code: u16,
    #[serde(rename = "codeText")]
    code_text: String,
    result: String,
    #[serde(rename = "durationMs")]
    duration_ms: u128,
    url: String,
}

#[derive(Debug, Serialize)]
struct WebSearchOutput {
    query: String,
    #[serde(rename = "providerTrace", skip_serializing_if = "Vec::is_empty")]
    provider_trace: Vec<String>,
    #[serde(rename = "enrichmentTrace", skip_serializing_if = "Vec::is_empty")]
    enrichment_trace: Vec<String>,
    #[serde(rename = "qualityFilter", skip_serializing_if = "Option::is_none")]
    quality_filter: Option<WebSearchQualityFilterDebug>,
    results: Vec<WebSearchResultItem>,
    #[serde(rename = "durationSeconds")]
    duration_seconds: f64,
}

#[derive(Debug, Serialize)]
struct WebSearchQualityFilterDebug {
    #[serde(rename = "beforeCount")]
    before_count: usize,
    #[serde(rename = "afterCount")]
    after_count: usize,
    #[serde(rename = "droppedCount")]
    dropped_count: usize,
    #[serde(rename = "beforeUrlsSample", skip_serializing_if = "Vec::is_empty")]
    before_urls_sample: Vec<String>,
    #[serde(rename = "afterUrlsSample", skip_serializing_if = "Vec::is_empty")]
    after_urls_sample: Vec<String>,
    #[serde(rename = "droppedUrlsSample", skip_serializing_if = "Vec::is_empty")]
    dropped_urls_sample: Vec<String>,
}

#[derive(Debug, Serialize)]
struct TodoWriteOutput {
    #[serde(rename = "oldTodos")]
    old_todos: Vec<TodoItem>,
    #[serde(rename = "newTodos")]
    new_todos: Vec<TodoItem>,
    #[serde(rename = "verificationNudgeNeeded")]
    verification_nudge_needed: Option<bool>,
}

#[derive(Debug, Serialize)]
struct SkillOutput {
    skill: String,
    path: String,
    args: Option<String>,
    description: Option<String>,
    prompt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AgentOutput {
    #[serde(rename = "agentId")]
    agent_id: String,
    name: String,
    description: String,
    #[serde(rename = "subagentType")]
    subagent_type: Option<String>,
    model: Option<String>,
    status: String,
    #[serde(rename = "outputFile")]
    output_file: String,
    #[serde(rename = "manifestFile")]
    manifest_file: String,
    #[serde(rename = "createdAt")]
    created_at: String,
    #[serde(rename = "startedAt", skip_serializing_if = "Option::is_none")]
    started_at: Option<String>,
    #[serde(rename = "completedAt", skip_serializing_if = "Option::is_none")]
    completed_at: Option<String>,
    #[serde(rename = "laneEvents", default, skip_serializing_if = "Vec::is_empty")]
    lane_events: Vec<LaneEvent>,
    #[serde(rename = "currentBlocker", skip_serializing_if = "Option::is_none")]
    current_blocker: Option<LaneEventBlocker>,
    #[serde(rename = "derivedState")]
    derived_state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Debug, Clone)]
struct AgentJob {
    manifest: AgentOutput,
    prompt: String,
    system_prompt: Vec<String>,
    allowed_tools: BTreeSet<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ToolSearchOutput {
    matches: Vec<String>,
    query: String,
    normalized_query: String,
    #[serde(rename = "total_deferred_tools")]
    total_deferred_tools: usize,
    #[serde(rename = "pending_mcp_servers")]
    pending_mcp_servers: Option<Vec<String>>,
    #[serde(rename = "mcp_degraded", skip_serializing_if = "Option::is_none")]
    mcp_degraded: Option<McpDegradedReport>,
}

#[derive(Debug, Serialize)]
struct NotebookEditOutput {
    new_source: String,
    cell_id: Option<String>,
    cell_type: Option<NotebookCellType>,
    language: String,
    edit_mode: String,
    error: Option<String>,
    notebook_path: String,
    original_file: String,
    updated_file: String,
}

#[derive(Debug, Serialize)]
struct SleepOutput {
    duration_ms: u64,
    message: String,
}

#[derive(Debug, Serialize)]
struct BriefOutput {
    message: String,
    attachments: Option<Vec<ResolvedAttachment>>,
    #[serde(rename = "sentAt")]
    sent_at: String,
}

#[derive(Debug, Serialize)]
struct ResolvedAttachment {
    path: String,
    size: u64,
    #[serde(rename = "isImage")]
    is_image: bool,
}

#[derive(Debug, Serialize)]
struct ConfigOutput {
    success: bool,
    operation: Option<String>,
    setting: Option<String>,
    value: Option<Value>,
    #[serde(rename = "previousValue")]
    previous_value: Option<Value>,
    #[serde(rename = "newValue")]
    new_value: Option<Value>,
    error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PlanModeState {
    #[serde(rename = "hadLocalOverride")]
    had_local_override: bool,
    #[serde(rename = "previousLocalMode")]
    previous_local_mode: Option<Value>,
}

#[derive(Debug, Serialize)]
#[allow(clippy::struct_excessive_bools)]
struct PlanModeOutput {
    success: bool,
    operation: String,
    changed: bool,
    active: bool,
    managed: bool,
    message: String,
    #[serde(rename = "settingsPath")]
    settings_path: String,
    #[serde(rename = "statePath")]
    state_path: String,
    #[serde(rename = "previousLocalMode")]
    previous_local_mode: Option<Value>,
    #[serde(rename = "currentLocalMode")]
    current_local_mode: Option<Value>,
}

#[derive(Debug, Clone)]
struct SearchableToolSpec {
    name: String,
    description: String,
}

#[derive(Debug, Serialize)]
struct StructuredOutputResult {
    data: String,
    structured_output: BTreeMap<String, Value>,
}

#[derive(Debug, Serialize)]
struct ReplOutput {
    language: String,
    stdout: String,
    stderr: String,
    #[serde(rename = "exitCode")]
    exit_code: i32,
    #[serde(rename = "durationMs")]
    duration_ms: u128,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
enum WebSearchResultItem {
    SearchResult {
        tool_use_id: String,
        content: Vec<SearchHit>,
    },
    Commentary(String),
}

#[derive(Debug, Clone, Serialize)]
struct SearchHit {
    title: String,
    url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    snippet: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    domain: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(rename = "contentChars", skip_serializing_if = "Option::is_none")]
    content_chars: Option<usize>,
    #[serde(rename = "contentSource", skip_serializing_if = "Option::is_none")]
    content_source: Option<String>,
    #[serde(rename = "relevanceScore", skip_serializing_if = "Option::is_none")]
    relevance_score: Option<f64>,
}

#[derive(Debug, Clone)]
struct WebSearchEnrichmentConfig {
    enabled: bool,
    target_valid_pages: usize,
    initial_fetch_candidates: usize,
    max_fetch_candidates: usize,
    min_content_chars: usize,
    max_content_chars: usize,
    fetch_timeout_secs: u64,
    connect_timeout_secs: u64,
    fetch_retry_attempts: usize,
    fetch_retry_backoff_ms: u64,
    fetch_retry_jitter_ms: u64,
    jina_fallback_enabled: bool,
    domain_repeat_penalty: f64,
}

impl WebSearchEnrichmentConfig {
    fn from_env() -> Self {
        let target_valid_pages =
            read_bounded_env_usize("AOSD_WEB_SEARCH_ENRICH_TARGET_VALID_PAGES", 8, 1, 20);
        let max_fetch_candidates =
            read_bounded_env_usize("AOSD_WEB_SEARCH_ENRICH_MAX_FETCH_CANDIDATES", 20, 4, 40);
        let initial_fetch_candidates =
            read_bounded_env_usize("AOSD_WEB_SEARCH_ENRICH_INITIAL_FETCH_CANDIDATES", 8, 2, 30)
                .min(max_fetch_candidates);
        Self {
            enabled: read_env_bool("AOSD_WEB_SEARCH_ENRICH_ENABLED", !cfg!(test)),
            target_valid_pages,
            initial_fetch_candidates,
            max_fetch_candidates,
            min_content_chars: read_bounded_env_usize(
                "AOSD_WEB_SEARCH_ENRICH_MIN_CHARS",
                450,
                120,
                10_000,
            ),
            max_content_chars: read_bounded_env_usize(
                "AOSD_WEB_SEARCH_ENRICH_MAX_CHARS",
                12_000,
                1_000,
                100_000,
            ),
            fetch_timeout_secs: read_timeout_env_secs(
                "AOSD_WEB_SEARCH_ENRICH_FETCH_TIMEOUT_SECS",
                12,
                3,
                45,
            ),
            connect_timeout_secs: read_timeout_env_secs(
                "AOSD_WEB_SEARCH_ENRICH_CONNECT_TIMEOUT_SECS",
                4,
                1,
                12,
            ),
            fetch_retry_attempts: read_bounded_env_usize(
                "AOSD_WEB_SEARCH_ENRICH_FETCH_RETRY_ATTEMPTS",
                2,
                0,
                6,
            ),
            fetch_retry_backoff_ms: read_timeout_env_secs(
                "AOSD_WEB_SEARCH_ENRICH_FETCH_RETRY_BACKOFF_MS",
                350,
                0,
                10_000,
            ),
            fetch_retry_jitter_ms: read_timeout_env_secs(
                "AOSD_WEB_SEARCH_ENRICH_FETCH_RETRY_JITTER_MS",
                220,
                0,
                5_000,
            ),
            jina_fallback_enabled: read_env_bool(
                "AOSD_WEB_SEARCH_ENRICH_JINA_FALLBACK_ENABLED",
                !cfg!(test),
            ),
            domain_repeat_penalty: read_bounded_env_f64(
                "AOSD_WEB_SEARCH_ENRICH_DOMAIN_REPEAT_PENALTY",
                0.08,
                0.0,
                0.4,
            ),
        }
    }
}

#[derive(Debug, Clone)]
struct RankedSearchCandidate {
    hit: SearchHit,
    base_score: f64,
    domain_key: String,
}

#[derive(Debug, Clone)]
struct ExtractedContent {
    text: String,
    source: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WebSearchProvider {
    Auto,
    Builtin,
    Brave,
    Tavily,
    Serper,
    Exa,
    Demo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApiSearchProvider {
    Builtin,
    Brave,
    Tavily,
    Serper,
    Exa,
    Searxng,
    GenericJson,
    InternalHttp,
    Demo,
}

#[derive(Debug, Clone)]
struct ConfiguredApiSearchProvider {
    provider: ApiSearchProvider,
    api_keys: Vec<String>,
    name: Option<String>,
    priority: i32,
    base_url: Option<String>,
    method: Option<String>,
    auth_type: Option<String>,
    headers_json: Option<Value>,
    query_template_json: Option<Value>,
    response_mapping_json: Option<Value>,
    max_results: Option<usize>,
}

impl ApiSearchProvider {
    fn as_str(self) -> &'static str {
        match self {
            Self::Builtin => "aos_builtin",
            Self::Brave => "brave",
            Self::Tavily => "tavily",
            Self::Serper => "serper",
            Self::Exa => "exa",
            Self::Searxng => "searxng",
            Self::GenericJson => "generic_json",
            Self::InternalHttp => "internal_http",
            Self::Demo => "demo_search",
        }
    }

    fn api_key_env_candidates(self) -> &'static [&'static str] {
        match self {
            Self::Builtin => &[],
            Self::Brave => &["AOSD_BRAVE_API_KEY", "BRAVE_SEARCH_API_KEY"],
            Self::Tavily => &["AOSD_TAVILY_API_KEY", "TAVILY_API_KEY"],
            Self::Serper => &["AOSD_SERPER_API_KEY", "SERPER_API_KEY"],
            Self::Exa => &["AOSD_EXA_API_KEY", "EXA_API_KEY"],
            Self::Searxng | Self::GenericJson | Self::InternalHttp => &[],
            Self::Demo => &[],
        }
    }

    fn api_keys_env_candidates(self) -> &'static [&'static str] {
        match self {
            Self::Builtin => &[],
            Self::Brave => &["AOSD_BRAVE_API_KEYS", "BRAVE_SEARCH_API_KEYS"],
            Self::Tavily => &["AOSD_TAVILY_API_KEYS", "TAVILY_API_KEYS"],
            Self::Serper => &["AOSD_SERPER_API_KEYS", "SERPER_API_KEYS"],
            Self::Exa => &["AOSD_EXA_API_KEYS", "EXA_API_KEYS"],
            Self::Searxng | Self::GenericJson | Self::InternalHttp => &[],
            Self::Demo => &[],
        }
    }

    fn endpoint_env(self) -> &'static str {
        match self {
            Self::Builtin => "",
            Self::Brave => "AOSD_BRAVE_BASE_URL",
            Self::Tavily => "AOSD_TAVILY_BASE_URL",
            Self::Serper => "AOSD_SERPER_BASE_URL",
            Self::Exa => "AOSD_EXA_BASE_URL",
            Self::Searxng | Self::GenericJson | Self::InternalHttp => "",
            Self::Demo => "",
        }
    }

    fn default_endpoint(self) -> &'static str {
        match self {
            Self::Builtin => "",
            Self::Brave => "https://api.search.brave.com/res/v1/web/search",
            Self::Tavily => "https://api.tavily.com/search",
            Self::Serper => "https://google.serper.dev/search",
            Self::Exa => "https://api.exa.ai/search",
            Self::Searxng | Self::GenericJson | Self::InternalHttp => "",
            Self::Demo => "",
        }
    }

    fn requires_api_key(self) -> bool {
        matches!(
            self,
            Self::Brave
                | Self::Tavily
                | Self::Serper
                | Self::Exa
                | Self::GenericJson
                | Self::InternalHttp
        )
    }
}

#[derive(Debug, Clone)]
struct ApiProviderError {
    message: String,
    retryable: bool,
    retry_after: Option<Duration>,
}

impl ApiProviderError {
    fn new(message: String, retryable: bool, retry_after: Option<Duration>) -> Self {
        Self {
            message,
            retryable,
            retry_after,
        }
    }
}

fn execute_web_fetch(input: &WebFetchInput) -> Result<WebFetchOutput, String> {
    let started = Instant::now();
    let timeout_secs = read_timeout_env_secs("AOSD_WEB_FETCH_TIMEOUT_SECS", 20, 5, 90);
    let connect_timeout_secs = read_timeout_env_secs("AOSD_WEB_CONNECT_TIMEOUT_SECS", 5, 1, 15);
    let client = build_http_client(timeout_secs, connect_timeout_secs)?;
    let request_url = normalize_fetch_url(&input.url)?;
    let response = client
        .get(request_url.clone())
        .send()
        .map_err(|error| explain_web_fetch_error(&request_url, &error))?;

    let status = response.status();
    let final_url = response.url().to_string();
    let code = status.as_u16();
    let code_text = status.canonical_reason().unwrap_or("Unknown").to_string();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let body = response.text().map_err(|error| error.to_string())?;
    let bytes = body.len();
    let normalized = normalize_fetched_content(&body, &content_type);
    if !status.is_success() {
        return Err(format!(
            "web fetch returned HTTP {code} {code_text} for {final_url}"
        ));
    }
    if let Some(reason) = obvious_web_fetch_content_failure(&body) {
        return Err(format!(
            "web fetch returned unusable content for {final_url}: {reason}"
        ));
    }
    let result = summarize_web_fetch(&final_url, &input.prompt, &normalized, &body, &content_type);

    Ok(WebFetchOutput {
        bytes,
        code,
        code_text,
        result,
        duration_ms: started.elapsed().as_millis(),
        url: final_url,
    })
}

fn obvious_web_fetch_content_failure(raw_body: &str) -> Option<&'static str> {
    let prefix = raw_body
        .chars()
        .take(4_000)
        .collect::<String>()
        .to_ascii_lowercase();
    if [
        "<title>403 forbidden",
        "<h1>403 forbidden",
        "<title>404 not found",
        "<h1>404 not found",
        "verify you are human",
        "checking your browser",
        "enable javascript and cookies to continue",
    ]
    .iter()
    .any(|marker| prefix.contains(marker))
    {
        return Some("blocked or error page");
    }
    None
}

fn explain_web_fetch_error(request_url: &str, error: &reqwest::Error) -> String {
    let mut message = error.to_string();
    let lower = message.to_ascii_lowercase();
    let redirect_like = error.is_redirect()
        || lower.contains("error following redirect")
        || lower.contains("too many redirects")
        || lower.contains("redirect");
    if redirect_like {
        message.push_str(
            " (possible redirect loop: site login/consent gate or anti-bot challenge blocked non-browser fetch)",
        );
        if request_url.contains("statista.com") {
            message.push_str(
                "; statista may loop between /outlook and /sso/iplogin without valid session/IP entitlement",
            );
        }
    } else if error.is_timeout() || lower.contains("timed out") {
        message.push_str(" (request timed out; target site may be slow or egress route unstable)");
    } else if lower.contains("connection") || lower.contains("dns") {
        message
            .push_str(" (network path issue: DNS/proxy/TLS/egress connectivity may be unstable)");
    }
    message
}

fn execute_web_search(input: &WebSearchInput) -> Result<WebSearchOutput, String> {
    let started = Instant::now();
    let timeout_secs = read_timeout_env_secs("AOSD_WEB_SEARCH_TIMEOUT_SECS", 12, 3, 30);
    let connect_timeout_secs = read_timeout_env_secs("AOSD_WEB_CONNECT_TIMEOUT_SECS", 5, 1, 15);
    let client = build_http_client(timeout_secs, connect_timeout_secs)?;
    let enrichment_cfg = WebSearchEnrichmentConfig::from_env();
    let providers = resolve_configured_api_search_providers(read_web_search_provider()?)?;
    let max_retries = read_bounded_env_usize("AOSD_WEB_SEARCH_MAX_RETRIES", 1, 0, 3);
    let retry_backoff_ms = read_timeout_env_secs("AOSD_WEB_SEARCH_RETRY_BACKOFF_MS", 200, 0, 3_000);
    let retry_jitter_ms = read_timeout_env_secs("AOSD_WEB_SEARCH_RETRY_JITTER_MS", 120, 0, 2_000);
    let key_cooldown_secs = read_web_search_key_cooldown_secs();
    let mut key_blocked_until = BTreeMap::<String, SystemTime>::new();
    let mut attempts: Vec<String> = Vec::new();
    let mut enrichment_trace: Vec<String> = Vec::new();
    let desired_raw_hits = read_web_search_max_results()
        .max(enrichment_cfg.max_fetch_candidates)
        .min(40);
    let mut hits = Vec::new();

    for configured in &providers {
        if configured.provider == ApiSearchProvider::Builtin && !hits.is_empty() {
            let (usable_hits, _) = filter_low_signal_search_hits(&input.query, hits.clone());
            if !usable_hits.is_empty() {
                attempts.push(
                    "aos_builtin: skipped because configured Search Extensions returned usable evidence"
                        .to_string(),
                );
                break;
            }
        }
        let provider_keys =
            available_keys_for_provider(configured, &key_blocked_until, &mut attempts);
        if provider_keys.is_empty() {
            attempts.push(format!(
                "{}: all configured keys cooling down; skipping provider this round",
                configured.provider.as_str()
            ));
            continue;
        }
        let mut provider_done = false;
        for provider_key in provider_keys {
            let key_id = provider_key_identity(configured.provider, &provider_key);
            for retry_idx in 0..=max_retries {
                let attempt_label = format!(
                    "attempt {}/{} key={}",
                    retry_idx + 1,
                    max_retries + 1,
                    key_id
                );
                match execute_web_search_via_api_provider(configured, &provider_key, input, &client)
                {
                    Ok(found_hits) if !found_hits.is_empty() => {
                        attempts.push(format!(
                            "{}: {attempt_label} ok",
                            configured.provider.as_str()
                        ));
                        hits.extend(found_hits);
                        dedupe_hits(&mut hits);
                        provider_done = true;
                        break;
                    }
                    Ok(_) => {
                        attempts.push(format!(
                            "{}: {attempt_label} ok_but_no_hits",
                            configured.provider.as_str()
                        ));
                        provider_done = true;
                        break;
                    }
                    Err(error) => {
                        attempts.push(format!(
                            "{}: {attempt_label} provider_error={} retryable={} retry_after_ms={}",
                            configured.provider.as_str(),
                            error.message,
                            error.retryable,
                            error.retry_after.map(|v| v.as_millis()).unwrap_or(0)
                        ));
                        let should_retry = error.retryable && retry_idx < max_retries;
                        if should_retry {
                            let retry_delay = compute_retry_delay(
                                retry_idx,
                                retry_backoff_ms,
                                retry_jitter_ms,
                                error.retry_after,
                            );
                            if !retry_delay.is_zero() {
                                std::thread::sleep(retry_delay);
                            }
                        } else {
                            if is_provider_key_cooldown_error(&error) {
                                apply_provider_key_cooldown(
                                    configured.provider,
                                    &provider_key,
                                    key_cooldown_secs,
                                    &mut key_blocked_until,
                                    &mut attempts,
                                );
                            }
                            break;
                        }
                    }
                }
            }
            if provider_done {
                break;
            }
        }
        if hits.len() >= desired_raw_hits {
            enrichment_trace.push(format!(
                "provider_merge: collected {} raw hits (target {}), stopping provider expansion",
                hits.len(),
                desired_raw_hits
            ));
            break;
        }
    }

    if hits.is_empty() && !attempts.is_empty() {
        if !have_any_provider_key_available(&providers, &key_blocked_until) {
            let retry_in = earliest_provider_key_retry(&key_blocked_until)
                .map(|d| d.as_secs().to_string())
                .unwrap_or_else(|| "unknown".to_string());
            return Err(format!(
                "web search key pool is cooling down on all configured providers (retry_in={}s): {}",
                retry_in,
                attempts.join(" | ")
            ));
        }
        if attempts
            .iter()
            .all(|entry| is_search_attempt_hard_error(entry))
        {
            return Err(format!(
                "web search unavailable on all configured API providers: {}",
                attempts.join(" | ")
            ));
        }
    }

    if let Some(allowed) = normalize_domain_filters(input.allowed_domains.as_deref()) {
        hits.retain(|hit| host_matches_list(&hit.url, &allowed));
    }
    if let Some(blocked) = normalize_domain_filters(input.blocked_domains.as_deref()) {
        hits.retain(|hit| !host_matches_list(&hit.url, &blocked));
    }

    let quality_input_hits = hits.len();
    let dropped_dup_pre_quality = dedupe_hits_with_stats(&mut hits);
    let quality_before_urls = hits.iter().map(|hit| hit.url.clone()).collect::<Vec<_>>();
    let quality_before_set = hits
        .iter()
        .map(|hit| canonical_search_hit_dedupe_key(&hit.url))
        .collect::<BTreeSet<_>>();
    let (filtered_hits, quality_stats) = filter_low_signal_search_hits(&input.query, hits);
    hits = filtered_hits;
    let dropped_dup_post_quality = dedupe_hits_with_stats(&mut hits);
    let quality_after_urls = hits.iter().map(|hit| hit.url.clone()).collect::<Vec<_>>();
    let quality_after_set = hits
        .iter()
        .map(|hit| canonical_search_hit_dedupe_key(&hit.url))
        .collect::<BTreeSet<_>>();
    let quality_dropped_urls = quality_before_urls
        .iter()
        .filter(|url| !quality_after_set.contains(&canonical_search_hit_dedupe_key(url)))
        .cloned()
        .collect::<Vec<_>>();
    let quality_filter_debug = Some(WebSearchQualityFilterDebug {
        before_count: quality_before_set.len(),
        after_count: quality_after_set.len(),
        dropped_count: quality_before_set
            .len()
            .saturating_sub(quality_after_set.len()),
        before_urls_sample: quality_before_urls.into_iter().take(20).collect(),
        after_urls_sample: quality_after_urls.into_iter().take(20).collect(),
        dropped_urls_sample: quality_dropped_urls.into_iter().take(20).collect(),
    });
    enrichment_trace.push(format!(
        "quality_filter: input={} dropped_dup_pre={} dropped_auth={} dropped_nav={} dropped_mismatch={} dropped_invalid={} dropped_dup_post={} kept={}",
        quality_input_hits,
        dropped_dup_pre_quality,
        quality_stats.dropped_auth,
        quality_stats.dropped_navigation,
        quality_stats.dropped_mismatch,
        quality_stats.dropped_invalid,
        dropped_dup_post_quality,
        hits.len()
    ));
    hits = enrich_hits_with_tavily_extract(hits, &client, &enrichment_cfg, &mut enrichment_trace);
    hits = enrich_web_search_hits(&input.query, hits, &enrichment_cfg, &mut enrichment_trace);
    let dropped_dup_post_enrich = dedupe_hits_with_stats(&mut hits);
    if dropped_dup_post_enrich > 0 {
        enrichment_trace.push(format!(
            "quality_filter: dropped_dup_post_enrich={dropped_dup_post_enrich}"
        ));
    }
    hits.truncate(read_web_search_output_limit());
    compact_web_search_model_payload(&mut hits);

    let summary = if hits.is_empty() {
        if attempts.is_empty() {
            format!("No web search results matched the query {:?}.", input.query)
        } else {
            format!(
                "No web search results matched the query {:?}. Probe trace: {}",
                input.query,
                attempts.join(" | ")
            )
        }
    } else {
        let rendered_hits = hits
            .iter()
            .map(render_search_hit_for_summary)
            .collect::<Vec<_>>()
            .join("\n");
        format!(
            "Search results for {:?}. Include a Sources section in the final answer.\n{}",
            input.query, rendered_hits
        )
    };

    Ok(WebSearchOutput {
        query: input.query.clone(),
        provider_trace: attempts,
        enrichment_trace,
        quality_filter: quality_filter_debug,
        results: vec![
            WebSearchResultItem::Commentary(summary),
            WebSearchResultItem::SearchResult {
                tool_use_id: String::from("web_search_1"),
                content: hits,
            },
        ],
        duration_seconds: started.elapsed().as_secs_f64(),
    })
}

/// Keep WebSearch useful to the model without replaying several complete web
/// pages in one tool result. Exact pages remain available through WebFetch, so
/// research flows can still inspect a focused source when the summary is not
/// enough.
fn compact_web_search_model_payload(hits: &mut [SearchHit]) {
    let per_hit_limit = read_bounded_env_usize(
        "AOSD_WEB_SEARCH_MODEL_CONTENT_CHARS_PER_HIT",
        4_000,
        500,
        12_000,
    );
    let total_limit = read_bounded_env_usize(
        "AOSD_WEB_SEARCH_MODEL_CONTENT_CHARS_TOTAL",
        16_000,
        2_000,
        48_000,
    );
    let mut remaining = total_limit;
    for hit in hits {
        if let Some(snippet) = hit.snippet.as_mut() {
            *snippet = preview_text(snippet, 600);
        }
        let Some(content) = hit.content.as_mut() else {
            continue;
        };
        if remaining == 0 {
            *content = String::new();
            continue;
        }
        let admitted = content.chars().count().min(per_hit_limit).min(remaining);
        *content = preview_text(content, admitted);
        remaining = remaining.saturating_sub(content.chars().count());
    }
}

fn read_web_search_provider() -> Result<WebSearchProvider, String> {
    let raw = std::env::var("AOSD_WEB_SEARCH_PROVIDER").unwrap_or_else(|_| "auto".to_string());
    let normalized = raw.trim().to_ascii_lowercase();
    if normalized.is_empty() || normalized == "auto" {
        return Ok(WebSearchProvider::Auto);
    }
    if normalized == "legacy_html" || normalized == "searxng_json" {
        return Err(format!(
            "AOSD_WEB_SEARCH_PROVIDER={normalized} is removed; use one of: auto, builtin, brave, tavily, serper, exa, demo_search"
        ));
    }
    match parse_api_search_provider(raw.trim()) {
        Some(ApiSearchProvider::Builtin) => Ok(WebSearchProvider::Builtin),
        Some(ApiSearchProvider::Brave) => Ok(WebSearchProvider::Brave),
        Some(ApiSearchProvider::Tavily) => Ok(WebSearchProvider::Tavily),
        Some(ApiSearchProvider::Serper) => Ok(WebSearchProvider::Serper),
        Some(ApiSearchProvider::Exa) => Ok(WebSearchProvider::Exa),
        Some(ApiSearchProvider::Demo) => Ok(WebSearchProvider::Demo),
        Some(ApiSearchProvider::Searxng | ApiSearchProvider::GenericJson | ApiSearchProvider::InternalHttp) => Err(format!(
            "AOSD_WEB_SEARCH_PROVIDER={} is DB-configured only; configure it in Search Extensions",
            raw.trim()
        )),
        None => Err(format!(
            "unsupported AOSD_WEB_SEARCH_PROVIDER={}; use one of: auto, builtin, brave, tavily, serper, exa, demo_search",
            raw.trim()
        )),
    }
}

fn is_search_attempt_hard_error(entry: &str) -> bool {
    entry.contains("request_error=")
        || entry.contains("http_status=")
        || entry.contains("provider_error=")
        || entry.contains("json_parse_error=")
        || entry.contains("invalid_endpoint=")
}

fn resolve_configured_api_search_providers(
    requested: WebSearchProvider,
) -> Result<Vec<ConfiguredApiSearchProvider>, String> {
    if let Some(mut providers) = resolve_web_search_provider_override()? {
        providers
            .sort_by_key(|provider| (provider.priority, provider.name.clone().unwrap_or_default()));
        return Ok(providers);
    }

    let explicit_provider = match requested {
        WebSearchProvider::Auto => None,
        WebSearchProvider::Builtin => Some(ApiSearchProvider::Builtin),
        WebSearchProvider::Brave => Some(ApiSearchProvider::Brave),
        WebSearchProvider::Tavily => Some(ApiSearchProvider::Tavily),
        WebSearchProvider::Serper => Some(ApiSearchProvider::Serper),
        WebSearchProvider::Exa => Some(ApiSearchProvider::Exa),
        WebSearchProvider::Demo => Some(ApiSearchProvider::Demo),
    };

    if let Some(provider) = explicit_provider {
        if provider == ApiSearchProvider::Demo && !read_env_bool("AOSD_DEMO_SEARCH_ENABLED", false)
        {
            return Err(missing_provider_key_error(provider));
        }
        let api_keys = if provider.requires_api_key() {
            read_provider_api_keys(provider).ok_or_else(|| missing_provider_key_error(provider))?
        } else {
            Vec::new()
        };
        return Ok(vec![ConfiguredApiSearchProvider {
            provider,
            api_keys,
            name: None,
            priority: 0,
            base_url: None,
            method: None,
            auth_type: None,
            headers_json: None,
            query_template_json: None,
            response_mapping_json: None,
            max_results: None,
        }]);
    }

    let mut configured = Vec::new();
    for provider in read_provider_order() {
        let Some(api_keys) = read_provider_api_keys(provider) else {
            continue;
        };
        configured.push(ConfiguredApiSearchProvider {
            provider,
            api_keys,
            name: None,
            priority: 0,
            base_url: None,
            method: None,
            auth_type: None,
            headers_json: None,
            query_template_json: None,
            response_mapping_json: None,
            max_results: None,
        });
    }
    configured.push(builtin_configured_search_provider());
    Ok(configured)
}

fn builtin_configured_search_provider() -> ConfiguredApiSearchProvider {
    ConfiguredApiSearchProvider {
        provider: ApiSearchProvider::Builtin,
        api_keys: Vec::new(),
        name: Some("AOS Built-in Web Search".to_string()),
        priority: i32::MAX,
        base_url: None,
        method: Some("GET".to_string()),
        auth_type: Some("none".to_string()),
        headers_json: None,
        query_template_json: None,
        response_mapping_json: None,
        max_results: None,
    }
}

fn resolve_web_search_provider_override() -> Result<Option<Vec<ConfiguredApiSearchProvider>>, String>
{
    let configured_override = WEB_SEARCH_PROVIDER_OVERRIDE.with(|slot| slot.borrow().clone());
    let Some(configured_override) = configured_override else {
        return Ok(None);
    };
    let mut out = Vec::new();
    for provider in configured_override
        .providers
        .into_iter()
        .filter(|provider| provider.enabled)
    {
        let Some(kind) = provider_type_to_api_search_provider(&provider.provider_type) else {
            continue;
        };
        let mut api_keys = Vec::new();
        if let Some(secret) = provider
            .auth_secret
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            api_keys.push(secret.to_string());
        } else if kind == ApiSearchProvider::Demo {
            api_keys.push("demo_search".to_string());
        } else if provider_requires_secret(kind, provider.auth_type.as_deref()) {
            return Err(format!(
                "configured search provider `{}` ({}) is enabled but has no secret",
                provider.name,
                kind.as_str()
            ));
        }
        out.push(ConfiguredApiSearchProvider {
            provider: kind,
            api_keys,
            name: Some(provider.name),
            priority: provider.priority,
            base_url: provider.base_url,
            method: provider.method,
            auth_type: provider.auth_type,
            headers_json: provider.headers_json,
            query_template_json: provider.query_template_json,
            response_mapping_json: provider.response_mapping_json,
            max_results: provider.max_results,
        });
    }
    if configured_override.include_builtin_fallback
        && !out
            .iter()
            .any(|provider| provider.provider == ApiSearchProvider::Builtin)
    {
        out.push(builtin_configured_search_provider());
    }
    Ok(Some(out))
}

fn provider_requires_secret(provider: ApiSearchProvider, auth_type: Option<&str>) -> bool {
    let normalized_auth = auth_type
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("api_key")
        .to_ascii_lowercase();
    if matches!(normalized_auth.as_str(), "none" | "no_auth" | "anonymous") {
        return false;
    }
    provider.requires_api_key()
}

fn provider_type_to_api_search_provider(
    provider_type: &WebSearchProviderType,
) -> Option<ApiSearchProvider> {
    match provider_type {
        WebSearchProviderType::Builtin => Some(ApiSearchProvider::Builtin),
        WebSearchProviderType::Brave => Some(ApiSearchProvider::Brave),
        WebSearchProviderType::Tavily => Some(ApiSearchProvider::Tavily),
        WebSearchProviderType::Serper => Some(ApiSearchProvider::Serper),
        WebSearchProviderType::Exa => Some(ApiSearchProvider::Exa),
        WebSearchProviderType::Searxng => Some(ApiSearchProvider::Searxng),
        WebSearchProviderType::GenericJson => Some(ApiSearchProvider::GenericJson),
        WebSearchProviderType::InternalHttp => Some(ApiSearchProvider::InternalHttp),
        WebSearchProviderType::DemoSearch => Some(ApiSearchProvider::Demo),
    }
}

fn read_provider_order() -> Vec<ApiSearchProvider> {
    let mut out = Vec::<ApiSearchProvider>::new();
    if let Some(raw) = read_trimmed_env("AOSD_WEB_SEARCH_PROVIDER_ORDER") {
        for token in raw.split(',') {
            let Some(provider) = parse_api_search_provider(token) else {
                continue;
            };
            if !out.contains(&provider) {
                out.push(provider);
            }
        }
    }
    if out.is_empty() {
        out = vec![
            ApiSearchProvider::Brave,
            ApiSearchProvider::Tavily,
            ApiSearchProvider::Serper,
            ApiSearchProvider::Exa,
        ];
    }
    if read_env_bool("AOSD_DEMO_SEARCH_ENABLED", false) && !out.contains(&ApiSearchProvider::Demo) {
        out.push(ApiSearchProvider::Demo);
    }
    out
}

fn parse_api_search_provider(raw: &str) -> Option<ApiSearchProvider> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "builtin" | "aos_builtin" => Some(ApiSearchProvider::Builtin),
        "brave" => Some(ApiSearchProvider::Brave),
        "tavily" => Some(ApiSearchProvider::Tavily),
        "serper" => Some(ApiSearchProvider::Serper),
        "exa" => Some(ApiSearchProvider::Exa),
        "searxng" => Some(ApiSearchProvider::Searxng),
        "generic_json" => Some(ApiSearchProvider::GenericJson),
        "internal_http" => Some(ApiSearchProvider::InternalHttp),
        "demo" | "demo_search" => Some(ApiSearchProvider::Demo),
        _ => None,
    }
}

fn missing_provider_key_error(provider: ApiSearchProvider) -> String {
    if provider == ApiSearchProvider::Builtin {
        return "AOS built-in web search does not require an API key".to_string();
    }
    if provider == ApiSearchProvider::Demo {
        return "demo_search provider is disabled; set AOSD_DEMO_SEARCH_ENABLED=true for demo/e2e only".to_string();
    }
    let mut hints = provider.api_key_env_candidates().join(", ");
    let multi = provider.api_keys_env_candidates().join(", ");
    if !multi.is_empty() {
        hints = format!("{hints}, {multi}");
    }
    format!(
        "web search provider `{}` is selected but no API key is configured; set one of: {}",
        provider.as_str(),
        hints
    )
}

fn read_provider_api_key(provider: ApiSearchProvider) -> Option<String> {
    read_provider_api_keys(provider).and_then(|mut keys| keys.drain(..).next())
}

fn split_provider_api_keys(raw: &str) -> Vec<String> {
    raw.split(|c: char| c == ',' || c == ';' || c == '\n' || c == '\r' || c.is_whitespace())
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(std::string::ToString::to_string)
        .collect()
}

fn read_provider_api_keys(provider: ApiSearchProvider) -> Option<Vec<String>> {
    if provider == ApiSearchProvider::Demo {
        return read_env_bool("AOSD_DEMO_SEARCH_ENABLED", false)
            .then(|| vec!["demo_search".to_string()]);
    }
    let mut out: Vec<String> = Vec::new();
    for env_name in provider.api_keys_env_candidates() {
        if let Some(raw) = read_trimmed_env(env_name) {
            for key in split_provider_api_keys(&raw) {
                if !out.iter().any(|existing| existing == &key) {
                    out.push(key);
                }
            }
        }
    }
    for env_name in provider.api_key_env_candidates() {
        if let Some(key) = read_trimmed_env(env_name) {
            if !out.iter().any(|existing| existing == &key) {
                out.push(key);
            }
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

fn api_key_masked_signature(key: &str) -> String {
    let trimmed = key.trim();
    let total = trimmed.chars().count();
    if total <= 2 {
        return trimmed.to_string();
    }
    let prefix: String = trimmed.chars().take(2).collect();
    let suffix: String = trimmed
        .chars()
        .skip(total.saturating_sub(2))
        .collect::<String>();
    format!("{prefix}..{suffix}:len{total}")
}

fn provider_key_identity(provider: ApiSearchProvider, key: &str) -> String {
    format!("{}#{}", provider.as_str(), api_key_masked_signature(key))
}

fn read_web_search_key_cooldown_secs() -> u64 {
    read_timeout_env_secs("AOSD_WEB_SEARCH_KEY_COOLDOWN_SECS", 75, 0, 3_600)
}

fn is_provider_key_cooldown_error(error: &ApiProviderError) -> bool {
    if !error.retryable {
        return false;
    }
    let msg = error.message.to_ascii_lowercase();
    msg.contains("http_status=429")
        || msg.contains("too many requests")
        || msg.contains("rate limit")
        || msg.contains("rate-limit")
        || msg.contains("quota")
}

fn apply_provider_key_cooldown(
    provider: ApiSearchProvider,
    key: &str,
    cooldown_secs: u64,
    blocked_until: &mut BTreeMap<String, SystemTime>,
    attempts: &mut Vec<String>,
) {
    if cooldown_secs == 0 {
        return;
    }
    let identity = provider_key_identity(provider, key);
    let until = SystemTime::now() + Duration::from_secs(cooldown_secs);
    blocked_until.insert(identity.clone(), until);
    attempts.push(format!(
        "{}: key {} cooled_down_for={}s",
        provider.as_str(),
        identity,
        cooldown_secs
    ));
}

fn append_available_api_keys(
    target: &mut Vec<String>,
    provider: ApiSearchProvider,
    blocked_until: &BTreeMap<String, SystemTime>,
    attempts: &mut Vec<String>,
) {
    let now = SystemTime::now();
    if let Some(keys) = read_provider_api_keys(provider) {
        for key in keys {
            let identity = provider_key_identity(provider, &key);
            if let Some(until) = blocked_until.get(&identity) {
                if *until > now {
                    let remaining = until
                        .duration_since(now)
                        .unwrap_or_else(|_| Duration::from_secs(0))
                        .as_secs();
                    attempts.push(format!(
                        "{}: key {} skipped_cooldown_remaining={}s",
                        provider.as_str(),
                        identity,
                        remaining
                    ));
                    continue;
                }
            }
            target.push(key);
        }
    }
}

fn available_keys_for_provider(
    configured: &ConfiguredApiSearchProvider,
    blocked_until: &BTreeMap<String, SystemTime>,
    attempts: &mut Vec<String>,
) -> Vec<String> {
    if configured.api_keys.is_empty()
        && !provider_requires_secret(configured.provider, configured.auth_type.as_deref())
    {
        return vec![String::new()];
    }
    let mut out = Vec::new();
    let now = SystemTime::now();
    for key in &configured.api_keys {
        let identity = provider_key_identity(configured.provider, key);
        if let Some(until) = blocked_until.get(&identity) {
            if *until > now {
                let remaining = until
                    .duration_since(now)
                    .unwrap_or_else(|_| Duration::from_secs(0))
                    .as_secs();
                attempts.push(format!(
                    "{}: key {} skipped_cooldown_remaining={}s",
                    configured.provider.as_str(),
                    identity,
                    remaining
                ));
                continue;
            }
        }
        out.push(key.clone());
    }
    if out.is_empty() {
        append_available_api_keys(&mut out, configured.provider, blocked_until, attempts);
    }
    out
}

fn have_any_provider_key_available(
    providers: &[ConfiguredApiSearchProvider],
    blocked_until: &BTreeMap<String, SystemTime>,
) -> bool {
    let now = SystemTime::now();
    for configured in providers {
        if !provider_requires_secret(configured.provider, configured.auth_type.as_deref()) {
            return true;
        }
        for key in &configured.api_keys {
            let identity = provider_key_identity(configured.provider, key);
            if !blocked_until
                .get(&identity)
                .is_some_and(|until| *until > now)
            {
                return true;
            }
        }
        if let Some(env_keys) = read_provider_api_keys(configured.provider) {
            for key in env_keys {
                let identity = provider_key_identity(configured.provider, &key);
                if !blocked_until
                    .get(&identity)
                    .is_some_and(|until| *until > now)
                {
                    return true;
                }
            }
        }
    }
    false
}

fn earliest_provider_key_retry(blocked_until: &BTreeMap<String, SystemTime>) -> Option<Duration> {
    let now = SystemTime::now();
    blocked_until
        .values()
        .filter_map(|until| until.duration_since(now).ok())
        .min()
}

fn provider_endpoint(
    configured: &ConfiguredApiSearchProvider,
    provider: ApiSearchProvider,
) -> Result<reqwest::Url, ApiProviderError> {
    let endpoint = configured
        .base_url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| read_trimmed_env(provider.endpoint_env()))
        .unwrap_or_else(|| provider.default_endpoint().to_string());
    reqwest::Url::parse(&endpoint)
        .map_err(|error| ApiProviderError::new(format!("invalid_endpoint={error}"), false, None))
}

fn read_web_search_max_results() -> usize {
    let configured = read_bounded_env_usize("AOSD_WEB_SEARCH_MAX_RESULTS", 20, 1, 40);
    let enrich_cfg = WebSearchEnrichmentConfig::from_env();
    if enrich_cfg.enabled {
        configured.max(enrich_cfg.max_fetch_candidates).min(40)
    } else {
        configured
    }
}

fn execute_web_search_via_api_provider(
    configured: &ConfiguredApiSearchProvider,
    api_key: &str,
    input: &WebSearchInput,
    client: &Client,
) -> Result<Vec<SearchHit>, ApiProviderError> {
    let max_results = configured
        .max_results
        .unwrap_or_else(read_web_search_max_results);
    let mut hits = match configured.provider {
        ApiSearchProvider::Builtin => execute_builtin_search(input, client, max_results)?,
        ApiSearchProvider::Brave => {
            execute_brave_search(configured, api_key, input, client, max_results)?
        }
        ApiSearchProvider::Tavily => {
            execute_tavily_search(configured, api_key, input, client, max_results)?
        }
        ApiSearchProvider::Serper => {
            execute_serper_search(configured, api_key, input, client, max_results)?
        }
        ApiSearchProvider::Exa => {
            execute_exa_search(configured, api_key, input, client, max_results)?
        }
        ApiSearchProvider::Searxng => {
            execute_searxng_search(configured, input, client, max_results)?
        }
        ApiSearchProvider::GenericJson | ApiSearchProvider::InternalHttp => {
            execute_generic_json_search(configured, api_key, input, client, max_results)?
        }
        ApiSearchProvider::Demo => execute_demo_search(input, max_results),
    };
    dedupe_hits(&mut hits);
    hits.retain(|hit| !looks_like_search_navigation_url(&hit.url));
    Ok(hits)
}

fn execute_builtin_search(
    input: &WebSearchInput,
    client: &Client,
    max_results: usize,
) -> Result<Vec<SearchHit>, ApiProviderError> {
    let weather_required = is_weather_search_query(&input.query);
    type SourceResult = (&'static str, Result<Vec<SearchHit>, ApiProviderError>);
    let (sender, receiver) = std::sync::mpsc::channel::<SourceResult>();
    macro_rules! spawn_source {
        ($name:literal, $executor:ident) => {{
            let sender = sender.clone();
            let input = input.clone();
            let client = client.clone();
            std::thread::spawn(move || {
                let result = $executor(&input, &client, max_results);
                let _ = sender.send(($name, result));
            });
        }};
    }
    spawn_source!("duckduckgo_html", execute_builtin_duckduckgo_search);
    spawn_source!("brave_html", execute_builtin_brave_search);
    spawn_source!("bing_html", execute_builtin_bing_search);
    spawn_source!("open_meteo_weather", execute_builtin_weather_search);
    spawn_source!("wikipedia", execute_builtin_wikipedia_search);
    spawn_source!("hacker_news", execute_builtin_hacker_news_search);
    spawn_source!("stack_exchange", execute_builtin_stack_exchange_search);
    drop(sender);

    let source_count = 7_usize;
    let source_timeout_secs =
        read_timeout_env_secs("AOSD_BUILTIN_SEARCH_SOURCE_TIMEOUT_SECS", 8, 3, 20);
    let multi_source_grace_ms = read_bounded_env_usize(
        "AOSD_BUILTIN_SEARCH_MULTI_SOURCE_GRACE_MS",
        1_200,
        100,
        5_000,
    ) as u64;
    let started = Instant::now();
    let hard_deadline = started + Duration::from_secs(source_timeout_secs + 1);
    // Fast specialist sources must not terminate the search before a general
    // web or first-party vertical source has had a chance to answer. This was
    // especially harmful for live queries: Wikipedia/Stack Overflow could win
    // the race while DuckDuckGo/Brave or the weather adapter were still in
    // flight, leaving only stale or unrelated evidence.
    let mut first_primary_success_at: Option<Instant> = None;
    let mut weather_completed = !weather_required;
    let mut completed = 0_usize;
    let mut hits = Vec::new();
    let mut errors = Vec::new();
    while completed < source_count {
        let deadline = if weather_required && !weather_completed {
            hard_deadline
        } else {
            first_primary_success_at
                .map(|instant| {
                    (instant + Duration::from_millis(multi_source_grace_ms)).min(hard_deadline)
                })
                .unwrap_or(hard_deadline)
        };
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            break;
        };
        let Ok((source, result)) = receiver.recv_timeout(remaining) else {
            break;
        };
        completed += 1;
        if source == "open_meteo_weather" {
            weather_completed = true;
        }
        match result {
            Ok(found) if !found.is_empty() => {
                if weather_required && source == "open_meteo_weather" {
                    // Structured first-party forecast data is sufficient for a
                    // simple weather lookup and is materially safer than mixing
                    // in stale city pages or unrelated search-engine results.
                    hits = found;
                    break;
                }
                if !weather_required
                    && matches!(source, "duckduckgo_html" | "brave_html" | "bing_html")
                    && first_primary_success_at.is_none()
                {
                    first_primary_success_at = Some(Instant::now());
                }
                hits.extend(found);
                dedupe_hits(&mut hits);
            }
            Ok(_) => errors.push(format!("{source}: no results")),
            Err(error) => errors.push(format!("{source}: {}", error.message)),
        }
    }
    dedupe_hits(&mut hits);

    // Weather is a first-party structured vertical. Once it succeeds, stale
    // encyclopaedia or generic HTML matches must not crowd it out or be sent to
    // the model as if they were current forecast evidence.
    if weather_required
        && hits
            .iter()
            .any(|hit| hit.content_source.as_deref() == Some("open_meteo_forecast"))
    {
        hits.retain(|hit| hit.content_source.as_deref() == Some("open_meteo_forecast"));
    }

    if hits.is_empty() {
        if completed < source_count {
            errors.push(format!(
                "{} source(s) did not finish within {}s",
                source_count - completed,
                source_timeout_secs + 1
            ));
        }
        return Err(ApiProviderError::new(
            format!(
                "all built-in search sources failed or returned no results: {}",
                errors.join(" | ")
            ),
            true,
            None,
        ));
    }
    hits.truncate(max_results.saturating_mul(2).max(max_results));
    Ok(hits)
}

fn execute_builtin_weather_search(
    input: &WebSearchInput,
    client: &Client,
    _max_results: usize,
) -> Result<Vec<SearchHit>, ApiProviderError> {
    if !is_weather_search_query(&input.query) {
        return Ok(Vec::new());
    }

    let geocoding_endpoint = read_trimmed_env("AOSD_BUILTIN_WEATHER_GEOCODING_URL")
        .unwrap_or_else(|| "https://geocoding-api.open-meteo.com/v1/search".to_string());
    let mut selected_location: Option<Value> = None;
    for candidate in weather_location_candidates(&input.query) {
        let mut url = reqwest::Url::parse(&geocoding_endpoint).map_err(|error| {
            ApiProviderError::new(
                format!("invalid_weather_geocoding_endpoint={error}"),
                false,
                None,
            )
        })?;
        {
            let mut pairs = url.query_pairs_mut();
            pairs.append_pair("name", &candidate);
            pairs.append_pair("count", "10");
            pairs.append_pair(
                "language",
                if query_contains_cjk(&candidate) {
                    "zh"
                } else {
                    "en"
                },
            );
            pairs.append_pair("format", "json");
        }
        let response = client
            .get(url)
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .map_err(map_provider_request_error)?;
        let payload = parse_success_json_response(response)?;
        let Some(locations) = payload.get("results").and_then(Value::as_array) else {
            continue;
        };
        selected_location = locations
            .iter()
            .max_by_key(|location| weather_location_relevance(&input.query, location))
            .cloned();
        if selected_location.is_some() {
            break;
        }
    }

    let Some(location) = selected_location else {
        return Ok(Vec::new());
    };
    let latitude = location
        .get("latitude")
        .and_then(Value::as_f64)
        .ok_or_else(|| {
            ApiProviderError::new("weather_location_missing_latitude".to_string(), false, None)
        })?;
    let longitude = location
        .get("longitude")
        .and_then(Value::as_f64)
        .ok_or_else(|| {
            ApiProviderError::new(
                "weather_location_missing_longitude".to_string(),
                false,
                None,
            )
        })?;
    let timezone = location
        .get("timezone")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("auto");

    let forecast_endpoint = read_trimmed_env("AOSD_BUILTIN_WEATHER_FORECAST_URL")
        .unwrap_or_else(|| "https://api.open-meteo.com/v1/forecast".to_string());
    let mut forecast_url = reqwest::Url::parse(&forecast_endpoint).map_err(|error| {
        ApiProviderError::new(
            format!("invalid_weather_forecast_endpoint={error}"),
            false,
            None,
        )
    })?;
    {
        let mut pairs = forecast_url.query_pairs_mut();
        pairs.append_pair("latitude", &latitude.to_string());
        pairs.append_pair("longitude", &longitude.to_string());
        pairs.append_pair(
            "current",
            "temperature_2m,apparent_temperature,relative_humidity_2m,precipitation,weather_code,wind_speed_10m",
        );
        pairs.append_pair(
            "daily",
            "weather_code,temperature_2m_max,temperature_2m_min,precipitation_probability_max",
        );
        pairs.append_pair("timezone", timezone);
        pairs.append_pair("forecast_days", "7");
    }
    let response = client
        .get(forecast_url.clone())
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .map_err(map_provider_request_error)?;
    let forecast = parse_success_json_response(response)?;
    let summary = render_open_meteo_weather_summary(&location, &forecast);
    if summary.is_empty() {
        return Ok(Vec::new());
    }

    let place = weather_location_label(&location);
    let mut hit = search_hit(
        format!("Open-Meteo weather forecast for {place}"),
        forecast_url.to_string(),
        Some(summary.clone()),
    );
    hit.content = Some(summary.clone());
    hit.content_chars = Some(summary.chars().count());
    hit.content_source = Some("open_meteo_forecast".to_string());
    hit.relevance_score = Some(1.0);
    Ok(vec![hit])
}

fn is_weather_search_query(query: &str) -> bool {
    let normalized = query.to_ascii_lowercase();
    let has_weather_marker = [
        "weather",
        "forecast",
        "temperature",
        "rainfall",
        "precipitation",
        "天气",
        "气温",
        "温度",
        "降雨",
        "降水",
        "下雨",
    ]
    .iter()
    .any(|marker| normalized.contains(marker));
    has_weather_marker
        || normalized
            .split(|ch: char| !ch.is_ascii_alphanumeric())
            .any(|token| matches!(token, "rain" | "snow" | "storm"))
}

fn weather_location_candidates(query: &str) -> Vec<String> {
    let mut cleaned = query.to_ascii_lowercase();
    for marker in [
        "weather forecast",
        "current weather",
        "weather",
        "forecast",
        "temperature",
        "rainfall",
        "precipitation",
        "rain",
        "snow",
        "tomorrow",
        "today",
        "tonight",
        "天气预报",
        "实时天气",
        "当前天气",
        "天气",
        "气温",
        "温度",
        "降雨",
        "降水",
        "下雨",
        "未来一周",
        "未来7天",
        "未来七天",
        "今天",
        "今日",
        "明天",
        "今晚",
        "实时",
        "当前",
        "查询",
        "查一下",
        "请问",
    ] {
        cleaned = cleaned.replace(marker, " ");
    }
    let cleaned = cleaned
        .split(|ch: char| ch.is_ascii_punctuation() && !matches!(ch, '-' | '_' | '\'' | '.'))
        .flat_map(str::split_whitespace)
        .collect::<Vec<_>>()
        .join(" ");
    let mut candidates = Vec::new();
    let mut seen = BTreeSet::new();
    let mut push = |candidate: String| {
        let candidate = candidate.trim().to_string();
        if candidate.chars().count() >= 2 && seen.insert(candidate.clone()) {
            candidates.push(candidate);
        }
    };
    push(cleaned.clone());

    let cjk = cleaned
        .chars()
        .filter(|ch| ('\u{3400}'..='\u{9fff}').contains(ch))
        .collect::<String>();
    let cjk_chars = cjk.chars().collect::<Vec<_>>();
    for length in [8_usize, 6, 4, 3, 2] {
        if cjk_chars.len() > length && (length == 2 || cjk_chars.len().saturating_sub(length) >= 2)
        {
            push(cjk_chars[cjk_chars.len() - length..].iter().collect());
        }
    }
    // Composite Chinese place names are often written as province/city + district
    // without separators. Geocoders may index either component but not the joined
    // form, so retain bounded administrative-prefix fallbacks as well.
    for length in [2_usize, 3, 4, 6, 8] {
        if cjk_chars.len() > length && cjk_chars.len().saturating_sub(length) >= 2 {
            push(cjk_chars[..length].iter().collect());
        }
    }
    candidates.truncate(6);
    candidates
}

fn weather_location_relevance(query: &str, location: &Value) -> usize {
    let normalized_query = query.to_ascii_lowercase();
    ["name", "admin1", "admin2", "admin3", "country"]
        .into_iter()
        .filter_map(|key| location.get(key).and_then(Value::as_str))
        .map(|value| value.trim().trim_end_matches(['市', '区', '县', '省']))
        .filter(|value| !value.is_empty() && normalized_query.contains(&value.to_ascii_lowercase()))
        .map(|value| value.chars().count())
        .sum()
}

fn weather_location_label(location: &Value) -> String {
    let mut parts = Vec::new();
    for key in ["name", "admin2", "admin1", "country"] {
        if let Some(value) = location
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            if !parts.iter().any(|part| *part == value) {
                parts.push(value);
            }
        }
    }
    parts.join(", ")
}

fn weather_number(value: Option<&Value>) -> Option<String> {
    value.and_then(Value::as_f64).map(|number| {
        if number.fract().abs() < f64::EPSILON {
            format!("{number:.0}")
        } else {
            format!("{number:.1}")
        }
    })
}

fn weather_array_value<'a>(value: &'a Value, key: &str, index: usize) -> Option<&'a Value> {
    value.get(key)?.as_array()?.get(index)
}

fn wmo_weather_description(code: i64) -> &'static str {
    match code {
        0 => "clear sky",
        1 => "mainly clear",
        2 => "partly cloudy",
        3 => "overcast",
        45 | 48 => "fog",
        51 | 53 | 55 => "drizzle",
        56 | 57 => "freezing drizzle",
        61 | 63 | 65 => "rain",
        66 | 67 => "freezing rain",
        71 | 73 | 75 => "snowfall",
        77 => "snow grains",
        80 | 81 | 82 => "rain showers",
        85 | 86 => "snow showers",
        95 => "thunderstorm",
        96 | 99 => "thunderstorm with hail",
        _ => "unknown conditions",
    }
}

fn render_open_meteo_weather_summary(location: &Value, forecast: &Value) -> String {
    let place = weather_location_label(location);
    let timezone = forecast
        .get("timezone")
        .and_then(Value::as_str)
        .unwrap_or("local time");
    let mut lines = vec![format!(
        "Weather data for {place} from Open-Meteo (CC BY 4.0), timezone {timezone}."
    )];
    if let Some(current) = forecast.get("current") {
        let observed_at = current
            .get("time")
            .and_then(Value::as_str)
            .unwrap_or("unknown time");
        let code = current
            .get("weather_code")
            .and_then(Value::as_i64)
            .unwrap_or(-1);
        let temperature =
            weather_number(current.get("temperature_2m")).unwrap_or_else(|| "unknown".to_string());
        let apparent = weather_number(current.get("apparent_temperature"))
            .unwrap_or_else(|| "unknown".to_string());
        let humidity = weather_number(current.get("relative_humidity_2m"))
            .unwrap_or_else(|| "unknown".to_string());
        let precipitation =
            weather_number(current.get("precipitation")).unwrap_or_else(|| "unknown".to_string());
        let wind =
            weather_number(current.get("wind_speed_10m")).unwrap_or_else(|| "unknown".to_string());
        lines.push(format!(
            "Current at {observed_at}: {temperature} C, feels like {apparent} C, {}, humidity {humidity}%, precipitation {precipitation} mm, wind {wind} km/h.",
            wmo_weather_description(code)
        ));
    }
    if let Some(daily) = forecast.get("daily") {
        let day_count = daily
            .get("time")
            .and_then(Value::as_array)
            .map_or(0, Vec::len)
            .min(7);
        for index in 0..day_count {
            let date = weather_array_value(daily, "time", index)
                .and_then(Value::as_str)
                .unwrap_or("unknown date");
            let code = weather_array_value(daily, "weather_code", index)
                .and_then(Value::as_i64)
                .unwrap_or(-1);
            let high = weather_number(weather_array_value(daily, "temperature_2m_max", index))
                .unwrap_or_else(|| "unknown".to_string());
            let low = weather_number(weather_array_value(daily, "temperature_2m_min", index))
                .unwrap_or_else(|| "unknown".to_string());
            let rain = weather_number(weather_array_value(
                daily,
                "precipitation_probability_max",
                index,
            ))
            .unwrap_or_else(|| "unknown".to_string());
            lines.push(format!(
                "{date}: {}, high {high} C, low {low} C, max precipitation probability {rain}%.",
                wmo_weather_description(code)
            ));
        }
    }
    lines.join("\n")
}

fn execute_builtin_duckduckgo_search(
    input: &WebSearchInput,
    client: &Client,
    max_results: usize,
) -> Result<Vec<SearchHit>, ApiProviderError> {
    let endpoint = read_trimmed_env("AOSD_BUILTIN_SEARCH_DDG_URL")
        .unwrap_or_else(|| "https://html.duckduckgo.com/html/".to_string());
    let mut url = reqwest::Url::parse(&endpoint)
        .map_err(|error| ApiProviderError::new(format!("invalid_endpoint={error}"), false, None))?;
    url.query_pairs_mut().append_pair("q", &input.query);
    let response = client
        .get(url)
        .timeout(Duration::from_secs(read_timeout_env_secs(
            "AOSD_BUILTIN_SEARCH_SOURCE_TIMEOUT_SECS",
            8,
            3,
            20,
        )))
        .header(reqwest::header::ACCEPT, "text/html,application/xhtml+xml")
        .send()
        .map_err(map_provider_request_error)?;
    let body = read_success_text_response(response)?;
    parse_duckduckgo_html_hits(&body, max_results)
}

fn execute_builtin_brave_search(
    input: &WebSearchInput,
    client: &Client,
    max_results: usize,
) -> Result<Vec<SearchHit>, ApiProviderError> {
    let endpoint = read_trimmed_env("AOSD_BUILTIN_SEARCH_BRAVE_URL")
        .unwrap_or_else(|| "https://search.brave.com/search".to_string());
    let mut url = reqwest::Url::parse(&endpoint)
        .map_err(|error| ApiProviderError::new(format!("invalid_endpoint={error}"), false, None))?;
    {
        let mut pairs = url.query_pairs_mut();
        pairs.append_pair("q", &input.query);
        pairs.append_pair("source", "web");
    }
    let response = client
        .get(url)
        .timeout(Duration::from_secs(read_timeout_env_secs(
            "AOSD_BUILTIN_SEARCH_SOURCE_TIMEOUT_SECS",
            8,
            3,
            20,
        )))
        .header(reqwest::header::ACCEPT, "text/html,application/xhtml+xml")
        .send()
        .map_err(map_provider_request_error)?;
    let body = read_success_text_response(response)?;
    parse_brave_html_hits(&body, max_results)
}

fn execute_builtin_bing_search(
    input: &WebSearchInput,
    client: &Client,
    max_results: usize,
) -> Result<Vec<SearchHit>, ApiProviderError> {
    let endpoint = read_trimmed_env("AOSD_BUILTIN_SEARCH_BING_URL")
        .unwrap_or_else(|| "https://www.bing.com/search".to_string());
    let mut url = reqwest::Url::parse(&endpoint)
        .map_err(|error| ApiProviderError::new(format!("invalid_endpoint={error}"), false, None))?;
    {
        let mut pairs = url.query_pairs_mut();
        pairs.append_pair("q", &input.query);
        pairs.append_pair("count", &max_results.min(20).to_string());
    }
    let response = client
        .get(url)
        .timeout(Duration::from_secs(read_timeout_env_secs(
            "AOSD_BUILTIN_SEARCH_SOURCE_TIMEOUT_SECS",
            8,
            3,
            20,
        )))
        .header(reqwest::header::ACCEPT, "text/html,application/xhtml+xml")
        .send()
        .map_err(map_provider_request_error)?;
    let body = read_success_text_response(response)?;
    parse_bing_html_hits(&body, max_results)
}

fn execute_builtin_hacker_news_search(
    input: &WebSearchInput,
    client: &Client,
    max_results: usize,
) -> Result<Vec<SearchHit>, ApiProviderError> {
    let endpoint = read_trimmed_env("AOSD_BUILTIN_SEARCH_HN_URL")
        .unwrap_or_else(|| "https://hn.algolia.com/api/v1/search".to_string());
    let mut url = reqwest::Url::parse(&endpoint)
        .map_err(|error| ApiProviderError::new(format!("invalid_endpoint={error}"), false, None))?;
    {
        let mut pairs = url.query_pairs_mut();
        pairs.append_pair("query", &public_api_search_query(&input.query));
        pairs.append_pair("hitsPerPage", &max_results.min(20).to_string());
    }
    let response = client
        .get(url)
        .timeout(Duration::from_secs(read_timeout_env_secs(
            "AOSD_BUILTIN_SEARCH_SOURCE_TIMEOUT_SECS",
            8,
            3,
            20,
        )))
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .map_err(map_provider_request_error)?;
    let payload = parse_success_json_response(response)?;
    Ok(parse_hacker_news_hits(&payload, max_results))
}

fn execute_builtin_stack_exchange_search(
    input: &WebSearchInput,
    client: &Client,
    max_results: usize,
) -> Result<Vec<SearchHit>, ApiProviderError> {
    let endpoint = read_trimmed_env("AOSD_BUILTIN_SEARCH_STACK_EXCHANGE_URL")
        .unwrap_or_else(|| "https://api.stackexchange.com/2.3/search/advanced".to_string());
    let mut url = reqwest::Url::parse(&endpoint)
        .map_err(|error| ApiProviderError::new(format!("invalid_endpoint={error}"), false, None))?;
    {
        let mut pairs = url.query_pairs_mut();
        pairs.append_pair("q", &public_api_search_query(&input.query));
        pairs.append_pair("site", "stackoverflow");
        pairs.append_pair("order", "desc");
        pairs.append_pair("sort", "relevance");
        pairs.append_pair("pagesize", &max_results.min(20).to_string());
    }
    let response = client
        .get(url)
        .timeout(Duration::from_secs(read_timeout_env_secs(
            "AOSD_BUILTIN_SEARCH_SOURCE_TIMEOUT_SECS",
            8,
            3,
            20,
        )))
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .map_err(map_provider_request_error)?;
    let payload = parse_success_json_response(response)?;
    Ok(parse_stack_exchange_hits(&payload, max_results))
}

fn execute_builtin_wikipedia_search(
    input: &WebSearchInput,
    client: &Client,
    max_results: usize,
) -> Result<Vec<SearchHit>, ApiProviderError> {
    let has_cjk = query_contains_cjk(&input.query);
    let endpoint = if let Some(endpoint) = read_trimmed_env("AOSD_BUILTIN_SEARCH_WIKIPEDIA_URL") {
        endpoint
    } else if has_cjk {
        "https://zh.wikipedia.org/w/api.php".to_string()
    } else {
        "https://en.wikipedia.org/w/api.php".to_string()
    };
    let mut url = reqwest::Url::parse(&endpoint)
        .map_err(|error| ApiProviderError::new(format!("invalid_endpoint={error}"), false, None))?;
    {
        let mut pairs = url.query_pairs_mut();
        pairs.append_pair("action", "query");
        pairs.append_pair("list", "search");
        pairs.append_pair("srsearch", &input.query);
        pairs.append_pair("utf8", "1");
        pairs.append_pair("format", "json");
        pairs.append_pair("srlimit", &max_results.min(10).to_string());
    }
    let response = client
        .get(url)
        .timeout(Duration::from_secs(read_timeout_env_secs(
            "AOSD_BUILTIN_SEARCH_SOURCE_TIMEOUT_SECS",
            8,
            3,
            20,
        )))
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .map_err(map_provider_request_error)?;
    let payload = parse_success_json_response(response)?;
    let language = if has_cjk { "zh" } else { "en" };
    let mut hits = Vec::new();
    if let Some(items) = payload
        .get("query")
        .and_then(|value| value.get("search"))
        .and_then(Value::as_array)
    {
        for item in items.iter().take(max_results) {
            let Some(title) = item.get("title").and_then(Value::as_str) else {
                continue;
            };
            let mut page_url = reqwest::Url::parse(&format!(
                "https://{language}.wikipedia.org/wiki/"
            ))
            .map_err(|error| {
                ApiProviderError::new(format!("invalid_result_url={error}"), false, None)
            })?;
            page_url
                .path_segments_mut()
                .map_err(|()| {
                    ApiProviderError::new("invalid Wikipedia base URL".to_string(), false, None)
                })?
                .pop_if_empty()
                .push(&title.replace(' ', "_"));
            let snippet = item
                .get("snippet")
                .and_then(Value::as_str)
                .map(html_to_text)
                .filter(|value| !value.is_empty());
            hits.push(search_hit(title.to_string(), page_url.to_string(), snippet));
        }
    }
    Ok(hits)
}

fn read_success_text_response(
    response: reqwest::blocking::Response,
) -> Result<String, ApiProviderError> {
    let status = response.status();
    if !status.is_success() {
        let retry_after = parse_retry_after_header(response.headers());
        let body_preview = response
            .text()
            .map(|body| preview_text(&collapse_whitespace(&body), 220))
            .unwrap_or_default();
        let retryable =
            status.as_u16() == 408 || status.as_u16() == 429 || status.is_server_error();
        return Err(ApiProviderError::new(
            format!("http_status={status} body_preview={body_preview}"),
            retryable,
            retry_after,
        ));
    }
    response
        .text()
        .map_err(|error| ApiProviderError::new(format!("body_read_error={error}"), true, None))
}

fn query_contains_cjk(query: &str) -> bool {
    query
        .chars()
        .any(|ch| ('\u{3400}'..='\u{9fff}').contains(&ch))
}

fn public_api_search_query(query: &str) -> String {
    if !query_contains_cjk(query) {
        return query.to_string();
    }
    let ascii_terms = query
        .split(|ch: char| !ch.is_ascii_alphanumeric() && !matches!(ch, '-' | '_' | '.'))
        .map(str::trim)
        .filter(|term| term.len() >= 2)
        .collect::<Vec<_>>();
    if ascii_terms.is_empty() {
        query.to_string()
    } else {
        ascii_terms.join(" ")
    }
}

fn parse_hacker_news_hits(payload: &Value, max_results: usize) -> Vec<SearchHit> {
    payload
        .get("hits")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let title = item
                .get("title")
                .or_else(|| item.get("story_title"))
                .and_then(Value::as_str)
                .map(|value| collapse_whitespace(&html_to_text(value)))
                .filter(|value| !value.is_empty())?;
            let url = item
                .get("url")
                .or_else(|| item.get("story_url"))
                .and_then(Value::as_str)
                .and_then(normalize_public_result_url)
                .or_else(|| {
                    item.get("objectID")
                        .and_then(Value::as_str)
                        .map(|id| format!("https://news.ycombinator.com/item?id={id}"))
                })?;
            Some(search_hit(title, url, None))
        })
        .take(max_results)
        .collect()
}

fn parse_stack_exchange_hits(payload: &Value, max_results: usize) -> Vec<SearchHit> {
    payload
        .get("items")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let title = item
                .get("title")
                .and_then(Value::as_str)
                .map(|value| collapse_whitespace(&html_to_text(value)))
                .filter(|value| !value.is_empty())?;
            let url = item
                .get("link")
                .and_then(Value::as_str)
                .and_then(normalize_public_result_url)?;
            let snippet = item
                .get("tags")
                .and_then(Value::as_array)
                .map(|tags| {
                    format!(
                        "Stack Overflow tags: {}",
                        tags.iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                })
                .filter(|value| !value.ends_with(": "));
            Some(search_hit(title, url, snippet))
        })
        .take(max_results)
        .collect()
}

fn parse_duckduckgo_html_hits(
    body: &str,
    max_results: usize,
) -> Result<Vec<SearchHit>, ApiProviderError> {
    let result_selector = Selector::parse(".result.web-result")
        .map_err(|error| ApiProviderError::new(format!("selector_error={error}"), false, None))?;
    let link_selector = Selector::parse("a.result__a")
        .map_err(|error| ApiProviderError::new(format!("selector_error={error}"), false, None))?;
    let snippet_selector = Selector::parse(".result__snippet")
        .map_err(|error| ApiProviderError::new(format!("selector_error={error}"), false, None))?;
    let document = Html::parse_document(body);
    let mut hits = Vec::new();
    for result in document.select(&result_selector).take(max_results) {
        let Some(link) = result.select(&link_selector).next() else {
            continue;
        };
        let Some(href) = link
            .value()
            .attr("href")
            .and_then(normalize_duckduckgo_result_url)
        else {
            continue;
        };
        let title = collapse_whitespace(&link.text().collect::<Vec<_>>().join(" "));
        if title.is_empty() {
            continue;
        }
        let snippet = result
            .select(&snippet_selector)
            .next()
            .map(|node| collapse_whitespace(&node.text().collect::<Vec<_>>().join(" ")))
            .filter(|value| !value.is_empty());
        hits.push(search_hit(title, href, snippet));
    }
    Ok(hits)
}

fn parse_brave_html_hits(
    body: &str,
    max_results: usize,
) -> Result<Vec<SearchHit>, ApiProviderError> {
    let result_selector = Selector::parse(".snippet[data-type='web']")
        .map_err(|error| ApiProviderError::new(format!("selector_error={error}"), false, None))?;
    let link_selector = Selector::parse("a.l1")
        .map_err(|error| ApiProviderError::new(format!("selector_error={error}"), false, None))?;
    let title_selector = Selector::parse(".search-snippet-title")
        .map_err(|error| ApiProviderError::new(format!("selector_error={error}"), false, None))?;
    let snippet_selector = Selector::parse(".generic-snippet .content")
        .map_err(|error| ApiProviderError::new(format!("selector_error={error}"), false, None))?;
    let document = Html::parse_document(body);
    let mut hits = Vec::new();
    for result in document.select(&result_selector).take(max_results) {
        let Some(link) = result.select(&link_selector).next() else {
            continue;
        };
        let Some(url) = link
            .value()
            .attr("href")
            .and_then(normalize_public_result_url)
        else {
            continue;
        };
        let title = result
            .select(&title_selector)
            .next()
            .map(|node| collapse_whitespace(&node.text().collect::<Vec<_>>().join(" ")))
            .unwrap_or_default();
        if title.is_empty() {
            continue;
        }
        let snippet = result
            .select(&snippet_selector)
            .next()
            .map(|node| collapse_whitespace(&node.text().collect::<Vec<_>>().join(" ")))
            .filter(|value| !value.is_empty());
        hits.push(search_hit(title, url, snippet));
    }
    Ok(hits)
}

fn parse_bing_html_hits(
    body: &str,
    max_results: usize,
) -> Result<Vec<SearchHit>, ApiProviderError> {
    let result_selector = Selector::parse("li.b_algo")
        .map_err(|error| ApiProviderError::new(format!("selector_error={error}"), false, None))?;
    let link_selector = Selector::parse("h2 a[href]")
        .map_err(|error| ApiProviderError::new(format!("selector_error={error}"), false, None))?;
    let snippet_selector = Selector::parse(".b_caption p")
        .map_err(|error| ApiProviderError::new(format!("selector_error={error}"), false, None))?;
    let document = Html::parse_document(body);
    let mut hits = Vec::new();
    for result in document.select(&result_selector).take(max_results) {
        let Some(link) = result.select(&link_selector).next() else {
            continue;
        };
        let Some(url) = link
            .value()
            .attr("href")
            .and_then(normalize_public_result_url)
        else {
            continue;
        };
        let title = collapse_whitespace(&link.text().collect::<Vec<_>>().join(" "));
        if title.is_empty() {
            continue;
        }
        let snippet = result
            .select(&snippet_selector)
            .next()
            .map(|node| collapse_whitespace(&node.text().collect::<Vec<_>>().join(" ")))
            .filter(|value| !value.is_empty());
        hits.push(search_hit(title, url, snippet));
    }
    Ok(hits)
}

fn normalize_duckduckgo_result_url(raw: &str) -> Option<String> {
    let absolute = if raw.starts_with("//") {
        format!("https:{raw}")
    } else if raw.starts_with('/') {
        format!("https://duckduckgo.com{raw}")
    } else {
        raw.to_string()
    };
    let parsed = reqwest::Url::parse(&absolute).ok()?;
    if parsed
        .host_str()
        .is_some_and(|host| host.ends_with("duckduckgo.com"))
        && parsed.path().starts_with("/l/")
    {
        return parsed
            .query_pairs()
            .find_map(|(key, value)| (key == "uddg").then(|| value.into_owned()))
            .and_then(|value| normalize_public_result_url(&value));
    }
    normalize_public_result_url(parsed.as_str())
}

fn normalize_public_result_url(raw: &str) -> Option<String> {
    let parsed = reqwest::Url::parse(raw).ok()?;
    matches!(parsed.scheme(), "http" | "https").then(|| parsed.to_string())
}

fn search_hit(title: String, url: String, snippet: Option<String>) -> SearchHit {
    SearchHit {
        domain: extract_domain_from_url(&url),
        title,
        url,
        snippet,
        content: None,
        content_chars: None,
        content_source: None,
        relevance_score: None,
    }
}

fn execute_brave_search(
    configured: &ConfiguredApiSearchProvider,
    api_key: &str,
    input: &WebSearchInput,
    client: &Client,
    max_results: usize,
) -> Result<Vec<SearchHit>, ApiProviderError> {
    let mut url = provider_endpoint(configured, ApiSearchProvider::Brave)?;
    {
        let mut pairs = url.query_pairs_mut();
        pairs.append_pair("q", &input.query);
        pairs.append_pair("count", &max_results.to_string());
        if let Some(country) = read_trimmed_env("AOSD_WEB_SEARCH_COUNTRY") {
            pairs.append_pair("country", &country);
        }
        if let Some(language) = read_trimmed_env("AOSD_WEB_SEARCH_LANGUAGE") {
            pairs.append_pair("search_lang", &language);
        }
    }
    let response = client
        .get(url)
        .header(reqwest::header::ACCEPT, "application/json")
        .header("X-Subscription-Token", api_key)
        .send()
        .map_err(map_provider_request_error)?;
    let payload = parse_success_json_response(response)?;
    Ok(parse_hits_from_array(
        payload
            .get("web")
            .and_then(|value| value.get("results"))
            .and_then(Value::as_array),
        &["url", "link"],
        &["title", "name"],
        default_search_snippet_keys(),
    ))
}

fn execute_tavily_search(
    configured: &ConfiguredApiSearchProvider,
    api_key: &str,
    input: &WebSearchInput,
    client: &Client,
    max_results: usize,
) -> Result<Vec<SearchHit>, ApiProviderError> {
    let url = provider_endpoint(configured, ApiSearchProvider::Tavily)?;
    let mut body = json!({
        "api_key": api_key,
        "query": input.query,
        "search_depth": read_trimmed_env("AOSD_TAVILY_SEARCH_DEPTH").unwrap_or_else(|| "basic".to_string()),
        "max_results": max_results,
        "include_answer": read_env_bool("AOSD_TAVILY_INCLUDE_ANSWER", false),
        "include_raw_content": read_env_bool("AOSD_TAVILY_INCLUDE_RAW_CONTENT", false),
    });
    if let Some(allowed_domains) = normalize_domain_filters(input.allowed_domains.as_deref()) {
        body["include_domains"] = Value::Array(
            allowed_domains
                .into_iter()
                .map(Value::String)
                .collect::<Vec<_>>(),
        );
    }
    if let Some(blocked_domains) = normalize_domain_filters(input.blocked_domains.as_deref()) {
        body["exclude_domains"] = Value::Array(
            blocked_domains
                .into_iter()
                .map(Value::String)
                .collect::<Vec<_>>(),
        );
    }
    let response = client
        .post(url)
        .header(reqwest::header::ACCEPT, "application/json")
        .json(&body)
        .send()
        .map_err(map_provider_request_error)?;
    let payload = parse_success_json_response(response)?;
    Ok(parse_hits_from_array(
        payload.get("results").and_then(Value::as_array),
        &["url", "link"],
        &["title", "name"],
        default_search_snippet_keys(),
    ))
}

fn execute_serper_search(
    configured: &ConfiguredApiSearchProvider,
    api_key: &str,
    input: &WebSearchInput,
    client: &Client,
    max_results: usize,
) -> Result<Vec<SearchHit>, ApiProviderError> {
    let url = provider_endpoint(configured, ApiSearchProvider::Serper)?;
    let mut body = json!({
        "q": input.query,
        "num": max_results,
    });
    if let Some(location) = read_trimmed_env("AOSD_WEB_SEARCH_LOCATION") {
        body["location"] = Value::String(location);
    }
    if let Some(language) = read_trimmed_env("AOSD_WEB_SEARCH_LANGUAGE") {
        body["hl"] = Value::String(language);
    }
    if let Some(country) = read_trimmed_env("AOSD_WEB_SEARCH_COUNTRY") {
        body["gl"] = Value::String(country);
    }
    let response = client
        .post(url)
        .header(reqwest::header::ACCEPT, "application/json")
        .header("X-API-KEY", api_key)
        .json(&body)
        .send()
        .map_err(map_provider_request_error)?;
    let payload = parse_success_json_response(response)?;
    let mut hits = parse_hits_from_array(
        payload.get("organic").and_then(Value::as_array),
        &["link", "url"],
        &["title", "name"],
        default_search_snippet_keys(),
    );
    if hits.is_empty() {
        hits = parse_hits_from_array(
            payload.get("results").and_then(Value::as_array),
            &["link", "url"],
            &["title", "name"],
            default_search_snippet_keys(),
        );
    }
    Ok(hits)
}

fn execute_exa_search(
    configured: &ConfiguredApiSearchProvider,
    api_key: &str,
    input: &WebSearchInput,
    client: &Client,
    max_results: usize,
) -> Result<Vec<SearchHit>, ApiProviderError> {
    let url = provider_endpoint(configured, ApiSearchProvider::Exa)?;
    let mut body = json!({
        "query": input.query,
        "numResults": max_results,
        "contents": {
            "text": false,
            "highlights": true,
        },
    });
    if let Some(allowed_domains) = normalize_domain_filters(input.allowed_domains.as_deref()) {
        body["includeDomains"] = Value::Array(
            allowed_domains
                .into_iter()
                .map(Value::String)
                .collect::<Vec<_>>(),
        );
    }
    if let Some(blocked_domains) = normalize_domain_filters(input.blocked_domains.as_deref()) {
        body["excludeDomains"] = Value::Array(
            blocked_domains
                .into_iter()
                .map(Value::String)
                .collect::<Vec<_>>(),
        );
    }
    let response = client
        .post(url)
        .header(reqwest::header::ACCEPT, "application/json")
        .header("x-api-key", api_key)
        .json(&body)
        .send()
        .map_err(map_provider_request_error)?;
    let payload = parse_success_json_response(response)?;
    Ok(parse_hits_from_array(
        payload.get("results").and_then(Value::as_array),
        &["url", "link"],
        &["title", "name"],
        default_search_snippet_keys(),
    ))
}

fn execute_searxng_search(
    configured: &ConfiguredApiSearchProvider,
    input: &WebSearchInput,
    client: &Client,
    max_results: usize,
) -> Result<Vec<SearchHit>, ApiProviderError> {
    let mut url = provider_endpoint(configured, ApiSearchProvider::Searxng)?;
    {
        let mut pairs = url.query_pairs_mut();
        pairs.append_pair("q", &input.query);
        pairs.append_pair("format", "json");
        pairs.append_pair("safesearch", "0");
        if let Some(language) = read_trimmed_env("AOSD_WEB_SEARCH_LANGUAGE") {
            pairs.append_pair("language", &language);
        }
    }
    let response = client
        .get(url)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .map_err(map_provider_request_error)?;
    let payload = parse_success_json_response(response)?;
    let mut hits = parse_hits_from_array(
        payload.get("results").and_then(Value::as_array),
        &["url", "link"],
        &["title", "name"],
        default_search_snippet_keys(),
    );
    hits.truncate(max_results);
    Ok(hits)
}

fn execute_generic_json_search(
    configured: &ConfiguredApiSearchProvider,
    api_key: &str,
    input: &WebSearchInput,
    client: &Client,
    max_results: usize,
) -> Result<Vec<SearchHit>, ApiProviderError> {
    let mapping = configured
        .response_mapping_json
        .as_ref()
        .filter(|value| value.is_object());
    let request_template = configured
        .query_template_json
        .as_ref()
        .and_then(|value| value.get("request"))
        .or(configured.query_template_json.as_ref());
    let method = request_template
        .and_then(|value| value.get("method"))
        .and_then(Value::as_str)
        .or(configured.method.as_deref())
        .unwrap_or("POST")
        .trim()
        .to_ascii_uppercase();
    let endpoint = request_template
        .and_then(|value| value.get("url"))
        .and_then(Value::as_str)
        .or(configured.base_url.as_deref())
        .map(|raw| render_search_template_string(raw, input, api_key, max_results))
        .ok_or_else(|| ApiProviderError::new("missing_endpoint".to_string(), false, None))?;
    let url = reqwest::Url::parse(&endpoint)
        .map_err(|error| ApiProviderError::new(format!("invalid_endpoint={error}"), false, None))?;

    let mut req = match method.as_str() {
        "GET" => {
            let mut url = url;
            apply_generic_query_params(&mut url, request_template, input, max_results, api_key);
            client.get(url)
        }
        "POST" => client.post(url),
        "PUT" => client.put(url),
        other => {
            return Err(ApiProviderError::new(
                format!("unsupported_method={other}"),
                false,
                None,
            ));
        }
    }
    .header(reqwest::header::ACCEPT, "application/json");

    req = apply_configured_headers(
        req,
        configured,
        request_template,
        api_key,
        input,
        max_results,
    );
    req = apply_configured_auth(req, configured, api_key);

    if method != "GET" {
        let body = request_template
            .and_then(|value| value.get("body"))
            .cloned()
            .unwrap_or_else(
                || json!({ "q": "{{query}}", "query": "{{query}}", "limit": "{{maxResults}}" }),
            );
        req = req.json(&render_search_template_value(
            &body,
            input,
            api_key,
            max_results,
        ));
    }

    let response = req.send().map_err(map_provider_request_error)?;
    let payload = parse_success_json_response(response)?;
    let mut hits = parse_hits_from_mapped_response(&payload, mapping, max_results);
    if hits.is_empty() {
        hits = parse_hits_from_array(
            payload
                .get("results")
                .or_else(|| payload.get("items"))
                .and_then(Value::as_array),
            &["url", "link"],
            &["title", "name"],
            default_search_snippet_keys(),
        );
    }
    hits.truncate(max_results);
    Ok(hits)
}

fn apply_generic_query_params(
    url: &mut reqwest::Url,
    request_template: Option<&Value>,
    input: &WebSearchInput,
    max_results: usize,
    api_key: &str,
) {
    let Some(query_value) = request_template.and_then(|value| value.get("query")) else {
        let mut pairs = url.query_pairs_mut();
        pairs.append_pair("q", &input.query);
        pairs.append_pair("limit", &max_results.to_string());
        return;
    };
    if let Some(map) = query_value.as_object() {
        let mut pairs = url.query_pairs_mut();
        for (key, value) in map {
            let rendered = render_search_template_value(value, input, api_key, max_results);
            if let Some(text) = value_to_search_text(&rendered) {
                pairs.append_pair(key, &text);
            }
        }
    }
}

fn apply_configured_headers(
    mut req: reqwest::blocking::RequestBuilder,
    configured: &ConfiguredApiSearchProvider,
    request_template: Option<&Value>,
    api_key: &str,
    input: &WebSearchInput,
    max_results: usize,
) -> reqwest::blocking::RequestBuilder {
    let mut headers = BTreeMap::<String, Value>::new();
    if let Some(map) = configured.headers_json.as_ref().and_then(Value::as_object) {
        headers.extend(map.iter().map(|(key, value)| (key.clone(), value.clone())));
    }
    if let Some(map) = request_template
        .and_then(|value| value.get("headers"))
        .and_then(Value::as_object)
    {
        headers.extend(map.iter().map(|(key, value)| (key.clone(), value.clone())));
    }
    for (key, value) in headers {
        let rendered = render_search_template_value(&value, input, api_key, max_results);
        if let Some(text) = value_to_search_text(&rendered) {
            req = req.header(key, text);
        }
    }
    req
}

fn apply_configured_auth(
    req: reqwest::blocking::RequestBuilder,
    configured: &ConfiguredApiSearchProvider,
    api_key: &str,
) -> reqwest::blocking::RequestBuilder {
    if api_key.trim().is_empty() {
        return req;
    }
    match configured
        .auth_type
        .as_deref()
        .unwrap_or("api_key")
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "none" | "no_auth" | "anonymous" => req,
        "bearer" | "bearer_token" => req.bearer_auth(api_key),
        "basic" => req.basic_auth(api_key, Option::<String>::None),
        "x_api_key" => req.header("X-API-KEY", api_key),
        "brave_subscription_token" => req.header("X-Subscription-Token", api_key),
        _ => req,
    }
}

fn render_search_template_value(
    value: &Value,
    input: &WebSearchInput,
    secret: &str,
    max_results: usize,
) -> Value {
    match value {
        Value::String(raw) => Value::String(render_search_template_string(
            raw,
            input,
            secret,
            max_results,
        )),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| render_search_template_value(item, input, secret, max_results))
                .collect(),
        ),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, value)| {
                    (
                        key.clone(),
                        render_search_template_value(value, input, secret, max_results),
                    )
                })
                .collect(),
        ),
        other => other.clone(),
    }
}

fn render_search_template_string(
    raw: &str,
    input: &WebSearchInput,
    secret: &str,
    max_results: usize,
) -> String {
    raw.replace("{{query}}", &input.query)
        .replace("{{q}}", &input.query)
        .replace("{{maxResults}}", &max_results.to_string())
        .replace("{{limit}}", &max_results.to_string())
        .replace("{{secret}}", secret)
        .replace(
            "{{locale}}",
            &read_trimmed_env("AOSD_WEB_SEARCH_LANGUAGE").unwrap_or_else(|| "en".to_string()),
        )
}

fn parse_hits_from_mapped_response(
    payload: &Value,
    mapping: Option<&Value>,
    max_results: usize,
) -> Vec<SearchHit> {
    let Some(mapping) = mapping else {
        return Vec::new();
    };
    let items_path = mapping
        .get("itemsPath")
        .or_else(|| mapping.get("items_path"))
        .and_then(Value::as_str)
        .unwrap_or("$.results");
    let Some(items) = json_path_lookup(payload, items_path).and_then(Value::as_array) else {
        return Vec::new();
    };
    let field_path = |camel: &str, snake: &str, fallback: &str| -> String {
        mapping
            .get(camel)
            .or_else(|| mapping.get(snake))
            .and_then(Value::as_str)
            .unwrap_or(fallback)
            .to_string()
    };
    let title_path = field_path("titlePath", "title_path", "$.title");
    let url_path = field_path("urlPath", "url_path", "$.url");
    let snippet_path = field_path("snippetPath", "snippet_path", "$.snippet");
    let content_path = field_path("contentPath", "content_path", "$.content");
    let mut hits = Vec::new();
    for item in items
        .iter()
        .take(max_results.saturating_mul(2).max(max_results))
    {
        let Some(url) = json_path_lookup(item, &url_path).and_then(value_to_search_text) else {
            continue;
        };
        let title = json_path_lookup(item, &title_path)
            .and_then(value_to_search_text)
            .unwrap_or_default();
        let snippet = json_path_lookup(item, &snippet_path)
            .and_then(value_to_search_text)
            .or_else(|| json_path_lookup(item, &content_path).and_then(value_to_search_text));
        let Some(hit) = build_search_hit_from_parts(&url, &title, snippet) else {
            continue;
        };
        hits.push(hit);
    }
    hits
}

fn json_path_lookup<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    let mut current = value;
    let trimmed = path.trim();
    let rest = trimmed
        .strip_prefix("$.")
        .or_else(|| trimmed.strip_prefix('$'))?;
    if rest.is_empty() {
        return Some(current);
    }
    for segment in rest.split('.') {
        let segment = segment.trim();
        if segment.is_empty() {
            continue;
        }
        current = current.get(segment)?;
    }
    Some(current)
}

fn execute_demo_search(input: &WebSearchInput, max_results: usize) -> Vec<SearchHit> {
    let query = input.query.trim();
    let query_lower = query.to_ascii_lowercase();
    let mut fixtures = Vec::new();
    if query_lower.contains("roi") || query.contains("投放") || query.contains("印尼") {
        fixtures.push((
            "AOS demo ROI drop evidence",
            "https://demo.aos.local/ops/roi-drop",
            "Demo evidence: Indonesia ROI dropped because Channel B cost increased while revenue stayed flat.",
        ));
        fixtures.push((
            "AOS demo ad spend by channel",
            "https://demo.aos.local/ops/ad-spend",
            "Demo SQL-backed slice for campaign spend, revenue, country, channel, and app dimensions.",
        ));
    } else if query_lower.contains("revenue")
        || query.contains("收入")
        || query_lower.contains("report")
    {
        fixtures.push((
            "AOS demo daily revenue report",
            "https://demo.aos.local/ops/daily-revenue",
            "Demo automation source for yesterday revenue by app and channel.",
        ));
    }
    fixtures.push((
        "AOS demo search provider",
        "https://demo.aos.local/search",
        "Demo-only deterministic search result. Configure a real provider for production web search.",
    ));

    fixtures
        .into_iter()
        .filter_map(|(title, url, snippet)| {
            build_search_hit_from_parts(url, title, Some(snippet.to_string()))
        })
        .take(max_results)
        .collect()
}

fn enrich_hits_with_tavily_extract(
    mut hits: Vec<SearchHit>,
    client: &Client,
    cfg: &WebSearchEnrichmentConfig,
    trace: &mut Vec<String>,
) -> Vec<SearchHit> {
    if hits.is_empty() {
        return hits;
    }
    if !read_env_bool("AOSD_TAVILY_EXTRACT_ENABLED", !cfg!(test)) {
        trace.push("tavily_extract: disabled".to_string());
        return hits;
    }
    let Some(api_key) = read_provider_api_key(ApiSearchProvider::Tavily) else {
        trace.push("tavily_extract: skipped (missing tavily api key)".to_string());
        return hits;
    };
    let endpoint = match reqwest::Url::parse(
        &read_trimmed_env("AOSD_TAVILY_EXTRACT_BASE_URL")
            .unwrap_or_else(|| "https://api.tavily.com/extract".to_string()),
    ) {
        Ok(url) => url,
        Err(error) => {
            trace.push(format!(
                "tavily_extract: skipped (invalid endpoint: {error})"
            ));
            return hits;
        }
    };

    let top_k = read_bounded_env_usize("AOSD_TAVILY_EXTRACT_TOP_K", 8, 1, 20);
    let urls = hits
        .iter()
        .filter(|hit| hit.content.is_none())
        .map(|hit| hit.url.clone())
        .take(top_k)
        .collect::<Vec<_>>();
    if urls.is_empty() {
        trace.push("tavily_extract: skipped (no uncached urls)".to_string());
        return hits;
    }

    let extract_depth =
        read_trimmed_env("AOSD_TAVILY_EXTRACT_DEPTH").unwrap_or_else(|| "basic".to_string());
    let output_format =
        read_trimmed_env("AOSD_TAVILY_EXTRACT_FORMAT").unwrap_or_else(|| "text".to_string());
    let timeout_secs = read_timeout_env_secs("AOSD_TAVILY_EXTRACT_TIMEOUT_SECS", 15, 1, 60);
    let body = json!({
        "api_key": api_key,
        "urls": urls,
        "extract_depth": extract_depth,
        "format": output_format,
        "include_images": false,
        "include_favicon": false,
        "include_usage": true,
        "timeout": timeout_secs,
    });

    let response = match client
        .post(endpoint)
        .header(reqwest::header::ACCEPT, "application/json")
        .header(reqwest::header::AUTHORIZATION, format!("Bearer {api_key}"))
        .json(&body)
        .send()
    {
        Ok(resp) => resp,
        Err(error) => {
            trace.push(format!("tavily_extract: failed (request_error={error})"));
            return hits;
        }
    };
    let payload = match parse_success_json_response(response) {
        Ok(json) => json,
        Err(error) => {
            trace.push(format!(
                "tavily_extract: failed (provider_error={})",
                error.message
            ));
            return hits;
        }
    };

    let mut by_url = BTreeMap::<String, String>::new();
    let mut success_count = 0usize;
    if let Some(results) = payload.get("results").and_then(Value::as_array) {
        for item in results {
            let Some(url) = read_item_field_text(item, &["url", "link"]) else {
                continue;
            };
            let Some(raw) = read_item_field_text(item, &["raw_content", "content", "text"]) else {
                continue;
            };
            let normalized = collapse_whitespace(&raw);
            if normalized.is_empty() {
                continue;
            }
            by_url.insert(url, normalized);
            success_count = success_count.saturating_add(1);
        }
    }
    let failed_count = payload
        .get("failed_results")
        .and_then(Value::as_array)
        .map(|arr| arr.len())
        .unwrap_or(0);
    for hit in &mut hits {
        let Some(raw) = by_url.get(&hit.url) else {
            continue;
        };
        let content_chars = raw.chars().count();
        hit.content = Some(preview_text(raw, cfg.max_content_chars));
        hit.content_chars = Some(content_chars);
        hit.content_source = Some("tavily_extract".to_string());
        if hit.snippet.is_none() {
            hit.snippet = Some(preview_text(raw, 280));
        }
    }
    trace.push(format!(
        "tavily_extract: success={} failed={} top_k={}",
        success_count, failed_count, top_k
    ));
    hits
}

fn parse_success_json_response(
    response: reqwest::blocking::Response,
) -> Result<Value, ApiProviderError> {
    let status = response.status();
    if !status.is_success() {
        let retry_after = parse_retry_after_header(response.headers());
        let body_preview = response
            .text()
            .map(|body| preview_text(&collapse_whitespace(&body), 220))
            .unwrap_or_default();
        let retryable =
            status.as_u16() == 408 || status.as_u16() == 429 || status.is_server_error();
        return Err(ApiProviderError::new(
            format!("http_status={status} body_preview={body_preview}"),
            retryable,
            retry_after,
        ));
    }
    response
        .json::<Value>()
        .map_err(|error| ApiProviderError::new(format!("json_parse_error={error}"), false, None))
}

fn map_provider_request_error(error: reqwest::Error) -> ApiProviderError {
    let retryable = is_retryable_network_error(&error);
    ApiProviderError::new(format!("request_error={error}"), retryable, None)
}

fn is_retryable_network_error(error: &reqwest::Error) -> bool {
    if error.is_timeout() || error.is_connect() {
        return true;
    }
    let lower = error.to_string().to_ascii_lowercase();
    lower.contains("timed out")
        || lower.contains("connection")
        || lower.contains("dns")
        || lower.contains("tls")
        || lower.contains("temporarily unavailable")
        || lower.contains("transport")
}

fn parse_retry_after_header(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let raw = headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    // Retry-After supports both seconds and HTTP-date; honor both formats.
    if let Ok(secs) = raw.parse::<u64>() {
        return Some(Duration::from_secs(secs));
    }
    let retry_at = httpdate::parse_http_date(raw).ok()?;
    let now = SystemTime::now();
    Some(
        retry_at
            .duration_since(now)
            .unwrap_or_else(|_| Duration::from_secs(0)),
    )
}

fn compute_retry_delay(
    retry_idx: usize,
    retry_backoff_ms: u64,
    retry_jitter_ms: u64,
    retry_after: Option<Duration>,
) -> Duration {
    let base = retry_backoff_ms;
    let exp = 1u64 << retry_idx.min(8);
    let exp_backoff = base.saturating_mul(exp);
    let jitter = if retry_jitter_ms == 0 {
        0
    } else {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| u64::from(d.subsec_nanos()))
            .unwrap_or(0);
        nanos % (retry_jitter_ms + 1)
    };
    let mut delay_ms = exp_backoff.saturating_add(jitter);
    if let Some(retry_after_value) = retry_after {
        let retry_after_ms = retry_after_value.as_millis().try_into().unwrap_or(u64::MAX);
        if retry_after_ms > delay_ms {
            delay_ms = retry_after_ms;
        }
    }
    Duration::from_millis(delay_ms)
}

fn default_search_snippet_keys() -> &'static [&'static str] {
    &[
        "snippet",
        "description",
        "content",
        "text",
        "summary",
        "excerpt",
        "highlights",
    ]
}

fn parse_hits_from_array(
    items: Option<&Vec<Value>>,
    url_keys: &[&str],
    title_keys: &[&str],
    snippet_keys: &[&str],
) -> Vec<SearchHit> {
    let mut hits = Vec::new();
    let Some(items) = items else {
        return hits;
    };
    for item in items {
        let Some(url) = read_item_field_text(item, url_keys) else {
            continue;
        };
        let title = read_item_field_text(item, title_keys).unwrap_or_default();
        let snippet = read_item_field_text(item, snippet_keys);
        let Some(hit) = build_search_hit_from_parts(&url, &title, snippet) else {
            continue;
        };
        hits.push(hit);
    }
    hits
}

fn read_item_field_text(item: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| item.get(*key).and_then(value_to_search_text))
}

fn value_to_search_text(value: &Value) -> Option<String> {
    match value {
        Value::String(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        }
        Value::Array(items) => items.iter().find_map(value_to_search_text),
        Value::Object(map) => ["text", "value", "snippet", "content", "description"]
            .iter()
            .find_map(|key| map.get(*key).and_then(value_to_search_text)),
        _ => None,
    }
}

fn build_search_hit_from_parts(
    url: &str,
    title: &str,
    snippet: Option<String>,
) -> Option<SearchHit> {
    let trimmed_url = url.trim();
    if !(trimmed_url.starts_with("http://") || trimmed_url.starts_with("https://")) {
        return None;
    }
    let canonical_url = canonicalize_search_hit_url(trimmed_url)?;
    if looks_like_search_navigation_url(&canonical_url) {
        return None;
    }
    let final_title = if title.trim().is_empty() {
        canonical_url.clone()
    } else {
        title.trim().to_string()
    };
    let final_snippet = snippet
        .map(|raw| collapse_whitespace(&raw))
        .filter(|value| !value.is_empty());
    Some(SearchHit {
        title: final_title,
        url: canonical_url.clone(),
        snippet: final_snippet,
        domain: extract_domain_from_url(&canonical_url),
        content: None,
        content_chars: None,
        content_source: None,
        relevance_score: None,
    })
}

fn extract_domain_from_url(url: &str) -> Option<String> {
    let parsed = reqwest::Url::parse(url).ok()?;
    let host = parsed.host_str()?.to_ascii_lowercase();
    Some(host.trim_start_matches("www.").to_string())
}

fn read_web_search_output_limit() -> usize {
    read_bounded_env_usize("AOSD_WEB_SEARCH_OUTPUT_HITS", 12, 1, 40)
}

fn render_search_hit_for_summary(hit: &SearchHit) -> String {
    let mut suffix = String::new();
    if let Some(score) = hit.relevance_score {
        suffix.push_str(&format!(" [score {:.2}]", score));
    }
    let excerpt = hit
        .content
        .as_deref()
        .or(hit.snippet.as_deref())
        .map(|text| preview_text(&collapse_whitespace(text), 220))
        .unwrap_or_default();
    if excerpt.is_empty() {
        format!("- [{}]({}){}", hit.title, hit.url, suffix)
    } else {
        format!("- [{}]({}){} — {}", hit.title, hit.url, suffix, excerpt)
    }
}

fn enrich_web_search_hits(
    query: &str,
    hits: Vec<SearchHit>,
    cfg: &WebSearchEnrichmentConfig,
    trace: &mut Vec<String>,
) -> Vec<SearchHit> {
    if !cfg.enabled {
        trace.push("enrich: disabled".to_string());
        return hits;
    }
    if hits.is_empty() {
        trace.push("enrich: skipped (no hits)".to_string());
        return hits;
    }

    let ranked = rank_web_search_candidates(query, &hits, cfg);
    if ranked.is_empty() {
        trace.push("enrich: skipped (ranked list empty)".to_string());
        return hits;
    }

    let max_candidates = ranked.len().min(cfg.max_fetch_candidates);
    let mut attempt_limit = cfg.initial_fetch_candidates.min(max_candidates);
    if attempt_limit == 0 {
        trace.push("enrich: skipped (attempt limit = 0)".to_string());
        return hits;
    }

    let client = match build_http_client(cfg.fetch_timeout_secs, cfg.connect_timeout_secs) {
        Ok(client) => client,
        Err(error) => {
            trace.push(format!("enrich: skipped (client init failed: {error})"));
            return hits;
        }
    };

    let mut enriched = ranked
        .iter()
        .map(|candidate| {
            let mut hit = candidate.hit.clone();
            hit.relevance_score = Some((candidate.base_score * 100.0).round() / 100.0);
            hit
        })
        .collect::<Vec<_>>();
    let index_by_url = enriched
        .iter()
        .enumerate()
        .map(|(index, hit)| (hit.url.clone(), index))
        .collect::<BTreeMap<_, _>>();

    let mut attempted = 0usize;
    let mut cursor = 0usize;
    let mut valid_pages = 0usize;

    while attempted < attempt_limit && cursor < max_candidates {
        let candidate = &ranked[cursor];
        cursor += 1;
        attempted += 1;

        let url = candidate.hit.url.clone();
        let fetch_outcome = fetch_and_extract_search_hit(&url, &client, cfg);
        let Some(index) = index_by_url.get(&url).copied() else {
            continue;
        };

        match fetch_outcome {
            Ok(Some(extracted)) => {
                let content_chars = extracted.text.chars().count();
                if content_chars >= cfg.min_content_chars {
                    valid_pages += 1;
                }
                let hit = &mut enriched[index];
                hit.content = Some(preview_text(&extracted.text, cfg.max_content_chars));
                hit.content_chars = Some(content_chars);
                hit.content_source = Some(extracted.source.to_string());
                if hit.snippet.is_none() {
                    hit.snippet = Some(preview_text(&extracted.text, 280));
                }
            }
            Ok(None) => {
                trace.push(format!(
                    "enrich: {} skipped (unsupported content type)",
                    url
                ));
            }
            Err(error) => {
                trace.push(format!("enrich: {} failed ({error})", url));
            }
        }

        if attempted == attempt_limit
            && valid_pages < cfg.target_valid_pages
            && attempt_limit < max_candidates
        {
            attempt_limit = max_candidates;
            trace.push(format!(
                "enrich: valid_pages={valid_pages}/{} after {} attempts, expanding to top-{}",
                cfg.target_valid_pages, attempted, attempt_limit
            ));
        }
    }

    trace.push(format!(
        "enrich: attempted={} valid_pages={} target={} max_candidates={}",
        attempted, valid_pages, cfg.target_valid_pages, max_candidates
    ));
    enriched
}

fn fetch_and_extract_search_hit(
    url: &str,
    client: &Client,
    cfg: &WebSearchEnrichmentConfig,
) -> Result<Option<ExtractedContent>, String> {
    let request_url = normalize_fetch_url(url)?;
    let mut fetch_channels = vec![("direct", request_url.clone())];
    if cfg.jina_fallback_enabled {
        fetch_channels.push(("jina", format!("https://r.jina.ai/{request_url}")));
    }

    let max_tries = cfg.fetch_retry_attempts.saturating_add(1).max(1);
    let mut errors = Vec::<String>::new();
    for (channel_name, channel_url) in fetch_channels {
        for retry_idx in 0..max_tries {
            match fetch_and_extract_search_hit_once(url, &channel_url, channel_name, client, cfg) {
                Ok(Some(mut extracted)) => {
                    if channel_name != "direct" {
                        extracted.source = format!("{channel_name}_{}", extracted.source);
                    }
                    return Ok(Some(extracted));
                }
                Ok(None) => {
                    errors.push(format!("{channel_name} skipped (unsupported content type)"));
                    break;
                }
                Err(error) => {
                    errors.push(format!(
                        "{channel_name} attempt {}/{} failed ({error})",
                        retry_idx + 1,
                        max_tries
                    ));
                    if retry_idx + 1 < max_tries && should_retry_fetch_error(&error) {
                        let delay = compute_retry_delay(
                            retry_idx,
                            cfg.fetch_retry_backoff_ms,
                            cfg.fetch_retry_jitter_ms,
                            None,
                        );
                        if !delay.is_zero() {
                            std::thread::sleep(delay);
                        }
                        continue;
                    }
                    break;
                }
            }
        }
    }

    if errors.is_empty() {
        Err("no fetch channels attempted".to_string())
    } else {
        Err(errors.join(" | "))
    }
}

fn fetch_and_extract_search_hit_once(
    original_url: &str,
    request_url: &str,
    channel_name: &str,
    client: &Client,
    cfg: &WebSearchEnrichmentConfig,
) -> Result<Option<ExtractedContent>, String> {
    let mut request = client.get(request_url.to_string());
    request = request
        .header(
            reqwest::header::ACCEPT,
            "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
        )
        .header(
            reqwest::header::ACCEPT_LANGUAGE,
            read_trimmed_env("AOSD_WEB_FETCH_ACCEPT_LANGUAGE")
                .unwrap_or_else(|| "en-US,en;q=0.9,zh-CN;q=0.8".to_string()),
        )
        .header(reqwest::header::CACHE_CONTROL, "no-cache")
        .header(reqwest::header::PRAGMA, "no-cache")
        .header("Upgrade-Insecure-Requests", "1");
    if let Some(referer) = read_trimmed_env("AOSD_WEB_FETCH_REFERER") {
        request = request.header(reqwest::header::REFERER, referer);
    }
    if channel_name == "jina" {
        request = request.header("x-timeout", cfg.fetch_timeout_secs.to_string());
    }

    let response = request.send().map_err(|error| error.to_string())?;
    if !response.status().is_success() {
        return Err(format!("http_status={}", response.status()));
    }
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if channel_name != "jina"
        && (content_type.contains("application/pdf")
            || content_type.contains("application/octet-stream")
            || content_type.starts_with("image/")
            || content_type.starts_with("video/"))
    {
        return Ok(None);
    }
    let body = response.text().map_err(|error| error.to_string())?;
    extract_main_content(original_url, &body, &content_type, cfg)
}

fn should_retry_fetch_error(error_text: &str) -> bool {
    let lower = error_text.to_ascii_lowercase();
    if lower.contains("http_status=403")
        || lower.contains("http_status=408")
        || lower.contains("http_status=429")
        || lower.contains("http_status=500")
        || lower.contains("http_status=502")
        || lower.contains("http_status=503")
        || lower.contains("http_status=504")
    {
        return true;
    }
    lower.contains("timed out")
        || lower.contains("timeout")
        || lower.contains("connection")
        || lower.contains("dns")
        || lower.contains("transport")
}

fn extract_main_content(
    url: &str,
    body: &str,
    content_type: &str,
    cfg: &WebSearchEnrichmentConfig,
) -> Result<Option<ExtractedContent>, String> {
    if content_type.contains("html") {
        if let Ok(article) = libreadability::extract(body, Some(url)) {
            let text = collapse_whitespace(&decode_html_entities(article.text_content.trim()));
            if text.chars().count() >= cfg.min_content_chars {
                return Ok(Some(ExtractedContent {
                    text,
                    source: "readability".to_string(),
                }));
            }
        }
        let fallback = collapse_whitespace(&decode_html_entities(&html_to_text(body)));
        if fallback.chars().count() >= cfg.min_content_chars {
            return Ok(Some(ExtractedContent {
                text: fallback,
                source: "html_to_text".to_string(),
            }));
        }
        return Err("content_too_short".to_string());
    }

    let text = collapse_whitespace(body);
    if text.chars().count() >= cfg.min_content_chars {
        return Ok(Some(ExtractedContent {
            text,
            source: "plain_text".to_string(),
        }));
    }
    Err("content_too_short".to_string())
}

fn rank_web_search_candidates(
    query: &str,
    hits: &[SearchHit],
    cfg: &WebSearchEnrichmentConfig,
) -> Vec<RankedSearchCandidate> {
    let query_tokens = tokenize_query_terms(query);
    let mut remaining = hits
        .iter()
        .cloned()
        .map(|hit| {
            let domain_key = hit.domain.clone().unwrap_or_default();
            RankedSearchCandidate {
                base_score: score_search_candidate(&query_tokens, query, &hit, &domain_key),
                hit,
                domain_key,
            }
        })
        .collect::<Vec<_>>();

    let mut ordered = Vec::with_capacity(remaining.len());
    let mut seen_domain_count = BTreeMap::<String, usize>::new();
    while !remaining.is_empty() {
        let mut best_index = 0usize;
        let mut best_adjusted = f64::NEG_INFINITY;
        for (idx, candidate) in remaining.iter().enumerate() {
            let repeats = seen_domain_count
                .get(&candidate.domain_key)
                .copied()
                .unwrap_or(0) as f64;
            let adjusted = candidate.base_score - (repeats * cfg.domain_repeat_penalty);
            if adjusted > best_adjusted {
                best_adjusted = adjusted;
                best_index = idx;
            }
        }
        let chosen = remaining.remove(best_index);
        if !chosen.domain_key.is_empty() {
            *seen_domain_count
                .entry(chosen.domain_key.clone())
                .or_insert(0) += 1;
        }
        ordered.push(chosen);
    }
    ordered
}

fn tokenize_query_terms(query: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for token in query
        .split(|ch: char| !ch.is_alphanumeric())
        .map(str::trim)
        .filter(|token| token.len() >= 2)
    {
        let normalized = token.to_ascii_lowercase();
        if seen.insert(normalized.clone()) {
            out.push(normalized);
        }
    }
    out
}

fn score_search_candidate(
    query_tokens: &[String],
    raw_query: &str,
    hit: &SearchHit,
    domain_key: &str,
) -> f64 {
    let title = hit.title.to_ascii_lowercase();
    let url = hit.url.to_ascii_lowercase();
    let snippet = hit
        .snippet
        .as_deref()
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    let token_count = query_tokens.len().max(1);
    let mut matched = 0usize;
    for token in query_tokens {
        if title.contains(token) || snippet.contains(token) || url.contains(token) {
            matched += 1;
        }
    }
    let token_score = matched as f64 / token_count as f64;
    let phrase = raw_query.trim().to_ascii_lowercase();
    let phrase_bonus =
        if phrase.len() >= 8 && (title.contains(&phrase) || snippet.contains(&phrase)) {
            0.2
        } else {
            0.0
        };
    let quality_penalty = if title.len() < 8 { 0.08 } else { 0.0 };
    let tracking_penalty =
        if url.contains("utm_") || url.contains("ref=") || url.contains("source=") {
            0.04
        } else {
            0.0
        };
    (token_score * 1.25) + domain_prior_score(domain_key) + phrase_bonus
        - quality_penalty
        - tracking_penalty
}

fn domain_prior_score(domain_key: &str) -> f64 {
    if domain_key.is_empty() {
        return 0.0;
    }
    if domain_key.ends_with(".gov") || domain_key.ends_with(".edu") {
        return 0.16;
    }
    if domain_key.ends_with(".org") {
        return 0.05;
    }
    const HIGH_SIGNAL_DOMAINS: &[&str] = &[
        "wikipedia.org",
        "arxiv.org",
        "github.com",
        "docs.rs",
        "who.int",
        "worldbank.org",
        "oecd.org",
        "imf.org",
        "nature.com",
        "science.org",
        "europa.eu",
    ];
    for domain in HIGH_SIGNAL_DOMAINS {
        if domain_key == *domain || domain_key.ends_with(&format!(".{domain}")) {
            return 0.12;
        }
    }
    0.0
}

fn normalize_domain_filters(domains: Option<&[String]>) -> Option<Vec<String>> {
    let mut normalized = domains
        .unwrap_or(&[])
        .iter()
        .map(|domain| normalize_domain_filter(domain))
        .filter(|domain| !domain.is_empty())
        .collect::<Vec<_>>();
    normalized.sort();
    normalized.dedup();
    if normalized.is_empty() {
        None
    } else {
        Some(normalized)
    }
}

fn read_trimmed_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn read_env_bool(name: &str, default: bool) -> bool {
    let Some(raw) = read_trimmed_env(name) else {
        return default;
    };
    match raw.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => true,
        "0" | "false" | "no" | "off" => false,
        _ => default,
    }
}

fn read_bounded_env_usize(name: &str, default: usize, min: usize, max: usize) -> usize {
    let raw = match std::env::var(name) {
        Ok(v) => v,
        Err(_) => return default,
    };
    match raw.trim().parse::<usize>() {
        Ok(parsed) => parsed.clamp(min, max),
        Err(_) => default,
    }
}

fn read_bounded_env_f64(name: &str, default: f64, min: f64, max: f64) -> f64 {
    let raw = match std::env::var(name) {
        Ok(v) => v,
        Err(_) => return default,
    };
    match raw.trim().parse::<f64>() {
        Ok(parsed) => parsed.clamp(min, max),
        Err(_) => default,
    }
}

fn read_timeout_env_secs(name: &str, default: u64, min: u64, max: u64) -> u64 {
    let raw = match std::env::var(name) {
        Ok(v) => v,
        Err(_) => return default,
    };
    match raw.trim().parse::<u64>() {
        Ok(parsed) => parsed.clamp(min, max),
        Err(_) => default,
    }
}

fn build_http_client(timeout_secs: u64, connect_timeout_secs: u64) -> Result<Client, String> {
    Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .connect_timeout(Duration::from_secs(connect_timeout_secs))
        .redirect(reqwest::redirect::Policy::limited(10))
        .user_agent(
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
             (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36 AOS-Research/1.0",
        )
        .build()
        .map_err(|error| error.to_string())
}

fn normalize_fetch_url(url: &str) -> Result<String, String> {
    let parsed = reqwest::Url::parse(url).map_err(|error| error.to_string())?;
    if parsed.scheme() == "http" {
        let host = parsed.host_str().unwrap_or_default();
        if host != "localhost" && host != "127.0.0.1" && host != "::1" {
            let mut upgraded = parsed;
            upgraded
                .set_scheme("https")
                .map_err(|()| String::from("failed to upgrade URL to https"))?;
            return Ok(upgraded.to_string());
        }
    }
    Ok(parsed.to_string())
}

fn normalize_fetched_content(body: &str, content_type: &str) -> String {
    if content_type.contains("html") {
        html_to_text(body)
    } else {
        body.trim().to_string()
    }
}

fn summarize_web_fetch(
    url: &str,
    prompt: &str,
    content: &str,
    raw_body: &str,
    content_type: &str,
) -> String {
    let lower_prompt = prompt.to_lowercase();
    let compact = collapse_whitespace(content);

    let detail = if lower_prompt.contains("title") {
        extract_title(content, raw_body, content_type).map_or_else(
            || preview_text(&compact, 600),
            |title| format!("Title: {title}"),
        )
    } else if lower_prompt.contains("summary") || lower_prompt.contains("summarize") {
        preview_text(&compact, 900)
    } else {
        let preview = preview_text(&compact, 900);
        format!("Prompt: {prompt}\nContent preview:\n{preview}")
    };

    format!("Fetched {url}\n{detail}")
}

fn extract_title(content: &str, raw_body: &str, content_type: &str) -> Option<String> {
    if content_type.contains("html") {
        let lowered = raw_body.to_lowercase();
        if let Some(start) = lowered.find("<title>") {
            let after = start + "<title>".len();
            if let Some(end_rel) = lowered[after..].find("</title>") {
                let title =
                    collapse_whitespace(&decode_html_entities(&raw_body[after..after + end_rel]));
                if !title.is_empty() {
                    return Some(title);
                }
            }
        }
    }

    for line in content.lines() {
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    None
}

fn html_to_text(html: &str) -> String {
    let mut text = String::with_capacity(html.len());
    let mut in_tag = false;
    let mut previous_was_space = false;

    for ch in html.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if in_tag => {}
            '&' => {
                text.push('&');
                previous_was_space = false;
            }
            ch if ch.is_whitespace() => {
                if !previous_was_space {
                    text.push(' ');
                    previous_was_space = true;
                }
            }
            _ => {
                text.push(ch);
                previous_was_space = false;
            }
        }
    }

    collapse_whitespace(&decode_html_entities(&text))
}

fn decode_html_entities(input: &str) -> String {
    input
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
}

fn collapse_whitespace(input: &str) -> String {
    input.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn preview_text(input: &str, max_chars: usize) -> String {
    if input.chars().count() <= max_chars {
        return input.to_string();
    }
    let shortened = input.chars().take(max_chars).collect::<String>();
    format!("{}…", shortened.trim_end())
}

fn looks_like_search_navigation_url(url: &str) -> bool {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return true;
    };
    let Some(host) = parsed.host_str() else {
        return true;
    };
    let host = host.to_ascii_lowercase();
    let path = parsed.path().to_ascii_lowercase();
    let query = parsed.query().unwrap_or("").to_ascii_lowercase();
    let is_search_host = host == "bing.com"
        || host.ends_with(".bing.com")
        || host == "search.yahoo.com"
        || host.ends_with(".yahoo.com")
        || host == "google.com"
        || host.ends_with(".google.com")
        || host.starts_with("search.")
        || host.starts_with("www.search.");
    if !is_search_host {
        return false;
    }
    path == "/"
        || path.starts_with("/search")
        || path.starts_with("/html")
        || path.starts_with("/lite")
        || path.starts_with("/settings")
        || query.contains("q=")
        || query.contains("p=")
        || query.contains("text=")
        || query.contains("wd=")
}

#[derive(Debug, Default)]
struct SearchHitQualityFilterStats {
    dropped_auth: usize,
    dropped_navigation: usize,
    dropped_mismatch: usize,
    dropped_invalid: usize,
}

fn filter_low_signal_search_hits(
    query: &str,
    hits: Vec<SearchHit>,
) -> (Vec<SearchHit>, SearchHitQualityFilterStats) {
    if hits.is_empty() {
        return (hits, SearchHitQualityFilterStats::default());
    }
    let query_tokens = tokenize_query_terms(query);
    let ascii_anchors = ascii_query_anchor_terms(query);
    let mut stats = SearchHitQualityFilterStats::default();
    let mut kept = Vec::with_capacity(hits.len());
    for hit in hits {
        let Ok(parsed) = reqwest::Url::parse(&hit.url) else {
            stats.dropped_invalid += 1;
            continue;
        };
        if is_auth_or_account_url(&parsed) {
            stats.dropped_auth += 1;
            continue;
        }
        if !ascii_anchors.is_empty() && !search_hit_contains_any_term(&hit, &ascii_anchors) {
            stats.dropped_mismatch += 1;
            continue;
        }
        let match_ratio = query_token_match_ratio(&query_tokens, &hit);
        if looks_like_low_signal_navigation_page(&parsed, match_ratio) {
            stats.dropped_navigation += 1;
            continue;
        }
        // For longer queries, extremely weak lexical alignment is usually broad catalog noise.
        if query_tokens.len() >= 4
            && match_ratio <= 0.01
            && !has_informative_text_signal(&hit)
            && is_shallow_path(&parsed)
        {
            stats.dropped_mismatch += 1;
            continue;
        }
        kept.push(hit);
    }
    (kept, stats)
}

fn ascii_query_anchor_terms(query: &str) -> Vec<String> {
    // Hard entity anchoring is only needed for mixed-language queries such as
    // `Tokio 是什么`, where a generic CJK result can otherwise score despite
    // omitting the named ASCII entity. Applying it to ordinary English queries
    // is too aggressive: useful results may answer with a related product or
    // library name without repeating any query token in the search snippet.
    if !query
        .chars()
        .any(|ch| !ch.is_ascii() && ch.is_alphanumeric())
    {
        return Vec::new();
    }

    const STOP_WORDS: &[&str] = &[
        "and", "are", "can", "for", "from", "how", "into", "latest", "the", "this", "what", "when",
        "where", "which", "who", "why", "with",
    ];
    let mut anchors = query
        .split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '-' && ch != '_')
        .map(str::trim)
        .map(str::to_ascii_lowercase)
        .filter(|term| {
            term.len() >= 3
                && term.chars().any(|ch| ch.is_ascii_alphabetic())
                && !STOP_WORDS.contains(&term.as_str())
        })
        .collect::<Vec<_>>();
    anchors.sort();
    anchors.dedup();
    anchors
}

fn search_hit_contains_any_term(hit: &SearchHit, terms: &[String]) -> bool {
    let title = hit.title.to_ascii_lowercase();
    let url = hit.url.to_ascii_lowercase();
    let snippet = hit
        .snippet
        .as_deref()
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    terms
        .iter()
        .any(|term| title.contains(term) || url.contains(term) || snippet.contains(term))
}

fn query_token_match_ratio(query_tokens: &[String], hit: &SearchHit) -> f64 {
    if query_tokens.is_empty() {
        return 1.0;
    }
    let title = hit.title.to_ascii_lowercase();
    let url = hit.url.to_ascii_lowercase();
    let snippet = hit
        .snippet
        .as_deref()
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    let matched = query_tokens
        .iter()
        .filter(|token| title.contains(*token) || snippet.contains(*token) || url.contains(*token))
        .count();
    matched as f64 / query_tokens.len() as f64
}

fn is_auth_or_account_url(parsed: &reqwest::Url) -> bool {
    let Some(host) = parsed.host_str() else {
        return true;
    };
    let host = host.to_ascii_lowercase();
    if host.starts_with("account.") || host.starts_with("auth.") || host.starts_with("login.") {
        return true;
    }
    let segments = path_segments(parsed);
    if segments.iter().any(|segment| {
        matches!(
            segment.as_str(),
            "account"
                | "accounts"
                | "auth"
                | "oauth"
                | "sso"
                | "session"
                | "sessions"
                | "login"
                | "log-in"
                | "signin"
                | "sign-in"
                | "signup"
                | "sign-up"
                | "register"
                | "logout"
                | "log-out"
                | "profile"
                | "profiles"
                | "settings"
                | "preferences"
                | "my-account"
                | "my-profile"
        )
    }) {
        return true;
    }
    false
}

fn looks_like_low_signal_navigation_page(parsed: &reqwest::Url, query_match_ratio: f64) -> bool {
    let segments = path_segments(parsed);
    let has_taxonomy_segment = segments
        .iter()
        .any(|segment| is_listing_taxonomy_segment(segment));
    let has_nav_query = parsed
        .query_pairs()
        .any(|(key, _)| is_navigation_query_key(&key.to_ascii_lowercase()));
    if (has_taxonomy_segment || has_nav_query) && query_match_ratio < 0.25 {
        return true;
    }
    false
}

fn path_segments(parsed: &reqwest::Url) -> Vec<String> {
    parsed
        .path_segments()
        .map(|segments| {
            segments
                .filter_map(|segment| {
                    let cleaned = segment.trim().to_ascii_lowercase();
                    if cleaned.is_empty() {
                        None
                    } else {
                        Some(cleaned)
                    }
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn is_listing_taxonomy_segment(segment: &str) -> bool {
    matches!(
        segment,
        "android"
            | "ios"
            | "iphone"
            | "apps"
            | "app"
            | "games"
            | "news"
            | "blog"
            | "blogs"
            | "article"
            | "articles"
            | "category"
            | "categories"
            | "tag"
            | "tags"
            | "topic"
            | "topics"
            | "section"
            | "sections"
            | "channel"
            | "channels"
            | "browse"
            | "directory"
            | "directories"
            | "archive"
            | "archives"
            | "collection"
            | "collections"
            | "listing"
            | "listings"
            | "sitemap"
            | "index"
            | "home"
            | "all"
    )
}

fn is_navigation_query_key(key: &str) -> bool {
    matches!(
        key,
        "q" | "query"
            | "search"
            | "page"
            | "p"
            | "offset"
            | "start"
            | "from"
            | "sort"
            | "order"
            | "filter"
            | "filters"
            | "category"
            | "cat"
            | "tag"
            | "topic"
            | "section"
            | "browse"
    )
}

fn has_informative_text_signal(hit: &SearchHit) -> bool {
    if hit.title.chars().count() >= 42 {
        return true;
    }
    hit.snippet
        .as_deref()
        .map(|snippet| snippet.chars().count() >= 90)
        .unwrap_or(false)
}

fn is_shallow_path(parsed: &reqwest::Url) -> bool {
    let segments = path_segments(parsed);
    if segments.is_empty() {
        return true;
    }
    if segments.len() > 2 {
        return false;
    }
    segments
        .iter()
        .all(|segment| !segment.contains('-') && !segment.chars().any(|ch| ch.is_ascii_digit()))
}

fn host_matches_list(url: &str, domains: &[String]) -> bool {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return false;
    };
    let Some(host) = parsed.host_str() else {
        return false;
    };
    let host = host.to_ascii_lowercase();
    domains.iter().any(|domain| {
        let normalized = normalize_domain_filter(domain);
        !normalized.is_empty() && (host == normalized || host.ends_with(&format!(".{normalized}")))
    })
}

fn normalize_domain_filter(domain: &str) -> String {
    let trimmed = domain.trim();
    let candidate = reqwest::Url::parse(trimmed)
        .ok()
        .and_then(|url| url.host_str().map(str::to_string))
        .unwrap_or_else(|| trimmed.to_string());
    candidate
        .trim()
        .trim_start_matches('.')
        .trim_end_matches('/')
        .to_ascii_lowercase()
}

fn canonicalize_search_hit_url(url: &str) -> Option<String> {
    let mut parsed = reqwest::Url::parse(url).ok()?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return None;
    }
    let host = parsed.host_str()?.to_ascii_lowercase();
    let normalized_host = host.trim_start_matches("www.");
    if normalized_host != host {
        parsed.set_host(Some(normalized_host)).ok()?;
    }
    if (parsed.scheme() == "http" && parsed.port() == Some(80))
        || (parsed.scheme() == "https" && parsed.port() == Some(443))
    {
        let _ = parsed.set_port(None);
    }
    parsed.set_fragment(None);
    let path = parsed.path().to_string();
    if path.len() > 1 {
        let trimmed = path.trim_end_matches('/');
        if trimmed.is_empty() {
            parsed.set_path("/");
        } else if trimmed != path {
            parsed.set_path(trimmed);
        }
    }
    let mut query_pairs = parsed
        .query_pairs()
        .filter_map(|(key, value)| {
            let normalized_key = key.trim().to_ascii_lowercase();
            if normalized_key.is_empty() || is_tracking_query_param(&normalized_key) {
                return None;
            }
            Some((key.into_owned(), value.into_owned()))
        })
        .collect::<Vec<_>>();
    query_pairs.sort_by(|left, right| left.0.cmp(&right.0).then(left.1.cmp(&right.1)));
    query_pairs.dedup();
    parsed.set_query(None);
    if !query_pairs.is_empty() {
        {
            let mut pairs = parsed.query_pairs_mut();
            for (key, value) in query_pairs {
                pairs.append_pair(&key, &value);
            }
        }
    }
    Some(parsed.to_string())
}

fn is_tracking_query_param(key: &str) -> bool {
    key.starts_with("utm_")
        || matches!(
            key,
            "fbclid"
                | "gclid"
                | "dclid"
                | "msclkid"
                | "yclid"
                | "twclid"
                | "mc_cid"
                | "mc_eid"
                | "mkt_tok"
                | "igshid"
                | "spm"
                | "cmpid"
                | "campaignid"
                | "adgroupid"
                | "adid"
                | "ref"
                | "ref_src"
                | "ref_url"
                | "source"
                | "src"
        )
}

fn canonical_search_hit_dedupe_key(url: &str) -> String {
    let Some(canonical_url) = canonicalize_search_hit_url(url) else {
        return url.trim().to_ascii_lowercase();
    };
    let Ok(parsed) = reqwest::Url::parse(&canonical_url) else {
        return canonical_url.to_ascii_lowercase();
    };
    let host = parsed
        .host_str()
        .map(|value| value.to_ascii_lowercase())
        .unwrap_or_default();
    let path = parsed.path().to_string();
    let query = parsed.query().unwrap_or_default();
    if query.is_empty() {
        format!("{host}{path}")
    } else {
        format!("{host}{path}?{query}")
    }
}

fn dedupe_hits(hits: &mut Vec<SearchHit>) {
    let _ = dedupe_hits_with_stats(hits);
}

fn dedupe_hits_with_stats(hits: &mut Vec<SearchHit>) -> usize {
    let before = hits.len();
    let mut seen = BTreeSet::new();
    hits.retain(|hit| seen.insert(canonical_search_hit_dedupe_key(&hit.url)));
    before.saturating_sub(hits.len())
}

fn execute_todo_write(input: TodoWriteInput) -> Result<TodoWriteOutput, String> {
    validate_todos(&input.todos)?;
    let store_path = todo_store_path()?;
    let old_todos = if store_path.exists() {
        serde_json::from_str::<Vec<TodoItem>>(
            &std::fs::read_to_string(&store_path).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?
    } else {
        Vec::new()
    };

    let all_done = input
        .todos
        .iter()
        .all(|todo| matches!(todo.status, TodoStatus::Completed));
    let persisted = if all_done {
        Vec::new()
    } else {
        input.todos.clone()
    };

    if let Some(parent) = store_path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    std::fs::write(
        &store_path,
        serde_json::to_string_pretty(&persisted).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;

    let verification_nudge_needed = (all_done
        && input.todos.len() >= 3
        && !input
            .todos
            .iter()
            .any(|todo| todo.content.to_lowercase().contains("verif")))
    .then_some(true);

    Ok(TodoWriteOutput {
        old_todos,
        new_todos: input.todos,
        verification_nudge_needed,
    })
}

fn execute_skill(input: SkillInput) -> Result<SkillOutput, String> {
    let skill_path = resolve_skill_path(&input.skill)?;
    let prompt = std::fs::read_to_string(&skill_path).map_err(|error| error.to_string())?;
    let description = parse_skill_description(&prompt);

    Ok(SkillOutput {
        skill: input.skill,
        path: skill_path.display().to_string(),
        args: input.args,
        description,
        prompt,
    })
}

fn validate_todos(todos: &[TodoItem]) -> Result<(), String> {
    if todos.is_empty() {
        return Err(String::from("todos must not be empty"));
    }
    // Allow multiple in_progress items for parallel workflows
    if todos.iter().any(|todo| todo.content.trim().is_empty()) {
        return Err(String::from("todo content must not be empty"));
    }
    if todos.iter().any(|todo| todo.active_form.trim().is_empty()) {
        return Err(String::from("todo activeForm must not be empty"));
    }
    Ok(())
}

fn todo_store_path() -> Result<std::path::PathBuf, String> {
    if let Ok(path) = std::env::var("AOSD_TODO_STORE") {
        return Ok(std::path::PathBuf::from(path));
    }
    let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
    Ok(cwd.join(".aosd-todos.json"))
}

fn resolve_skill_path(skill: &str) -> Result<std::path::PathBuf, String> {
    let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
    match commands::resolve_skill_path(&cwd, skill) {
        Ok(path) => Ok(path),
        Err(_) => resolve_skill_path_from_compat_roots(skill),
    }
}

fn resolve_skill_path_from_compat_roots(skill: &str) -> Result<std::path::PathBuf, String> {
    let requested = skill.trim().trim_start_matches('/').trim_start_matches('$');
    if requested.is_empty() {
        return Err(String::from("skill must not be empty"));
    }

    for root in skill_lookup_roots() {
        if let Some(path) = resolve_skill_path_in_root(&root, requested) {
            return Ok(path);
        }
    }

    Err(format!("unknown skill: {requested}"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SkillLookupOrigin {
    SkillsDir,
    LegacyCommandsDir,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SkillLookupRoot {
    path: std::path::PathBuf,
    origin: SkillLookupOrigin,
}

fn skill_lookup_roots() -> Vec<SkillLookupRoot> {
    let mut roots = Vec::new();

    if let Ok(cwd) = std::env::current_dir() {
        push_project_skill_lookup_roots(&mut roots, &cwd);
    }

    if let Ok(aos_config_home) = std::env::var("AOS_CONFIG_HOME") {
        push_prefixed_skill_lookup_roots(&mut roots, std::path::Path::new(&aos_config_home));
    }
    if let Ok(codex_home) = std::env::var("CODEX_HOME") {
        push_prefixed_skill_lookup_roots(&mut roots, std::path::Path::new(&codex_home));
    }
    if let Ok(home) = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")) {
        push_home_skill_lookup_roots(&mut roots, std::path::Path::new(&home));
    }
    if let Ok(claude_config_dir) = std::env::var("CLAUDE_CONFIG_DIR") {
        let claude_config_dir = std::path::PathBuf::from(claude_config_dir);
        push_skill_lookup_root(
            &mut roots,
            claude_config_dir.join("skills"),
            SkillLookupOrigin::SkillsDir,
        );
        push_skill_lookup_root(
            &mut roots,
            claude_config_dir.join("skills").join("omc-learned"),
            SkillLookupOrigin::SkillsDir,
        );
        push_skill_lookup_root(
            &mut roots,
            claude_config_dir.join("commands"),
            SkillLookupOrigin::LegacyCommandsDir,
        );
    }
    push_skill_lookup_root(
        &mut roots,
        std::path::PathBuf::from("/home/bellman/.aos/skills"),
        SkillLookupOrigin::SkillsDir,
    );
    push_skill_lookup_root(
        &mut roots,
        std::path::PathBuf::from("/home/bellman/.codex/skills"),
        SkillLookupOrigin::SkillsDir,
    );

    roots
}

fn push_project_skill_lookup_roots(roots: &mut Vec<SkillLookupRoot>, cwd: &std::path::Path) {
    for ancestor in cwd.ancestors() {
        push_prefixed_skill_lookup_roots(roots, &ancestor.join(".omc"));
        push_prefixed_skill_lookup_roots(roots, &ancestor.join(".agents"));
        push_prefixed_skill_lookup_roots(roots, &ancestor.join(".aos"));
        push_prefixed_skill_lookup_roots(roots, &ancestor.join(".codex"));
        push_prefixed_skill_lookup_roots(roots, &ancestor.join(".claude"));
    }
}

fn push_home_skill_lookup_roots(roots: &mut Vec<SkillLookupRoot>, home: &std::path::Path) {
    push_prefixed_skill_lookup_roots(roots, &home.join(".omc"));
    push_prefixed_skill_lookup_roots(roots, &home.join(".aos"));
    push_prefixed_skill_lookup_roots(roots, &home.join(".codex"));
    push_prefixed_skill_lookup_roots(roots, &home.join(".claude"));
    push_skill_lookup_root(
        roots,
        home.join(".agents").join("skills"),
        SkillLookupOrigin::SkillsDir,
    );
    push_skill_lookup_root(
        roots,
        home.join(".config").join("opencode").join("skills"),
        SkillLookupOrigin::SkillsDir,
    );
    push_skill_lookup_root(
        roots,
        home.join(".claude").join("skills").join("omc-learned"),
        SkillLookupOrigin::SkillsDir,
    );
}

fn push_prefixed_skill_lookup_roots(roots: &mut Vec<SkillLookupRoot>, prefix: &std::path::Path) {
    push_skill_lookup_root(roots, prefix.join("skills"), SkillLookupOrigin::SkillsDir);
    push_skill_lookup_root(
        roots,
        prefix.join("commands"),
        SkillLookupOrigin::LegacyCommandsDir,
    );
}

fn push_skill_lookup_root(
    roots: &mut Vec<SkillLookupRoot>,
    path: std::path::PathBuf,
    origin: SkillLookupOrigin,
) {
    if path.is_dir() && !roots.iter().any(|existing| existing.path == path) {
        roots.push(SkillLookupRoot { path, origin });
    }
}

fn resolve_skill_path_in_root(
    root: &SkillLookupRoot,
    requested: &str,
) -> Option<std::path::PathBuf> {
    match root.origin {
        SkillLookupOrigin::SkillsDir => resolve_skill_path_in_skills_dir(&root.path, requested),
        SkillLookupOrigin::LegacyCommandsDir => {
            resolve_skill_path_in_legacy_commands_dir(&root.path, requested)
        }
    }
}

fn resolve_skill_path_in_skills_dir(
    root: &std::path::Path,
    requested: &str,
) -> Option<std::path::PathBuf> {
    let direct = root.join(requested).join("SKILL.md");
    if direct.is_file() {
        return Some(direct);
    }

    let entries = std::fs::read_dir(root).ok()?;
    for entry in entries.flatten() {
        if !entry.path().is_dir() {
            continue;
        }
        let skill_path = entry.path().join("SKILL.md");
        if !skill_path.is_file() {
            continue;
        }
        if entry
            .file_name()
            .to_string_lossy()
            .eq_ignore_ascii_case(requested)
            || skill_frontmatter_name_matches(&skill_path, requested)
        {
            return Some(skill_path);
        }
    }

    None
}

fn resolve_skill_path_in_legacy_commands_dir(
    root: &std::path::Path,
    requested: &str,
) -> Option<std::path::PathBuf> {
    let direct_dir = root.join(requested).join("SKILL.md");
    if direct_dir.is_file() {
        return Some(direct_dir);
    }

    let direct_markdown = root.join(format!("{requested}.md"));
    if direct_markdown.is_file() {
        return Some(direct_markdown);
    }

    let entries = std::fs::read_dir(root).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        let candidate_path = if path.is_dir() {
            let skill_path = path.join("SKILL.md");
            if !skill_path.is_file() {
                continue;
            }
            skill_path
        } else if path
            .extension()
            .is_some_and(|ext| ext.to_string_lossy().eq_ignore_ascii_case("md"))
        {
            path
        } else {
            continue;
        };

        let matches_entry_name = candidate_path
            .file_stem()
            .is_some_and(|stem| stem.to_string_lossy().eq_ignore_ascii_case(requested))
            || entry
                .file_name()
                .to_string_lossy()
                .trim_end_matches(".md")
                .eq_ignore_ascii_case(requested);
        if matches_entry_name || skill_frontmatter_name_matches(&candidate_path, requested) {
            return Some(candidate_path);
        }
    }

    None
}

fn skill_frontmatter_name_matches(path: &std::path::Path, requested: &str) -> bool {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|contents| parse_skill_name(&contents))
        .is_some_and(|name| name.eq_ignore_ascii_case(requested))
}

fn parse_skill_name(contents: &str) -> Option<String> {
    parse_skill_frontmatter_value(contents, "name")
}

fn parse_skill_frontmatter_value(contents: &str, key: &str) -> Option<String> {
    let mut lines = contents.lines();
    if lines.next().map(str::trim) != Some("---") {
        return None;
    }

    for line in lines {
        let trimmed = line.trim();
        if trimmed == "---" {
            break;
        }
        if let Some(value) = trimmed.strip_prefix(&format!("{key}:")) {
            let value = value
                .trim()
                .trim_matches(|ch| matches!(ch, '"' | '\''))
                .trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }

    None
}

const DEFAULT_AGENT_MODEL: &str = "claude-opus-4-8";
const DEFAULT_AGENT_SYSTEM_DATE: &str = "2026-03-31";
const DEFAULT_AGENT_MAX_ITERATIONS: usize = 32;

fn execute_agent(input: AgentInput) -> Result<AgentOutput, String> {
    execute_agent_with_spawn(input, spawn_agent_job)
}

fn execute_agent_with_spawn<F>(input: AgentInput, spawn_fn: F) -> Result<AgentOutput, String>
where
    F: FnOnce(AgentJob) -> Result<(), String>,
{
    if input.description.trim().is_empty() {
        return Err(String::from("description must not be empty"));
    }
    if input.prompt.trim().is_empty() {
        return Err(String::from("prompt must not be empty"));
    }

    let agent_id = make_agent_id();
    let output_dir = agent_store_dir()?;
    std::fs::create_dir_all(&output_dir).map_err(|error| error.to_string())?;
    let output_file = output_dir.join(format!("{agent_id}.md"));
    let manifest_file = output_dir.join(format!("{agent_id}.json"));
    let normalized_subagent_type = normalize_subagent_type(input.subagent_type.as_deref());
    let model = resolve_agent_model(input.model.as_deref());
    let agent_name = input
        .name
        .as_deref()
        .map(slugify_agent_name)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| slugify_agent_name(&input.description));
    let created_at = iso8601_now();
    let system_prompt = build_agent_system_prompt(&normalized_subagent_type, &model)?;
    let allowed_tools = allowed_tools_for_subagent(&normalized_subagent_type);

    let output_contents = format!(
        "# Agent Task

- id: {}
- name: {}
- description: {}
- subagent_type: {}
- created_at: {}

## Prompt

{}
",
        agent_id, agent_name, input.description, normalized_subagent_type, created_at, input.prompt
    );
    std::fs::write(&output_file, output_contents).map_err(|error| error.to_string())?;

    let manifest = AgentOutput {
        agent_id,
        name: agent_name,
        description: input.description,
        subagent_type: Some(normalized_subagent_type),
        model: Some(model),
        status: String::from("running"),
        output_file: output_file.display().to_string(),
        manifest_file: manifest_file.display().to_string(),
        created_at: created_at.clone(),
        started_at: Some(created_at),
        completed_at: None,
        lane_events: vec![LaneEvent::started(iso8601_now())],
        current_blocker: None,
        derived_state: String::from("working"),
        error: None,
    };
    write_agent_manifest(&manifest)?;

    let manifest_for_spawn = manifest.clone();
    let job = AgentJob {
        manifest: manifest_for_spawn,
        prompt: input.prompt,
        system_prompt,
        allowed_tools,
    };
    if let Err(error) = spawn_fn(job) {
        let error = format!("failed to spawn sub-agent: {error}");
        persist_agent_terminal_state(&manifest, "failed", None, Some(error.clone()))?;
        return Err(error);
    }

    Ok(manifest)
}

fn spawn_agent_job(job: AgentJob) -> Result<(), String> {
    let thread_name = format!("aosd-agent-{}", job.manifest.agent_id);
    std::thread::Builder::new()
        .name(thread_name)
        .spawn(move || {
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_agent_job(&job)));
            match result {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    let _ =
                        persist_agent_terminal_state(&job.manifest, "failed", None, Some(error));
                }
                Err(_) => {
                    let _ = persist_agent_terminal_state(
                        &job.manifest,
                        "failed",
                        None,
                        Some(String::from("sub-agent thread panicked")),
                    );
                }
            }
        })
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn run_agent_job(job: &AgentJob) -> Result<(), String> {
    let mut runtime = build_agent_runtime(job)?.with_max_iterations(DEFAULT_AGENT_MAX_ITERATIONS);
    let summary = futures::executor::block_on(runtime.run_turn(job.prompt.clone(), None, ()))
        .map_err(|error| error.to_string())?;
    let final_text = final_assistant_text(&summary);
    persist_agent_terminal_state(&job.manifest, "completed", Some(final_text.as_str()), None)
}

fn build_agent_runtime(
    job: &AgentJob,
) -> Result<ConversationRuntime<ProviderRuntimeClient, SubagentToolExecutor>, String> {
    let model = job
        .manifest
        .model
        .clone()
        .unwrap_or_else(|| DEFAULT_AGENT_MODEL.to_string());
    let allowed_tools = job.allowed_tools.clone();
    let api_client = ProviderRuntimeClient::new(model, allowed_tools.clone())?;
    let permission_policy = agent_permission_policy();
    let tool_executor = SubagentToolExecutor::new(allowed_tools)
        .with_enforcer(PermissionEnforcer::new(permission_policy.clone()));
    Ok(ConversationRuntime::new(
        Session::new(),
        api_client,
        tool_executor,
        permission_policy,
        job.system_prompt.clone(),
    ))
}

fn build_agent_system_prompt(subagent_type: &str, model: &str) -> Result<Vec<String>, String> {
    let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
    let mut prompt = load_system_prompt(
        cwd,
        DEFAULT_AGENT_SYSTEM_DATE.to_string(),
        std::env::consts::OS,
        "unknown",
        runtime::ModelFamilyIdentity::from_model(model),
    )
    .map_err(|error| error.to_string())?;
    prompt.push(format!(
        "You are a background sub-agent of type `{subagent_type}`. Work only on the delegated task, use only the tools available to you, do not ask the user questions, and finish with a concise result."
    ));
    Ok(prompt)
}

fn resolve_agent_model(model: Option<&str>) -> String {
    model
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .unwrap_or(DEFAULT_AGENT_MODEL)
        .to_string()
}

fn allowed_tools_for_subagent(subagent_type: &str) -> BTreeSet<String> {
    let tools = match subagent_type {
        "Explore" => vec![
            "read_file",
            "glob_search",
            "grep_search",
            "WebFetch",
            "WebSearch",
            "ToolSearch",
            "Skill",
            "StructuredOutput",
        ],
        "Plan" => vec![
            "read_file",
            "glob_search",
            "grep_search",
            "WebFetch",
            "WebSearch",
            "ToolSearch",
            "Skill",
            "TodoWrite",
            "StructuredOutput",
            "SendUserMessage",
        ],
        "Verification" => vec![
            "bash",
            "read_file",
            "glob_search",
            "grep_search",
            "WebFetch",
            "WebSearch",
            "ToolSearch",
            "TodoWrite",
            "StructuredOutput",
            "SendUserMessage",
            "PowerShell",
        ],
        "aos-guide" => vec![
            "read_file",
            "glob_search",
            "grep_search",
            "WebFetch",
            "WebSearch",
            "ToolSearch",
            "Skill",
            "StructuredOutput",
            "SendUserMessage",
        ],
        "statusline-setup" => vec![
            "bash",
            "read_file",
            "write_file",
            "edit_file",
            "glob_search",
            "grep_search",
            "ToolSearch",
        ],
        _ => vec![
            "bash",
            "read_file",
            "write_file",
            "edit_file",
            "glob_search",
            "grep_search",
            "WebFetch",
            "WebSearch",
            "TodoWrite",
            "Skill",
            "ToolSearch",
            "NotebookEdit",
            "Sleep",
            "SendUserMessage",
            "Config",
            "StructuredOutput",
            "REPL",
            "PowerShell",
        ],
    };
    tools.into_iter().map(canonical_allowed_tool_name).collect()
}

fn agent_permission_policy() -> PermissionPolicy {
    mvp_tool_specs().into_iter().fold(
        PermissionPolicy::new(PermissionMode::DangerFullAccess),
        |policy, spec| policy.with_tool_requirement(spec.name, spec.required_permission),
    )
}

fn write_agent_manifest(manifest: &AgentOutput) -> Result<(), String> {
    let mut normalized = manifest.clone();
    normalized.lane_events = dedupe_superseded_commit_events(&normalized.lane_events);
    std::fs::write(
        &normalized.manifest_file,
        serde_json::to_string_pretty(&normalized).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())
}

fn persist_agent_terminal_state(
    manifest: &AgentOutput,
    status: &str,
    result: Option<&str>,
    error: Option<String>,
) -> Result<(), String> {
    let blocker = error.as_deref().map(classify_lane_blocker);
    append_agent_output(
        &manifest.output_file,
        &format_agent_terminal_output(status, result, blocker.as_ref(), error.as_deref()),
    )?;
    let mut next_manifest = manifest.clone();
    next_manifest.status = status.to_string();
    next_manifest.completed_at = Some(iso8601_now());
    next_manifest.current_blocker.clone_from(&blocker);
    next_manifest.derived_state =
        derive_agent_state(status, result, error.as_deref(), blocker.as_ref()).to_string();
    next_manifest.error = error;
    if let Some(blocker) = blocker {
        next_manifest
            .lane_events
            .push(LaneEvent::blocked(iso8601_now(), &blocker));
        next_manifest
            .lane_events
            .push(LaneEvent::failed(iso8601_now(), &blocker));
    } else {
        next_manifest.current_blocker = None;
        let mut finished_summary = build_lane_finished_summary(&next_manifest, result);
        finished_summary.data.disabled_cron_ids = disable_matching_crons(&next_manifest, result);
        next_manifest.lane_events.push(
            LaneEvent::finished(iso8601_now(), finished_summary.detail).with_data(
                serde_json::to_value(&finished_summary.data)
                    .expect("lane summary metadata should serialize"),
            ),
        );
        if let Some(provenance) = maybe_commit_provenance(result) {
            next_manifest.lane_events.push(LaneEvent::commit_created(
                iso8601_now(),
                Some(format!("commit {}", provenance.commit)),
                provenance,
            ));
        }
    }
    write_agent_manifest(&next_manifest)
}

const MIN_LANE_SUMMARY_WORDS: usize = 7;
const REVIEW_VERDICTS: &[(&str, &str)] = &[
    ("APPROVE", "approve"),
    ("REJECT", "reject"),
    ("BLOCKED", "blocked"),
];
const CONTROL_ONLY_SUMMARY_WORDS: &[&str] = &[
    "ack",
    "commit",
    "continue",
    "everyting",
    "everything",
    "keep",
    "next",
    "push",
    "ralph",
    "resume",
    "retry",
    "run",
    "stop",
    "sweep",
    "sweeping",
    "team",
];
const CONTEXTUAL_SUMMARY_WORDS: &[&str] = &[
    "added",
    "audited",
    "blocked",
    "completed",
    "documented",
    "failed",
    "finished",
    "fixed",
    "implemented",
    "investigated",
    "merged",
    "pushed",
    "refactored",
    "removed",
    "reviewed",
    "tested",
    "updated",
    "verified",
];

#[derive(Debug, Clone, Serialize)]
struct LaneFinishedSummaryData {
    #[serde(rename = "qualityFloorApplied")]
    quality_floor_applied: bool,
    reasons: Vec<String>,
    #[serde(rename = "rawSummary", skip_serializing_if = "Option::is_none")]
    raw_summary: Option<String>,
    #[serde(rename = "wordCount")]
    word_count: usize,
    #[serde(rename = "reviewVerdict", skip_serializing_if = "Option::is_none")]
    review_verdict: Option<String>,
    #[serde(rename = "reviewTarget", skip_serializing_if = "Option::is_none")]
    review_target: Option<String>,
    #[serde(rename = "reviewRationale", skip_serializing_if = "Option::is_none")]
    review_rationale: Option<String>,
    #[serde(rename = "selectionOutcome", skip_serializing_if = "Option::is_none")]
    selection_outcome: Option<SelectionOutcome>,
    #[serde(rename = "recoveryOutcome", skip_serializing_if = "Option::is_none")]
    recovery_outcome: Option<RecoveryOutcome>,
    #[serde(rename = "artifactProvenance", skip_serializing_if = "Option::is_none")]
    artifact_provenance: Option<ArtifactProvenance>,
    #[serde(rename = "disabledCronIds", skip_serializing_if = "Vec::is_empty")]
    disabled_cron_ids: Vec<String>,
}

#[derive(Debug, Clone)]
struct LaneFinishedSummary {
    detail: Option<String>,
    data: LaneFinishedSummaryData,
}

#[derive(Debug)]
struct LaneSummaryAssessment {
    apply_quality_floor: bool,
    reasons: Vec<String>,
    word_count: usize,
    review_outcome: Option<ReviewLaneOutcome>,
    recovery_outcome: Option<RecoveryOutcome>,
}

#[derive(Debug, Clone)]
struct ReviewLaneOutcome {
    verdict: String,
    rationale: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct SelectionOutcome {
    #[serde(rename = "chosenItems", skip_serializing_if = "Vec::is_empty")]
    chosen_items: Vec<String>,
    #[serde(rename = "skippedItems", skip_serializing_if = "Vec::is_empty")]
    skipped_items: Vec<String>,
    action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    rationale: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct RecoveryOutcome {
    cause: String,
    #[serde(rename = "targetLane", skip_serializing_if = "Option::is_none")]
    target_lane: Option<String>,
    #[serde(rename = "preservedState", skip_serializing_if = "Option::is_none")]
    preserved_state: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct ArtifactProvenance {
    #[serde(rename = "sourceLanes", skip_serializing_if = "Vec::is_empty")]
    source_lanes: Vec<String>,
    #[serde(rename = "roadmapIds", skip_serializing_if = "Vec::is_empty")]
    roadmap_ids: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    files: Vec<String>,
    #[serde(rename = "diffStat", skip_serializing_if = "Option::is_none")]
    diff_stat: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    verification: Vec<String>,
    #[serde(rename = "commitSha", skip_serializing_if = "Option::is_none")]
    commit_sha: Option<String>,
}

fn build_lane_finished_summary(
    manifest: &AgentOutput,
    result: Option<&str>,
) -> LaneFinishedSummary {
    let raw_summary = result.map(str::trim).filter(|value| !value.is_empty());
    let assessment = assess_lane_summary_quality(raw_summary.unwrap_or_default());
    let detail = match raw_summary {
        Some(summary) if !assessment.apply_quality_floor => Some(compress_summary_text(summary)),
        Some(summary) => Some(compose_lane_summary_fallback(
            manifest,
            Some(summary),
            assessment.recovery_outcome.as_ref(),
        )),
        None => Some(compose_lane_summary_fallback(manifest, None, None)),
    };
    let review_outcome = assessment.review_outcome.clone();
    let recovery_outcome = assessment.recovery_outcome.clone();
    let review_target = review_outcome
        .as_ref()
        .map(|_| manifest.description.trim())
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let artifact_provenance = extract_artifact_provenance(manifest, raw_summary);

    LaneFinishedSummary {
        detail,
        data: LaneFinishedSummaryData {
            quality_floor_applied: raw_summary.is_none() || assessment.apply_quality_floor,
            reasons: assessment.reasons,
            raw_summary: raw_summary.map(str::to_string),
            word_count: assessment.word_count,
            review_verdict: review_outcome
                .as_ref()
                .map(|outcome| outcome.verdict.clone()),
            review_target,
            review_rationale: review_outcome.and_then(|outcome| outcome.rationale),
            selection_outcome: extract_selection_outcome(raw_summary.unwrap_or_default()),
            recovery_outcome,
            artifact_provenance,
            disabled_cron_ids: Vec::new(),
        },
    }
}

fn assess_lane_summary_quality(summary: &str) -> LaneSummaryAssessment {
    let words = summary
        .split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '-' || ch == '#'))
        .filter(|token| !token.is_empty())
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>();

    let word_count = words.len();
    let mut reasons = Vec::new();
    if summary.trim().is_empty() {
        reasons.push(String::from("empty"));
    }

    let review_outcome = extract_review_outcome(summary);
    let recovery_outcome = extract_recovery_outcome(summary);
    if recovery_outcome.is_some() {
        reasons.push(String::from("recovery_control_prose"));
    }

    let control_only = !words.is_empty()
        && words
            .iter()
            .all(|word| CONTROL_ONLY_SUMMARY_WORDS.contains(&word.as_str()));
    if control_only && review_outcome.is_none() {
        reasons.push(String::from("control_only"));
    }

    let has_context_signal = summary.contains('`')
        || summary.contains('/')
        || summary.contains(':')
        || summary.contains('#')
        || review_outcome.is_some()
        || words
            .iter()
            .any(|word| CONTEXTUAL_SUMMARY_WORDS.contains(&word.as_str()));
    if word_count < MIN_LANE_SUMMARY_WORDS && !has_context_signal {
        reasons.push(String::from("too_short_without_context"));
    }

    LaneSummaryAssessment {
        apply_quality_floor: !reasons.is_empty(),
        reasons,
        word_count,
        review_outcome,
        recovery_outcome,
    }
}

fn compose_lane_summary_fallback(
    manifest: &AgentOutput,
    raw_summary: Option<&str>,
    recovery_outcome: Option<&RecoveryOutcome>,
) -> String {
    let target = manifest.description.trim();
    let base = format!(
        "Completed lane `{}` for target: {}. Status: completed.",
        manifest.name,
        if target.is_empty() {
            "unspecified task"
        } else {
            target
        }
    );
    if let Some(outcome) = recovery_outcome {
        let mut detail = format!(
            "{base} Recovery handoff observed via tmux reinjection (cause: `{}`).",
            outcome.cause
        );
        if let Some(target_lane) = &outcome.target_lane {
            let _ = std::fmt::Write::write_fmt(
                &mut detail,
                format_args!(" Target lane: `{target_lane}`."),
            );
        }
        if let Some(preserved_state) = &outcome.preserved_state {
            let _ = std::fmt::Write::write_fmt(
                &mut detail,
                format_args!(" Preserved state: {preserved_state}."),
            );
        }
        return detail;
    }
    match raw_summary {
        Some(summary) => format!(
            "{base} Original stop summary was too vague to keep as the lane result: \"{}\".",
            summary.trim()
        ),
        None => format!("{base} No usable stop summary was produced by the lane."),
    }
}

fn extract_review_outcome(summary: &str) -> Option<ReviewLaneOutcome> {
    let mut lines = summary
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty());
    let first = lines.next()?;
    let verdict = REVIEW_VERDICTS.iter().find_map(|(prefix, verdict)| {
        first
            .eq_ignore_ascii_case(prefix)
            .then(|| (*verdict).to_string())
    })?;
    let rationale = lines.collect::<Vec<_>>().join(" ").trim().to_string();
    Some(ReviewLaneOutcome {
        verdict,
        rationale: (!rationale.is_empty()).then_some(compress_summary_text(&rationale)),
    })
}

fn extract_selection_outcome(summary: &str) -> Option<SelectionOutcome> {
    let mut chosen_items = Vec::new();
    let mut skipped_items = Vec::new();
    let mut action = None;
    let mut rationale = None;

    for line in summary
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        let lowered = line.to_ascii_lowercase();
        let roadmap_items = extract_roadmap_items(line);

        if lowered.starts_with("chosen:")
            || lowered.starts_with("picked:")
            || lowered.starts_with("selected:")
            || (lowered.contains("picked") && !roadmap_items.is_empty())
            || (lowered.contains("selected") && !roadmap_items.is_empty())
        {
            chosen_items.extend(roadmap_items);
        } else if lowered.starts_with("skipped:")
            || lowered.starts_with("skip:")
            || (lowered.contains("skipped") && !roadmap_items.is_empty())
        {
            skipped_items.extend(roadmap_items);
        }

        if let Some(rest) = lowered.strip_prefix("action:") {
            if rest.contains("execute") || rest.contains("implement") || rest.contains("fix") {
                action = Some(String::from("execute"));
            } else if rest.contains("review") || rest.contains("audit") {
                action = Some(String::from("review"));
            } else if rest.contains("no-op") || rest.contains("noop") {
                action = Some(String::from("no-op"));
            }
        }

        if let Some(rest) = line.strip_prefix("Rationale:") {
            let trimmed = rest.trim();
            if !trimmed.is_empty() {
                rationale = Some(compress_summary_text(trimmed));
            }
        }
    }

    chosen_items.sort();
    chosen_items.dedup();
    skipped_items.sort();
    skipped_items.dedup();

    if chosen_items.is_empty() && skipped_items.is_empty() && action.is_none() {
        return None;
    }

    let default_action = if chosen_items.is_empty() {
        String::from("no-op")
    } else {
        String::from("execute")
    };

    Some(SelectionOutcome {
        chosen_items,
        skipped_items,
        action: action.unwrap_or(default_action),
        rationale,
    })
}

fn extract_recovery_outcome(summary: &str) -> Option<RecoveryOutcome> {
    let trimmed = summary.trim();
    if trimmed.is_empty() {
        return None;
    }

    let lowered = trimmed.to_ascii_lowercase();
    let has_tmux_inject_marker = lowered.contains("omx_tmux_inject");
    let has_recovery_phrase = lowered.contains("continue from current mode state")
        || (lowered.starts_with("team ") && lowered.contains(" next:"));
    if !has_tmux_inject_marker && !has_recovery_phrase {
        return None;
    }

    let cause = if lowered.contains("current mode state") {
        "resume_after_stop"
    } else if lowered.contains("tool failure") {
        "retry_after_tool_failure"
    } else if lowered.contains("worker panes stalled")
        || lowered.contains("no progress")
        || lowered.contains("leader stale")
        || lowered.contains("all workers idle")
        || lowered.contains("all 1 worker idle")
        || lowered.contains("pane(s) active")
    {
        "tmux_reinject_after_idle"
    } else {
        "manual_recovery"
    };

    let target_lane = trimmed.lines().map(str::trim).find_map(|line| {
        let lower = line.to_ascii_lowercase();
        if !lower.starts_with("team ") {
            return None;
        }
        line[5..]
            .split_once(':')
            .map(|(name, _)| name.trim())
            .filter(|name| !name.is_empty())
            .map(str::to_string)
    });

    let preserved_state = lowered
        .contains("current mode state")
        .then(|| String::from("current mode state"));

    Some(RecoveryOutcome {
        cause: cause.to_string(),
        target_lane,
        preserved_state,
    })
}

fn extract_roadmap_items(line: &str) -> Vec<String> {
    let mut items = Vec::new();
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '#' {
            let mut digits = String::new();
            while let Some(next) = chars.peek() {
                if next.is_ascii_digit() {
                    digits.push(*next);
                    chars.next();
                } else {
                    break;
                }
            }
            if !digits.is_empty() {
                items.push(format!("ROADMAP #{digits}"));
            }
        }
    }
    items
}

fn extract_artifact_provenance(
    manifest: &AgentOutput,
    raw_summary: Option<&str>,
) -> Option<ArtifactProvenance> {
    let summary = raw_summary?;
    let mut roadmap_ids = extract_roadmap_items(summary);
    roadmap_ids.extend(extract_roadmap_items(&manifest.description));
    roadmap_ids.sort();
    roadmap_ids.dedup();

    let mut files = extract_file_paths(summary);
    files.sort();
    files.dedup();

    let mut verification = Vec::new();
    let lowered = summary.to_ascii_lowercase();
    for (needle, label) in [
        ("tested", "tested"),
        ("committed", "committed"),
        ("pushed", "pushed"),
        ("merged", "merged"),
    ] {
        if lowered.contains(needle) {
            verification.push(label.to_string());
        }
    }

    let commit_sha = extract_commit_sha(summary);
    let diff_stat = extract_diff_stat(summary);
    let source_lanes = vec![manifest.name.clone()];

    if roadmap_ids.is_empty()
        && files.is_empty()
        && verification.is_empty()
        && commit_sha.is_none()
        && diff_stat.is_none()
    {
        return None;
    }

    Some(ArtifactProvenance {
        source_lanes,
        roadmap_ids,
        files,
        diff_stat,
        verification,
        commit_sha,
    })
}

fn extract_file_paths(summary: &str) -> Vec<String> {
    summary
        .split(|ch: char| ch.is_whitespace() || matches!(ch, ',' | ';' | '(' | ')' | '[' | ']'))
        .map(|token| {
            token
                .trim_matches('`')
                .trim_matches('"')
                .trim_matches('\'')
                .trim_end_matches('.')
        })
        .filter(|token| {
            token.contains('.')
                && !token.starts_with("http")
                && !token
                    .chars()
                    .all(|ch| ch.is_ascii_digit() || ch == '.' || ch == '+' || ch == '-')
        })
        .map(str::to_string)
        .collect()
}

fn extract_diff_stat(summary: &str) -> Option<String> {
    summary
        .split('\n')
        .map(str::trim)
        .find_map(|line| {
            line.find("Diff stat:")
                .map(|index| normalize_diff_stat(&line[(index + "Diff stat:".len())..]))
                .or_else(|| {
                    line.find("Diff:")
                        .map(|index| normalize_diff_stat(&line[(index + "Diff:".len())..]))
                })
        })
        .filter(|value| !value.is_empty())
}

fn normalize_diff_stat(value: &str) -> String {
    let trimmed = value.trim();
    for marker in [" Tested", " Committed", " committed", " pushed", " merged"] {
        if let Some((prefix, _)) = trimmed.split_once(marker) {
            return prefix.trim().to_string();
        }
    }
    trimmed.to_string()
}

fn disable_matching_crons(manifest: &AgentOutput, result: Option<&str>) -> Vec<String> {
    let tokens = cron_match_tokens(manifest, result);
    if tokens.is_empty() {
        return Vec::new();
    }

    let mut disabled = Vec::new();
    for entry in global_cron_registry().list(true) {
        let haystack = format!(
            "{} {}",
            entry.prompt,
            entry.description.as_deref().unwrap_or_default()
        )
        .to_ascii_lowercase();
        if tokens.iter().any(|token| haystack.contains(token))
            && global_cron_registry().disable(&entry.cron_id).is_ok()
        {
            disabled.push(entry.cron_id);
        }
    }
    disabled.sort();
    disabled
}

fn cron_match_tokens(manifest: &AgentOutput, result: Option<&str>) -> Vec<String> {
    let mut tokens = extract_roadmap_items(manifest.description.as_str())
        .into_iter()
        .chain(extract_roadmap_items(result.unwrap_or_default()))
        .map(|item| item.to_ascii_lowercase())
        .collect::<Vec<_>>();

    if tokens.is_empty() && !manifest.name.trim().is_empty() {
        tokens.push(manifest.name.trim().to_ascii_lowercase());
    }

    tokens.sort();
    tokens.dedup();
    tokens
}

fn derive_agent_state(
    status: &str,
    result: Option<&str>,
    error: Option<&str>,
    blocker: Option<&LaneEventBlocker>,
) -> &'static str {
    let normalized_status = status.trim().to_ascii_lowercase();
    let normalized_error = error.unwrap_or_default().to_ascii_lowercase();

    if normalized_status == "running" {
        return "working";
    }
    if normalized_status == "completed" {
        return if result.is_some_and(|value| !value.trim().is_empty()) {
            "finished_cleanable"
        } else {
            "finished_pending_report"
        };
    }
    if normalized_error.contains("background") {
        return "blocked_background_job";
    }
    if normalized_error.contains("merge conflict") || normalized_error.contains("cherry-pick") {
        return "blocked_merge_conflict";
    }
    if normalized_error.contains("mcp") {
        return "degraded_mcp";
    }
    if normalized_error.contains("transport")
        || normalized_error.contains("broken pipe")
        || normalized_error.contains("connection")
        || normalized_error.contains("interrupted")
    {
        return "interrupted_transport";
    }
    if blocker.is_some() {
        return "truly_idle";
    }
    "truly_idle"
}

fn maybe_commit_provenance(result: Option<&str>) -> Option<LaneCommitProvenance> {
    let commit = extract_commit_sha(result?)?;
    let branch = current_git_branch().unwrap_or_else(|| "unknown".to_string());
    let worktree = std::env::current_dir()
        .ok()
        .map(|path| path.display().to_string());
    Some(LaneCommitProvenance {
        commit: commit.clone(),
        branch,
        worktree,
        canonical_commit: Some(commit.clone()),
        superseded_by: None,
        lineage: vec![commit],
    })
}

fn extract_commit_sha(result: &str) -> Option<String> {
    result
        .split(|c: char| !c.is_ascii_hexdigit())
        .find(|token| token.len() >= 7 && token.len() <= 40)
        .map(str::to_string)
}

fn current_git_branch() -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!branch.is_empty() && branch != "HEAD").then_some(branch)
}

fn append_agent_output(path: &str, suffix: &str) -> Result<(), String> {
    use std::io::Write as _;

    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .map_err(|error| error.to_string())?;
    file.write_all(suffix.as_bytes())
        .map_err(|error| error.to_string())
}

fn format_agent_terminal_output(
    status: &str,
    result: Option<&str>,
    blocker: Option<&LaneEventBlocker>,
    error: Option<&str>,
) -> String {
    let mut sections = vec![format!("\n## Result\n\n- status: {status}\n")];
    if let Some(blocker) = blocker {
        sections.push(format!(
            "\n### Blocker\n\n- failure_class: {}\n- detail: {}\n",
            serde_json::to_string(&blocker.failure_class)
                .unwrap_or_else(|_| "\"infra\"".to_string())
                .trim_matches('"'),
            blocker.detail.trim()
        ));
    }
    if let Some(result) = result.filter(|value| !value.trim().is_empty()) {
        sections.push(format!("\n### Final response\n\n{}\n", result.trim()));
    }
    if let Some(error) = error.filter(|value| !value.trim().is_empty()) {
        sections.push(format!("\n### Error\n\n{}\n", error.trim()));
    }
    sections.join("")
}

fn classify_lane_blocker(error: &str) -> LaneEventBlocker {
    let detail = error.trim().to_string();
    LaneEventBlocker {
        failure_class: classify_lane_failure(error),
        detail,
    }
}

fn classify_lane_failure(error: &str) -> LaneFailureClass {
    let normalized = error.to_ascii_lowercase();

    if normalized.contains("prompt") && normalized.contains("deliver") {
        LaneFailureClass::PromptDelivery
    } else if normalized.contains("trust") {
        LaneFailureClass::TrustGate
    } else if normalized.contains("branch")
        && (normalized.contains("stale") || normalized.contains("diverg"))
    {
        LaneFailureClass::BranchDivergence
    } else if normalized.contains("gateway") || normalized.contains("routing") {
        LaneFailureClass::GatewayRouting
    } else if normalized.contains("compile")
        || normalized.contains("build failed")
        || normalized.contains("cargo check")
    {
        LaneFailureClass::Compile
    } else if normalized.contains("test") {
        LaneFailureClass::Test
    } else if normalized.contains("tool failed")
        || normalized.contains("runtime tool")
        || normalized.contains("tool runtime")
    {
        LaneFailureClass::ToolRuntime
    } else if normalized.contains("workspace") && normalized.contains("mismatch") {
        LaneFailureClass::WorkspaceMismatch
    } else if normalized.contains("plugin") {
        LaneFailureClass::PluginStartup
    } else if normalized.contains("mcp") && normalized.contains("handshake") {
        LaneFailureClass::McpHandshake
    } else if normalized.contains("mcp") {
        LaneFailureClass::McpStartup
    } else {
        LaneFailureClass::Infra
    }
}

#[derive(Clone)]
struct ProviderEntry {
    model: String,
    client: ProviderClient,
}

struct ProviderRuntimeClient {
    #[expect(dead_code)]
    runtime: tokio::runtime::Runtime,
    chain: Vec<ProviderEntry>,
    allowed_tools: BTreeSet<String>,
}

impl ProviderRuntimeClient {
    #[allow(clippy::needless_pass_by_value)]
    fn new(model: String, allowed_tools: BTreeSet<String>) -> Result<Self, String> {
        let fallback_config = load_provider_fallback_config();
        Self::new_with_fallback_config(model, allowed_tools, &fallback_config)
    }

    #[allow(clippy::needless_pass_by_value)]
    fn new_with_fallback_config(
        model: String,
        allowed_tools: BTreeSet<String>,
        fallback_config: &ProviderFallbackConfig,
    ) -> Result<Self, String> {
        let primary_model = fallback_config.primary().map_or(model, str::to_string);
        let primary = build_provider_entry(&primary_model)?;
        let mut chain = vec![primary];
        for fallback_model in fallback_config.fallbacks() {
            match build_provider_entry(fallback_model) {
                Ok(entry) => chain.push(entry),
                Err(error) => {
                    eprintln!(
                        "warning: skipping unavailable fallback provider {fallback_model}: {error}"
                    );
                }
            }
        }
        Ok(Self {
            runtime: tokio::runtime::Runtime::new().map_err(|error| error.to_string())?,
            chain,
            allowed_tools,
        })
    }
}

fn build_provider_entry(model: &str) -> Result<ProviderEntry, String> {
    let resolved = resolve_model_alias(model).clone();
    let client = ProviderClient::from_model(&resolved).map_err(|error| error.to_string())?;
    Ok(ProviderEntry {
        model: resolved,
        client,
    })
}

fn load_provider_fallback_config() -> ProviderFallbackConfig {
    std::env::current_dir()
        .ok()
        .and_then(|cwd| ConfigLoader::default_for(cwd).load().ok())
        .map_or_else(ProviderFallbackConfig::default, |config| {
            config.provider_fallbacks().clone()
        })
}

#[::async_trait::async_trait]
impl ApiClient for ProviderRuntimeClient {
    async fn stream(&mut self, request: ApiRequest) -> Result<Vec<AssistantEvent>, RuntimeError> {
        let tools = tool_specs_for_allowed_tools(Some(&self.allowed_tools))
            .into_iter()
            .map(|spec| ToolDefinition {
                name: spec.name.to_string(),
                description: Some(spec.description.to_string()),
                input_schema: spec.input_schema,
            })
            .collect::<Vec<_>>();
        let messages = convert_messages(&request.messages);
        let system =
            (!request.system_prompt.is_empty()).then(|| request.system_prompt.join("\n\n"));
        let tool_choice = (!self.allowed_tools.is_empty()).then_some(ToolChoice::Auto);

        let chain: Vec<_> = self.chain.clone();
        let mut last_error: Option<ApiError> = None;
        for (index, entry) in chain.iter().enumerate() {
            let message_request = MessageRequest {
                model: entry.model.clone(),
                max_tokens: max_tokens_for_model(&entry.model),
                messages: messages.clone(),
                system: system.clone(),
                tools: (!tools.is_empty()).then(|| tools.clone()),
                tool_choice: tool_choice.clone(),
                stream: true,
                ..Default::default()
            };

            let client = entry.client.clone();
            let attempt =
                futures::executor::block_on(stream_with_provider(&client, &message_request));
            match attempt {
                Ok(events) => return Ok(events),
                Err(error) if error.is_retryable() && index + 1 < chain.len() => {
                    eprintln!(
                        "provider {} failed with retryable error, falling back: {error}",
                        entry.model
                    );
                    last_error = Some(error);
                }
                Err(error) => return Err(RuntimeError::new(error.to_string())),
            }
        }

        Err(RuntimeError::new(last_error.map_or_else(
            || String::from("provider chain exhausted with no attempts"),
            |error| error.to_string(),
        )))
    }
}

#[allow(clippy::too_many_lines)]
async fn stream_with_provider(
    client: &ProviderClient,
    message_request: &MessageRequest,
) -> Result<Vec<AssistantEvent>, ApiError> {
    let mut stream = client.stream_message(message_request).await?;
    let mut events = Vec::new();
    let mut pending_tools: BTreeMap<u32, (String, String, String)> = BTreeMap::new();
    let mut saw_stop = false;

    while let Some(event) = stream.next_event().await? {
        match event {
            ApiStreamEvent::MessageStart(start) => {
                for block in start.message.content {
                    push_output_block(block, 0, &mut events, &mut pending_tools, true);
                }
            }
            ApiStreamEvent::ContentBlockStart(start) => {
                push_output_block(
                    start.content_block,
                    start.index,
                    &mut events,
                    &mut pending_tools,
                    true,
                );
            }
            ApiStreamEvent::ContentBlockDelta(delta) => match delta.delta {
                ContentBlockDelta::TextDelta { text } => {
                    if !text.is_empty() {
                        events.push(AssistantEvent::TextDelta(text));
                    }
                }
                ContentBlockDelta::InputJsonDelta { partial_json } => {
                    if let Some((_, _, input)) = pending_tools.get_mut(&delta.index) {
                        input.push_str(&partial_json);
                    }
                }
                ContentBlockDelta::ThinkingDelta { .. }
                | ContentBlockDelta::SignatureDelta { .. } => {}
            },
            ApiStreamEvent::ContentBlockStop(stop) => {
                if let Some((id, name, input)) = pending_tools.remove(&stop.index) {
                    events.push(AssistantEvent::ToolUse { id, name, input });
                }
            }
            ApiStreamEvent::MessageDelta(delta) => {
                events.push(AssistantEvent::Usage(delta.usage.token_usage()));
            }
            ApiStreamEvent::MessageStop(_) => {
                saw_stop = true;
                events.push(AssistantEvent::MessageStop);
            }
        }
    }

    push_prompt_cache_record(client, &mut events);

    if !saw_stop
        && events.iter().any(|event| {
            matches!(event, AssistantEvent::TextDelta(text) if !text.is_empty())
                || matches!(event, AssistantEvent::ToolUse { .. })
        })
    {
        events.push(AssistantEvent::MessageStop);
    }

    if events
        .iter()
        .any(|event| matches!(event, AssistantEvent::MessageStop))
    {
        return Ok(events);
    }

    let response = client
        .send_message(&MessageRequest {
            stream: false,
            ..message_request.clone()
        })
        .await?;
    let mut events = response_to_events(response);
    push_prompt_cache_record(client, &mut events);
    Ok(events)
}

struct SubagentToolExecutor {
    allowed_tools: BTreeSet<String>,
    enforcer: Option<PermissionEnforcer>,
}

impl SubagentToolExecutor {
    fn new(allowed_tools: BTreeSet<String>) -> Self {
        Self {
            allowed_tools,
            enforcer: None,
        }
    }

    fn with_enforcer(mut self, enforcer: PermissionEnforcer) -> Self {
        self.enforcer = Some(enforcer);
        self
    }
}

impl ToolExecutor for SubagentToolExecutor {
    fn execute(&mut self, tool_name: &str, input: &str) -> Result<String, ToolError> {
        if !self
            .allowed_tools
            .contains(&canonical_allowed_tool_name(tool_name))
        {
            return Err(ToolError::new(format!(
                "tool `{tool_name}` is not enabled for this sub-agent"
            )));
        }
        let value = serde_json::from_str(input)
            .map_err(|error| ToolError::new(format!("invalid tool input JSON: {error}")))?;
        execute_tool_with_enforcer(self.enforcer.as_ref(), tool_name, &value)
            .map_err(ToolError::new)
    }
}

fn tool_specs_for_allowed_tools(allowed_tools: Option<&BTreeSet<String>>) -> Vec<ToolSpec> {
    mvp_tool_specs()
        .into_iter()
        .filter(|spec| {
            allowed_tools
                .is_none_or(|allowed| allowed.contains(&canonical_allowed_tool_name(spec.name)))
        })
        .collect()
}

fn convert_messages(messages: &[ConversationMessage]) -> Vec<InputMessage> {
    messages
        .iter()
        .filter_map(|message| {
            let role = match message.role {
                MessageRole::System | MessageRole::User | MessageRole::Tool => "user",
                MessageRole::Assistant => "assistant",
            };
            let content = message
                .blocks
                .iter()
                .map(|block| match block {
                    ContentBlock::Text { text } => InputContentBlock::Text { text: text.clone() },
                    ContentBlock::ToolUse { id, name, input } => InputContentBlock::ToolUse {
                        id: id.clone(),
                        name: name.clone(),
                        input: serde_json::from_str(input)
                            .unwrap_or_else(|_| serde_json::json!({ "raw": input })),
                    },
                    ContentBlock::ToolResult {
                        tool_use_id,
                        output,
                        is_error,
                        ..
                    } => InputContentBlock::ToolResult {
                        tool_use_id: tool_use_id.clone(),
                        content: vec![ToolResultContentBlock::Text {
                            text: output.clone(),
                        }],
                        is_error: *is_error,
                    },
                })
                .collect::<Vec<_>>();
            (!content.is_empty()).then(|| InputMessage {
                role: role.to_string(),
                content,
            })
        })
        .collect()
}

fn push_output_block(
    block: OutputContentBlock,
    block_index: u32,
    events: &mut Vec<AssistantEvent>,
    pending_tools: &mut BTreeMap<u32, (String, String, String)>,
    streaming_tool_input: bool,
) {
    match block {
        OutputContentBlock::Text { text } => {
            if !text.is_empty() {
                events.push(AssistantEvent::TextDelta(text));
            }
        }
        OutputContentBlock::ToolUse { id, name, input } => {
            let initial_input = if streaming_tool_input
                && input.is_object()
                && input.as_object().is_some_and(serde_json::Map::is_empty)
            {
                String::new()
            } else {
                input.to_string()
            };
            pending_tools.insert(block_index, (id, name, initial_input));
        }
        OutputContentBlock::Thinking { .. } | OutputContentBlock::RedactedThinking { .. } => {}
    }
}

fn response_to_events(response: MessageResponse) -> Vec<AssistantEvent> {
    let mut events = Vec::new();
    let mut pending_tools = BTreeMap::new();

    for (index, block) in response.content.into_iter().enumerate() {
        let index = u32::try_from(index).expect("response block index overflow");
        push_output_block(block, index, &mut events, &mut pending_tools, false);
        if let Some((id, name, input)) = pending_tools.remove(&index) {
            events.push(AssistantEvent::ToolUse { id, name, input });
        }
    }

    events.push(AssistantEvent::Usage(response.usage.token_usage()));
    events.push(AssistantEvent::MessageStop);
    events
}

fn push_prompt_cache_record(client: &ProviderClient, events: &mut Vec<AssistantEvent>) {
    if let Some(record) = client.take_last_prompt_cache_record() {
        if let Some(event) = prompt_cache_record_to_runtime_event(record) {
            events.push(AssistantEvent::PromptCache(event));
        }
    }
}

fn prompt_cache_record_to_runtime_event(
    record: api::PromptCacheRecord,
) -> Option<PromptCacheEvent> {
    let cache_break = record.cache_break?;
    Some(PromptCacheEvent {
        unexpected: cache_break.unexpected,
        reason: cache_break.reason,
        previous_cache_read_input_tokens: cache_break.previous_cache_read_input_tokens,
        current_cache_read_input_tokens: cache_break.current_cache_read_input_tokens,
        token_drop: cache_break.token_drop,
    })
}

fn final_assistant_text(summary: &runtime::TurnSummary) -> String {
    summary
        .assistant_messages
        .last()
        .map(|message| {
            message
                .blocks
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default()
}

#[allow(clippy::needless_pass_by_value)]
fn execute_tool_search(input: ToolSearchInput) -> ToolSearchOutput {
    GlobalToolRegistry::builtin().search(&input.query, input.max_results.unwrap_or(5), None, None)
}

fn deferred_tool_specs() -> Vec<ToolSpec> {
    mvp_tool_specs()
        .into_iter()
        .filter(|spec| {
            !matches!(
                spec.name,
                "bash" | "read_file" | "write_file" | "edit_file" | "glob_search" | "grep_search"
            )
        })
        .collect()
}

fn search_tool_specs(query: &str, max_results: usize, specs: &[SearchableToolSpec]) -> Vec<String> {
    let lowered = query.to_lowercase();
    if let Some(selection) = lowered.strip_prefix("select:") {
        return selection
            .split(',')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .filter_map(|wanted| {
                let wanted = canonical_tool_token(wanted);
                specs
                    .iter()
                    .find(|spec| canonical_tool_token(&spec.name) == wanted)
                    .map(|spec| spec.name.clone())
            })
            .take(max_results)
            .collect();
    }

    let mut required = Vec::new();
    let mut optional = Vec::new();
    for term in lowered.split_whitespace() {
        if let Some(rest) = term.strip_prefix('+') {
            if !rest.is_empty() {
                required.push(rest);
            }
        } else {
            optional.push(term);
        }
    }
    let terms = if required.is_empty() {
        optional.clone()
    } else {
        required.iter().chain(optional.iter()).copied().collect()
    };

    let mut scored = specs
        .iter()
        .filter_map(|spec| {
            let name = spec.name.to_lowercase();
            let canonical_name = canonical_tool_token(&spec.name);
            let normalized_description = normalize_tool_search_query(&spec.description);
            let haystack = format!(
                "{name} {} {canonical_name}",
                spec.description.to_lowercase()
            );
            let normalized_haystack = format!("{canonical_name} {normalized_description}");
            if required.iter().any(|term| !haystack.contains(term)) {
                return None;
            }

            let mut score = 0_i32;
            for term in &terms {
                let canonical_term = canonical_tool_token(term);
                if haystack.contains(term) {
                    score += 2;
                }
                if name == *term {
                    score += 8;
                }
                if name.contains(term) {
                    score += 4;
                }
                if canonical_name == canonical_term {
                    score += 12;
                }
                if normalized_haystack.contains(&canonical_term) {
                    score += 3;
                }
            }

            if score == 0 && !lowered.is_empty() {
                return None;
            }
            Some((score, spec.name.clone()))
        })
        .collect::<Vec<_>>();

    scored.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
    scored
        .into_iter()
        .map(|(_, name)| name)
        .take(max_results)
        .collect()
}

fn normalize_tool_search_query(query: &str) -> String {
    query
        .trim()
        .split(|ch: char| ch.is_whitespace() || ch == ',')
        .filter(|term| !term.is_empty())
        .map(canonical_tool_token)
        .collect::<Vec<_>>()
        .join(" ")
}

fn canonical_tool_token(value: &str) -> String {
    let mut canonical = value
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .flat_map(char::to_lowercase)
        .collect::<String>();
    if let Some(stripped) = canonical.strip_suffix("tool") {
        canonical = stripped.to_string();
    }
    canonical
}

fn agent_store_dir() -> Result<std::path::PathBuf, String> {
    if let Ok(path) = std::env::var("AOSD_AGENT_STORE") {
        return Ok(std::path::PathBuf::from(path));
    }
    let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
    if let Some(workspace_root) = cwd.ancestors().nth(2) {
        return Ok(workspace_root.join(".aosd-agents"));
    }
    Ok(cwd.join(".aosd-agents"))
}

fn make_agent_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("agent-{nanos}")
}

fn slugify_agent_name(description: &str) -> String {
    let mut out = description
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>();
    while out.contains("--") {
        out = out.replace("--", "-");
    }
    out.trim_matches('-').chars().take(32).collect()
}

fn normalize_subagent_type(subagent_type: Option<&str>) -> String {
    let trimmed = subagent_type.map(str::trim).unwrap_or_default();
    if trimmed.is_empty() {
        return String::from("general-purpose");
    }

    match canonical_tool_token(trimmed).as_str() {
        "general" | "generalpurpose" | "generalpurposeagent" => String::from("general-purpose"),
        "explore" | "explorer" | "exploreagent" => String::from("Explore"),
        "plan" | "planagent" => String::from("Plan"),
        "verification" | "verificationagent" | "verify" | "verifier" => {
            String::from("Verification")
        }
        "aosguide" | "aosguideagent" | "guide" => String::from("aos-guide"),
        "statusline" | "statuslinesetup" => String::from("statusline-setup"),
        _ => trimmed.to_string(),
    }
}

fn iso8601_now() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .to_string()
}

#[allow(clippy::too_many_lines)]
fn execute_notebook_edit(input: NotebookEditInput) -> Result<NotebookEditOutput, String> {
    let path = std::path::PathBuf::from(&input.notebook_path);
    if path.extension().and_then(|ext| ext.to_str()) != Some("ipynb") {
        return Err(String::from(
            "File must be a Jupyter notebook (.ipynb file).",
        ));
    }

    let original_file = std::fs::read_to_string(&path).map_err(|error| error.to_string())?;
    let mut notebook: serde_json::Value =
        serde_json::from_str(&original_file).map_err(|error| error.to_string())?;
    let language = notebook
        .get("metadata")
        .and_then(|metadata| metadata.get("kernelspec"))
        .and_then(|kernelspec| kernelspec.get("language"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("python")
        .to_string();
    let cells = notebook
        .get_mut("cells")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or_else(|| String::from("Notebook cells array not found"))?;

    let edit_mode = input.edit_mode.unwrap_or(NotebookEditMode::Replace);
    let target_index = match input.cell_id.as_deref() {
        Some(cell_id) => Some(resolve_cell_index(cells, Some(cell_id), edit_mode)?),
        None if matches!(
            edit_mode,
            NotebookEditMode::Replace | NotebookEditMode::Delete
        ) =>
        {
            Some(resolve_cell_index(cells, None, edit_mode)?)
        }
        None => None,
    };
    let resolved_cell_type = match edit_mode {
        NotebookEditMode::Delete => None,
        NotebookEditMode::Insert => Some(input.cell_type.unwrap_or(NotebookCellType::Code)),
        NotebookEditMode::Replace => Some(input.cell_type.unwrap_or_else(|| {
            target_index
                .and_then(|index| cells.get(index))
                .and_then(cell_kind)
                .unwrap_or(NotebookCellType::Code)
        })),
    };
    let new_source = require_notebook_source(input.new_source, edit_mode)?;

    let cell_id = match edit_mode {
        NotebookEditMode::Insert => {
            let resolved_cell_type = resolved_cell_type
                .ok_or_else(|| String::from("insert mode requires a cell type"))?;
            let new_id = make_cell_id(cells.len());
            let new_cell = build_notebook_cell(&new_id, resolved_cell_type, &new_source);
            let insert_at = target_index.map_or(cells.len(), |index| index + 1);
            cells.insert(insert_at, new_cell);
            cells
                .get(insert_at)
                .and_then(|cell| cell.get("id"))
                .and_then(serde_json::Value::as_str)
                .map(ToString::to_string)
        }
        NotebookEditMode::Delete => {
            let idx = target_index
                .ok_or_else(|| String::from("delete mode requires a target cell index"))?;
            let removed = cells.remove(idx);
            removed
                .get("id")
                .and_then(serde_json::Value::as_str)
                .map(ToString::to_string)
        }
        NotebookEditMode::Replace => {
            let resolved_cell_type = resolved_cell_type
                .ok_or_else(|| String::from("replace mode requires a cell type"))?;
            let idx = target_index
                .ok_or_else(|| String::from("replace mode requires a target cell index"))?;
            let cell = cells
                .get_mut(idx)
                .ok_or_else(|| String::from("Cell index out of range"))?;
            cell["source"] = serde_json::Value::Array(source_lines(&new_source));
            cell["cell_type"] = serde_json::Value::String(match resolved_cell_type {
                NotebookCellType::Code => String::from("code"),
                NotebookCellType::Markdown => String::from("markdown"),
            });
            match resolved_cell_type {
                NotebookCellType::Code => {
                    if !cell.get("outputs").is_some_and(serde_json::Value::is_array) {
                        cell["outputs"] = json!([]);
                    }
                    if cell.get("execution_count").is_none() {
                        cell["execution_count"] = serde_json::Value::Null;
                    }
                }
                NotebookCellType::Markdown => {
                    if let Some(object) = cell.as_object_mut() {
                        object.remove("outputs");
                        object.remove("execution_count");
                    }
                }
            }
            cell.get("id")
                .and_then(serde_json::Value::as_str)
                .map(ToString::to_string)
        }
    };

    let updated_file =
        serde_json::to_string_pretty(&notebook).map_err(|error| error.to_string())?;
    std::fs::write(&path, &updated_file).map_err(|error| error.to_string())?;

    Ok(NotebookEditOutput {
        new_source,
        cell_id,
        cell_type: resolved_cell_type,
        language,
        edit_mode: format_notebook_edit_mode(edit_mode),
        error: None,
        notebook_path: path.display().to_string(),
        original_file,
        updated_file,
    })
}

fn require_notebook_source(
    source: Option<String>,
    edit_mode: NotebookEditMode,
) -> Result<String, String> {
    match edit_mode {
        NotebookEditMode::Delete => Ok(source.unwrap_or_default()),
        NotebookEditMode::Insert | NotebookEditMode::Replace => source
            .ok_or_else(|| String::from("new_source is required for insert and replace edits")),
    }
}

fn build_notebook_cell(cell_id: &str, cell_type: NotebookCellType, source: &str) -> Value {
    let mut cell = json!({
        "cell_type": match cell_type {
            NotebookCellType::Code => "code",
            NotebookCellType::Markdown => "markdown",
        },
        "id": cell_id,
        "metadata": {},
        "source": source_lines(source),
    });
    if let Some(object) = cell.as_object_mut() {
        match cell_type {
            NotebookCellType::Code => {
                object.insert(String::from("outputs"), json!([]));
                object.insert(String::from("execution_count"), Value::Null);
            }
            NotebookCellType::Markdown => {}
        }
    }
    cell
}

fn cell_kind(cell: &serde_json::Value) -> Option<NotebookCellType> {
    cell.get("cell_type")
        .and_then(serde_json::Value::as_str)
        .map(|kind| {
            if kind == "markdown" {
                NotebookCellType::Markdown
            } else {
                NotebookCellType::Code
            }
        })
}

const MAX_SLEEP_DURATION_MS: u64 = 300_000;

#[allow(clippy::needless_pass_by_value)]
fn execute_sleep(input: SleepInput) -> Result<SleepOutput, String> {
    if input.duration_ms > MAX_SLEEP_DURATION_MS {
        return Err(format!(
            "duration_ms {} exceeds maximum allowed sleep of {MAX_SLEEP_DURATION_MS}ms",
            input.duration_ms,
        ));
    }
    std::thread::sleep(Duration::from_millis(input.duration_ms));
    Ok(SleepOutput {
        duration_ms: input.duration_ms,
        message: format!("Slept for {}ms", input.duration_ms),
    })
}

fn execute_brief(input: BriefInput) -> Result<BriefOutput, String> {
    if input.message.trim().is_empty() {
        return Err(String::from("message must not be empty"));
    }

    let attachments = input
        .attachments
        .as_ref()
        .map(|paths| {
            paths
                .iter()
                .map(|path| resolve_attachment(path))
                .collect::<Result<Vec<_>, String>>()
        })
        .transpose()?;

    let message = match input.status {
        BriefStatus::Normal | BriefStatus::Proactive => input.message,
    };

    Ok(BriefOutput {
        message,
        attachments,
        sent_at: iso8601_timestamp(),
    })
}

fn resolve_attachment(path: &str) -> Result<ResolvedAttachment, String> {
    let resolved = std::fs::canonicalize(path).map_err(|error| error.to_string())?;
    let metadata = std::fs::metadata(&resolved).map_err(|error| error.to_string())?;
    Ok(ResolvedAttachment {
        path: resolved.display().to_string(),
        size: metadata.len(),
        is_image: is_image_path(&resolved),
    })
}

fn is_image_path(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|ext| ext.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "svg")
    )
}

fn execute_config(input: ConfigInput) -> Result<ConfigOutput, String> {
    let setting = input.setting.trim();
    if setting.is_empty() {
        return Err(String::from("setting must not be empty"));
    }
    let Some(spec) = supported_config_setting(setting) else {
        return Ok(ConfigOutput {
            success: false,
            operation: None,
            setting: None,
            value: None,
            previous_value: None,
            new_value: None,
            error: Some(format!("Unknown setting: \"{setting}\"")),
        });
    };

    let path = config_file_for_scope(spec.scope)?;
    let mut document = read_json_object(&path)?;

    if let Some(value) = input.value {
        let normalized = normalize_config_value(spec, value)?;
        let previous_value = get_nested_value(&document, spec.path).cloned();
        set_nested_value(&mut document, spec.path, normalized.clone());
        write_json_object(&path, &document)?;
        Ok(ConfigOutput {
            success: true,
            operation: Some(String::from("set")),
            setting: Some(setting.to_string()),
            value: Some(normalized.clone()),
            previous_value,
            new_value: Some(normalized),
            error: None,
        })
    } else {
        Ok(ConfigOutput {
            success: true,
            operation: Some(String::from("get")),
            setting: Some(setting.to_string()),
            value: get_nested_value(&document, spec.path).cloned(),
            previous_value: None,
            new_value: None,
            error: None,
        })
    }
}

const PERMISSION_DEFAULT_MODE_PATH: &[&str] = &["permissions", "defaultMode"];

fn execute_enter_plan_mode(_input: EnterPlanModeInput) -> Result<PlanModeOutput, String> {
    let settings_path = config_file_for_scope(ConfigScope::Settings)?;
    let state_path = plan_mode_state_file()?;
    let mut document = read_json_object(&settings_path)?;
    let current_local_mode = get_nested_value(&document, PERMISSION_DEFAULT_MODE_PATH).cloned();
    let current_is_plan =
        matches!(current_local_mode.as_ref(), Some(Value::String(value)) if value == "plan");

    if let Some(state) = read_plan_mode_state(&state_path)? {
        if current_is_plan {
            return Ok(PlanModeOutput {
                success: true,
                operation: String::from("enter"),
                changed: false,
                active: true,
                managed: true,
                message: String::from("Plan mode override is already active for this worktree."),
                settings_path: settings_path.display().to_string(),
                state_path: state_path.display().to_string(),
                previous_local_mode: state.previous_local_mode,
                current_local_mode,
            });
        }
        clear_plan_mode_state(&state_path)?;
    }

    if current_is_plan {
        return Ok(PlanModeOutput {
            success: true,
            operation: String::from("enter"),
            changed: false,
            active: true,
            managed: false,
            message: String::from(
                "Worktree-local plan mode is already enabled outside EnterPlanMode; leaving it unchanged.",
            ),
            settings_path: settings_path.display().to_string(),
            state_path: state_path.display().to_string(),
            previous_local_mode: None,
            current_local_mode,
        });
    }

    let state = PlanModeState {
        had_local_override: current_local_mode.is_some(),
        previous_local_mode: current_local_mode.clone(),
    };
    write_plan_mode_state(&state_path, &state)?;
    set_nested_value(
        &mut document,
        PERMISSION_DEFAULT_MODE_PATH,
        Value::String(String::from("plan")),
    );
    write_json_object(&settings_path, &document)?;

    Ok(PlanModeOutput {
        success: true,
        operation: String::from("enter"),
        changed: true,
        active: true,
        managed: true,
        message: String::from("Enabled worktree-local plan mode override."),
        settings_path: settings_path.display().to_string(),
        state_path: state_path.display().to_string(),
        previous_local_mode: state.previous_local_mode,
        current_local_mode: get_nested_value(&document, PERMISSION_DEFAULT_MODE_PATH).cloned(),
    })
}

fn execute_exit_plan_mode(_input: ExitPlanModeInput) -> Result<PlanModeOutput, String> {
    let settings_path = config_file_for_scope(ConfigScope::Settings)?;
    let state_path = plan_mode_state_file()?;
    let mut document = read_json_object(&settings_path)?;
    let current_local_mode = get_nested_value(&document, PERMISSION_DEFAULT_MODE_PATH).cloned();
    let current_is_plan =
        matches!(current_local_mode.as_ref(), Some(Value::String(value)) if value == "plan");

    let Some(state) = read_plan_mode_state(&state_path)? else {
        return Ok(PlanModeOutput {
            success: true,
            operation: String::from("exit"),
            changed: false,
            active: current_is_plan,
            managed: false,
            message: String::from("No EnterPlanMode override is active for this worktree."),
            settings_path: settings_path.display().to_string(),
            state_path: state_path.display().to_string(),
            previous_local_mode: None,
            current_local_mode,
        });
    };

    if !current_is_plan {
        clear_plan_mode_state(&state_path)?;
        return Ok(PlanModeOutput {
            success: true,
            operation: String::from("exit"),
            changed: false,
            active: false,
            managed: false,
            message: String::from(
                "Cleared stale EnterPlanMode state because plan mode was already changed outside the tool.",
            ),
            settings_path: settings_path.display().to_string(),
            state_path: state_path.display().to_string(),
            previous_local_mode: state.previous_local_mode,
            current_local_mode,
        });
    }

    if state.had_local_override {
        if let Some(previous_local_mode) = state.previous_local_mode.clone() {
            set_nested_value(
                &mut document,
                PERMISSION_DEFAULT_MODE_PATH,
                previous_local_mode,
            );
        } else {
            remove_nested_value(&mut document, PERMISSION_DEFAULT_MODE_PATH);
        }
    } else {
        remove_nested_value(&mut document, PERMISSION_DEFAULT_MODE_PATH);
    }
    write_json_object(&settings_path, &document)?;
    clear_plan_mode_state(&state_path)?;

    Ok(PlanModeOutput {
        success: true,
        operation: String::from("exit"),
        changed: true,
        active: false,
        managed: false,
        message: String::from("Restored the prior worktree-local plan mode setting."),
        settings_path: settings_path.display().to_string(),
        state_path: state_path.display().to_string(),
        previous_local_mode: state.previous_local_mode,
        current_local_mode: get_nested_value(&document, PERMISSION_DEFAULT_MODE_PATH).cloned(),
    })
}

fn execute_structured_output(
    input: StructuredOutputInput,
) -> Result<StructuredOutputResult, String> {
    if input.0.is_empty() {
        return Err(String::from("structured output payload must not be empty"));
    }
    Ok(StructuredOutputResult {
        data: String::from("Structured output provided successfully"),
        structured_output: input.0,
    })
}

fn execute_repl(input: ReplInput) -> Result<ReplOutput, String> {
    if input.code.trim().is_empty() {
        return Err(String::from("code must not be empty"));
    }
    let runtime = resolve_repl_runtime(&input.language)?;
    let started = Instant::now();
    let mut process = Command::new(runtime.program);
    process
        .args(runtime.args)
        .arg(&input.code)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let output = if let Some(timeout_ms) = input.timeout_ms {
        let mut child = process.spawn().map_err(|error| error.to_string())?;
        loop {
            if child
                .try_wait()
                .map_err(|error| error.to_string())?
                .is_some()
            {
                break child
                    .wait_with_output()
                    .map_err(|error| error.to_string())?;
            }
            if started.elapsed() >= Duration::from_millis(timeout_ms) {
                child.kill().map_err(|error| error.to_string())?;
                child
                    .wait_with_output()
                    .map_err(|error| error.to_string())?;
                return Err(format!(
                    "REPL execution exceeded timeout of {timeout_ms} ms"
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    } else {
        process
            .spawn()
            .map_err(|error| error.to_string())?
            .wait_with_output()
            .map_err(|error| error.to_string())?
    };

    Ok(ReplOutput {
        language: input.language,
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        exit_code: output.status.code().unwrap_or(1),
        duration_ms: started.elapsed().as_millis(),
    })
}

struct ReplRuntime {
    program: &'static str,
    args: &'static [&'static str],
}

fn resolve_repl_runtime(language: &str) -> Result<ReplRuntime, String> {
    match language.trim().to_ascii_lowercase().as_str() {
        "python" | "py" => Ok(ReplRuntime {
            program: detect_first_command(&["python3", "python"])
                .ok_or_else(|| String::from("python runtime not found"))?,
            args: &["-c"],
        }),
        "javascript" | "js" | "node" => Ok(ReplRuntime {
            program: detect_first_command(&["node"])
                .ok_or_else(|| String::from("node runtime not found"))?,
            args: &["-e"],
        }),
        "sh" | "shell" | "bash" => Ok(ReplRuntime {
            program: detect_first_command(&["bash", "sh"])
                .ok_or_else(|| String::from("shell runtime not found"))?,
            args: &["-lc"],
        }),
        other => Err(format!("unsupported REPL language: {other}")),
    }
}

fn detect_first_command(commands: &[&'static str]) -> Option<&'static str> {
    commands
        .iter()
        .copied()
        .find(|command| command_exists(command))
}

#[derive(Clone, Copy)]
enum ConfigScope {
    Global,
    Settings,
}

#[derive(Clone, Copy)]
struct ConfigSettingSpec {
    scope: ConfigScope,
    kind: ConfigKind,
    path: &'static [&'static str],
    options: Option<&'static [&'static str]>,
}

#[derive(Clone, Copy)]
enum ConfigKind {
    Boolean,
    String,
}

fn supported_config_setting(setting: &str) -> Option<ConfigSettingSpec> {
    Some(match setting {
        "theme" => ConfigSettingSpec {
            scope: ConfigScope::Global,
            kind: ConfigKind::String,
            path: &["theme"],
            options: None,
        },
        "editorMode" => ConfigSettingSpec {
            scope: ConfigScope::Global,
            kind: ConfigKind::String,
            path: &["editorMode"],
            options: Some(&["default", "vim", "emacs"]),
        },
        "verbose" => ConfigSettingSpec {
            scope: ConfigScope::Global,
            kind: ConfigKind::Boolean,
            path: &["verbose"],
            options: None,
        },
        "preferredNotifChannel" => ConfigSettingSpec {
            scope: ConfigScope::Global,
            kind: ConfigKind::String,
            path: &["preferredNotifChannel"],
            options: None,
        },
        "autoCompactEnabled" => ConfigSettingSpec {
            scope: ConfigScope::Global,
            kind: ConfigKind::Boolean,
            path: &["autoCompactEnabled"],
            options: None,
        },
        "autoMemoryEnabled" => ConfigSettingSpec {
            scope: ConfigScope::Settings,
            kind: ConfigKind::Boolean,
            path: &["autoMemoryEnabled"],
            options: None,
        },
        "autoDreamEnabled" => ConfigSettingSpec {
            scope: ConfigScope::Settings,
            kind: ConfigKind::Boolean,
            path: &["autoDreamEnabled"],
            options: None,
        },
        "fileCheckpointingEnabled" => ConfigSettingSpec {
            scope: ConfigScope::Global,
            kind: ConfigKind::Boolean,
            path: &["fileCheckpointingEnabled"],
            options: None,
        },
        "showTurnDuration" => ConfigSettingSpec {
            scope: ConfigScope::Global,
            kind: ConfigKind::Boolean,
            path: &["showTurnDuration"],
            options: None,
        },
        "terminalProgressBarEnabled" => ConfigSettingSpec {
            scope: ConfigScope::Global,
            kind: ConfigKind::Boolean,
            path: &["terminalProgressBarEnabled"],
            options: None,
        },
        "todoFeatureEnabled" => ConfigSettingSpec {
            scope: ConfigScope::Global,
            kind: ConfigKind::Boolean,
            path: &["todoFeatureEnabled"],
            options: None,
        },
        "model" => ConfigSettingSpec {
            scope: ConfigScope::Settings,
            kind: ConfigKind::String,
            path: &["model"],
            options: None,
        },
        "alwaysThinkingEnabled" => ConfigSettingSpec {
            scope: ConfigScope::Settings,
            kind: ConfigKind::Boolean,
            path: &["alwaysThinkingEnabled"],
            options: None,
        },
        "permissions.defaultMode" => ConfigSettingSpec {
            scope: ConfigScope::Settings,
            kind: ConfigKind::String,
            path: &["permissions", "defaultMode"],
            options: Some(&["default", "plan", "acceptEdits", "dontAsk", "auto"]),
        },
        "language" => ConfigSettingSpec {
            scope: ConfigScope::Settings,
            kind: ConfigKind::String,
            path: &["language"],
            options: None,
        },
        "teammateMode" => ConfigSettingSpec {
            scope: ConfigScope::Global,
            kind: ConfigKind::String,
            path: &["teammateMode"],
            options: Some(&["tmux", "in-process", "auto"]),
        },
        _ => return None,
    })
}

fn normalize_config_value(spec: ConfigSettingSpec, value: ConfigValue) -> Result<Value, String> {
    let normalized = match (spec.kind, value) {
        (ConfigKind::Boolean, ConfigValue::Bool(value)) => Value::Bool(value),
        (ConfigKind::Boolean, ConfigValue::String(value)) => {
            match value.trim().to_ascii_lowercase().as_str() {
                "true" => Value::Bool(true),
                "false" => Value::Bool(false),
                _ => return Err(String::from("setting requires true or false")),
            }
        }
        (ConfigKind::Boolean, ConfigValue::Number(_)) => {
            return Err(String::from("setting requires true or false"))
        }
        (ConfigKind::String, ConfigValue::String(value)) => Value::String(value),
        (ConfigKind::String, ConfigValue::Bool(value)) => Value::String(value.to_string()),
        (ConfigKind::String, ConfigValue::Number(value)) => json!(value),
    };

    if let Some(options) = spec.options {
        let Some(as_str) = normalized.as_str() else {
            return Err(String::from("setting requires a string value"));
        };
        if !options.iter().any(|option| option == &as_str) {
            return Err(format!(
                "Invalid value \"{as_str}\". Options: {}",
                options.join(", ")
            ));
        }
    }

    Ok(normalized)
}

fn config_file_for_scope(scope: ConfigScope) -> Result<PathBuf, String> {
    let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
    Ok(match scope {
        ConfigScope::Global => config_home_dir()?.join("settings.json"),
        ConfigScope::Settings => cwd.join(".aos").join("settings.local.json"),
    })
}

fn config_home_dir() -> Result<PathBuf, String> {
    if let Ok(path) = std::env::var("AOS_CONFIG_HOME") {
        return Ok(PathBuf::from(path));
    }
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map_err(|_| {
            String::from(
                "HOME is not set (on Windows, set USERPROFILE or HOME, \
                 or use AOS_CONFIG_HOME to point directly at the config directory)",
            )
        })?;
    Ok(PathBuf::from(home).join(".aos"))
}

fn read_json_object(path: &Path) -> Result<serde_json::Map<String, Value>, String> {
    match std::fs::read_to_string(path) {
        Ok(contents) => {
            if contents.trim().is_empty() {
                return Ok(serde_json::Map::new());
            }
            serde_json::from_str::<Value>(&contents)
                .map_err(|error| error.to_string())?
                .as_object()
                .cloned()
                .ok_or_else(|| String::from("config file must contain a JSON object"))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(serde_json::Map::new()),
        Err(error) => Err(error.to_string()),
    }
}

fn write_json_object(path: &Path, value: &serde_json::Map<String, Value>) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    std::fs::write(
        path,
        serde_json::to_string_pretty(value).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())
}

fn get_nested_value<'a>(
    value: &'a serde_json::Map<String, Value>,
    path: &[&str],
) -> Option<&'a Value> {
    let (first, rest) = path.split_first()?;
    let mut current = value.get(*first)?;
    for key in rest {
        current = current.as_object()?.get(*key)?;
    }
    Some(current)
}

fn set_nested_value(root: &mut serde_json::Map<String, Value>, path: &[&str], new_value: Value) {
    let (first, rest) = path.split_first().expect("config path must not be empty");
    if rest.is_empty() {
        root.insert((*first).to_string(), new_value);
        return;
    }

    let entry = root
        .entry((*first).to_string())
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if !entry.is_object() {
        *entry = Value::Object(serde_json::Map::new());
    }
    let map = entry.as_object_mut().expect("object inserted");
    set_nested_value(map, rest, new_value);
}

fn remove_nested_value(root: &mut serde_json::Map<String, Value>, path: &[&str]) -> bool {
    let Some((first, rest)) = path.split_first() else {
        return false;
    };
    if rest.is_empty() {
        return root.remove(*first).is_some();
    }

    let mut should_remove_parent = false;
    let removed = root.get_mut(*first).is_some_and(|entry| {
        entry.as_object_mut().is_some_and(|map| {
            let removed = remove_nested_value(map, rest);
            should_remove_parent = removed && map.is_empty();
            removed
        })
    });

    if should_remove_parent {
        root.remove(*first);
    }

    removed
}

fn plan_mode_state_file() -> Result<PathBuf, String> {
    Ok(config_file_for_scope(ConfigScope::Settings)?
        .parent()
        .ok_or_else(|| String::from("settings.local.json has no parent directory"))?
        .join("tool-state")
        .join("plan-mode.json"))
}

fn read_plan_mode_state(path: &Path) -> Result<Option<PlanModeState>, String> {
    match std::fs::read_to_string(path) {
        Ok(contents) => {
            if contents.trim().is_empty() {
                return Ok(None);
            }
            serde_json::from_str(&contents)
                .map(Some)
                .map_err(|error| error.to_string())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

fn write_plan_mode_state(path: &Path, state: &PlanModeState) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    std::fs::write(
        path,
        serde_json::to_string_pretty(state).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())
}

fn clear_plan_mode_state(path: &Path) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

fn iso8601_timestamp() -> String {
    if let Ok(output) = Command::new("date")
        .args(["-u", "+%Y-%m-%dT%H:%M:%SZ"])
        .output()
    {
        if output.status.success() {
            return String::from_utf8_lossy(&output.stdout).trim().to_string();
        }
    }
    iso8601_now()
}

#[allow(clippy::needless_pass_by_value)]
fn execute_powershell(input: PowerShellInput) -> std::io::Result<runtime::BashCommandOutput> {
    let _ = &input.description;
    if let Some(output) = workspace_test_branch_preflight(&input.command) {
        return Ok(output);
    }
    let shell = detect_powershell_shell()?;
    execute_shell_command(
        shell,
        &input.command,
        input.timeout,
        input.run_in_background,
    )
}

fn detect_powershell_shell() -> std::io::Result<&'static str> {
    if command_exists("pwsh") {
        Ok("pwsh")
    } else if command_exists("powershell") {
        Ok("powershell")
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "PowerShell executable not found (expected `pwsh` or `powershell` in PATH)",
        ))
    }
}

fn command_exists(command: &str) -> bool {
    std::process::Command::new("sh")
        .current_dir(command_working_directory())
        .arg("-lc")
        .arg(format!("command -v {command} >/dev/null 2>&1"))
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn command_working_directory() -> PathBuf {
    std::env::current_dir()
        .ok()
        .filter(|path| path.is_dir())
        .unwrap_or_else(std::env::temp_dir)
}

#[allow(clippy::too_many_lines)]
fn execute_shell_command(
    shell: &str,
    command: &str,
    timeout: Option<u64>,
    run_in_background: Option<bool>,
) -> std::io::Result<runtime::BashCommandOutput> {
    let working_directory = command_working_directory();
    if run_in_background.unwrap_or(false) {
        let child = std::process::Command::new(shell)
            .current_dir(&working_directory)
            .arg("-NoProfile")
            .arg("-NonInteractive")
            .arg("-Command")
            .arg(command)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?;
        return Ok(runtime::BashCommandOutput {
            stdout: String::new(),
            stderr: String::new(),
            raw_output_path: None,
            interrupted: false,
            is_image: None,
            background_task_id: Some(child.id().to_string()),
            backgrounded_by_user: Some(true),
            assistant_auto_backgrounded: Some(false),
            dangerously_disable_sandbox: None,
            return_code_interpretation: None,
            no_output_expected: Some(true),
            structured_content: None,
            persisted_output_path: None,
            persisted_output_size: None,
            sandbox_status: None,
        });
    }

    let mut process = std::process::Command::new(shell);
    process
        .current_dir(&working_directory)
        .arg("-NoProfile")
        .arg("-NonInteractive")
        .arg("-Command")
        .arg(command);
    process
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    if let Some(timeout_ms) = timeout {
        let mut child = process.spawn()?;
        let started = Instant::now();
        loop {
            if let Some(status) = child.try_wait()? {
                let output = child.wait_with_output()?;
                return Ok(runtime::BashCommandOutput {
                    stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                    stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                    raw_output_path: None,
                    interrupted: false,
                    is_image: None,
                    background_task_id: None,
                    backgrounded_by_user: None,
                    assistant_auto_backgrounded: None,
                    dangerously_disable_sandbox: None,
                    return_code_interpretation: status
                        .code()
                        .filter(|code| *code != 0)
                        .map(|code| format!("exit_code:{code}")),
                    no_output_expected: Some(output.stdout.is_empty() && output.stderr.is_empty()),
                    structured_content: None,
                    persisted_output_path: None,
                    persisted_output_size: None,
                    sandbox_status: None,
                });
            }
            if started.elapsed() >= Duration::from_millis(timeout_ms) {
                let _ = child.kill();
                let output = child.wait_with_output()?;
                let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
                let stderr = if stderr.trim().is_empty() {
                    format!("Command exceeded timeout of {timeout_ms} ms")
                } else {
                    format!(
                        "{}
Command exceeded timeout of {timeout_ms} ms",
                        stderr.trim_end()
                    )
                };
                return Ok(runtime::BashCommandOutput {
                    stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                    stderr,
                    raw_output_path: None,
                    interrupted: true,
                    is_image: None,
                    background_task_id: None,
                    backgrounded_by_user: None,
                    assistant_auto_backgrounded: None,
                    dangerously_disable_sandbox: None,
                    return_code_interpretation: Some(String::from("timeout")),
                    no_output_expected: Some(false),
                    structured_content: None,
                    persisted_output_path: None,
                    persisted_output_size: None,
                    sandbox_status: None,
                });
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    let output = process.output()?;
    Ok(runtime::BashCommandOutput {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        raw_output_path: None,
        interrupted: false,
        is_image: None,
        background_task_id: None,
        backgrounded_by_user: None,
        assistant_auto_backgrounded: None,
        dangerously_disable_sandbox: None,
        return_code_interpretation: output
            .status
            .code()
            .filter(|code| *code != 0)
            .map(|code| format!("exit_code:{code}")),
        no_output_expected: Some(output.stdout.is_empty() && output.stderr.is_empty()),
        structured_content: None,
        persisted_output_path: None,
        persisted_output_size: None,
        sandbox_status: None,
    })
}

fn resolve_cell_index(
    cells: &[serde_json::Value],
    cell_id: Option<&str>,
    edit_mode: NotebookEditMode,
) -> Result<usize, String> {
    if cells.is_empty()
        && matches!(
            edit_mode,
            NotebookEditMode::Replace | NotebookEditMode::Delete
        )
    {
        return Err(String::from("Notebook has no cells to edit"));
    }
    if let Some(cell_id) = cell_id {
        cells
            .iter()
            .position(|cell| cell.get("id").and_then(serde_json::Value::as_str) == Some(cell_id))
            .ok_or_else(|| format!("Cell id not found: {cell_id}"))
    } else {
        Ok(cells.len().saturating_sub(1))
    }
}

fn source_lines(source: &str) -> Vec<serde_json::Value> {
    if source.is_empty() {
        return vec![serde_json::Value::String(String::new())];
    }
    source
        .split_inclusive('\n')
        .map(|line| serde_json::Value::String(line.to_string()))
        .collect()
}

fn format_notebook_edit_mode(mode: NotebookEditMode) -> String {
    match mode {
        NotebookEditMode::Replace => String::from("replace"),
        NotebookEditMode::Insert => String::from("insert"),
        NotebookEditMode::Delete => String::from("delete"),
    }
}

fn make_cell_id(index: usize) -> String {
    format!("cell-{}", index + 1)
}

fn parse_skill_description(contents: &str) -> Option<String> {
    for line in contents.lines() {
        if let Some(value) = line.strip_prefix("description:") {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

pub mod lane_completion;
pub mod pdf_extract;

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::collections::BTreeSet;
    use std::fs;
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpListener};
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex, OnceLock};
    use std::thread;
    use std::time::Duration;

    use super::{
        agent_permission_policy, allowed_tools_for_subagent, classify_lane_failure,
        derive_agent_state, execute_agent_with_spawn, execute_tool, extract_recovery_outcome,
        final_assistant_text, global_cron_registry, maybe_commit_provenance, mvp_tool_specs,
        normalize_duckduckgo_result_url, parse_bing_html_hits, parse_brave_html_hits,
        parse_duckduckgo_html_hits, parse_hacker_news_hits, parse_stack_exchange_hits,
        permission_mode_from_plugin, persist_agent_terminal_state, public_api_search_query,
        push_output_block, run_task_packet, with_strict_web_search_provider_override,
        with_web_search_provider_override, AgentInput, AgentJob, GlobalToolRegistry, LaneEventName,
        LaneFailureClass, ProviderRuntimeClient, SubagentToolExecutor, WebSearchProviderConfig,
        WebSearchProviderType,
    };
    use api::OutputContentBlock;
    use runtime::ProviderFallbackConfig;
    use runtime::{
        permission_enforcer::PermissionEnforcer, ApiRequest, AssistantEvent, ConversationRuntime,
        PermissionMode, PermissionPolicy, RuntimeError, Session, TaskPacket, ToolExecutor,
    };
    use serde_json::json;

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn env_guard() -> std::sync::MutexGuard<'static, ()> {
        env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    struct TestEnvVar {
        key: &'static str,
        previous: Option<std::ffi::OsString>,
    }

    impl TestEnvVar {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = std::env::var_os(key);
            std::env::set_var(key, value);
            Self { key, previous }
        }

        fn remove(key: &'static str) -> Self {
            let previous = std::env::var_os(key);
            std::env::remove_var(key);
            Self { key, previous }
        }
    }

    impl Drop for TestEnvVar {
        fn drop(&mut self) {
            if let Some(previous) = self.previous.take() {
                std::env::set_var(self.key, previous);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

    #[test]
    fn env_guard_recovers_after_poisoning() {
        let poisoned = std::thread::spawn(|| {
            let _guard = env_guard();
            panic!("poison env lock");
        })
        .join();
        assert!(poisoned.is_err(), "poisoning thread should panic");

        let _guard = env_guard();
    }

    fn temp_path(name: &str) -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        std::env::temp_dir().join(format!("aosd-tools-{unique}-{name}"))
    }

    fn run_git(cwd: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .status()
            .unwrap_or_else(|error| panic!("git {} failed: {error}", args.join(" ")));
        assert!(
            status.success(),
            "git {} exited with {status}",
            args.join(" ")
        );
    }

    fn init_git_repo(path: &Path) {
        std::fs::create_dir_all(path).expect("create repo");
        run_git(path, &["init", "--quiet"]);
        run_git(path, &["checkout", "-B", "main", "--quiet"]);
        run_git(path, &["config", "user.email", "tests@example.com"]);
        run_git(path, &["config", "user.name", "Tools Tests"]);
        std::fs::write(path.join("README.md"), "initial\n").expect("write readme");
        run_git(path, &["add", "README.md"]);
        run_git(path, &["commit", "-m", "initial commit", "--quiet"]);
    }

    fn commit_file(path: &Path, file: &str, contents: &str, message: &str) {
        std::fs::write(path.join(file), contents).expect("write file");
        run_git(path, &["add", file]);
        run_git(path, &["commit", "-m", message, "--quiet"]);
    }

    fn permission_policy_for_mode(mode: PermissionMode) -> PermissionPolicy {
        mvp_tool_specs()
            .into_iter()
            .fold(PermissionPolicy::new(mode), |policy, spec| {
                policy.with_tool_requirement(spec.name, spec.required_permission)
            })
    }

    #[test]
    fn exposes_mvp_tools() {
        let names = mvp_tool_specs()
            .into_iter()
            .map(|spec| spec.name)
            .collect::<Vec<_>>();
        assert!(names.contains(&"bash"));
        assert!(names.contains(&"read_file"));
        assert!(names.contains(&"WebFetch"));
        assert!(names.contains(&"WebSearch"));
        assert!(names.contains(&"TodoWrite"));
        assert!(names.contains(&"Skill"));
        assert!(names.contains(&"Agent"));
        assert!(names.contains(&"ToolSearch"));
        assert!(names.contains(&"NotebookEdit"));
        assert!(names.contains(&"Sleep"));
        assert!(names.contains(&"SendUserMessage"));
        assert!(names.contains(&"Config"));
        assert!(names.contains(&"EnterPlanMode"));
        assert!(names.contains(&"ExitPlanMode"));
        assert!(names.contains(&"StructuredOutput"));
        assert!(names.contains(&"REPL"));
        assert!(names.contains(&"PowerShell"));
        assert!(names.contains(&"WorkerCreate"));
        assert!(names.contains(&"WorkerObserve"));
        assert!(names.contains(&"WorkerAwaitReady"));
        assert!(names.contains(&"WorkerSendPrompt"));
    }

    #[test]
    fn rejects_unknown_tool_names() {
        let error = execute_tool("nope", &json!({})).expect_err("tool should be rejected");
        assert!(error.contains("unsupported tool"));
    }

    #[test]
    fn worker_tools_gate_prompt_delivery_until_ready_and_support_auto_trust() {
        let created = execute_tool(
            "WorkerCreate",
            &json!({
                "cwd": "/tmp/worktree/repo",
                "trusted_roots": ["/tmp/worktree"]
            }),
        )
        .expect("WorkerCreate should succeed");
        let created_output: serde_json::Value = serde_json::from_str(&created).expect("json");
        let worker_id = created_output["worker_id"]
            .as_str()
            .expect("worker id")
            .to_string();
        assert_eq!(created_output["status"], "spawning");
        assert_eq!(created_output["trust_auto_resolve"], true);

        let gated = execute_tool(
            "WorkerSendPrompt",
            &json!({
                "worker_id": worker_id,
                "prompt": "ship the change"
            }),
        )
        .expect_err("prompt delivery before ready should fail");
        assert!(gated.contains("not ready for prompt delivery"));

        let observed = execute_tool(
            "WorkerObserve",
            &json!({
                "worker_id": created_output["worker_id"],
                "screen_text": "Do you trust the files in this folder?\n1. Yes, proceed\n2. No"
            }),
        )
        .expect("WorkerObserve should auto-resolve trust");
        let observed_output: serde_json::Value = serde_json::from_str(&observed).expect("json");
        assert_eq!(observed_output["status"], "spawning");
        assert_eq!(observed_output["trust_gate_cleared"], true);
        assert_eq!(
            observed_output["events"][1]["payload"]["type"],
            "trust_prompt"
        );
        assert_eq!(
            observed_output["events"][2]["payload"]["resolution"],
            "auto_allowlisted"
        );

        let ready = execute_tool(
            "WorkerObserve",
            &json!({
                "worker_id": created_output["worker_id"],
                "screen_text": "Ready for your input\n>"
            }),
        )
        .expect("WorkerObserve should mark worker ready");
        let ready_output: serde_json::Value = serde_json::from_str(&ready).expect("json");
        assert_eq!(ready_output["status"], "ready_for_prompt");

        let await_ready = execute_tool(
            "WorkerAwaitReady",
            &json!({
                "worker_id": created_output["worker_id"]
            }),
        )
        .expect("WorkerAwaitReady should succeed");
        let await_ready_output: serde_json::Value =
            serde_json::from_str(&await_ready).expect("json");
        assert_eq!(await_ready_output["ready"], true);

        let accepted = execute_tool(
            "WorkerSendPrompt",
            &json!({
                "worker_id": created_output["worker_id"],
                "prompt": "ship the change"
            }),
        )
        .expect("WorkerSendPrompt should succeed after ready");
        let accepted_output: serde_json::Value = serde_json::from_str(&accepted).expect("json");
        assert_eq!(accepted_output["status"], "running");
        assert_eq!(accepted_output["prompt_delivery_attempts"], 1);
        assert_eq!(accepted_output["prompt_in_flight"], true);
    }

    #[test]
    fn worker_create_merges_config_trusted_roots_without_per_call_override() {
        use std::fs;
        // Write a .aos/settings.json in a temp dir with trustedRoots
        let worktree = temp_path("config-trust-worktree");
        let aos_dir = worktree.join(".aos");
        fs::create_dir_all(&aos_dir).expect("create .aos dir");
        // Use the actual OS temp dir so the worktree path matches the allowlist
        let tmp_root = std::env::temp_dir().to_str().expect("utf-8").to_string();
        let settings = format!("{{\"trustedRoots\": [\"{tmp_root}\"]}}");
        fs::write(aos_dir.join("settings.json"), settings).expect("write settings");

        // WorkerCreate with no per-call trusted_roots — config should supply them
        let cwd = worktree.to_str().expect("valid utf-8").to_string();
        let created = execute_tool(
            "WorkerCreate",
            &json!({
                "cwd": cwd
                // trusted_roots intentionally omitted
            }),
        )
        .expect("WorkerCreate should succeed");
        let output: serde_json::Value = serde_json::from_str(&created).expect("json");

        // worktree is under /tmp, so config roots auto-resolve trust
        assert_eq!(
            output["trust_auto_resolve"], true,
            "config-level trustedRoots should auto-resolve trust without per-call override"
        );

        fs::remove_dir_all(&worktree).ok();
    }

    #[test]
    fn worker_terminate_sets_finished_status() {
        // Create a worker in running state
        let created = execute_tool(
            "WorkerCreate",
            &json!({"cwd": "/tmp/terminate-test", "trusted_roots": ["/tmp"]}),
        )
        .expect("WorkerCreate should succeed");
        let output: serde_json::Value = serde_json::from_str(&created).expect("json");
        let worker_id = output["worker_id"].as_str().expect("worker_id").to_string();

        // Terminate
        let terminated = execute_tool("WorkerTerminate", &json!({"worker_id": worker_id}))
            .expect("WorkerTerminate should succeed");
        let term_output: serde_json::Value = serde_json::from_str(&terminated).expect("json");
        assert_eq!(
            term_output["status"], "finished",
            "terminated worker should be finished"
        );
        assert_eq!(
            term_output["prompt_in_flight"], false,
            "prompt_in_flight should be cleared on termination"
        );
    }

    #[test]
    fn worker_restart_resets_to_spawning() {
        // Create and advance worker to ready_for_prompt
        let created = execute_tool(
            "WorkerCreate",
            &json!({"cwd": "/tmp/restart-test", "trusted_roots": ["/tmp"]}),
        )
        .expect("WorkerCreate should succeed");
        let output: serde_json::Value = serde_json::from_str(&created).expect("json");
        let worker_id = output["worker_id"].as_str().expect("worker_id").to_string();

        // Advance to ready_for_prompt via observe
        execute_tool(
            "WorkerObserve",
            &json!({"worker_id": worker_id, "screen_text": "Ready for input\n>"}),
        )
        .expect("WorkerObserve should succeed");

        // Restart
        let restarted = execute_tool("WorkerRestart", &json!({"worker_id": worker_id}))
            .expect("WorkerRestart should succeed");
        let restart_output: serde_json::Value = serde_json::from_str(&restarted).expect("json");
        assert_eq!(
            restart_output["status"], "spawning",
            "restarted worker should return to spawning"
        );
        assert_eq!(
            restart_output["prompt_in_flight"], false,
            "prompt_in_flight should be cleared on restart"
        );
        assert_eq!(
            restart_output["trust_gate_cleared"], false,
            "trust_gate_cleared should be reset on restart (re-trust required)"
        );
    }

    #[test]
    fn worker_get_returns_worker_state() {
        let created = execute_tool(
            "WorkerCreate",
            &json!({"cwd": "/tmp/worker-get-test", "trusted_roots": ["/tmp"]}),
        )
        .expect("WorkerCreate should succeed");
        let created_output: serde_json::Value = serde_json::from_str(&created).expect("json");
        let worker_id = created_output["worker_id"].as_str().expect("worker_id");

        let fetched = execute_tool("WorkerGet", &json!({"worker_id": worker_id}))
            .expect("WorkerGet should succeed");
        let fetched_output: serde_json::Value = serde_json::from_str(&fetched).expect("json");
        assert_eq!(fetched_output["worker_id"], worker_id);
        assert_eq!(fetched_output["status"], "spawning");
        assert_eq!(fetched_output["cwd"], "/tmp/worker-get-test");
    }

    #[test]
    fn worker_get_on_unknown_id_returns_error() {
        let result = execute_tool(
            "WorkerGet",
            &json!({"worker_id": "worker_nonexistent_get_00000000"}),
        );
        assert!(
            result.is_err(),
            "WorkerGet on unknown id should return error"
        );
        assert!(
            result.unwrap_err().contains("worker not found"),
            "error should mention worker not found"
        );
    }

    #[test]
    fn worker_await_ready_on_spawning_worker_returns_not_ready() {
        let created = execute_tool(
            "WorkerCreate",
            &json!({"cwd": "/tmp/worker-await-not-ready"}),
        )
        .expect("WorkerCreate should succeed");
        let created_output: serde_json::Value = serde_json::from_str(&created).expect("json");
        let worker_id = created_output["worker_id"].as_str().expect("worker_id");

        // Worker is still in spawning — await_ready should return not-ready snapshot
        let snapshot = execute_tool("WorkerAwaitReady", &json!({"worker_id": worker_id}))
            .expect("WorkerAwaitReady should succeed even when not ready");
        let snap_output: serde_json::Value = serde_json::from_str(&snapshot).expect("json");
        assert_eq!(
            snap_output["ready"], false,
            "WorkerAwaitReady on a spawning worker must return ready=false"
        );
        assert_eq!(snap_output["worker_id"], worker_id);
    }

    #[test]
    fn worker_send_prompt_on_non_ready_worker_returns_error() {
        let created = execute_tool(
            "WorkerCreate",
            &json!({"cwd": "/tmp/worker-send-not-ready"}),
        )
        .expect("WorkerCreate should succeed");
        let created_output: serde_json::Value = serde_json::from_str(&created).expect("json");
        let worker_id = created_output["worker_id"].as_str().expect("worker_id");

        let result = execute_tool(
            "WorkerSendPrompt",
            &json!({"worker_id": worker_id, "prompt": "too early"}),
        );
        assert!(
            result.is_err(),
            "WorkerSendPrompt on a non-ready worker should fail"
        );
    }

    #[test]
    fn recovery_loop_state_file_reflects_transitions() {
        // End-to-end proof: .aos/worker-state.json reflects every transition
        // through the stall-detect -> resolve-trust -> ready loop.
        use std::fs;

        // Use a real temp CWD so state file can be written
        let worktree = temp_path("recovery-loop-state");
        fs::create_dir_all(&worktree).expect("create worktree");
        let cwd = worktree.to_str().expect("utf-8").to_string();
        let state_path = worktree.join(".aos").join("worker-state.json");

        // 1. Create worker WITHOUT trusted_roots
        let created = execute_tool("WorkerCreate", &json!({"cwd": cwd}))
            .expect("WorkerCreate should succeed");
        let created_output: serde_json::Value = serde_json::from_str(&created).expect("json");
        let worker_id = created_output["worker_id"]
            .as_str()
            .expect("worker_id")
            .to_string();
        // State file should exist after create
        assert!(
            state_path.exists(),
            "state file should be written after WorkerCreate"
        );
        let state: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&state_path).expect("read state"))
                .expect("parse state");
        assert_eq!(state["status"], "spawning");
        assert_eq!(state["is_ready"], false);
        assert!(
            state["seconds_since_update"].is_number(),
            "seconds_since_update must be present"
        );

        // 2. Force trust_required via observe
        execute_tool(
            "WorkerObserve",
            &json!({"worker_id": worker_id, "screen_text": "Do you trust the files in this folder?"}),
        )
        .expect("WorkerObserve should succeed");
        let state: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&state_path).expect("read state"))
                .expect("parse state");
        assert_eq!(
            state["status"], "trust_required",
            "state file must reflect trust_required stall"
        );
        assert_eq!(state["is_ready"], false);
        assert_eq!(state["trust_gate_cleared"], false);
        assert!(state["seconds_since_update"].is_number());

        // 3. WorkerResolveTrust -> state file reflects recovery
        execute_tool("WorkerResolveTrust", &json!({"worker_id": worker_id}))
            .expect("WorkerResolveTrust should succeed");
        let state: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&state_path).expect("read state"))
                .expect("parse state");
        assert_eq!(
            state["status"], "spawning",
            "state file must show spawning after trust resolved"
        );
        assert_eq!(state["trust_gate_cleared"], true);

        // 4. Observe ready screen -> state file shows ready_for_prompt
        execute_tool(
            "WorkerObserve",
            &json!({"worker_id": worker_id, "screen_text": "Ready for input\n>"}),
        )
        .expect("WorkerObserve ready should succeed");
        let state: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&state_path).expect("read state"))
                .expect("parse state");
        assert_eq!(
            state["status"], "ready_for_prompt",
            "state file must show ready_for_prompt after ready screen"
        );
        assert_eq!(
            state["is_ready"], true,
            "is_ready must be true in state file at ready_for_prompt"
        );

        fs::remove_dir_all(&worktree).ok();
    }

    #[test]
    fn stall_detect_and_resolve_trust_end_to_end() {
        // 1. Create worker WITHOUT trusted_roots so trust won't auto-resolve
        let created = execute_tool("WorkerCreate", &json!({"cwd": "/no/trusted/root/here"}))
            .expect("WorkerCreate should succeed");
        let created_output: serde_json::Value = serde_json::from_str(&created).expect("json");
        let worker_id = created_output["worker_id"]
            .as_str()
            .expect("worker_id")
            .to_string();
        assert_eq!(created_output["trust_auto_resolve"], false);

        // 2. Observe trust prompt screen text -> worker stalls at trust_required
        let stalled = execute_tool(
            "WorkerObserve",
            &json!({
                "worker_id": worker_id,
                "screen_text": "Do you trust the files in this folder?\n[Allow] [Deny]"
            }),
        )
        .expect("WorkerObserve should succeed");
        let stalled_output: serde_json::Value = serde_json::from_str(&stalled).expect("json");
        assert_eq!(
            stalled_output["status"], "trust_required",
            "worker should stall at trust_required when trust prompt seen without allowlist"
        );
        assert_eq!(stalled_output["trust_gate_cleared"], false);
        // 3. Clawhip calls WorkerResolveTrust to unblock
        let resolved = execute_tool("WorkerResolveTrust", &json!({"worker_id": worker_id}))
            .expect("WorkerResolveTrust should succeed");
        let resolved_output: serde_json::Value = serde_json::from_str(&resolved).expect("json");
        assert_eq!(
            resolved_output["status"], "spawning",
            "worker should return to spawning after trust resolved"
        );
        assert_eq!(resolved_output["trust_gate_cleared"], true);

        // 4. Ready screen text now advances worker normally
        let ready = execute_tool(
            "WorkerObserve",
            &json!({
                "worker_id": worker_id,
                "screen_text": "Ready for input\n>"
            }),
        )
        .expect("WorkerObserve should succeed after trust resolved");
        let ready_output: serde_json::Value = serde_json::from_str(&ready).expect("json");
        assert_eq!(
            ready_output["status"], "ready_for_prompt",
            "worker should reach ready_for_prompt after trust resolved and ready screen seen"
        );
    }

    #[test]
    fn stall_detect_and_restart_recovery_end_to_end() {
        // Worker stalls at trust_required, orchestrator restarts instead of resolving
        let created = execute_tool(
            "WorkerCreate",
            &json!({"cwd": "/no/trusted/root/restart-test"}),
        )
        .expect("WorkerCreate should succeed");
        let created_output: serde_json::Value = serde_json::from_str(&created).expect("json");
        let worker_id = created_output["worker_id"]
            .as_str()
            .expect("worker_id")
            .to_string();

        // Force trust_required
        let stalled = execute_tool(
            "WorkerObserve",
            &json!({
                "worker_id": worker_id,
                "screen_text": "trust this folder? [Yes] [No]"
            }),
        )
        .expect("WorkerObserve should succeed");
        let stalled_output: serde_json::Value = serde_json::from_str(&stalled).expect("json");
        assert_eq!(stalled_output["status"], "trust_required");

        // WorkerRestart resets the worker
        let restarted = execute_tool("WorkerRestart", &json!({"worker_id": worker_id}))
            .expect("WorkerRestart should succeed");
        let restarted_output: serde_json::Value = serde_json::from_str(&restarted).expect("json");
        assert_eq!(
            restarted_output["status"], "spawning",
            "restarted worker should be back at spawning"
        );
        assert_eq!(
            restarted_output["trust_gate_cleared"], false,
            "restart clears trust — next observe loop must re-acquire trust"
        );
    }

    #[test]
    fn worker_terminate_on_unknown_id_returns_error() {
        let result = execute_tool(
            "WorkerTerminate",
            &json!({"worker_id": "worker_nonexistent_00000000"}),
        );
        assert!(result.is_err(), "terminating unknown worker should fail");
        assert!(
            result.unwrap_err().contains("worker not found"),
            "error should mention worker not found"
        );
    }

    #[test]
    fn worker_restart_on_unknown_id_returns_error() {
        let result = execute_tool(
            "WorkerRestart",
            &json!({"worker_id": "worker_nonexistent_00000001"}),
        );
        assert!(result.is_err(), "restarting unknown worker should fail");
        assert!(
            result.unwrap_err().contains("worker not found"),
            "error should mention worker not found"
        );
    }

    #[test]
    fn worker_observe_completion_success_finish_sets_finished_status() {
        let created = execute_tool(
            "WorkerCreate",
            &json!({"cwd": "/tmp/observe-completion-test", "trusted_roots": ["/tmp"]}),
        )
        .expect("WorkerCreate should succeed");
        let output: serde_json::Value = serde_json::from_str(&created).expect("json");
        let worker_id = output["worker_id"].as_str().expect("worker_id").to_string();

        let completed = execute_tool(
            "WorkerObserveCompletion",
            &json!({
                "worker_id": worker_id,
                "finish_reason": "end_turn",
                "tokens_output": 512
            }),
        )
        .expect("WorkerObserveCompletion should succeed");
        let completed_output: serde_json::Value = serde_json::from_str(&completed).expect("json");
        assert_eq!(completed_output["status"], "finished");
        assert_eq!(completed_output["prompt_in_flight"], false);
    }

    #[test]
    fn worker_observe_completion_degraded_provider_sets_failed_status() {
        let created = execute_tool(
            "WorkerCreate",
            &json!({"cwd": "/tmp/observe-degraded-test", "trusted_roots": ["/tmp"]}),
        )
        .expect("WorkerCreate should succeed");
        let output: serde_json::Value = serde_json::from_str(&created).expect("json");
        let worker_id = output["worker_id"].as_str().expect("worker_id").to_string();

        // finish=unknown + 0 tokens = degraded provider classification
        let failed = execute_tool(
            "WorkerObserveCompletion",
            &json!({
                "worker_id": worker_id,
                "finish_reason": "unknown",
                "tokens_output": 0
            }),
        )
        .expect("WorkerObserveCompletion should succeed");
        let failed_output: serde_json::Value = serde_json::from_str(&failed).expect("json");
        assert_eq!(
            failed_output["status"], "failed",
            "finish=unknown + 0 tokens should classify as provider failure"
        );
        assert_eq!(failed_output["prompt_in_flight"], false);
        // last_error should be set with provider failure message
        assert!(
            !failed_output["last_error"].is_null(),
            "last_error should be populated for provider failure"
        );
    }

    #[test]
    fn worker_tools_detect_misdelivery_and_arm_prompt_replay() {
        let created = execute_tool(
            "WorkerCreate",
            &json!({
                "cwd": "/tmp/repo/worker-misdelivery"
            }),
        )
        .expect("WorkerCreate should succeed");
        let created_output: serde_json::Value = serde_json::from_str(&created).expect("json");
        let worker_id = created_output["worker_id"]
            .as_str()
            .expect("worker id")
            .to_string();

        execute_tool(
            "WorkerObserve",
            &json!({
                "worker_id": worker_id,
                "screen_text": "Ready for input\n>"
            }),
        )
        .expect("worker should become ready");

        execute_tool(
            "WorkerSendPrompt",
            &json!({
                "worker_id": worker_id,
                "prompt": "Investigate flaky boot"
            }),
        )
        .expect("prompt send should succeed");

        let recovered = execute_tool(
            "WorkerObserve",
            &json!({
                "worker_id": worker_id,
                "screen_text": "% Investigate flaky boot\nzsh: command not found: Investigate"
            }),
        )
        .expect("misdelivery observe should succeed");
        let recovered_output: serde_json::Value = serde_json::from_str(&recovered).expect("json");
        assert_eq!(recovered_output["status"], "ready_for_prompt");
        assert_eq!(recovered_output["last_error"]["kind"], "prompt_delivery");
        assert_eq!(recovered_output["replay_prompt"], "Investigate flaky boot");
        assert_eq!(
            recovered_output["events"][3]["payload"]["observed_target"],
            "shell"
        );
        assert_eq!(
            recovered_output["events"][4]["payload"]["recovery_armed"],
            true
        );

        let replayed = execute_tool(
            "WorkerSendPrompt",
            &json!({
                "worker_id": worker_id
            }),
        )
        .expect("WorkerSendPrompt should replay recovered prompt");
        let replayed_output: serde_json::Value = serde_json::from_str(&replayed).expect("json");
        assert_eq!(replayed_output["status"], "running");
        assert_eq!(replayed_output["prompt_delivery_attempts"], 2);
        assert_eq!(replayed_output["prompt_in_flight"], true);
    }

    #[test]
    fn global_tool_registry_denies_blocked_tool_before_dispatch() {
        // given
        let policy = permission_policy_for_mode(PermissionMode::ReadOnly);
        let registry = GlobalToolRegistry::builtin().with_enforcer(PermissionEnforcer::new(policy));

        // when
        let error = registry
            .execute(
                "write_file",
                &json!({
                    "path": "blocked.txt",
                    "content": "blocked"
                }),
            )
            .expect_err("write tool should be denied before dispatch");

        // then
        assert!(error.contains("requires 'workspace-write' permission"));
    }

    #[test]
    fn subagent_tool_executor_denies_blocked_tool_before_dispatch() {
        // given
        let policy = permission_policy_for_mode(PermissionMode::ReadOnly);
        let mut executor = SubagentToolExecutor::new(BTreeSet::from([String::from("write_file")]))
            .with_enforcer(PermissionEnforcer::new(policy));

        // when
        let error = executor
            .execute(
                "write_file",
                &json!({
                    "path": "blocked.txt",
                    "content": "blocked"
                })
                .to_string(),
            )
            .expect_err("subagent write tool should be denied before dispatch");

        // then
        assert!(error
            .to_string()
            .contains("requires 'workspace-write' permission"));
    }

    #[test]
    fn permission_mode_from_plugin_rejects_invalid_inputs() {
        let unknown_permission = permission_mode_from_plugin("admin")
            .expect_err("unknown plugin permission should fail");
        assert!(unknown_permission.contains("unsupported plugin permission: admin"));

        let empty_permission =
            permission_mode_from_plugin("").expect_err("empty plugin permission should fail");
        assert!(empty_permission.contains("unsupported plugin permission: "));
    }

    #[test]
    fn runtime_tools_extend_registry_definitions_permissions_and_search() {
        let registry = GlobalToolRegistry::builtin()
            .with_runtime_tools(vec![super::RuntimeToolDefinition {
                name: "mcp__demo__echo".to_string(),
                description: Some("Echo text from the demo MCP server".to_string()),
                input_schema: json!({
                    "type": "object",
                    "properties": { "text": { "type": "string" } },
                    "additionalProperties": false
                }),
                required_permission: runtime::PermissionMode::ReadOnly,
            }])
            .expect("runtime tools should register");

        let allowed = registry
            .normalize_allowed_tools(&["mcp__demo__echo".to_string()])
            .expect("runtime tool should be allow-listable")
            .expect("allow-list should be populated");
        assert!(allowed.contains("mcp__demo__echo"));

        let definitions = registry.definitions(Some(&allowed));
        assert_eq!(definitions.len(), 1);
        assert_eq!(definitions[0].name, "mcp__demo__echo");

        let permissions = registry
            .permission_specs(Some(&allowed))
            .expect("runtime tool permissions should resolve");
        assert_eq!(
            permissions,
            vec![(
                "mcp__demo__echo".to_string(),
                runtime::PermissionMode::ReadOnly
            )]
        );

        let search = registry.search(
            "demo echo",
            5,
            Some(vec!["pending-server".to_string()]),
            Some(runtime::McpDegradedReport::new(
                vec!["demo".to_string()],
                vec![runtime::McpFailedServer {
                    server_name: "pending-server".to_string(),
                    phase: runtime::McpLifecyclePhase::ToolDiscovery,
                    error: runtime::McpErrorSurface::new(
                        runtime::McpLifecyclePhase::ToolDiscovery,
                        Some("pending-server".to_string()),
                        "tool discovery failed",
                        BTreeMap::new(),
                        true,
                    ),
                }],
                vec!["mcp__demo__echo".to_string()],
                vec!["mcp__demo__echo".to_string()],
            )),
        );
        let output = serde_json::to_value(search).expect("search output should serialize");
        assert_eq!(output["matches"][0], "mcp__demo__echo");
        assert_eq!(output["pending_mcp_servers"][0], "pending-server");
        assert_eq!(
            output["mcp_degraded"]["failed_servers"][0]["phase"],
            "tool_discovery"
        );
    }

    #[test]
    fn runtime_tools_are_appended_across_registration_batches() {
        let registry = GlobalToolRegistry::builtin()
            .with_runtime_tools(vec![super::RuntimeToolDefinition {
                name: "mcp__demo__echo".to_string(),
                description: Some("Echo text".to_string()),
                input_schema: json!({"type": "object"}),
                required_permission: runtime::PermissionMode::ReadOnly,
            }])
            .expect("first runtime tool batch should register")
            .with_runtime_tools(vec![super::RuntimeToolDefinition {
                name: "memory_search".to_string(),
                description: Some("Search memory".to_string()),
                input_schema: json!({"type": "object"}),
                required_permission: runtime::PermissionMode::ReadOnly,
            }])
            .expect("second runtime tool batch should append");

        let definitions = registry.definitions(None);
        assert!(definitions
            .iter()
            .any(|tool| tool.name == "mcp__demo__echo"));
        assert!(definitions.iter().any(|tool| tool.name == "memory_search"));
    }

    #[test]
    fn normalize_allowed_tools_accepts_claude_code_casing_and_separators() {
        let registry = GlobalToolRegistry::builtin();

        // A migrating Claude Code / Cursor user may write the built-in tools in
        // any of camelCase, snake_case, kebab-case, or flattened form. All must
        // resolve to the same canonical tool so their `allowedTools` keeps working.
        for variant in ["WebFetch", "web_fetch", "web-fetch", "webfetch", "webFetch"] {
            let allowed = registry
                .normalize_allowed_tools(&[variant.to_string()])
                .unwrap_or_else(|error| panic!("`{variant}` should resolve: {error}"))
                .expect("allow-list should be populated");
            assert!(
                allowed.contains("web_fetch"),
                "`{variant}` should canonicalize to web_fetch, got {allowed:?}"
            );
            assert!(!allowed.contains("WebFetch"));
        }

        // snake_case canonical tools accept PascalCase/kebab variants too.
        for variant in ["read_file", "Read-File", "readFile", "read"] {
            let allowed = registry
                .normalize_allowed_tools(&[variant.to_string()])
                .unwrap_or_else(|error| panic!("`{variant}` should resolve: {error}"))
                .expect("allow-list should be populated");
            assert!(
                allowed.contains("read_file"),
                "`{variant}` should canonicalize to read_file, got {allowed:?}"
            );
        }

        // Comma/space separated lists and unknown tools are handled.
        let allowed = registry
            .normalize_allowed_tools(&["WebFetch, read_file glob".to_string()])
            .expect("mixed list should resolve")
            .expect("allow-list should be populated");
        assert!(allowed.contains("web_fetch"));
        assert!(allowed.contains("read_file"));
        assert!(allowed.contains("glob_search"));

        let error = registry
            .normalize_allowed_tools(&["NoSuchTool".to_string()])
            .expect_err("unknown tool should error");
        assert!(error.contains("invalid_tool_name: unsupported tool in --allowedTools: NoSuchTool"));
        assert!(error.contains("Available:"));
        assert!(error.contains("Aliases:"));
    }

    #[test]
    fn canonical_allowed_tool_name_splits_camel_case() {
        use super::{
            allowed_tool_lookup_key, canonical_allowed_tool_name, canonical_builtin_execution_name,
        };
        assert_eq!(canonical_allowed_tool_name("WebFetch"), "web_fetch");
        assert_eq!(canonical_allowed_tool_name("web-fetch"), "web_fetch");
        assert_eq!(canonical_allowed_tool_name("read_file"), "read_file");
        assert_eq!(canonical_allowed_tool_name("ToolSearch"), "tool_search");
        assert_eq!(allowed_tool_lookup_key("WebFetch"), "webfetch");
        assert_eq!(allowed_tool_lookup_key("web_fetch"), "webfetch");
        assert_eq!(allowed_tool_lookup_key("web-fetch"), "webfetch");
        assert_eq!(
            canonical_builtin_execution_name("web_search"),
            Some("WebSearch")
        );
        assert_eq!(
            canonical_builtin_execution_name("web-search"),
            Some("WebSearch")
        );
        assert_eq!(
            canonical_builtin_execution_name("WebSearch"),
            Some("WebSearch")
        );
    }

    #[test]
    fn allowed_tools_rejects_empty_token_lists() {
        let registry = GlobalToolRegistry::builtin();

        for raw in ["", ",,", "   "] {
            let err = registry
                .normalize_allowed_tools(&[raw.to_string()])
                .expect_err("empty allow-list input should be rejected");
            assert!(
                err.contains("--allowedTools was provided with no usable tool names"),
                "unexpected error for {raw:?}: {err}"
            );
        }
    }

    #[test]
    fn allowed_tools_normalize_to_canonical_snake_case_and_aliases_432() {
        let registry = GlobalToolRegistry::builtin();
        let allowed = registry
            .normalize_allowed_tools(&["Read,WebFetch,MCP".to_string()])
            .expect("aliases and legacy names should normalize")
            .expect("allow-list should be populated");
        assert!(allowed.contains("read_file"));
        assert!(allowed.contains("web_fetch"));
        assert!(allowed.contains("mcp"));
        assert!(!allowed.contains("Read"));
        assert!(!allowed.contains("WebFetch"));

        let canonical = registry.canonical_allowed_tool_names();
        assert!(canonical.contains(&"web_fetch".to_string()));
        assert!(canonical.contains(&"todo_write".to_string()));
        assert!(!canonical.contains(&"WebFetch".to_string()));
        assert_eq!(
            registry.allowed_tool_aliases().get("WebFetch"),
            Some(&"web_fetch".to_string())
        );
    }

    #[test]
    fn web_fetch_returns_prompt_aware_summary() {
        let server = TestServer::spawn(Arc::new(|request_line: &str, _raw_request: &str| {
            assert!(request_line.starts_with("GET /page "));
            HttpResponse::html(
                200,
                "OK",
                "<html><head><title>Ignored</title></head><body><h1>Test Page</h1><p>Hello <b>world</b> from local server.</p></body></html>",
            )
        }));

        let result = execute_tool(
            "WebFetch",
            &json!({
                "url": format!("http://{}/page", server.addr()),
                "prompt": "Summarize this page"
            }),
        )
        .expect("WebFetch should succeed");

        let output: serde_json::Value = serde_json::from_str(&result).expect("valid json");
        assert_eq!(output["code"], 200);
        let summary = output["result"].as_str().expect("result string");
        assert!(summary.contains("Fetched"));
        assert!(summary.contains("Test Page"));
        assert!(summary.contains("Hello world from local server"));

        let titled = execute_tool(
            "WebFetch",
            &json!({
                "url": format!("http://{}/page", server.addr()),
                "prompt": "What is the page title?"
            }),
        )
        .expect("WebFetch title query should succeed");
        let titled_output: serde_json::Value = serde_json::from_str(&titled).expect("valid json");
        let titled_summary = titled_output["result"].as_str().expect("result string");
        assert!(titled_summary.contains("Title: Ignored"));
    }

    #[test]
    fn web_fetch_supports_plain_text_and_rejects_invalid_url() {
        let server = TestServer::spawn(Arc::new(|request_line: &str, _raw_request: &str| {
            assert!(request_line.starts_with("GET /plain "));
            HttpResponse::text(200, "OK", "plain text response")
        }));

        let result = execute_tool(
            "WebFetch",
            &json!({
                "url": format!("http://{}/plain", server.addr()),
                "prompt": "Show me the content"
            }),
        )
        .expect("WebFetch should succeed for text content");

        let output: serde_json::Value = serde_json::from_str(&result).expect("valid json");
        assert_eq!(output["url"], format!("http://{}/plain", server.addr()));
        assert!(output["result"]
            .as_str()
            .expect("result")
            .contains("plain text response"));

        let error = execute_tool(
            "WebFetch",
            &json!({
                "url": "not a url",
                "prompt": "Summarize"
            }),
        )
        .expect_err("invalid URL should fail");
        assert!(error.contains("relative URL without a base") || error.contains("invalid"));
    }

    #[test]
    fn web_fetch_rejects_non_success_and_obvious_challenge_pages() {
        let not_found = TestServer::spawn(Arc::new(|_request_line: &str, _raw_request: &str| {
            HttpResponse::html(
                404,
                "Not Found",
                "<html><head><title>404 Not Found</title></head><body>missing</body></html>",
            )
        }));
        let not_found_error = execute_tool(
            "WebFetch",
            &json!({
                "url": format!("http://{}/missing", not_found.addr()),
                "prompt": "Summarize"
            }),
        )
        .expect_err("non-success HTTP status must fail the tool call");
        assert!(not_found_error.contains("HTTP 404 Not Found"));

        let challenge = TestServer::spawn(Arc::new(|_request_line: &str, _raw_request: &str| {
            HttpResponse::html(
                200,
                "OK",
                "<html><head><title>Verify you are human</title></head><body>Checking your browser before accessing the requested page.</body></html>",
            )
        }));
        let challenge_error = execute_tool(
            "WebFetch",
            &json!({
                "url": format!("http://{}/challenge", challenge.addr()),
                "prompt": "Summarize"
            }),
        )
        .expect_err("obvious anti-bot pages must fail the tool call");
        assert!(challenge_error.contains("blocked or error page"));
    }

    #[test]
    fn web_search_uses_builtin_provider_when_no_api_key_is_configured() {
        let _guard = env_guard();
        let server = TestServer::spawn(Arc::new(|request_line: &str, _raw_request: &str| {
            if request_line.contains("GET /ddg?q=rust+async+runtime") {
                return HttpResponse::html(
                    200,
                    "OK",
                    r#"<html><body><div class="result web-result"><a class="result__a" href="https://docs.rs/tokio/latest/tokio/">Tokio async runtime documentation</a><div class="result__snippet">Rust async runtime APIs and guides.</div></div></body></html>"#,
                );
            }
            if request_line.contains("GET /brave?q=rust+async+runtime&source=web") {
                return HttpResponse::html(
                    200,
                    "OK",
                    r#"<html><body><div class="snippet" data-type="web"><a class="l1" href="https://www.rust-lang.org/learn"><div class="search-snippet-title">Learn Rust async programming</div></a><div class="generic-snippet"><div class="content">Official Rust learning resources for async programs.</div></div></div></body></html>"#,
                );
            }
            HttpResponse::text(404, "Not Found", "missing")
        }));
        let _provider = TestEnvVar::remove("AOSD_WEB_SEARCH_PROVIDER");
        let _provider_order = TestEnvVar::remove("AOSD_WEB_SEARCH_PROVIDER_ORDER");
        let _ddg = TestEnvVar::set(
            "AOSD_BUILTIN_SEARCH_DDG_URL",
            &format!("http://{}/ddg", server.addr()),
        );
        let _brave = TestEnvVar::set(
            "AOSD_BUILTIN_SEARCH_BRAVE_URL",
            &format!("http://{}/brave", server.addr()),
        );
        let _bing = TestEnvVar::set(
            "AOSD_BUILTIN_SEARCH_BING_URL",
            &format!("http://{}/bing", server.addr()),
        );
        let _hacker_news = TestEnvVar::set(
            "AOSD_BUILTIN_SEARCH_HN_URL",
            &format!("http://{}/hn", server.addr()),
        );
        let _stack_exchange = TestEnvVar::set(
            "AOSD_BUILTIN_SEARCH_STACK_EXCHANGE_URL",
            &format!("http://{}/stack", server.addr()),
        );
        let _wikipedia = TestEnvVar::set(
            "AOSD_BUILTIN_SEARCH_WIKIPEDIA_URL",
            &format!("http://{}/wikipedia", server.addr()),
        );
        let _max_retries = TestEnvVar::set("AOSD_WEB_SEARCH_MAX_RETRIES", "0");
        let _removed_keys = [
            "AOSD_WEB_SEARCH_PROVIDER",
            "AOSD_WEB_SEARCH_PROVIDER_ORDER",
            "AOSD_BRAVE_API_KEY",
            "BRAVE_SEARCH_API_KEY",
            "AOSD_BRAVE_API_KEYS",
            "BRAVE_SEARCH_API_KEYS",
            "AOSD_TAVILY_API_KEY",
            "TAVILY_API_KEY",
            "AOSD_TAVILY_API_KEYS",
            "TAVILY_API_KEYS",
            "AOSD_SERPER_API_KEY",
            "SERPER_API_KEY",
            "AOSD_SERPER_API_KEYS",
            "SERPER_API_KEYS",
            "AOSD_EXA_API_KEY",
            "EXA_API_KEY",
            "AOSD_EXA_API_KEYS",
            "EXA_API_KEYS",
        ]
        .map(TestEnvVar::remove);

        let output = execute_tool("WebSearch", &json!({ "query": "rust async runtime" }))
            .expect("built-in web search should work without an API key");
        assert!(output.contains("docs.rs/tokio"));
        assert!(output.contains("rust-lang.org/learn"));
        assert!(output.contains("builtin: attempt 1/1"));
    }

    #[test]
    fn builtin_search_html_parsers_extract_organic_results() {
        let ddg = r#"
            <div class="result result--ad"><a class="result__a" href="https://ads.example/">Ad</a></div>
            <div class="result web-result">
              <a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fdocs.rs%2Faxum%2Flatest%2Faxum%2F">Axum docs</a>
              <div class="result__snippet">Web framework for Rust.</div>
            </div>
        "#;
        let ddg_hits = parse_duckduckgo_html_hits(ddg, 5).expect("parse DuckDuckGo fixture");
        assert_eq!(ddg_hits.len(), 1);
        assert!(ddg_hits[0]
            .url
            .starts_with("https://docs.rs/axum/latest/axum/"));
        assert_eq!(ddg_hits[0].title, "Axum docs");

        let brave = r#"
            <div class="snippet" data-type="web">
              <a class="l1" href="https://doc.rust-lang.org/book/"><div class="search-snippet-title">The Rust Book</div></a>
              <div class="generic-snippet"><div class="content">Official Rust language guide.</div></div>
            </div>
        "#;
        let brave_hits = parse_brave_html_hits(brave, 5).expect("parse Brave fixture");
        assert_eq!(brave_hits.len(), 1);
        assert_eq!(brave_hits[0].title, "The Rust Book");
        assert!(brave_hits[0]
            .url
            .starts_with("https://doc.rust-lang.org/book/"));
        assert_eq!(
            normalize_duckduckgo_result_url(
                "https://duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fguide"
            )
            .as_deref(),
            Some("https://example.com/guide")
        );

        let bing = r#"
            <ol id="b_results">
              <li class="b_algo">
                <h2><a href="https://tokio.rs/">Tokio - An asynchronous Rust runtime</a></h2>
                <div class="b_caption"><p>Reliable asynchronous applications with Rust.</p></div>
              </li>
            </ol>
        "#;
        let bing_hits = parse_bing_html_hits(bing, 5).expect("parse Bing fixture");
        assert_eq!(bing_hits.len(), 1);
        assert_eq!(bing_hits[0].url, "https://tokio.rs/");
        assert_eq!(
            bing_hits[0].snippet.as_deref(),
            Some("Reliable asynchronous applications with Rust.")
        );

        let hacker_news_hits = parse_hacker_news_hits(
            &json!({
                "hits": [{
                    "title": "Tokio 1.0 - async runtime for Rust",
                    "url": "https://tokio.rs/blog/2020-12-announcing-tokio-1-0",
                    "objectID": "123"
                }]
            }),
            5,
        );
        assert_eq!(hacker_news_hits.len(), 1);
        assert_eq!(
            hacker_news_hits[0].title,
            "Tokio 1.0 - async runtime for Rust"
        );

        let stack_exchange_hits = parse_stack_exchange_hits(
            &json!({
                "items": [{
                    "title": "How does Tokio schedule async tasks?",
                    "link": "https://stackoverflow.com/questions/123/tokio-scheduler",
                    "tags": ["rust", "tokio", "async-await"]
                }]
            }),
            5,
        );
        assert_eq!(stack_exchange_hits.len(), 1);
        assert_eq!(
            stack_exchange_hits[0].snippet.as_deref(),
            Some("Stack Overflow tags: rust, tokio, async-await")
        );
        assert_eq!(public_api_search_query("Tokio 是什么"), "Tokio");
    }

    #[test]
    fn weather_search_extracts_location_without_binding_to_a_specific_city() {
        assert_eq!(
            super::weather_location_candidates("北京海淀天气预报 今天 明天"),
            vec!["北京海淀", "海淀", "北京"]
        );
        assert_eq!(
            super::weather_location_candidates("New York weather forecast tomorrow"),
            vec!["new york"]
        );
        assert!(super::is_weather_search_query(
            "Will it rain in London today?"
        ));
        assert!(!super::is_weather_search_query("London transport policy"));
    }

    #[test]
    fn weather_search_ranks_administrative_context_and_renders_forecast() {
        let beijing = json!({
            "name": "朝阳",
            "admin1": "北京市",
            "admin2": "北京市",
            "country": "中国",
            "latitude": 39.92,
            "longitude": 116.44,
            "timezone": "Asia/Shanghai"
        });
        let liaoning = json!({
            "name": "朝阳",
            "admin1": "辽宁省",
            "admin2": "朝阳市",
            "country": "中国",
            "latitude": 41.57,
            "longitude": 120.45,
            "timezone": "Asia/Shanghai"
        });
        assert!(
            super::weather_location_relevance("北京朝阳天气", &beijing)
                > super::weather_location_relevance("北京朝阳天气", &liaoning)
        );

        let forecast = json!({
            "timezone": "Asia/Shanghai",
            "current": {
                "time": "2026-08-07T16:30",
                "temperature_2m": 31.1,
                "apparent_temperature": 35.4,
                "relative_humidity_2m": 61,
                "precipitation": 0.2,
                "weather_code": 53,
                "wind_speed_10m": 7.6
            },
            "daily": {
                "time": ["2026-08-07", "2026-08-08"],
                "weather_code": [96, 51],
                "temperature_2m_max": [36.1, 30.9],
                "temperature_2m_min": [26.1, 25.6],
                "precipitation_probability_max": [98, 53]
            }
        });
        let summary = super::render_open_meteo_weather_summary(&beijing, &forecast);
        assert!(summary.contains("Open-Meteo (CC BY 4.0)"));
        assert!(summary.contains("31.1 C"));
        assert!(summary.contains("thunderstorm with hail"));
        assert!(summary.contains("2026-08-08"));
    }

    #[test]
    fn builtin_weather_waits_for_structured_forecast_and_drops_unrelated_hits() {
        let _guard = env_guard();
        let general = TestServer::spawn(Arc::new(|request_line: &str, _raw_request: &str| {
            if request_line.starts_with("GET /ddg") {
                return HttpResponse::html(
                    200,
                    "OK",
                    r#"<div class="result web-result"><a class="result__a" href="https://irrelevant.example/city">Unrelated city</a><div class="result__snippet">Historical city information.</div></div>"#,
                );
            }
            if request_line.starts_with("GET /brave") {
                return HttpResponse::html(
                    200,
                    "OK",
                    r#"<div class="snippet" data-type="web"><a class="l1" href="https://another.example/page"><div class="search-snippet-title">Another unrelated result</div></a><div class="generic-snippet"><div class="content">No current forecast.</div></div></div>"#,
                );
            }
            if request_line.starts_with("GET /wikipedia") {
                return HttpResponse::json(
                    200,
                    "OK",
                    r#"{"query":{"search":[{"title":"青岛市","snippet":"城市历史"}]}}"#,
                );
            }
            if request_line.starts_with("GET /hn") {
                return HttpResponse::json(200, "OK", r#"{"hits":[]}"#);
            }
            if request_line.starts_with("GET /stack") {
                return HttpResponse::json(200, "OK", r#"{"items":[]}"#);
            }
            HttpResponse::text(404, "Not Found", "missing")
        }));
        let weather = TestServer::spawn(Arc::new(|request_line: &str, _raw_request: &str| {
            if request_line.starts_with("GET /geocoding") {
                thread::sleep(Duration::from_millis(300));
                return HttpResponse::json(
                    200,
                    "OK",
                    r#"{"results":[{"name":"海淀","admin1":"北京市","country":"中国","latitude":39.96,"longitude":116.30,"timezone":"Asia/Shanghai"}]}"#,
                );
            }
            if request_line.starts_with("GET /forecast") {
                return HttpResponse::json(
                    200,
                    "OK",
                    r#"{"timezone":"Asia/Shanghai","current":{"time":"2026-08-07T16:30","temperature_2m":31.1,"apparent_temperature":34.0,"relative_humidity_2m":60,"precipitation":0.0,"weather_code":1,"wind_speed_10m":5.0},"daily":{"time":["2026-08-07"],"weather_code":[1],"temperature_2m_max":[35.0],"temperature_2m_min":[25.0],"precipitation_probability_max":[20]}}"#,
                );
            }
            HttpResponse::text(404, "Not Found", "missing")
        }));

        let _provider = TestEnvVar::set("AOSD_WEB_SEARCH_PROVIDER", "builtin");
        let _ddg = TestEnvVar::set(
            "AOSD_BUILTIN_SEARCH_DDG_URL",
            &format!("http://{}/ddg", general.addr()),
        );
        let _brave = TestEnvVar::set(
            "AOSD_BUILTIN_SEARCH_BRAVE_URL",
            &format!("http://{}/brave", general.addr()),
        );
        let _bing = TestEnvVar::set(
            "AOSD_BUILTIN_SEARCH_BING_URL",
            &format!("http://{}/bing", general.addr()),
        );
        let _wikipedia = TestEnvVar::set(
            "AOSD_BUILTIN_SEARCH_WIKIPEDIA_URL",
            &format!("http://{}/wikipedia", general.addr()),
        );
        let _hacker_news = TestEnvVar::set(
            "AOSD_BUILTIN_SEARCH_HN_URL",
            &format!("http://{}/hn", general.addr()),
        );
        let _stack_exchange = TestEnvVar::set(
            "AOSD_BUILTIN_SEARCH_STACK_EXCHANGE_URL",
            &format!("http://{}/stack", general.addr()),
        );
        let _geocoding = TestEnvVar::set(
            "AOSD_BUILTIN_WEATHER_GEOCODING_URL",
            &format!("http://{}/geocoding", weather.addr()),
        );
        let _forecast = TestEnvVar::set(
            "AOSD_BUILTIN_WEATHER_FORECAST_URL",
            &format!("http://{}/forecast", weather.addr()),
        );
        let _enrichment = TestEnvVar::set("AOSD_WEB_SEARCH_ENRICH_ENABLED", "false");
        let _retries = TestEnvVar::set("AOSD_WEB_SEARCH_MAX_RETRIES", "0");

        let output = execute_tool("WebSearch", &json!({"query":"北京海淀天气预报"}))
            .expect("weather search should return structured evidence");
        assert!(output.contains("open_meteo_forecast"));
        assert!(output.contains("31.1 C"));
        assert!(!output.contains("irrelevant.example"));
        assert!(!output.contains("青岛市"));
    }

    #[test]
    fn web_search_model_payload_has_per_hit_and_total_content_budgets() {
        let _guard = env_guard();
        let _per_hit = TestEnvVar::remove("AOSD_WEB_SEARCH_MODEL_CONTENT_CHARS_PER_HIT");
        let _total = TestEnvVar::remove("AOSD_WEB_SEARCH_MODEL_CONTENT_CHARS_TOTAL");
        let mut hits = (0..8)
            .map(|index| {
                let mut hit = super::search_hit(
                    format!("Result {index}"),
                    format!("https://example.com/{index}"),
                    Some("s".repeat(2_000)),
                );
                hit.content = Some("x".repeat(10_000));
                hit
            })
            .collect::<Vec<_>>();

        super::compact_web_search_model_payload(&mut hits);

        let total = hits
            .iter()
            .filter_map(|hit| hit.content.as_deref())
            .map(|content| content.chars().count())
            .sum::<usize>();
        assert!(total <= 16_100, "total model payload was {total} chars");
        assert!(hits.iter().all(|hit| {
            hit.content
                .as_deref()
                .is_none_or(|content| content.chars().count() <= 4_100)
        }));
        assert!(hits.iter().all(|hit| {
            hit.snippet
                .as_deref()
                .is_none_or(|snippet| snippet.chars().count() <= 700)
        }));
    }

    #[test]
    fn normal_provider_override_falls_back_to_builtin_but_strict_mode_does_not() {
        let _guard = env_guard();
        let server = TestServer::spawn(Arc::new(|request_line: &str, _raw_request: &str| {
            if request_line.starts_with("GET /broken") {
                return HttpResponse::text(400, "Bad Request", "broken extension");
            }
            if request_line.starts_with("GET /ddg") {
                return HttpResponse::html(
                    200,
                    "OK",
                    r#"<div class="result web-result"><a class="result__a" href="https://docs.rs/reqwest/latest/reqwest/">Reqwest HTTP client</a><div class="result__snippet">Rust HTTP client documentation and examples.</div></div>"#,
                );
            }
            if request_line.starts_with("GET /brave") {
                return HttpResponse::html(
                    200,
                    "OK",
                    r#"<div class="snippet" data-type="web"><a class="l1" href="https://github.com/seanmonstar/reqwest"><div class="search-snippet-title">Reqwest source repository</div></a><div class="generic-snippet"><div class="content">Source and release notes for the Rust HTTP client.</div></div></div>"#,
                );
            }
            HttpResponse::text(404, "Not Found", "missing")
        }));
        let _ddg = TestEnvVar::set(
            "AOSD_BUILTIN_SEARCH_DDG_URL",
            &format!("http://{}/ddg", server.addr()),
        );
        let _brave = TestEnvVar::set(
            "AOSD_BUILTIN_SEARCH_BRAVE_URL",
            &format!("http://{}/brave", server.addr()),
        );
        let _bing = TestEnvVar::set(
            "AOSD_BUILTIN_SEARCH_BING_URL",
            &format!("http://{}/bing", server.addr()),
        );
        let _hacker_news = TestEnvVar::set(
            "AOSD_BUILTIN_SEARCH_HN_URL",
            &format!("http://{}/hn", server.addr()),
        );
        let _stack_exchange = TestEnvVar::set(
            "AOSD_BUILTIN_SEARCH_STACK_EXCHANGE_URL",
            &format!("http://{}/stack", server.addr()),
        );
        let _wikipedia = TestEnvVar::set(
            "AOSD_BUILTIN_SEARCH_WIKIPEDIA_URL",
            &format!("http://{}/wikipedia", server.addr()),
        );
        let _max_retries = TestEnvVar::set("AOSD_WEB_SEARCH_MAX_RETRIES", "0");
        let broken = WebSearchProviderConfig {
            id: "broken".to_string(),
            name: "Broken Generic Search".to_string(),
            provider_type: WebSearchProviderType::GenericJson,
            enabled: true,
            priority: 1,
            base_url: Some(format!("http://{}/broken", server.addr())),
            method: Some("GET".to_string()),
            auth_type: Some("none".to_string()),
            auth_secret: None,
            headers_json: None,
            query_template_json: None,
            response_mapping_json: None,
            timeout_secs: Some(3),
            max_results: Some(5),
            fetch_content_enabled: Some(false),
            content_extract_mode: None,
            domain_allowlist: None,
            domain_blocklist: None,
            rate_limit_json: None,
        };

        let strict = with_strict_web_search_provider_override(vec![broken.clone()], || {
            execute_tool("WebSearch", &json!({ "query": "reqwest rust client" }))
        });
        assert!(
            strict.is_err(),
            "strict extension test must expose the failure"
        );

        let normal = with_web_search_provider_override(vec![broken], || {
            execute_tool("WebSearch", &json!({ "query": "reqwest rust client" }))
        })
        .expect("normal runtime should fall back to built-in search");
        assert!(normal.contains("docs.rs/reqwest"));
        assert!(normal.contains("builtin: attempt 1/1"));
    }

    #[test]
    #[ignore = "requires public internet access"]
    fn builtin_search_live_smoke_covers_chinese_and_english() {
        let _guard = env_guard();
        let _provider = TestEnvVar::set("AOSD_WEB_SEARCH_PROVIDER", "builtin");
        let _ddg = TestEnvVar::remove("AOSD_BUILTIN_SEARCH_DDG_URL");
        let _brave = TestEnvVar::remove("AOSD_BUILTIN_SEARCH_BRAVE_URL");
        let _bing = TestEnvVar::remove("AOSD_BUILTIN_SEARCH_BING_URL");
        let _hacker_news = TestEnvVar::remove("AOSD_BUILTIN_SEARCH_HN_URL");
        let _stack_exchange = TestEnvVar::remove("AOSD_BUILTIN_SEARCH_STACK_EXCHANGE_URL");
        let _wikipedia = TestEnvVar::remove("AOSD_BUILTIN_SEARCH_WIKIPEDIA_URL");
        let _enrichment = TestEnvVar::set("AOSD_WEB_SEARCH_ENRICH_ENABLED", "false");
        let _max_retries = TestEnvVar::set("AOSD_WEB_SEARCH_MAX_RETRIES", "0");
        let mut domains = BTreeSet::new();

        for query in [
            "北京海淀天气预报",
            "Tokio 是什么",
            "Tokio Rust async runtime",
        ] {
            let output = execute_tool("WebSearch", &json!({ "query": query }))
                .unwrap_or_else(|error| panic!("live built-in search failed for {query}: {error}"));
            let payload: serde_json::Value =
                serde_json::from_str(&output).expect("live WebSearch output should be JSON");
            eprintln!(
                "live built-in search query={query:?} provider_trace={} quality_filter={}",
                payload["providerTrace"], payload["qualityFilter"]
            );
            let hits = payload["results"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|result| result.get("content").and_then(serde_json::Value::as_array))
                .flatten()
                .collect::<Vec<_>>();
            assert!(
                !hits.is_empty(),
                "live search returned no citations for {query}"
            );
            eprintln!(
                "live built-in search query={query:?} citations={}",
                hits.len()
            );
            let ascii_anchors = super::ascii_query_anchor_terms(query);
            if !ascii_anchors.is_empty() {
                assert!(
                    hits.iter().all(|hit| {
                        let searchable = format!(
                            "{} {} {}",
                            hit.get("title")
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or_default(),
                            hit.get("url")
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or_default(),
                            hit.get("snippet")
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or_default(),
                        )
                        .to_ascii_lowercase();
                        ascii_anchors
                            .iter()
                            .any(|anchor| searchable.contains(anchor))
                    }),
                    "live search returned a result without a distinctive query entity for {query}"
                );
            }
            let mut query_domains = BTreeSet::new();
            for hit in hits {
                let Some(url) = hit.get("url").and_then(serde_json::Value::as_str) else {
                    continue;
                };
                let parsed = reqwest::Url::parse(url)
                    .unwrap_or_else(|error| panic!("invalid live citation URL {url}: {error}"));
                assert!(matches!(parsed.scheme(), "http" | "https"));
                if let Some(domain) = parsed.host_str() {
                    query_domains.insert(domain.to_string());
                    domains.insert(domain.to_string());
                }
            }
            if super::is_weather_search_query(query) {
                assert!(
                    query_domains.contains("api.open-meteo.com"),
                    "live weather search must return structured Open-Meteo evidence, got {query_domains:?}"
                );
            } else {
                assert!(
                    !query_domains.is_empty(),
                    "live search should return at least one relevant domain for {query}"
                );
            }
            eprintln!("live built-in search query={query:?} domains={query_domains:?}");
        }

        assert!(
            domains.len() >= 2,
            "live built-in search should cover at least two domains, got {domains:?}"
        );
        eprintln!("live built-in search domains={domains:?}");
    }

    #[test]
    fn web_search_brave_provider_parses_and_filters_results() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let server = TestServer::spawn(Arc::new(|request_line: &str, _raw_request: &str| {
            assert!(request_line.contains("GET /res/v1/web/search?q=rust+search+provider"));
            HttpResponse::json(
                200,
                "OK",
                r#"{
                  "web": {
                    "results": [
                      {"title": "Tokio Docs", "url": "https://docs.rs/tokio"},
                      {"title": "Blocked", "url": "https://example.com/blocked"},
                      {"title": "Search Nav", "url": "https://www.bing.com/search?q=rust+search+provider"}
                    ]
                  }
                }"#,
            )
        }));

        std::env::set_var("AOSD_WEB_SEARCH_PROVIDER", "brave");
        std::env::set_var("AOSD_BRAVE_API_KEY", "test-key");
        std::env::set_var(
            "AOSD_BRAVE_BASE_URL",
            format!("http://{}/res/v1/web/search", server.addr()),
        );
        let result = execute_tool(
            "WebSearch",
            &json!({
                "query": "rust search provider",
                "allowed_domains": ["docs.rs"],
                "blocked_domains": ["example.com"]
            }),
        )
        .expect("WebSearch should succeed via brave provider");
        std::env::remove_var("AOSD_WEB_SEARCH_PROVIDER");
        std::env::remove_var("AOSD_BRAVE_API_KEY");
        std::env::remove_var("AOSD_BRAVE_BASE_URL");

        let output: serde_json::Value = serde_json::from_str(&result).expect("valid json");
        let results = output["results"].as_array().expect("results array");
        let search_result = results
            .iter()
            .find(|item| item.get("content").is_some())
            .expect("search result block present");
        let content = search_result["content"].as_array().expect("content array");
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["title"], "Tokio Docs");
        assert_eq!(content[0]["url"], "https://docs.rs/tokio");
    }

    #[test]
    fn web_search_empty_domain_filters_do_not_drop_all_hits() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let server = TestServer::spawn(Arc::new(|request_line: &str, _raw_request: &str| {
            assert!(request_line.contains("GET /res/v1/web/search?q=rust+ewma"));
            HttpResponse::json(
                200,
                "OK",
                r#"{
                  "web": {
                    "results": [
                      {"title": "Tokio Docs", "url": "https://docs.rs/tokio"},
                      {"title": "Reqwest Docs", "url": "https://docs.rs/reqwest"},
                      {"title": "Search Nav", "url": "https://www.bing.com/search?q=rust+ewma"}
                    ]
                  }
                }"#,
            )
        }));

        std::env::set_var("AOSD_WEB_SEARCH_PROVIDER", "brave");
        std::env::set_var("AOSD_BRAVE_API_KEY", "test-key");
        std::env::set_var(
            "AOSD_BRAVE_BASE_URL",
            format!("http://{}/res/v1/web/search", server.addr()),
        );
        let result = execute_tool(
            "WebSearch",
            &json!({
                "query": "rust ewma",
                "allowed_domains": [],
                "blocked_domains": []
            }),
        )
        .expect("WebSearch should keep hits when domain lists are empty");
        std::env::remove_var("AOSD_WEB_SEARCH_PROVIDER");
        std::env::remove_var("AOSD_BRAVE_API_KEY");
        std::env::remove_var("AOSD_BRAVE_BASE_URL");

        let output: serde_json::Value = serde_json::from_str(&result).expect("valid json");
        let results = output["results"].as_array().expect("results array");
        let search_result = results
            .iter()
            .find(|item| item.get("content").is_some())
            .expect("search result block present");
        let content = search_result["content"].as_array().expect("content array");
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["url"], "https://docs.rs/tokio");
        assert_eq!(content[1]["url"], "https://docs.rs/reqwest");
    }

    #[test]
    fn web_search_output_includes_quality_filter_url_diff_samples() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let server = TestServer::spawn(Arc::new(|request_line: &str, _raw_request: &str| {
            assert!(request_line.contains("GET /res/v1/web/search?q=tokio+docs+rewarded"));
            HttpResponse::json(
                200,
                "OK",
                r#"{
                  "web": {
                    "results": [
                      {"title": "Tokio Docs", "url": "https://docs.rs/tokio"},
                      {"title": "Account", "url": "https://account.example.com/settings/profile"},
                      {"title": "Android Apps", "url": "https://example.com/android#child-1"}
                    ]
                  }
                }"#,
            )
        }));

        std::env::set_var("AOSD_WEB_SEARCH_PROVIDER", "brave");
        std::env::set_var("AOSD_BRAVE_API_KEY", "test-key");
        std::env::set_var(
            "AOSD_BRAVE_BASE_URL",
            format!("http://{}/res/v1/web/search", server.addr()),
        );
        let result = execute_tool("WebSearch", &json!({ "query": "tokio docs rewarded" }))
            .expect("WebSearch should produce quality filter debug info");
        std::env::remove_var("AOSD_WEB_SEARCH_PROVIDER");
        std::env::remove_var("AOSD_BRAVE_API_KEY");
        std::env::remove_var("AOSD_BRAVE_BASE_URL");

        let output: serde_json::Value = serde_json::from_str(&result).expect("valid json");
        let quality = output
            .get("qualityFilter")
            .and_then(serde_json::Value::as_object)
            .expect("qualityFilter should be present");
        assert_eq!(quality.get("beforeCount").and_then(|v| v.as_u64()), Some(3));
        assert_eq!(quality.get("afterCount").and_then(|v| v.as_u64()), Some(1));
        assert_eq!(
            quality.get("droppedCount").and_then(|v| v.as_u64()),
            Some(2)
        );
        let dropped = quality
            .get("droppedUrlsSample")
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert!(dropped
            .iter()
            .any(|v| { v.as_str() == Some("https://account.example.com/settings/profile") }));
        assert!(dropped
            .iter()
            .any(|v| v.as_str() == Some("https://example.com/android")));
    }

    #[test]
    fn web_search_auto_falls_back_to_next_api_provider() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let server = TestServer::spawn(Arc::new(|request_line: &str, _raw_request: &str| {
            if request_line.starts_with("POST /tavily ") {
                return HttpResponse::json(
                    200,
                    "OK",
                    r#"{
                      "results": []
                    }"#,
                );
            }
            assert!(request_line.starts_with("GET /res/v1/web/search?q=rust+fallback+chain"));
            HttpResponse::json(
                200,
                "OK",
                r#"{
                  "web": {
                    "results": [
                    {"title": "Reqwest docs", "url": "https://docs.rs/reqwest"}
                    ]
                  }
                }"#,
            )
        }));

        std::env::set_var("AOSD_WEB_SEARCH_PROVIDER", "auto");
        std::env::set_var("AOSD_WEB_SEARCH_PROVIDER_ORDER", "tavily,brave");
        std::env::set_var("AOSD_TAVILY_API_KEY", "tavily-key");
        std::env::set_var("AOSD_BRAVE_API_KEY", "brave-key");
        std::env::set_var(
            "AOSD_TAVILY_BASE_URL",
            format!("http://{}/tavily", server.addr()),
        );
        std::env::set_var(
            "AOSD_BRAVE_BASE_URL",
            format!("http://{}/res/v1/web/search", server.addr()),
        );
        let result = execute_tool("WebSearch", &json!({ "query": "rust fallback chain" }))
            .expect("WebSearch should fallback to the next configured API provider");
        std::env::remove_var("AOSD_WEB_SEARCH_PROVIDER");
        std::env::remove_var("AOSD_WEB_SEARCH_PROVIDER_ORDER");
        std::env::remove_var("AOSD_TAVILY_API_KEY");
        std::env::remove_var("AOSD_BRAVE_API_KEY");
        std::env::remove_var("AOSD_TAVILY_BASE_URL");
        std::env::remove_var("AOSD_BRAVE_BASE_URL");

        let output: serde_json::Value = serde_json::from_str(&result).expect("valid json");
        let results = output["results"].as_array().expect("results array");
        let search_result = results
            .iter()
            .find(|item| item.get("content").is_some())
            .expect("search result block present");
        let content = search_result["content"].as_array().expect("content array");
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["url"], "https://docs.rs/reqwest");
    }

    #[test]
    fn web_search_tavily_provider_parses_results() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let server = TestServer::spawn(Arc::new(|request_line: &str, _raw_request: &str| {
            assert!(request_line.starts_with("POST /tavily "));
            HttpResponse::json(
                200,
                "OK",
                r#"{
                  "results": [
                    {"title": "Tokio Docs", "url": "https://docs.rs/tokio"}
                  ]
                }"#,
            )
        }));

        std::env::set_var("AOSD_WEB_SEARCH_PROVIDER", "tavily");
        std::env::set_var("AOSD_TAVILY_API_KEY", "tavily-key");
        std::env::set_var(
            "AOSD_TAVILY_BASE_URL",
            format!("http://{}/tavily", server.addr()),
        );
        let result = execute_tool("WebSearch", &json!({ "query": "tokio docs" }))
            .expect("WebSearch should parse Tavily results");
        std::env::remove_var("AOSD_WEB_SEARCH_PROVIDER");
        std::env::remove_var("AOSD_TAVILY_API_KEY");
        std::env::remove_var("AOSD_TAVILY_BASE_URL");

        let output: serde_json::Value = serde_json::from_str(&result).expect("valid json");
        let results = output["results"].as_array().expect("results array");
        let search_result = results
            .iter()
            .find(|item| item.get("content").is_some())
            .expect("search result block present");
        let content = search_result["content"].as_array().expect("content array");
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["url"], "https://docs.rs/tokio");
    }

    #[test]
    fn web_search_serper_provider_parses_results() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let server = TestServer::spawn(Arc::new(|request_line: &str, raw_request: &str| {
            assert!(request_line.starts_with("POST /serper "));
            assert!(raw_request
                .to_ascii_lowercase()
                .contains("x-api-key: serper-key"));
            HttpResponse::json(
                200,
                "OK",
                r#"{
                  "organic": [
                    {"title": "Tokio Docs", "link": "https://docs.rs/tokio", "snippet": "Async runtime"}
                  ]
                }"#,
            )
        }));

        std::env::set_var("AOSD_WEB_SEARCH_PROVIDER", "serper");
        std::env::set_var("AOSD_SERPER_API_KEY", "serper-key");
        std::env::set_var(
            "AOSD_SERPER_BASE_URL",
            format!("http://{}/serper", server.addr()),
        );
        let result = execute_tool("WebSearch", &json!({ "query": "tokio docs" }))
            .expect("WebSearch should parse Serper results");
        std::env::remove_var("AOSD_WEB_SEARCH_PROVIDER");
        std::env::remove_var("AOSD_SERPER_API_KEY");
        std::env::remove_var("AOSD_SERPER_BASE_URL");

        let output: serde_json::Value = serde_json::from_str(&result).expect("valid json");
        let results = output["results"].as_array().expect("results array");
        let search_result = results
            .iter()
            .find(|item| item.get("content").is_some())
            .expect("search result block present");
        let content = search_result["content"].as_array().expect("content array");
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["title"], "Tokio Docs");
        assert_eq!(content[0]["url"], "https://docs.rs/tokio");
    }

    #[test]
    fn web_search_exa_provider_parses_results() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let server = TestServer::spawn(Arc::new(|request_line: &str, raw_request: &str| {
            assert!(request_line.starts_with("POST /exa "));
            assert!(raw_request
                .to_ascii_lowercase()
                .contains("x-api-key: exa-key"));
            HttpResponse::json(
                200,
                "OK",
                r#"{
                  "results": [
                    {"title": "Reqwest Docs", "url": "https://docs.rs/reqwest", "highlights": ["HTTP client"]}
                  ]
                }"#,
            )
        }));

        std::env::set_var("AOSD_WEB_SEARCH_PROVIDER", "exa");
        std::env::set_var("AOSD_EXA_API_KEY", "exa-key");
        std::env::set_var("AOSD_EXA_BASE_URL", format!("http://{}/exa", server.addr()));
        let result = execute_tool("WebSearch", &json!({ "query": "reqwest docs" }))
            .expect("WebSearch should parse Exa results");
        std::env::remove_var("AOSD_WEB_SEARCH_PROVIDER");
        std::env::remove_var("AOSD_EXA_API_KEY");
        std::env::remove_var("AOSD_EXA_BASE_URL");

        let output: serde_json::Value = serde_json::from_str(&result).expect("valid json");
        let results = output["results"].as_array().expect("results array");
        let search_result = results
            .iter()
            .find(|item| item.get("content").is_some())
            .expect("search result block present");
        let content = search_result["content"].as_array().expect("content array");
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["title"], "Reqwest Docs");
        assert_eq!(content[0]["url"], "https://docs.rs/reqwest");
    }

    #[test]
    fn web_search_demo_provider_is_explicit_and_deterministic() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        std::env::set_var("AOSD_WEB_SEARCH_PROVIDER", "demo_search");
        std::env::remove_var("AOSD_DEMO_SEARCH_ENABLED");
        let disabled = execute_tool("WebSearch", &json!({ "query": "yesterday ROI drop" }))
            .expect_err("demo_search must be explicit opt-in");
        assert!(disabled.contains("demo_search provider is disabled"));

        std::env::set_var("AOSD_DEMO_SEARCH_ENABLED", "true");
        let result = execute_tool("WebSearch", &json!({ "query": "yesterday ROI drop" }))
            .expect("demo_search should return deterministic demo hits");
        std::env::remove_var("AOSD_WEB_SEARCH_PROVIDER");
        std::env::remove_var("AOSD_DEMO_SEARCH_ENABLED");

        let output: serde_json::Value = serde_json::from_str(&result).expect("valid json");
        let results = output["results"].as_array().expect("results array");
        let search_result = results
            .iter()
            .find(|item| item.get("content").is_some())
            .expect("search result block present");
        let content = search_result["content"].as_array().expect("content array");
        assert!(content
            .iter()
            .any(|item| item["url"] == "https://demo.aos.local/ops/roi-drop"));
    }

    #[test]
    fn web_search_retries_on_429_with_retry_after() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let attempt_counter = Arc::new(AtomicUsize::new(0));
        let counter = attempt_counter.clone();
        let server = TestServer::spawn(Arc::new(move |request_line: &str, _raw_request: &str| {
            assert!(request_line.contains("GET /res/v1/web/search?q=retry+429"));
            let idx = counter.fetch_add(1, Ordering::SeqCst);
            if idx == 0 {
                return HttpResponse::json(429, "Too Many Requests", r#"{"error":"rate limited"}"#)
                    .with_header("Retry-After", "0");
            }
            HttpResponse::json(
                200,
                "OK",
                r#"{
                  "web": {
                    "results": [
                      {"title": "Reqwest docs", "url": "https://docs.rs/reqwest"}
                    ]
                  }
                }"#,
            )
        }));

        std::env::set_var("AOSD_WEB_SEARCH_PROVIDER", "brave");
        std::env::set_var("AOSD_BRAVE_API_KEY", "test-key");
        std::env::set_var(
            "AOSD_BRAVE_BASE_URL",
            format!("http://{}/res/v1/web/search", server.addr()),
        );
        std::env::set_var("AOSD_WEB_SEARCH_MAX_RETRIES", "1");
        std::env::set_var("AOSD_WEB_SEARCH_RETRY_BACKOFF_MS", "0");
        std::env::set_var("AOSD_WEB_SEARCH_RETRY_JITTER_MS", "0");

        let result = execute_tool("WebSearch", &json!({ "query": "retry 429" }))
            .expect("WebSearch should retry once on 429 and then succeed");
        let output: serde_json::Value = serde_json::from_str(&result).expect("valid json");
        let results = output["results"].as_array().expect("results array");
        let search_result = results
            .iter()
            .find(|item| item.get("content").is_some())
            .expect("search result block present");
        let content = search_result["content"].as_array().expect("content array");
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["url"], "https://docs.rs/reqwest");
        assert_eq!(attempt_counter.load(Ordering::SeqCst), 2);

        std::env::remove_var("AOSD_WEB_SEARCH_PROVIDER");
        std::env::remove_var("AOSD_BRAVE_API_KEY");
        std::env::remove_var("AOSD_BRAVE_BASE_URL");
        std::env::remove_var("AOSD_WEB_SEARCH_MAX_RETRIES");
        std::env::remove_var("AOSD_WEB_SEARCH_RETRY_BACKOFF_MS");
        std::env::remove_var("AOSD_WEB_SEARCH_RETRY_JITTER_MS");
    }

    #[test]
    fn web_search_rotates_multi_keys_with_cooldown_after_429() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let server = TestServer::spawn(Arc::new(|request_line: &str, raw_request: &str| {
            assert!(request_line.contains("q=multi+key+rotate"));
            let lowered = raw_request.to_ascii_lowercase();
            if lowered.contains("x-subscription-token: bad-key") {
                return HttpResponse::json(429, "Too Many Requests", r#"{"error":"rate limited"}"#);
            }
            if !lowered.contains("x-subscription-token: good-key") {
                return HttpResponse::json(400, "Bad Request", r#"{"error":"unexpected key"}"#);
            }
            HttpResponse::json(
                200,
                "OK",
                r#"{
                  "web": {
                    "results": [
                      {"title": "Reqwest docs", "url": "https://docs.rs/reqwest"}
                    ]
                  }
                }"#,
            )
        }));

        std::env::set_var("AOSD_WEB_SEARCH_PROVIDER", "brave");
        std::env::set_var("AOSD_BRAVE_API_KEYS", "bad-key,good-key");
        std::env::set_var("AOSD_WEB_SEARCH_KEY_COOLDOWN_SECS", "120");
        std::env::set_var(
            "AOSD_BRAVE_BASE_URL",
            format!("http://{}/res/v1/web/search", server.addr()),
        );
        std::env::set_var("AOSD_WEB_SEARCH_MAX_RETRIES", "1");
        std::env::set_var("AOSD_WEB_SEARCH_RETRY_BACKOFF_MS", "0");
        std::env::set_var("AOSD_WEB_SEARCH_RETRY_JITTER_MS", "0");

        let result = execute_tool("WebSearch", &json!({ "query": "multi key rotate" }))
            .expect("WebSearch should rotate to next key after 429 on first key");
        let output: serde_json::Value = serde_json::from_str(&result).expect("valid json");
        let trace = output["providerTrace"]
            .as_array()
            .expect("provider trace array")
            .iter()
            .filter_map(|v| v.as_str())
            .collect::<Vec<_>>()
            .join(" | ");
        assert!(
            trace.contains("cooled_down_for=120s"),
            "provider trace should include key cooldown line: {trace}"
        );
        assert!(
            trace.contains("key=brave#ba..ey:len7") && trace.contains("key=brave#go..ey:len8"),
            "provider trace should include key identity tails: {trace}"
        );
        let results = output["results"].as_array().expect("results array");
        let search_result = results
            .iter()
            .find(|item| item.get("content").is_some())
            .expect("search result block present");
        let content = search_result["content"].as_array().expect("content array");
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["url"], "https://docs.rs/reqwest");

        std::env::remove_var("AOSD_WEB_SEARCH_PROVIDER");
        std::env::remove_var("AOSD_BRAVE_API_KEYS");
        std::env::remove_var("AOSD_WEB_SEARCH_KEY_COOLDOWN_SECS");
        std::env::remove_var("AOSD_BRAVE_BASE_URL");
        std::env::remove_var("AOSD_WEB_SEARCH_MAX_RETRIES");
        std::env::remove_var("AOSD_WEB_SEARCH_RETRY_BACKOFF_MS");
        std::env::remove_var("AOSD_WEB_SEARCH_RETRY_JITTER_MS");
    }

    #[test]
    fn web_search_does_not_retry_on_non_retryable_4xx() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let attempt_counter = Arc::new(AtomicUsize::new(0));
        let counter = attempt_counter.clone();
        let server = TestServer::spawn(Arc::new(move |request_line: &str, _raw_request: &str| {
            assert!(request_line.contains("GET /res/v1/web/search?q=bad+request"));
            counter.fetch_add(1, Ordering::SeqCst);
            HttpResponse::json(400, "Bad Request", r#"{"error":"bad request"}"#)
        }));

        std::env::set_var("AOSD_WEB_SEARCH_PROVIDER", "brave");
        std::env::set_var("AOSD_BRAVE_API_KEY", "test-key");
        std::env::set_var(
            "AOSD_BRAVE_BASE_URL",
            format!("http://{}/res/v1/web/search", server.addr()),
        );
        std::env::set_var("AOSD_WEB_SEARCH_MAX_RETRIES", "3");
        std::env::set_var("AOSD_WEB_SEARCH_RETRY_BACKOFF_MS", "0");
        std::env::set_var("AOSD_WEB_SEARCH_RETRY_JITTER_MS", "0");

        let error = execute_tool("WebSearch", &json!({ "query": "bad request" }))
            .expect_err("WebSearch should stop retrying on 400");
        assert!(error.contains("http_status=400"));
        assert_eq!(attempt_counter.load(Ordering::SeqCst), 1);

        std::env::remove_var("AOSD_WEB_SEARCH_PROVIDER");
        std::env::remove_var("AOSD_BRAVE_API_KEY");
        std::env::remove_var("AOSD_BRAVE_BASE_URL");
        std::env::remove_var("AOSD_WEB_SEARCH_MAX_RETRIES");
        std::env::remove_var("AOSD_WEB_SEARCH_RETRY_BACKOFF_MS");
        std::env::remove_var("AOSD_WEB_SEARCH_RETRY_JITTER_MS");
    }

    #[test]
    fn web_search_enrichment_expands_to_top7_when_initial_results_are_weak() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let page_counter = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&page_counter);
        let addr_slot = Arc::new(OnceLock::<SocketAddr>::new());
        let addr_slot_for_handler = Arc::clone(&addr_slot);
        let server = TestServer::spawn(Arc::new(move |request_line: &str, _raw_request: &str| {
            let addr = *addr_slot_for_handler.get().expect("address initialized");
            if request_line.contains("GET /res/v1/web/search?q=enrich+fallback") {
                let payload = format!(
                    r#"{{
                      "web": {{
                        "results": [
                          {{"title":"Weak 1","url":"http://{addr}/p1"}},
                          {{"title":"Weak 2","url":"http://{addr}/p2"}},
                          {{"title":"Strong 3","url":"http://{addr}/p3"}},
                          {{"title":"Strong 4","url":"http://{addr}/p4"}},
                          {{"title":"Strong 5","url":"http://{addr}/p5"}},
                          {{"title":"Strong 6","url":"http://{addr}/p6"}},
                          {{"title":"Strong 7","url":"http://{addr}/p7"}}
                        ]
                      }}
                    }}"#
                );
                return HttpResponse::json(200, "OK", &payload);
            }

            if request_line.starts_with("GET /p") {
                counter.fetch_add(1, Ordering::SeqCst);
                if request_line.starts_with("GET /p1 ") || request_line.starts_with("GET /p2 ") {
                    return HttpResponse::html(
                        200,
                        "OK",
                        "<html><body><article><p>too short</p></article></body></html>",
                    );
                }
                let long_text = "EWMA versus pLTV empirical analysis shows measurable revenue uplift across bidding, mediation, retention, and lifecycle cohorts. ";
                let body = format!(
                    "<html><body><article><h1>Evidence</h1><p>{}</p><p>{}</p></article></body></html>",
                    long_text.repeat(5),
                    long_text.repeat(5)
                );
                return HttpResponse::html(200, "OK", &body);
            }

            HttpResponse::text(404, "Not Found", "not found")
        }));
        addr_slot
            .set(server.addr())
            .expect("server address should initialize once");

        std::env::set_var("AOSD_WEB_SEARCH_PROVIDER", "brave");
        std::env::set_var("AOSD_BRAVE_API_KEY", "test-key");
        std::env::set_var(
            "AOSD_BRAVE_BASE_URL",
            format!("http://{}/res/v1/web/search", server.addr()),
        );
        std::env::set_var("AOSD_WEB_SEARCH_ENRICH_ENABLED", "1");
        std::env::set_var("AOSD_WEB_SEARCH_ENRICH_TARGET_VALID_PAGES", "6");
        std::env::set_var("AOSD_WEB_SEARCH_ENRICH_INITIAL_FETCH_CANDIDATES", "4");
        std::env::set_var("AOSD_WEB_SEARCH_ENRICH_MAX_FETCH_CANDIDATES", "7");
        std::env::set_var("AOSD_WEB_SEARCH_ENRICH_MIN_CHARS", "120");
        std::env::set_var("AOSD_WEB_SEARCH_ENRICH_MAX_CHARS", "1200");
        std::env::set_var("AOSD_WEB_SEARCH_ENRICH_FETCH_TIMEOUT_SECS", "2");
        std::env::set_var("AOSD_WEB_SEARCH_ENRICH_CONNECT_TIMEOUT_SECS", "1");

        let result = execute_tool("WebSearch", &json!({ "query": "enrich fallback" }))
            .expect("WebSearch should enrich top hits and auto-expand fetch window");
        let output: serde_json::Value = serde_json::from_str(&result).expect("valid json");
        let enrichment_trace = output["enrichmentTrace"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|value| value.as_str().map(str::to_string))
            .collect::<Vec<_>>();
        assert!(
            enrichment_trace
                .iter()
                .any(|line| line.contains("expanding to top-7")),
            "trace should record top-7 expansion: {enrichment_trace:?}"
        );
        let results = output["results"].as_array().expect("results array");
        let content = results
            .iter()
            .find(|item| item.get("content").is_some())
            .and_then(|item| item.get("content"))
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();
        let enriched_count = content
            .iter()
            .filter(|row| {
                row.get("content")
                    .and_then(serde_json::Value::as_str)
                    .is_some()
            })
            .count();
        assert!(
            enriched_count >= 3,
            "expected at least 3 enriched pages, got {enriched_count}"
        );
        let page_attempts = page_counter.load(Ordering::SeqCst);
        assert!(
            (5..=7).contains(&page_attempts),
            "fetch attempts should auto-expand into top-7 window, got {page_attempts}"
        );

        for key in [
            "AOSD_WEB_SEARCH_PROVIDER",
            "AOSD_BRAVE_API_KEY",
            "AOSD_BRAVE_BASE_URL",
            "AOSD_WEB_SEARCH_ENRICH_ENABLED",
            "AOSD_WEB_SEARCH_ENRICH_TARGET_VALID_PAGES",
            "AOSD_WEB_SEARCH_ENRICH_INITIAL_FETCH_CANDIDATES",
            "AOSD_WEB_SEARCH_ENRICH_MAX_FETCH_CANDIDATES",
            "AOSD_WEB_SEARCH_ENRICH_MIN_CHARS",
            "AOSD_WEB_SEARCH_ENRICH_MAX_CHARS",
            "AOSD_WEB_SEARCH_ENRICH_FETCH_TIMEOUT_SECS",
            "AOSD_WEB_SEARCH_ENRICH_CONNECT_TIMEOUT_SECS",
        ] {
            std::env::remove_var(key);
        }
    }

    #[test]
    fn web_search_ranking_applies_domain_diversity_penalty() {
        let cfg = super::WebSearchEnrichmentConfig {
            enabled: true,
            target_valid_pages: 2,
            initial_fetch_candidates: 2,
            max_fetch_candidates: 4,
            min_content_chars: 100,
            max_content_chars: 600,
            fetch_timeout_secs: 2,
            connect_timeout_secs: 1,
            fetch_retry_attempts: 0,
            fetch_retry_backoff_ms: 0,
            fetch_retry_jitter_ms: 0,
            jina_fallback_enabled: false,
            domain_repeat_penalty: 0.35,
        };
        let hits = vec![
            super::build_search_hit_from_parts(
                "https://same.example/a",
                "rust async runtime guide",
                Some("deep dive".to_string()),
            )
            .expect("valid hit"),
            super::build_search_hit_from_parts(
                "https://same.example/b",
                "rust async runtime benchmarks",
                Some("benchmarks".to_string()),
            )
            .expect("valid hit"),
            super::build_search_hit_from_parts(
                "https://docs.rs/tokio",
                "rust async runtime official docs",
                Some("official docs".to_string()),
            )
            .expect("valid hit"),
        ];
        let ranked = super::rank_web_search_candidates("rust async runtime", &hits, &cfg);
        assert_eq!(ranked.len(), 3);
        assert_ne!(ranked[0].domain_key, ranked[1].domain_key);
    }

    #[test]
    fn web_search_hit_builder_canonicalizes_tracking_and_fragment_urls() {
        let hit = super::build_search_hit_from_parts(
            "https://www.example.com/guides/rewarded/?utm_source=ads&fbclid=abc#child-3",
            "Rewarded guide",
            Some("snippet".to_string()),
        )
        .expect("hit should be accepted");
        assert_eq!(hit.url, "https://example.com/guides/rewarded");
        assert_eq!(hit.domain.as_deref(), Some("example.com"));
    }

    #[test]
    fn web_search_dedupe_uses_canonical_url_key() {
        let mut hits = vec![
            super::build_search_hit_from_parts(
                "https://example.com/article?utm_source=a#child-1",
                "A",
                None,
            )
            .expect("first hit"),
            super::build_search_hit_from_parts(
                "https://www.example.com/article/#child-2?utm_source=b",
                "B",
                None,
            )
            .expect("second hit"),
        ];
        let dropped = super::dedupe_hits_with_stats(&mut hits);
        assert_eq!(dropped, 1);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].url, "https://example.com/article");
    }

    #[test]
    fn web_search_quality_filter_drops_auth_and_low_signal_navigation_hits() {
        let hits = vec![
            super::build_search_hit_from_parts(
                "https://account.example.com/settings/profile",
                "Profile Settings",
                None,
            )
            .expect("auth hit"),
            super::build_search_hit_from_parts(
                "https://example.com/android",
                "Android Apps Directory",
                None,
            )
            .expect("directory hit"),
            super::build_search_hit_from_parts(
                "https://example.com/blog/rewarded-video-ads-optimization",
                "Rewarded Video Ads Optimization for Mobile Games",
                Some("Practical benchmarks and experiments for rewarded monetization.".to_string()),
            )
            .expect("article hit"),
        ];
        let (kept, stats) =
            super::filter_low_signal_search_hits("mobile game rewarded ads optimization", hits);
        assert_eq!(stats.dropped_auth, 1);
        assert_eq!(stats.dropped_navigation, 1);
        assert_eq!(kept.len(), 1);
        assert_eq!(
            kept[0].url,
            "https://example.com/blog/rewarded-video-ads-optimization"
        );
    }

    #[test]
    fn web_search_quality_filter_requires_distinctive_ascii_entity_anchor() {
        let hits = vec![
            super::build_search_hit_from_parts(
                "https://zh.wikipedia.org/wiki/unrelated",
                "无关的中文百科页面",
                Some("这个页面没有查询中的技术实体。".to_string()),
            )
            .expect("unrelated hit"),
            super::build_search_hit_from_parts(
                "https://tokio.rs/",
                "Tokio asynchronous runtime",
                Some("An asynchronous runtime for Rust.".to_string()),
            )
            .expect("relevant hit"),
        ];

        let (kept, stats) = super::filter_low_signal_search_hits("Tokio 是什么", hits);

        assert_eq!(stats.dropped_mismatch, 1);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].url, "https://tokio.rs/");
    }

    #[test]
    fn retry_after_header_supports_http_date_format() {
        let mut headers = reqwest::header::HeaderMap::new();
        let retry_at = std::time::SystemTime::now() + Duration::from_secs(5);
        let retry_after_value = httpdate::fmt_http_date(retry_at);
        headers.insert(
            reqwest::header::RETRY_AFTER,
            reqwest::header::HeaderValue::from_str(&retry_after_value).expect("valid header value"),
        );

        let parsed =
            super::parse_retry_after_header(&headers).expect("http-date Retry-After should parse");
        assert!(
            parsed <= Duration::from_secs(5),
            "parsed retry delay should be bounded by the requested date"
        );
    }

    #[test]
    fn pending_tools_preserve_multiple_streaming_tool_calls_by_index() {
        let mut events = Vec::new();
        let mut pending_tools = BTreeMap::new();

        push_output_block(
            OutputContentBlock::ToolUse {
                id: "tool-1".to_string(),
                name: "read_file".to_string(),
                input: json!({}),
            },
            1,
            &mut events,
            &mut pending_tools,
            true,
        );
        push_output_block(
            OutputContentBlock::ToolUse {
                id: "tool-2".to_string(),
                name: "grep_search".to_string(),
                input: json!({}),
            },
            2,
            &mut events,
            &mut pending_tools,
            true,
        );

        pending_tools
            .get_mut(&1)
            .expect("first tool pending")
            .2
            .push_str("{\"path\":\"src/main.rs\"}");
        pending_tools
            .get_mut(&2)
            .expect("second tool pending")
            .2
            .push_str("{\"pattern\":\"TODO\"}");

        assert_eq!(
            pending_tools.remove(&1),
            Some((
                "tool-1".to_string(),
                "read_file".to_string(),
                "{\"path\":\"src/main.rs\"}".to_string(),
            ))
        );
        assert_eq!(
            pending_tools.remove(&2),
            Some((
                "tool-2".to_string(),
                "grep_search".to_string(),
                "{\"pattern\":\"TODO\"}".to_string(),
            ))
        );
    }

    #[test]
    fn todo_write_persists_and_returns_previous_state() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let path = temp_path("todos.json");
        std::env::set_var("AOSD_TODO_STORE", &path);

        let first = execute_tool(
            "TodoWrite",
            &json!({
                "todos": [
                    {"content": "Add tool", "activeForm": "Adding tool", "status": "in_progress"},
                    {"content": "Run tests", "activeForm": "Running tests", "status": "pending"}
                ]
            }),
        )
        .expect("TodoWrite should succeed");
        let first_output: serde_json::Value = serde_json::from_str(&first).expect("valid json");
        assert_eq!(first_output["oldTodos"].as_array().expect("array").len(), 0);

        let second = execute_tool(
            "TodoWrite",
            &json!({
                "todos": [
                    {"content": "Add tool", "activeForm": "Adding tool", "status": "completed"},
                    {"content": "Run tests", "activeForm": "Running tests", "status": "completed"},
                    {"content": "Verify", "activeForm": "Verifying", "status": "completed"}
                ]
            }),
        )
        .expect("TodoWrite should succeed");
        std::env::remove_var("AOSD_TODO_STORE");
        let _ = std::fs::remove_file(path);

        let second_output: serde_json::Value = serde_json::from_str(&second).expect("valid json");
        assert_eq!(
            second_output["oldTodos"].as_array().expect("array").len(),
            2
        );
        assert_eq!(
            second_output["newTodos"].as_array().expect("array").len(),
            3
        );
        assert!(second_output["verificationNudgeNeeded"].is_null());
    }

    #[test]
    fn todo_write_rejects_invalid_payloads_and_sets_verification_nudge() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let path = temp_path("todos-errors.json");
        std::env::set_var("AOSD_TODO_STORE", &path);

        let empty = execute_tool("TodoWrite", &json!({ "todos": [] }))
            .expect_err("empty todos should fail");
        assert!(empty.contains("todos must not be empty"));

        // Multiple in_progress items are now allowed for parallel workflows
        let _multi_active = execute_tool(
            "TodoWrite",
            &json!({
                "todos": [
                    {"content": "One", "activeForm": "Doing one", "status": "in_progress"},
                    {"content": "Two", "activeForm": "Doing two", "status": "in_progress"}
                ]
            }),
        )
        .expect("multiple in-progress todos should succeed");

        let blank_content = execute_tool(
            "TodoWrite",
            &json!({
                "todos": [
                    {"content": "   ", "activeForm": "Doing it", "status": "pending"}
                ]
            }),
        )
        .expect_err("blank content should fail");
        assert!(blank_content.contains("todo content must not be empty"));

        let nudge = execute_tool(
            "TodoWrite",
            &json!({
                "todos": [
                    {"content": "Write tests", "activeForm": "Writing tests", "status": "completed"},
                    {"content": "Fix errors", "activeForm": "Fixing errors", "status": "completed"},
                    {"content": "Ship branch", "activeForm": "Shipping branch", "status": "completed"}
                ]
            }),
        )
        .expect("completed todos should succeed");
        std::env::remove_var("AOSD_TODO_STORE");
        let _ = fs::remove_file(path);

        let output: serde_json::Value = serde_json::from_str(&nudge).expect("valid json");
        assert_eq!(output["verificationNudgeNeeded"], true);
    }

    #[test]
    fn skill_loads_local_skill_prompt() {
        let _guard = env_guard();
        let home = temp_path("skills-home");
        let skill_dir = home.join(".agents").join("skills").join("help");
        fs::create_dir_all(&skill_dir).expect("skill dir should exist");
        fs::write(
            skill_dir.join("SKILL.md"),
            "# help\n\nGuide on using oh-my-codex plugin\n",
        )
        .expect("skill file should exist");
        let original_home = std::env::var("HOME").ok();
        std::env::set_var("HOME", &home);

        let result = execute_tool(
            "Skill",
            &json!({
                "skill": "help",
                "args": "overview"
            }),
        )
        .expect("Skill should succeed");

        let output: serde_json::Value = serde_json::from_str(&result).expect("valid json");
        assert_eq!(output["skill"], "help");
        assert!(output["path"]
            .as_str()
            .expect("path")
            .ends_with("/help/SKILL.md"));
        assert!(output["prompt"]
            .as_str()
            .expect("prompt")
            .contains("Guide on using oh-my-codex plugin"));

        let dollar_result = execute_tool(
            "Skill",
            &json!({
                "skill": "$help"
            }),
        )
        .expect("Skill should accept $skill invocation form");
        let dollar_output: serde_json::Value =
            serde_json::from_str(&dollar_result).expect("valid json");
        assert_eq!(dollar_output["skill"], "$help");
        assert!(dollar_output["path"]
            .as_str()
            .expect("path")
            .ends_with("/help/SKILL.md"));

        if let Some(home) = original_home {
            std::env::set_var("HOME", home);
        } else {
            std::env::remove_var("HOME");
        }
        fs::remove_dir_all(home).expect("temp home should clean up");
    }

    #[test]
    fn skill_resolves_project_local_skills_and_legacy_commands() {
        let _guard = env_guard();
        let root = temp_path("project-skills");
        let skill_dir = root.join(".aos").join("skills").join("plan");
        let command_dir = root.join(".aos").join("commands");
        fs::create_dir_all(&skill_dir).expect("skill dir should exist");
        fs::create_dir_all(&command_dir).expect("command dir should exist");
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: plan\ndescription: Project planning guidance\n---\n\n# plan\n",
        )
        .expect("skill file should exist");
        fs::write(
            command_dir.join("handoff.md"),
            "---\nname: handoff\ndescription: Legacy handoff guidance\n---\n\n# handoff\n",
        )
        .expect("command file should exist");

        let original_dir = std::env::current_dir().expect("cwd");
        std::env::set_current_dir(&root).expect("set cwd");

        let skill_result = execute_tool("Skill", &json!({ "skill": "$plan" }))
            .expect("project-local skill should resolve");
        let skill_output: serde_json::Value =
            serde_json::from_str(&skill_result).expect("valid json");
        assert!(skill_output["path"]
            .as_str()
            .expect("path")
            .ends_with(".aos/skills/plan/SKILL.md"));

        let command_result = execute_tool("Skill", &json!({ "skill": "/handoff" }))
            .expect("legacy command should resolve");
        let command_output: serde_json::Value =
            serde_json::from_str(&command_result).expect("valid json");
        assert!(command_output["path"]
            .as_str()
            .expect("path")
            .ends_with(".aos/commands/handoff.md"));

        std::env::set_current_dir(&original_dir).expect("restore cwd");
        fs::remove_dir_all(root).expect("temp project should clean up");
    }

    #[test]
    fn skill_loads_project_local_claude_skill_prompt() {
        let _guard = env_guard();
        let root = temp_path("project-skills");
        let home = root.join("home");
        let workspace = root.join("workspace");
        let nested = workspace.join("nested");
        let skill_dir = workspace.join(".claude").join("skills").join("trace");
        fs::create_dir_all(&skill_dir).expect("skill dir should exist");
        fs::create_dir_all(&nested).expect("nested cwd should exist");
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: trace\ndescription: Project-local trace helper\n---\n# trace\n",
        )
        .expect("skill file should exist");

        let original_home = std::env::var("HOME").ok();
        let original_config_home = std::env::var("AOS_CONFIG_HOME").ok();
        let original_codex_home = std::env::var("CODEX_HOME").ok();
        let original_dir = std::env::current_dir().expect("cwd");
        std::env::set_var("HOME", &home);
        std::env::remove_var("AOS_CONFIG_HOME");
        std::env::remove_var("CODEX_HOME");
        std::env::set_current_dir(&nested).expect("set cwd");

        let result = execute_tool("Skill", &json!({ "skill": "trace" }))
            .expect("project-local skill should resolve");

        let output: serde_json::Value = serde_json::from_str(&result).expect("valid json");
        assert!(output["path"]
            .as_str()
            .expect("path")
            .ends_with(".claude/skills/trace/SKILL.md"));
        assert_eq!(output["description"], "Project-local trace helper");

        std::env::set_current_dir(&original_dir).expect("restore cwd");
        match original_home {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
        match original_config_home {
            Some(value) => std::env::set_var("AOS_CONFIG_HOME", value),
            None => std::env::remove_var("AOS_CONFIG_HOME"),
        }
        match original_codex_home {
            Some(value) => std::env::set_var("CODEX_HOME", value),
            None => std::env::remove_var("CODEX_HOME"),
        }
        fs::remove_dir_all(root).expect("temp tree should clean up");
    }

    #[test]
    fn skill_loads_project_local_omc_and_agents_skill_prompts() {
        let _guard = env_guard();
        let root = temp_path("project-omc-skills");
        let home = root.join("home");
        let workspace = root.join("workspace");
        let nested = workspace.join("nested");
        let omc_skill_dir = workspace.join(".omc").join("skills").join("hud");
        let agents_skill_dir = workspace.join(".agents").join("skills").join("trace");
        fs::create_dir_all(&omc_skill_dir).expect("omc skill dir should exist");
        fs::create_dir_all(&agents_skill_dir).expect("agents skill dir should exist");
        fs::create_dir_all(&nested).expect("nested cwd should exist");
        fs::write(
            omc_skill_dir.join("SKILL.md"),
            "---\nname: hud\ndescription: Project-local OMC HUD helper\n---\n# hud\n",
        )
        .expect("omc skill file should exist");
        fs::write(
            agents_skill_dir.join("SKILL.md"),
            "---\nname: trace\ndescription: Project-local agents compatibility helper\n---\n# trace\n",
        )
        .expect("agents skill file should exist");

        let original_home = std::env::var("HOME").ok();
        let original_config_home = std::env::var("AOS_CONFIG_HOME").ok();
        let original_codex_home = std::env::var("CODEX_HOME").ok();
        let original_dir = std::env::current_dir().expect("cwd");
        std::env::set_var("HOME", &home);
        std::env::remove_var("AOS_CONFIG_HOME");
        std::env::remove_var("CODEX_HOME");
        std::env::set_current_dir(&nested).expect("set cwd");

        let omc_result =
            execute_tool("Skill", &json!({ "skill": "hud" })).expect("omc skill should resolve");
        let agents_result = execute_tool("Skill", &json!({ "skill": "trace" }))
            .expect("agents skill should resolve");

        let omc_output: serde_json::Value = serde_json::from_str(&omc_result).expect("valid json");
        let agents_output: serde_json::Value =
            serde_json::from_str(&agents_result).expect("valid json");
        assert!(omc_output["path"]
            .as_str()
            .expect("path")
            .ends_with(".omc/skills/hud/SKILL.md"));
        assert_eq!(omc_output["description"], "Project-local OMC HUD helper");
        assert!(agents_output["path"]
            .as_str()
            .expect("path")
            .ends_with(".agents/skills/trace/SKILL.md"));
        assert_eq!(
            agents_output["description"],
            "Project-local agents compatibility helper"
        );

        std::env::set_current_dir(&original_dir).expect("restore cwd");
        match original_home {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
        match original_config_home {
            Some(value) => std::env::set_var("AOS_CONFIG_HOME", value),
            None => std::env::remove_var("AOS_CONFIG_HOME"),
        }
        match original_codex_home {
            Some(value) => std::env::set_var("CODEX_HOME", value),
            None => std::env::remove_var("CODEX_HOME"),
        }
        fs::remove_dir_all(root).expect("temp tree should clean up");
    }

    #[test]
    fn skill_loads_learned_skill_from_claude_config_dir() {
        let _guard = env_guard();
        let root = temp_path("claude-config-learned-skill");
        let home = root.join("home");
        let claude_config_dir = root.join("claude-config");
        let learned_skill_dir = claude_config_dir
            .join("skills")
            .join("omc-learned")
            .join("learned");
        fs::create_dir_all(&learned_skill_dir).expect("learned skill dir should exist");
        fs::write(
            learned_skill_dir.join("SKILL.md"),
            "---\nname: learned\ndescription: Learned OMC skill\n---\n# learned\n",
        )
        .expect("learned skill file should exist");

        let original_home = std::env::var("HOME").ok();
        let original_config_home = std::env::var("AOS_CONFIG_HOME").ok();
        let original_codex_home = std::env::var("CODEX_HOME").ok();
        let original_claude_config_dir = std::env::var("CLAUDE_CONFIG_DIR").ok();
        std::env::set_var("HOME", &home);
        std::env::remove_var("AOS_CONFIG_HOME");
        std::env::remove_var("CODEX_HOME");
        std::env::set_var("CLAUDE_CONFIG_DIR", &claude_config_dir);

        let result = execute_tool("Skill", &json!({ "skill": "learned" }))
            .expect("learned skill should resolve");

        let output: serde_json::Value = serde_json::from_str(&result).expect("valid json");
        assert!(output["path"]
            .as_str()
            .expect("path")
            .ends_with("skills/omc-learned/learned/SKILL.md"));
        assert_eq!(output["description"], "Learned OMC skill");

        match original_home {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
        match original_config_home {
            Some(value) => std::env::set_var("AOS_CONFIG_HOME", value),
            None => std::env::remove_var("AOS_CONFIG_HOME"),
        }
        match original_codex_home {
            Some(value) => std::env::set_var("CODEX_HOME", value),
            None => std::env::remove_var("CODEX_HOME"),
        }
        match original_claude_config_dir {
            Some(value) => std::env::set_var("CLAUDE_CONFIG_DIR", value),
            None => std::env::remove_var("CLAUDE_CONFIG_DIR"),
        }
        fs::remove_dir_all(root).expect("temp tree should clean up");
    }

    #[test]
    fn skill_loads_direct_skill_and_legacy_command_from_claude_config_dir() {
        let _guard = env_guard();
        let root = temp_path("claude-config-direct-skill");
        let home = root.join("home");
        let claude_config_dir = root.join("claude-config");
        let skill_dir = claude_config_dir.join("skills").join("statusline");
        let command_dir = claude_config_dir.join("commands");
        fs::create_dir_all(&skill_dir).expect("direct skill dir should exist");
        fs::create_dir_all(&command_dir).expect("command dir should exist");
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: statusline\ndescription: Claude config skill\n---\n# statusline\n",
        )
        .expect("direct skill file should exist");
        fs::write(
            command_dir.join("doctor-check.md"),
            "---\nname: doctor-check\ndescription: Claude config command\n---\n# doctor-check\n",
        )
        .expect("direct command file should exist");

        let original_home = std::env::var("HOME").ok();
        let original_config_home = std::env::var("AOS_CONFIG_HOME").ok();
        let original_codex_home = std::env::var("CODEX_HOME").ok();
        let original_claude_config_dir = std::env::var("CLAUDE_CONFIG_DIR").ok();
        std::env::set_var("HOME", &home);
        std::env::remove_var("AOS_CONFIG_HOME");
        std::env::remove_var("CODEX_HOME");
        std::env::set_var("CLAUDE_CONFIG_DIR", &claude_config_dir);

        let direct_skill =
            execute_tool("Skill", &json!({ "skill": "statusline" })).expect("direct skill");
        let direct_skill_output: serde_json::Value =
            serde_json::from_str(&direct_skill).expect("valid skill json");
        assert!(direct_skill_output["path"]
            .as_str()
            .expect("path")
            .ends_with("skills/statusline/SKILL.md"));
        assert_eq!(direct_skill_output["description"], "Claude config skill");

        let legacy_command =
            execute_tool("Skill", &json!({ "skill": "doctor-check" })).expect("direct command");
        let legacy_command_output: serde_json::Value =
            serde_json::from_str(&legacy_command).expect("valid command json");
        assert!(legacy_command_output["path"]
            .as_str()
            .expect("path")
            .ends_with("commands/doctor-check.md"));
        assert_eq!(
            legacy_command_output["description"],
            "Claude config command"
        );

        match original_home {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
        match original_config_home {
            Some(value) => std::env::set_var("AOS_CONFIG_HOME", value),
            None => std::env::remove_var("AOS_CONFIG_HOME"),
        }
        match original_codex_home {
            Some(value) => std::env::set_var("CODEX_HOME", value),
            None => std::env::remove_var("CODEX_HOME"),
        }
        match original_claude_config_dir {
            Some(value) => std::env::set_var("CLAUDE_CONFIG_DIR", value),
            None => std::env::remove_var("CLAUDE_CONFIG_DIR"),
        }
        fs::remove_dir_all(root).expect("temp tree should clean up");
    }

    #[test]
    fn skill_loads_project_local_legacy_command_markdown() {
        let _guard = env_guard();
        let root = temp_path("project-legacy-command");
        let home = root.join("home");
        let workspace = root.join("workspace");
        let nested = workspace.join("nested");
        let command_dir = workspace.join(".claude").join("commands");
        fs::create_dir_all(&command_dir).expect("legacy command dir should exist");
        fs::create_dir_all(&nested).expect("nested cwd should exist");
        fs::write(
            command_dir.join("team.md"),
            "---\nname: team\ndescription: Legacy team workflow\n---\n# team\n",
        )
        .expect("legacy command file should exist");

        let original_home = std::env::var("HOME").ok();
        let original_config_home = std::env::var("AOS_CONFIG_HOME").ok();
        let original_codex_home = std::env::var("CODEX_HOME").ok();
        let original_dir = std::env::current_dir().expect("cwd");
        std::env::set_var("HOME", &home);
        std::env::remove_var("AOS_CONFIG_HOME");
        std::env::remove_var("CODEX_HOME");
        std::env::set_current_dir(&nested).expect("set cwd");

        let result = execute_tool("Skill", &json!({ "skill": "team" }))
            .expect("legacy command markdown should resolve");

        let output: serde_json::Value = serde_json::from_str(&result).expect("valid json");
        assert!(output["path"]
            .as_str()
            .expect("path")
            .ends_with(".claude/commands/team.md"));
        assert_eq!(output["description"], "Legacy team workflow");

        std::env::set_current_dir(&original_dir).expect("restore cwd");
        match original_home {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
        match original_config_home {
            Some(value) => std::env::set_var("AOS_CONFIG_HOME", value),
            None => std::env::remove_var("AOS_CONFIG_HOME"),
        }
        match original_codex_home {
            Some(value) => std::env::set_var("CODEX_HOME", value),
            None => std::env::remove_var("CODEX_HOME"),
        }
        fs::remove_dir_all(root).expect("temp tree should clean up");
    }

    #[test]
    fn tool_search_supports_keyword_and_select_queries() {
        let keyword = execute_tool(
            "ToolSearch",
            &json!({"query": "web current", "max_results": 3}),
        )
        .expect("ToolSearch should succeed");
        let keyword_output: serde_json::Value = serde_json::from_str(&keyword).expect("valid json");
        let matches = keyword_output["matches"].as_array().expect("matches");
        assert!(matches.iter().any(|value| value == "WebSearch"));

        let selected = execute_tool("ToolSearch", &json!({"query": "select:Agent,Skill"}))
            .expect("ToolSearch should succeed");
        let selected_output: serde_json::Value =
            serde_json::from_str(&selected).expect("valid json");
        assert_eq!(selected_output["matches"][0], "Agent");
        assert_eq!(selected_output["matches"][1], "Skill");

        let aliased = execute_tool("ToolSearch", &json!({"query": "AgentTool"}))
            .expect("ToolSearch should support tool aliases");
        let aliased_output: serde_json::Value = serde_json::from_str(&aliased).expect("valid json");
        assert_eq!(aliased_output["matches"][0], "Agent");
        assert_eq!(aliased_output["normalized_query"], "agent");

        let selected_with_alias =
            execute_tool("ToolSearch", &json!({"query": "select:AgentTool,Skill"}))
                .expect("ToolSearch alias select should succeed");
        let selected_with_alias_output: serde_json::Value =
            serde_json::from_str(&selected_with_alias).expect("valid json");
        assert_eq!(selected_with_alias_output["matches"][0], "Agent");
        assert_eq!(selected_with_alias_output["matches"][1], "Skill");
    }

    #[test]
    fn agent_persists_handoff_metadata() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = temp_path("agent-store");
        std::env::set_var("AOSD_AGENT_STORE", &dir);
        let captured = Arc::new(Mutex::new(None::<AgentJob>));
        let captured_for_spawn = Arc::clone(&captured);

        let manifest = execute_agent_with_spawn(
            AgentInput {
                description: "Audit the branch".to_string(),
                prompt: "Check tests and outstanding work.".to_string(),
                subagent_type: Some("Explore".to_string()),
                name: Some("ship-audit".to_string()),
                model: None,
            },
            move |job| {
                *captured_for_spawn
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(job);
                Ok(())
            },
        )
        .expect("Agent should succeed");
        std::env::remove_var("AOSD_AGENT_STORE");

        assert_eq!(manifest.name, "ship-audit");
        assert_eq!(manifest.subagent_type.as_deref(), Some("Explore"));
        assert_eq!(manifest.status, "running");
        assert!(!manifest.created_at.is_empty());
        assert!(manifest.started_at.is_some());
        assert!(manifest.completed_at.is_none());
        let contents = std::fs::read_to_string(&manifest.output_file).expect("agent file exists");
        let manifest_contents =
            std::fs::read_to_string(&manifest.manifest_file).expect("manifest file exists");
        let manifest_json: serde_json::Value =
            serde_json::from_str(&manifest_contents).expect("manifest should be valid json");
        assert!(contents.contains("Audit the branch"));
        assert!(contents.contains("Check tests and outstanding work."));
        assert!(manifest_contents.contains("\"subagentType\": \"Explore\""));
        assert!(manifest_contents.contains("\"status\": \"running\""));
        assert_eq!(manifest_json["laneEvents"][0]["event"], "lane.started");
        assert_eq!(manifest_json["laneEvents"][0]["status"], "running");
        assert!(manifest_json["currentBlocker"].is_null());
        let captured_job = captured
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .expect("spawn job should be captured");
        assert_eq!(captured_job.prompt, "Check tests and outstanding work.");
        assert!(captured_job.allowed_tools.contains("read_file"));
        assert!(!captured_job.allowed_tools.contains("agent"));

        let normalized = execute_tool(
            "Agent",
            &json!({
                "description": "Verify the branch",
                "prompt": "Check tests.",
                "subagent_type": "explorer"
            }),
        )
        .expect("Agent should normalize built-in aliases");
        let normalized_output: serde_json::Value =
            serde_json::from_str(&normalized).expect("valid json");
        assert_eq!(normalized_output["subagentType"], "Explore");

        let named = execute_tool(
            "Agent",
            &json!({
                "description": "Review the branch",
                "prompt": "Inspect diff.",
                "name": "Ship Audit!!!"
            }),
        )
        .expect("Agent should normalize explicit names");
        let named_output: serde_json::Value = serde_json::from_str(&named).expect("valid json");
        assert_eq!(named_output["name"], "ship-audit");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn agent_fake_runner_can_persist_completion_and_failure() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = temp_path("agent-runner");
        std::env::set_var("AOSD_AGENT_STORE", &dir);

        let completed = execute_agent_with_spawn(
            AgentInput {
                description: "Complete the task".to_string(),
                prompt: "Do the work".to_string(),
                subagent_type: Some("Explore".to_string()),
                name: Some("complete-task".to_string()),
                model: Some("claude-sonnet-4-6".to_string()),
            },
            |job| {
                persist_agent_terminal_state(
                    &job.manifest,
                    "completed",
                    Some("Finished successfully in commit abc1234"),
                    None,
                )
            },
        )
        .expect("completed agent should succeed");

        let completed_manifest = std::fs::read_to_string(&completed.manifest_file)
            .expect("completed manifest should exist");
        let completed_manifest_json: serde_json::Value =
            serde_json::from_str(&completed_manifest).expect("completed manifest json");
        let completed_output =
            std::fs::read_to_string(&completed.output_file).expect("completed output should exist");
        assert!(completed_manifest.contains("\"status\": \"completed\""));
        assert!(completed_output.contains("Finished successfully"));
        assert_eq!(
            completed_manifest_json["laneEvents"][0]["event"],
            "lane.started"
        );
        assert_eq!(
            completed_manifest_json["laneEvents"][1]["event"],
            "lane.finished"
        );
        assert_eq!(
            completed_manifest_json["laneEvents"][1]["data"]["qualityFloorApplied"],
            false
        );
        assert_eq!(
            completed_manifest_json["laneEvents"][1]["detail"],
            "Finished successfully in commit abc1234"
        );
        assert_eq!(
            completed_manifest_json["laneEvents"][2]["event"],
            "lane.commit.created"
        );
        assert_eq!(
            completed_manifest_json["laneEvents"][2]["data"]["commit"],
            "abc1234"
        );
        assert!(completed_manifest_json["currentBlocker"].is_null());
        assert_eq!(
            completed_manifest_json["derivedState"],
            "finished_cleanable"
        );

        let failed = execute_agent_with_spawn(
            AgentInput {
                description: "Fail the task".to_string(),
                prompt: "Do the failing work".to_string(),
                subagent_type: Some("Verification".to_string()),
                name: Some("fail-task".to_string()),
                model: None,
            },
            |job| {
                persist_agent_terminal_state(
                    &job.manifest,
                    "failed",
                    None,
                    Some(String::from("tool failed: simulated failure")),
                )
            },
        )
        .expect("failed agent should still spawn");

        let failed_manifest =
            std::fs::read_to_string(&failed.manifest_file).expect("failed manifest should exist");
        let failed_manifest_json: serde_json::Value =
            serde_json::from_str(&failed_manifest).expect("failed manifest json");
        let failed_output =
            std::fs::read_to_string(&failed.output_file).expect("failed output should exist");
        assert!(failed_manifest.contains("\"status\": \"failed\""));
        assert!(failed_manifest.contains("simulated failure"));
        assert!(failed_output.contains("simulated failure"));
        assert!(failed_output.contains("failure_class: tool_runtime"));
        assert_eq!(
            failed_manifest_json["currentBlocker"]["failureClass"],
            "tool_runtime"
        );
        assert_eq!(
            failed_manifest_json["laneEvents"][1]["event"],
            "lane.blocked"
        );
        assert_eq!(
            failed_manifest_json["laneEvents"][2]["event"],
            "lane.failed"
        );
        assert_eq!(
            failed_manifest_json["laneEvents"][2]["failureClass"],
            "tool_runtime"
        );
        assert_eq!(failed_manifest_json["derivedState"], "truly_idle");

        let normalized = execute_agent_with_spawn(
            AgentInput {
                description: "Sweep the next backlog item".to_string(),
                prompt: "Produce a low-signal stop summary".to_string(),
                subagent_type: Some("Explore".to_string()),
                name: Some("summary-floor".to_string()),
                model: None,
            },
            |job| {
                persist_agent_terminal_state(
                    &job.manifest,
                    "completed",
                    Some("commit push everyting, keep sweeping $ralph"),
                    None,
                )
            },
        )
        .expect("normalized agent should succeed");

        let normalized_manifest = std::fs::read_to_string(&normalized.manifest_file)
            .expect("normalized manifest should exist");
        let normalized_manifest_json: serde_json::Value =
            serde_json::from_str(&normalized_manifest).expect("normalized manifest json");
        assert_eq!(
            normalized_manifest_json["laneEvents"][1]["event"],
            "lane.finished"
        );
        let normalized_detail = normalized_manifest_json["laneEvents"][1]["detail"]
            .as_str()
            .expect("normalized detail");
        assert!(normalized_detail.contains("Completed lane `summary-floor`"));
        assert!(normalized_detail.contains("Sweep the next backlog item"));
        assert_eq!(
            normalized_manifest_json["laneEvents"][1]["data"]["qualityFloorApplied"],
            true
        );
        assert_eq!(
            normalized_manifest_json["laneEvents"][1]["data"]["rawSummary"],
            "commit push everyting, keep sweeping $ralph"
        );
        assert_eq!(
            normalized_manifest_json["laneEvents"][1]["data"]["reasons"][0],
            "control_only"
        );

        let recovery = execute_agent_with_spawn(
            AgentInput {
                description: "Recover the stalled audit lane".to_string(),
                prompt: "Normalize OMX reinjection control prose".to_string(),
                subagent_type: Some("Explore".to_string()),
                name: Some("recovery-lane".to_string()),
                model: None,
            },
            |job| {
                persist_agent_terminal_state(
                    &job.manifest,
                    "completed",
                    Some(
                        "Team read-only-audit-only-for-roadm: worker panes stalled, no progress 2m30s. Next: omx team status read-only-audit-only-for-roadm; read worker messages; unblock/reassign or shutdown. [OMX_TMUX_INJECT]",
                    ),
                    None,
                )
            },
        )
        .expect("recovery agent should succeed");

        let recovery_manifest = std::fs::read_to_string(&recovery.manifest_file)
            .expect("recovery manifest should exist");
        let recovery_manifest_json: serde_json::Value =
            serde_json::from_str(&recovery_manifest).expect("recovery manifest json");
        let recovery_detail = recovery_manifest_json["laneEvents"][1]["detail"]
            .as_str()
            .expect("recovery detail");
        assert!(recovery_detail.contains("Recovery handoff observed via tmux reinjection"));
        assert!(recovery_detail.contains("read-only-audit-only-for-roadm"));
        assert!(!recovery_detail.contains("OMX_TMUX_INJECT"));
        assert_eq!(
            recovery_manifest_json["laneEvents"][1]["data"]["recoveryOutcome"]["cause"],
            "tmux_reinject_after_idle"
        );
        assert_eq!(
            recovery_manifest_json["laneEvents"][1]["data"]["recoveryOutcome"]["targetLane"],
            "read-only-audit-only-for-roadm"
        );
        assert_eq!(
            recovery_manifest_json["laneEvents"][1]["data"]["qualityFloorApplied"],
            true
        );
        assert_eq!(
            recovery_manifest_json["laneEvents"][1]["data"]["reasons"][0],
            "recovery_control_prose"
        );

        let review = execute_agent_with_spawn(
            AgentInput {
                description: "Review commit 1234abcd for ROADMAP #67".to_string(),
                prompt: "Review the scoped diff".to_string(),
                subagent_type: Some("Verification".to_string()),
                name: Some("review-lane".to_string()),
                model: None,
            },
            |job| {
                persist_agent_terminal_state(
                    &job.manifest,
                    "completed",
                    Some("APPROVE\n\nTarget: commit 1234abcd\nRationale: scoped diff is safe."),
                    None,
                )
            },
        )
        .expect("review agent should succeed");

        let review_manifest =
            std::fs::read_to_string(&review.manifest_file).expect("review manifest should exist");
        let review_manifest_json: serde_json::Value =
            serde_json::from_str(&review_manifest).expect("review manifest json");
        assert_eq!(
            review_manifest_json["laneEvents"][1]["data"]["reviewVerdict"],
            "approve"
        );
        assert_eq!(
            review_manifest_json["laneEvents"][1]["data"]["reviewTarget"],
            "Review commit 1234abcd for ROADMAP #67"
        );
        assert_eq!(
            review_manifest_json["laneEvents"][1]["data"]["reviewRationale"],
            "Target: commit 1234abcd Rationale: scoped diff is safe."
        );
        assert_eq!(
            review_manifest_json["laneEvents"][1]["data"]["qualityFloorApplied"],
            false
        );

        let selection = execute_agent_with_spawn(
            AgentInput {
                description: "Scan ROADMAP Immediate Backlog for the next repo-local item".to_string(),
                prompt: "Choose the next backlog target".to_string(),
                subagent_type: Some("Explore".to_string()),
                name: Some("backlog-scan".to_string()),
                model: None,
            },
            |job| {
                persist_agent_terminal_state(
                    &job.manifest,
                    "completed",
                    Some(
                        "Selected next backlog target.\nChosen: ROADMAP #65\nSkipped: ROADMAP #63, ROADMAP #64\nAction: execute\nRationale: #65 is the next repo-local lane-finished metadata task.",
                    ),
                    None,
                )
            },
        )
        .expect("selection agent should succeed");

        let selection_manifest = std::fs::read_to_string(&selection.manifest_file)
            .expect("selection manifest should exist");
        let selection_manifest_json: serde_json::Value =
            serde_json::from_str(&selection_manifest).expect("selection manifest json");
        assert_eq!(
            selection_manifest_json["laneEvents"][1]["data"]["selectionOutcome"]["chosenItems"][0],
            "ROADMAP #65"
        );
        assert_eq!(
            selection_manifest_json["laneEvents"][1]["data"]["selectionOutcome"]["skippedItems"][0],
            "ROADMAP #63"
        );
        assert_eq!(
            selection_manifest_json["laneEvents"][1]["data"]["selectionOutcome"]["skippedItems"][1],
            "ROADMAP #64"
        );
        assert_eq!(
            selection_manifest_json["laneEvents"][1]["data"]["selectionOutcome"]["action"],
            "execute"
        );
        assert_eq!(
            selection_manifest_json["laneEvents"][1]["data"]["selectionOutcome"]["rationale"],
            "#65 is the next repo-local lane-finished metadata task."
        );

        let artifact = execute_agent_with_spawn(
            AgentInput {
                description: "Land ROADMAP #64 provenance hardening".to_string(),
                prompt: "Ship structured artifact provenance".to_string(),
                subagent_type: Some("Explore".to_string()),
                name: Some("artifact-lane".to_string()),
                model: None,
            },
            |job| {
                persist_agent_terminal_state(
                    &job.manifest,
                    "completed",
                    Some(
                        "Completed ROADMAP #64. Files: rust/crates/tools/src/lib.rs PLAN.md. Diff stat: 2 files, +12/-1. Tested, committed, pushed as commit deadbee.",
                    ),
                    None,
                )
            },
        )
        .expect("artifact agent should succeed");

        let artifact_manifest = std::fs::read_to_string(&artifact.manifest_file)
            .expect("artifact manifest should exist");
        let artifact_manifest_json: serde_json::Value =
            serde_json::from_str(&artifact_manifest).expect("artifact manifest json");
        assert_eq!(
            artifact_manifest_json["laneEvents"][1]["data"]["artifactProvenance"]["sourceLanes"][0],
            "artifact-lane"
        );
        assert_eq!(
            artifact_manifest_json["laneEvents"][1]["data"]["artifactProvenance"]["roadmapIds"][0],
            "ROADMAP #64"
        );
        assert_eq!(
            artifact_manifest_json["laneEvents"][1]["data"]["artifactProvenance"]["files"][0],
            "PLAN.md"
        );
        assert_eq!(
            artifact_manifest_json["laneEvents"][1]["data"]["artifactProvenance"]["files"][1],
            "rust/crates/tools/src/lib.rs"
        );
        assert_eq!(
            artifact_manifest_json["laneEvents"][1]["data"]["artifactProvenance"]["diffStat"],
            "2 files, +12/-1."
        );
        assert_eq!(
            artifact_manifest_json["laneEvents"][1]["data"]["artifactProvenance"]["verification"]
                [0],
            "tested"
        );
        assert_eq!(
            artifact_manifest_json["laneEvents"][1]["data"]["artifactProvenance"]["verification"]
                [1],
            "committed"
        );
        assert_eq!(
            artifact_manifest_json["laneEvents"][1]["data"]["artifactProvenance"]["verification"]
                [2],
            "pushed"
        );
        assert_eq!(
            artifact_manifest_json["laneEvents"][1]["data"]["artifactProvenance"]["commitSha"],
            "deadbee"
        );

        let cron = global_cron_registry().create(
            "*/10 * * * *",
            "roadmap-nudge-10min for ROADMAP #66",
            Some("ROADMAP #66 reminder"),
        );
        let reminder = execute_agent_with_spawn(
            AgentInput {
                description: "Close ROADMAP #66 reminder shutdown".to_string(),
                prompt: "Finish the cron shutdown fix".to_string(),
                subagent_type: Some("Explore".to_string()),
                name: Some("cron-closeout".to_string()),
                model: None,
            },
            |job| {
                persist_agent_terminal_state(
                    &job.manifest,
                    "completed",
                    Some("Completed ROADMAP #66 after verification."),
                    None,
                )
            },
        )
        .expect("reminder agent should succeed");

        let reminder_manifest = std::fs::read_to_string(&reminder.manifest_file)
            .expect("reminder manifest should exist");
        let reminder_manifest_json: serde_json::Value =
            serde_json::from_str(&reminder_manifest).expect("reminder manifest json");
        assert_eq!(
            reminder_manifest_json["laneEvents"][1]["data"]["disabledCronIds"][0],
            cron.cron_id
        );
        let disabled_entry = global_cron_registry()
            .get(&cron.cron_id)
            .expect("cron should still exist");
        assert!(!disabled_entry.enabled);

        let resume_outcome =
            extract_recovery_outcome("Continue from current mode state. [OMX_TMUX_INJECT]")
                .expect("resume outcome should be detected");
        assert_eq!(resume_outcome.cause, "resume_after_stop");
        assert_eq!(
            resume_outcome.preserved_state.as_deref(),
            Some("current mode state")
        );

        let spawn_error = execute_agent_with_spawn(
            AgentInput {
                description: "Spawn error task".to_string(),
                prompt: "Never starts".to_string(),
                subagent_type: None,
                name: Some("spawn-error".to_string()),
                model: None,
            },
            |_| Err(String::from("thread creation failed")),
        )
        .expect_err("spawn errors should surface");
        assert!(spawn_error.contains("failed to spawn sub-agent"));
        let spawn_error_manifest = std::fs::read_dir(&dir)
            .expect("agent dir should exist")
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("json"))
            .find_map(|path| {
                let contents = std::fs::read_to_string(&path).ok()?;
                contents
                    .contains("\"name\": \"spawn-error\"")
                    .then_some(contents)
            })
            .expect("failed manifest should still be written");
        let spawn_error_manifest_json: serde_json::Value =
            serde_json::from_str(&spawn_error_manifest).expect("spawn error manifest json");
        assert!(spawn_error_manifest.contains("\"status\": \"failed\""));
        assert!(spawn_error_manifest.contains("thread creation failed"));
        assert_eq!(
            spawn_error_manifest_json["currentBlocker"]["failureClass"],
            "infra"
        );
        assert_eq!(spawn_error_manifest_json["derivedState"], "truly_idle");

        std::env::remove_var("AOSD_AGENT_STORE");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn agent_state_classification_covers_finished_and_specific_blockers() {
        assert_eq!(derive_agent_state("running", None, None, None), "working");
        assert_eq!(
            derive_agent_state("completed", Some("done"), None, None),
            "finished_cleanable"
        );
        assert_eq!(
            derive_agent_state("completed", None, None, None),
            "finished_pending_report"
        );
        assert_eq!(
            derive_agent_state("failed", None, Some("mcp handshake timed out"), None),
            "degraded_mcp"
        );
        assert_eq!(
            derive_agent_state(
                "failed",
                None,
                Some("background terminal still running"),
                None
            ),
            "blocked_background_job"
        );
        assert_eq!(
            derive_agent_state("failed", None, Some("merge conflict while rebasing"), None),
            "blocked_merge_conflict"
        );
        assert_eq!(
            derive_agent_state(
                "failed",
                None,
                Some("transport interrupted after partial progress"),
                None
            ),
            "interrupted_transport"
        );
    }

    #[test]
    fn commit_provenance_is_extracted_from_agent_results() {
        let provenance = maybe_commit_provenance(Some("landed as commit deadbee with clean push"))
            .expect("commit provenance");
        assert_eq!(provenance.commit, "deadbee");
        assert_eq!(provenance.canonical_commit.as_deref(), Some("deadbee"));
        assert_eq!(provenance.lineage, vec!["deadbee".to_string()]);
    }
    #[test]
    fn lane_failure_taxonomy_normalizes_common_blockers() {
        let cases = [
            (
                "prompt delivery failed in tmux pane",
                LaneFailureClass::PromptDelivery,
            ),
            (
                "trust prompt is still blocking startup",
                LaneFailureClass::TrustGate,
            ),
            (
                "branch stale against main after divergence",
                LaneFailureClass::BranchDivergence,
            ),
            (
                "compile failed after cargo check",
                LaneFailureClass::Compile,
            ),
            ("targeted tests failed", LaneFailureClass::Test),
            ("plugin bootstrap failed", LaneFailureClass::PluginStartup),
            ("mcp handshake timed out", LaneFailureClass::McpHandshake),
            (
                "mcp startup failed before listing tools",
                LaneFailureClass::McpStartup,
            ),
            (
                "gateway routing rejected the request",
                LaneFailureClass::GatewayRouting,
            ),
            (
                "tool failed: denied tool execution from hook",
                LaneFailureClass::ToolRuntime,
            ),
            (
                "workspace mismatch while resuming the managed session",
                LaneFailureClass::WorkspaceMismatch,
            ),
            ("thread creation failed", LaneFailureClass::Infra),
        ];

        for (message, expected) in cases {
            assert_eq!(classify_lane_failure(message), expected, "{message}");
        }
    }

    #[test]
    fn lane_event_schema_serializes_to_canonical_names() {
        let cases = [
            (LaneEventName::Started, "lane.started"),
            (LaneEventName::Ready, "lane.ready"),
            (LaneEventName::PromptMisdelivery, "lane.prompt_misdelivery"),
            (LaneEventName::Blocked, "lane.blocked"),
            (LaneEventName::Red, "lane.red"),
            (LaneEventName::Green, "lane.green"),
            (LaneEventName::CommitCreated, "lane.commit.created"),
            (LaneEventName::PrOpened, "lane.pr.opened"),
            (LaneEventName::MergeReady, "lane.merge.ready"),
            (LaneEventName::Finished, "lane.finished"),
            (LaneEventName::Failed, "lane.failed"),
            (
                LaneEventName::BranchStaleAgainstMain,
                "branch.stale_against_main",
            ),
            (
                LaneEventName::BranchWorkspaceMismatch,
                "branch.workspace_mismatch",
            ),
        ];

        for (event, expected) in cases {
            assert_eq!(
                serde_json::to_value(event).expect("serialize lane event"),
                json!(expected)
            );
        }
    }

    #[test]
    fn agent_tool_subset_mapping_is_expected() {
        let general = allowed_tools_for_subagent("general-purpose");
        assert!(general.contains("bash"));
        assert!(general.contains("write_file"));
        assert!(!general.contains("agent"));

        let explore = allowed_tools_for_subagent("Explore");
        assert!(explore.contains("read_file"));
        assert!(explore.contains("grep_search"));
        assert!(!explore.contains("bash"));

        let plan = allowed_tools_for_subagent("Plan");
        assert!(plan.contains("todo_write"));
        assert!(plan.contains("structured_output"));
        assert!(!plan.contains("agent"));

        let verification = allowed_tools_for_subagent("Verification");
        assert!(verification.contains("bash"));
        assert!(verification.contains("power_shell"));
        assert!(!verification.contains("write_file"));
    }

    #[derive(Debug)]
    struct MockSubagentApiClient {
        calls: usize,
        input_path: String,
    }

    #[::async_trait::async_trait]
    impl runtime::ApiClient for MockSubagentApiClient {
        async fn stream(
            &mut self,
            request: ApiRequest,
        ) -> Result<Vec<AssistantEvent>, RuntimeError> {
            self.calls += 1;
            match self.calls {
                1 => {
                    assert_eq!(request.messages.len(), 1);
                    Ok(vec![
                        AssistantEvent::ToolUse {
                            id: "tool-1".to_string(),
                            name: "read_file".to_string(),
                            input: json!({ "path": self.input_path }).to_string(),
                        },
                        AssistantEvent::MessageStop,
                    ])
                }
                2 => {
                    assert!(request.messages.len() >= 3);
                    Ok(vec![
                        AssistantEvent::TextDelta("Scope: completed mock review".to_string()),
                        AssistantEvent::MessageStop,
                    ])
                }
                _ => unreachable!("extra mock stream call"),
            }
        }
    }

    #[tokio::test]
    async fn subagent_runtime_executes_tool_loop_with_isolated_session() {
        let path = temp_path("subagent-input.txt");
        std::fs::write(&path, "hello from child").expect("write input file");

        let mut runtime = {
            let _guard = env_lock()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);

            ConversationRuntime::new(
                Session::new(),
                MockSubagentApiClient {
                    calls: 0,
                    input_path: path.display().to_string(),
                },
                SubagentToolExecutor::new(BTreeSet::from([String::from("read_file")])),
                agent_permission_policy(),
                vec![String::from("system prompt")],
            )
        };

        let summary = runtime
            .run_turn("Inspect the delegated file", None, ())
            .await
            .expect("subagent loop should succeed");

        assert_eq!(
            final_assistant_text(&summary),
            "Scope: completed mock review"
        );
        assert!(runtime
            .session()
            .messages
            .iter()
            .flat_map(|message| message.blocks.iter())
            .any(|block| matches!(
                block,
                runtime::ContentBlock::ToolResult { output, .. }
                    if output.contains("hello from child")
            )));

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn agent_rejects_blank_required_fields() {
        let missing_description = execute_tool(
            "Agent",
            &json!({
                "description": "  ",
                "prompt": "Inspect"
            }),
        )
        .expect_err("blank description should fail");
        assert!(missing_description.contains("description must not be empty"));

        let missing_prompt = execute_tool(
            "Agent",
            &json!({
                "description": "Inspect branch",
                "prompt": " "
            }),
        )
        .expect_err("blank prompt should fail");
        assert!(missing_prompt.contains("prompt must not be empty"));
    }

    #[test]
    fn notebook_edit_replaces_inserts_and_deletes_cells() {
        let path = temp_path("notebook.ipynb");
        std::fs::write(
            &path,
            r#"{
  "cells": [
    {"cell_type": "code", "id": "cell-a", "metadata": {}, "source": ["print(1)\n"], "outputs": [], "execution_count": null}
  ],
  "metadata": {"kernelspec": {"language": "python"}},
  "nbformat": 4,
  "nbformat_minor": 5
}"#,
        )
        .expect("write notebook");

        let replaced = execute_tool(
            "NotebookEdit",
            &json!({
                "notebook_path": path.display().to_string(),
                "cell_id": "cell-a",
                "new_source": "print(2)\n",
                "edit_mode": "replace"
            }),
        )
        .expect("NotebookEdit replace should succeed");
        let replaced_output: serde_json::Value = serde_json::from_str(&replaced).expect("json");
        assert_eq!(replaced_output["cell_id"], "cell-a");
        assert_eq!(replaced_output["cell_type"], "code");

        let inserted = execute_tool(
            "NotebookEdit",
            &json!({
                "notebook_path": path.display().to_string(),
                "cell_id": "cell-a",
                "new_source": "# heading\n",
                "cell_type": "markdown",
                "edit_mode": "insert"
            }),
        )
        .expect("NotebookEdit insert should succeed");
        let inserted_output: serde_json::Value = serde_json::from_str(&inserted).expect("json");
        assert_eq!(inserted_output["cell_type"], "markdown");
        let appended = execute_tool(
            "NotebookEdit",
            &json!({
                "notebook_path": path.display().to_string(),
                "new_source": "print(3)\n",
                "edit_mode": "insert"
            }),
        )
        .expect("NotebookEdit append should succeed");
        let appended_output: serde_json::Value = serde_json::from_str(&appended).expect("json");
        assert_eq!(appended_output["cell_type"], "code");

        let deleted = execute_tool(
            "NotebookEdit",
            &json!({
                "notebook_path": path.display().to_string(),
                "cell_id": "cell-a",
                "edit_mode": "delete"
            }),
        )
        .expect("NotebookEdit delete should succeed without new_source");
        let deleted_output: serde_json::Value = serde_json::from_str(&deleted).expect("json");
        assert!(deleted_output["cell_type"].is_null());
        assert_eq!(deleted_output["new_source"], "");

        let final_notebook: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("read notebook"))
                .expect("valid notebook json");
        let cells = final_notebook["cells"].as_array().expect("cells array");
        assert_eq!(cells.len(), 2);
        assert_eq!(cells[0]["cell_type"], "markdown");
        assert!(cells[0].get("outputs").is_none());
        assert_eq!(cells[1]["cell_type"], "code");
        assert_eq!(cells[1]["source"][0], "print(3)\n");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn notebook_edit_rejects_invalid_inputs() {
        let text_path = temp_path("notebook.txt");
        fs::write(&text_path, "not a notebook").expect("write text file");
        let wrong_extension = execute_tool(
            "NotebookEdit",
            &json!({
                "notebook_path": text_path.display().to_string(),
                "new_source": "print(1)\n"
            }),
        )
        .expect_err("non-ipynb file should fail");
        assert!(wrong_extension.contains("Jupyter notebook"));
        let _ = fs::remove_file(&text_path);

        let empty_notebook = temp_path("empty.ipynb");
        fs::write(
            &empty_notebook,
            r#"{"cells":[],"metadata":{"kernelspec":{"language":"python"}},"nbformat":4,"nbformat_minor":5}"#,
        )
        .expect("write empty notebook");

        let missing_source = execute_tool(
            "NotebookEdit",
            &json!({
                "notebook_path": empty_notebook.display().to_string(),
                "edit_mode": "insert"
            }),
        )
        .expect_err("insert without source should fail");
        assert!(missing_source.contains("new_source is required"));

        let missing_cell = execute_tool(
            "NotebookEdit",
            &json!({
                "notebook_path": empty_notebook.display().to_string(),
                "edit_mode": "delete"
            }),
        )
        .expect_err("delete on empty notebook should fail");
        assert!(missing_cell.contains("Notebook has no cells to edit"));
        let _ = fs::remove_file(empty_notebook);
    }

    #[test]
    fn bash_tool_reports_success_exit_failure_timeout_and_background() {
        let success = execute_tool("bash", &json!({ "command": "printf 'hello'" }))
            .expect("bash should succeed");
        let success_output: serde_json::Value = serde_json::from_str(&success).expect("json");
        assert_eq!(success_output["stdout"], "hello");
        assert_eq!(success_output["interrupted"], false);

        let failure = execute_tool("bash", &json!({ "command": "printf 'oops' >&2; exit 7" }))
            .expect("bash failure should still return structured output");
        let failure_output: serde_json::Value = serde_json::from_str(&failure).expect("json");
        assert_eq!(failure_output["returnCodeInterpretation"], "exit_code:7");
        assert!(failure_output["stderr"]
            .as_str()
            .expect("stderr")
            .contains("oops"));

        let timeout = execute_tool("bash", &json!({ "command": "sleep 1", "timeout": 10 }))
            .expect("bash timeout should return output");
        let timeout_output: serde_json::Value = serde_json::from_str(&timeout).expect("json");
        assert_eq!(timeout_output["interrupted"], true);
        assert_eq!(timeout_output["returnCodeInterpretation"], "timeout");
        assert!(timeout_output["stderr"]
            .as_str()
            .expect("stderr")
            .contains("Command exceeded timeout"));

        let background = execute_tool(
            "bash",
            &json!({ "command": "sleep 1", "run_in_background": true }),
        )
        .expect("bash background should succeed");
        let background_output: serde_json::Value = serde_json::from_str(&background).expect("json");
        assert!(background_output["backgroundTaskId"].as_str().is_some());
        assert_eq!(background_output["noOutputExpected"], true);
    }

    #[test]
    fn bash_workspace_tests_are_blocked_when_branch_is_behind_main() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let root = temp_path("workspace-test-preflight");
        let original_dir = std::env::current_dir().expect("cwd");
        init_git_repo(&root);
        run_git(&root, &["checkout", "-b", "feature/stale-tests"]);
        run_git(&root, &["checkout", "main"]);
        commit_file(
            &root,
            "hotfix.txt",
            "fix from main\n",
            "fix: unblock workspace tests",
        );
        run_git(&root, &["checkout", "feature/stale-tests"]);
        std::env::set_current_dir(&root).expect("set cwd");

        let output = execute_tool(
            "bash",
            &json!({ "command": "cargo test --workspace --all-targets" }),
        )
        .expect("preflight should return structured output");
        let output_json: serde_json::Value = serde_json::from_str(&output).expect("json");
        assert_eq!(
            output_json["returnCodeInterpretation"],
            "preflight_blocked:branch_divergence"
        );
        assert!(output_json["stderr"]
            .as_str()
            .expect("stderr")
            .contains("branch divergence detected before workspace tests"));
        assert_eq!(
            output_json["structuredContent"][0]["event"],
            "branch.stale_against_main"
        );
        assert_eq!(
            output_json["structuredContent"][0]["failureClass"],
            "branch_divergence"
        );
        assert_eq!(
            output_json["structuredContent"][0]["data"]["missingCommits"][0],
            "fix: unblock workspace tests"
        );

        std::env::set_current_dir(&original_dir).expect("restore cwd");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn bash_targeted_tests_skip_branch_preflight() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let root = temp_path("targeted-test-no-preflight");
        let original_dir = std::env::current_dir().expect("cwd");
        init_git_repo(&root);
        run_git(&root, &["checkout", "-b", "feature/targeted-tests"]);
        run_git(&root, &["checkout", "main"]);
        commit_file(
            &root,
            "hotfix.txt",
            "fix from main\n",
            "fix: only broad tests should block",
        );
        run_git(&root, &["checkout", "feature/targeted-tests"]);
        std::env::set_current_dir(&root).expect("set cwd");

        let output = execute_tool(
            "bash",
            &json!({ "command": "printf 'targeted ok'; cargo test -p runtime stale_branch" }),
        )
        .expect("targeted commands should still execute");
        let output_json: serde_json::Value = serde_json::from_str(&output).expect("json");
        assert_ne!(
            output_json["returnCodeInterpretation"],
            "preflight_blocked:branch_divergence"
        );

        std::env::set_current_dir(&original_dir).expect("restore cwd");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn file_tools_cover_read_write_and_edit_behaviors() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let root = temp_path("fs-suite");
        fs::create_dir_all(&root).expect("create root");
        let original_dir = std::env::current_dir().expect("cwd");
        std::env::set_current_dir(&root).expect("set cwd");

        let write_create = execute_tool(
            "write_file",
            &json!({ "path": "nested/demo.txt", "content": "alpha\nbeta\nalpha\n" }),
        )
        .expect("write create should succeed");
        let write_create_output: serde_json::Value =
            serde_json::from_str(&write_create).expect("json");
        assert_eq!(write_create_output["type"], "create");
        assert!(root.join("nested/demo.txt").exists());

        let write_update = execute_tool(
            "write_file",
            &json!({ "path": "nested/demo.txt", "content": "alpha\nbeta\ngamma\n" }),
        )
        .expect("write update should succeed");
        let write_update_output: serde_json::Value =
            serde_json::from_str(&write_update).expect("json");
        assert_eq!(write_update_output["type"], "update");
        assert_eq!(write_update_output["originalFile"], "alpha\nbeta\nalpha\n");

        let read_full = execute_tool("read_file", &json!({ "path": "nested/demo.txt" }))
            .expect("read full should succeed");
        let read_full_output: serde_json::Value = serde_json::from_str(&read_full).expect("json");
        assert_eq!(read_full_output["file"]["content"], "alpha\nbeta\ngamma");
        assert_eq!(read_full_output["file"]["startLine"], 1);

        let read_slice = execute_tool(
            "read_file",
            &json!({ "path": "nested/demo.txt", "offset": 1, "limit": 1 }),
        )
        .expect("read slice should succeed");
        let read_slice_output: serde_json::Value = serde_json::from_str(&read_slice).expect("json");
        let read_slice_content = read_slice_output["file"]["content"]
            .as_str()
            .expect("read slice content");
        assert!(read_slice_content.starts_with("beta"));
        assert!(read_slice_content.contains("[read_file truncated:"));
        assert_eq!(read_slice_output["file"]["startLine"], 2);

        let read_past_end = execute_tool(
            "read_file",
            &json!({ "path": "nested/demo.txt", "offset": 50 }),
        )
        .expect("read past EOF should succeed");
        let read_past_end_output: serde_json::Value =
            serde_json::from_str(&read_past_end).expect("json");
        assert_eq!(read_past_end_output["file"]["content"], "");
        assert_eq!(read_past_end_output["file"]["startLine"], 4);

        let read_error = execute_tool("read_file", &json!({ "path": "missing.txt" }))
            .expect_err("missing file should fail");
        assert!(!read_error.is_empty());

        let edit_once = execute_tool(
            "edit_file",
            &json!({ "path": "nested/demo.txt", "old_string": "alpha", "new_string": "omega" }),
        )
        .expect("single edit should succeed");
        let edit_once_output: serde_json::Value = serde_json::from_str(&edit_once).expect("json");
        assert_eq!(edit_once_output["replaceAll"], false);
        assert_eq!(
            fs::read_to_string(root.join("nested/demo.txt")).expect("read file"),
            "omega\nbeta\ngamma\n"
        );

        execute_tool(
            "write_file",
            &json!({ "path": "nested/demo.txt", "content": "alpha\nbeta\nalpha\n" }),
        )
        .expect("reset file");
        let edit_all = execute_tool(
            "edit_file",
            &json!({
                "path": "nested/demo.txt",
                "old_string": "alpha",
                "new_string": "omega",
                "replace_all": true
            }),
        )
        .expect("replace all should succeed");
        let edit_all_output: serde_json::Value = serde_json::from_str(&edit_all).expect("json");
        assert_eq!(edit_all_output["replaceAll"], true);
        assert_eq!(
            fs::read_to_string(root.join("nested/demo.txt")).expect("read file"),
            "omega\nbeta\nomega\n"
        );

        let edit_same = execute_tool(
            "edit_file",
            &json!({ "path": "nested/demo.txt", "old_string": "omega", "new_string": "omega" }),
        )
        .expect_err("identical old/new should fail");
        assert!(edit_same.contains("must differ"));

        let edit_missing = execute_tool(
            "edit_file",
            &json!({ "path": "nested/demo.txt", "old_string": "missing", "new_string": "omega" }),
        )
        .expect_err("missing substring should fail");
        assert!(edit_missing.contains("old_string not found"));

        std::env::set_current_dir(&original_dir).expect("restore cwd");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn glob_and_grep_tools_cover_success_and_errors() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let root = temp_path("search-suite");
        fs::create_dir_all(root.join("nested")).expect("create root");
        let original_dir = std::env::current_dir().expect("cwd");
        std::env::set_current_dir(&root).expect("set cwd");

        fs::write(
            root.join("nested/lib.rs"),
            "fn main() {}\nlet alpha = 1;\nlet alpha = 2;\n",
        )
        .expect("write rust file");
        fs::write(root.join("nested/notes.txt"), "alpha\nbeta\n").expect("write txt file");

        let globbed = execute_tool("glob_search", &json!({ "pattern": "nested/*.rs" }))
            .expect("glob should succeed");
        let globbed_output: serde_json::Value = serde_json::from_str(&globbed).expect("json");
        assert_eq!(globbed_output["numFiles"], 1);
        assert!(globbed_output["filenames"][0]
            .as_str()
            .expect("filename")
            .ends_with("nested/lib.rs"));

        let glob_error = execute_tool("glob_search", &json!({ "pattern": "[" }))
            .expect_err("invalid glob should fail");
        assert!(!glob_error.is_empty());

        let grep_content = execute_tool(
            "grep_search",
            &json!({
                "pattern": "alpha",
                "path": "nested",
                "glob": "*.rs",
                "output_mode": "content",
                "-n": true,
                "head_limit": 1,
                "offset": 1
            }),
        )
        .expect("grep content should succeed");
        let grep_content_output: serde_json::Value =
            serde_json::from_str(&grep_content).expect("json");
        assert_eq!(grep_content_output["numFiles"], 0);
        assert!(grep_content_output["appliedLimit"].is_null());
        assert_eq!(grep_content_output["appliedOffset"], 1);
        assert!(grep_content_output["content"]
            .as_str()
            .expect("content")
            .contains("let alpha = 2;"));

        let grep_count = execute_tool(
            "grep_search",
            &json!({ "pattern": "alpha", "path": "nested", "output_mode": "count" }),
        )
        .expect("grep count should succeed");
        let grep_count_output: serde_json::Value = serde_json::from_str(&grep_count).expect("json");
        assert_eq!(grep_count_output["numMatches"], 3);

        let grep_error = execute_tool(
            "grep_search",
            &json!({ "pattern": "(alpha", "path": "nested" }),
        )
        .expect_err("invalid regex should fail");
        assert!(!grep_error.is_empty());

        std::env::set_current_dir(&original_dir).expect("restore cwd");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn sleep_waits_and_reports_duration() {
        let started = std::time::Instant::now();
        let result =
            execute_tool("Sleep", &json!({"duration_ms": 20})).expect("Sleep should succeed");
        let elapsed = started.elapsed();
        let output: serde_json::Value = serde_json::from_str(&result).expect("json");
        assert_eq!(output["duration_ms"], 20);
        assert!(output["message"]
            .as_str()
            .expect("message")
            .contains("Slept for 20ms"));
        assert!(elapsed >= Duration::from_millis(15));
    }

    #[test]
    fn given_excessive_duration_when_sleep_then_rejects_with_error() {
        let result = execute_tool("Sleep", &json!({"duration_ms": 999_999_999_u64}));
        let error = result.expect_err("excessive sleep should fail");
        assert!(error.contains("exceeds maximum allowed sleep"));
    }

    #[test]
    fn given_zero_duration_when_sleep_then_succeeds() {
        let result =
            execute_tool("Sleep", &json!({"duration_ms": 0})).expect("0ms sleep should succeed");
        let output: serde_json::Value = serde_json::from_str(&result).expect("json");
        assert_eq!(output["duration_ms"], 0);
    }

    #[test]
    fn brief_returns_sent_message_and_attachment_metadata() {
        let attachment = std::env::temp_dir().join(format!(
            "aosd-brief-{}.png",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ));
        std::fs::write(&attachment, b"png-data").expect("write attachment");

        let result = execute_tool(
            "SendUserMessage",
            &json!({
                "message": "hello user",
                "attachments": [attachment.display().to_string()],
                "status": "normal"
            }),
        )
        .expect("SendUserMessage should succeed");

        let output: serde_json::Value = serde_json::from_str(&result).expect("json");
        assert_eq!(output["message"], "hello user");
        assert!(output["sentAt"].as_str().is_some());
        assert_eq!(output["attachments"][0]["isImage"], true);
        let _ = std::fs::remove_file(attachment);
    }

    #[test]
    fn config_reads_and_writes_supported_values() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let root = std::env::temp_dir().join(format!(
            "aosd-config-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ));
        let home = root.join("home");
        let cwd = root.join("cwd");
        std::fs::create_dir_all(home.join(".aos")).expect("home dir");
        std::fs::create_dir_all(cwd.join(".aos")).expect("cwd dir");
        std::fs::write(
            home.join(".aos").join("settings.json"),
            r#"{"verbose":false}"#,
        )
        .expect("write global settings");

        let original_home = std::env::var("HOME").ok();
        let original_config_home = std::env::var("AOS_CONFIG_HOME").ok();
        let original_dir = std::env::current_dir().expect("cwd");
        std::env::set_var("HOME", &home);
        std::env::remove_var("AOS_CONFIG_HOME");
        std::env::set_current_dir(&cwd).expect("set cwd");

        let get = execute_tool("Config", &json!({"setting": "verbose"})).expect("get config");
        let get_output: serde_json::Value = serde_json::from_str(&get).expect("json");
        assert_eq!(get_output["value"], false);

        let set = execute_tool(
            "Config",
            &json!({"setting": "permissions.defaultMode", "value": "plan"}),
        )
        .expect("set config");
        let set_output: serde_json::Value = serde_json::from_str(&set).expect("json");
        assert_eq!(set_output["operation"], "set");
        assert_eq!(set_output["newValue"], "plan");

        let invalid = execute_tool(
            "Config",
            &json!({"setting": "permissions.defaultMode", "value": "bogus"}),
        )
        .expect_err("invalid config value should error");
        assert!(invalid.contains("Invalid value"));

        let unknown =
            execute_tool("Config", &json!({"setting": "nope"})).expect("unknown setting result");
        let unknown_output: serde_json::Value = serde_json::from_str(&unknown).expect("json");
        assert_eq!(unknown_output["success"], false);

        std::env::set_current_dir(&original_dir).expect("restore cwd");
        match original_home {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
        match original_config_home {
            Some(value) => std::env::set_var("AOS_CONFIG_HOME", value),
            None => std::env::remove_var("AOS_CONFIG_HOME"),
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn enter_and_exit_plan_mode_round_trip_existing_local_override() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let root = std::env::temp_dir().join(format!(
            "aosd-plan-mode-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ));
        let home = root.join("home");
        let cwd = root.join("cwd");
        std::fs::create_dir_all(home.join(".aos")).expect("home dir");
        std::fs::create_dir_all(cwd.join(".aos")).expect("cwd dir");
        std::fs::write(
            cwd.join(".aos").join("settings.local.json"),
            r#"{"permissions":{"defaultMode":"acceptEdits"}}"#,
        )
        .expect("write local settings");

        let original_home = std::env::var("HOME").ok();
        let original_config_home = std::env::var("AOS_CONFIG_HOME").ok();
        let original_dir = std::env::current_dir().expect("cwd");
        std::env::set_var("HOME", &home);
        std::env::remove_var("AOS_CONFIG_HOME");
        std::env::set_current_dir(&cwd).expect("set cwd");

        let enter = execute_tool("EnterPlanMode", &json!({})).expect("enter plan mode");
        let enter_output: serde_json::Value = serde_json::from_str(&enter).expect("json");
        assert_eq!(enter_output["changed"], true);
        assert_eq!(enter_output["managed"], true);
        assert_eq!(enter_output["previousLocalMode"], "acceptEdits");
        assert_eq!(enter_output["currentLocalMode"], "plan");

        let local_settings = std::fs::read_to_string(cwd.join(".aos").join("settings.local.json"))
            .expect("local settings after enter");
        assert!(local_settings.contains(r#""defaultMode": "plan""#));
        let state =
            std::fs::read_to_string(cwd.join(".aos").join("tool-state").join("plan-mode.json"))
                .expect("plan mode state");
        assert!(state.contains(r#""hadLocalOverride": true"#));
        assert!(state.contains(r#""previousLocalMode": "acceptEdits""#));

        let exit = execute_tool("ExitPlanMode", &json!({})).expect("exit plan mode");
        let exit_output: serde_json::Value = serde_json::from_str(&exit).expect("json");
        assert_eq!(exit_output["changed"], true);
        assert_eq!(exit_output["managed"], false);
        assert_eq!(exit_output["previousLocalMode"], "acceptEdits");
        assert_eq!(exit_output["currentLocalMode"], "acceptEdits");

        let local_settings = std::fs::read_to_string(cwd.join(".aos").join("settings.local.json"))
            .expect("local settings after exit");
        assert!(local_settings.contains(r#""defaultMode": "acceptEdits""#));
        assert!(!cwd
            .join(".aos")
            .join("tool-state")
            .join("plan-mode.json")
            .exists());

        std::env::set_current_dir(&original_dir).expect("restore cwd");
        match original_home {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
        match original_config_home {
            Some(value) => std::env::set_var("AOS_CONFIG_HOME", value),
            None => std::env::remove_var("AOS_CONFIG_HOME"),
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn exit_plan_mode_clears_override_when_enter_created_it_from_empty_local_state() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let root = std::env::temp_dir().join(format!(
            "aosd-plan-mode-empty-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ));
        let home = root.join("home");
        let cwd = root.join("cwd");
        std::fs::create_dir_all(home.join(".aos")).expect("home dir");
        std::fs::create_dir_all(cwd.join(".aos")).expect("cwd dir");

        let original_home = std::env::var("HOME").ok();
        let original_config_home = std::env::var("AOS_CONFIG_HOME").ok();
        let original_dir = std::env::current_dir().expect("cwd");
        std::env::set_var("HOME", &home);
        std::env::remove_var("AOS_CONFIG_HOME");
        std::env::set_current_dir(&cwd).expect("set cwd");

        let enter = execute_tool("EnterPlanMode", &json!({})).expect("enter plan mode");
        let enter_output: serde_json::Value = serde_json::from_str(&enter).expect("json");
        assert_eq!(enter_output["previousLocalMode"], serde_json::Value::Null);
        assert_eq!(enter_output["currentLocalMode"], "plan");

        let exit = execute_tool("ExitPlanMode", &json!({})).expect("exit plan mode");
        let exit_output: serde_json::Value = serde_json::from_str(&exit).expect("json");
        assert_eq!(exit_output["changed"], true);
        assert_eq!(exit_output["currentLocalMode"], serde_json::Value::Null);

        let local_settings = std::fs::read_to_string(cwd.join(".aos").join("settings.local.json"))
            .expect("local settings after exit");
        let local_settings_json: serde_json::Value =
            serde_json::from_str(&local_settings).expect("valid settings json");
        assert_eq!(
            local_settings_json.get("permissions"),
            None,
            "permissions override should be removed on exit"
        );
        assert!(!cwd
            .join(".aos")
            .join("tool-state")
            .join("plan-mode.json")
            .exists());

        std::env::set_current_dir(&original_dir).expect("restore cwd");
        match original_home {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
        match original_config_home {
            Some(value) => std::env::set_var("AOS_CONFIG_HOME", value),
            None => std::env::remove_var("AOS_CONFIG_HOME"),
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn structured_output_echoes_input_payload() {
        let result = execute_tool("StructuredOutput", &json!({"ok": true, "items": [1, 2, 3]}))
            .expect("StructuredOutput should succeed");
        let output: serde_json::Value = serde_json::from_str(&result).expect("json");
        assert_eq!(output["data"], "Structured output provided successfully");
        assert_eq!(output["structured_output"]["ok"], true);
        assert_eq!(output["structured_output"]["items"][1], 2);
    }

    #[test]
    fn given_empty_payload_when_structured_output_then_rejects_with_error() {
        let result = execute_tool("StructuredOutput", &json!({}));
        let error = result.expect_err("empty payload should fail");
        assert!(error.contains("must not be empty"));
    }

    #[test]
    fn repl_executes_python_code() {
        let _guard = env_guard();
        let result = execute_tool(
            "REPL",
            &json!({"language": "python", "code": "print(1 + 1)", "timeout_ms": 10_000}),
        )
        .expect("REPL should succeed");
        let output: serde_json::Value = serde_json::from_str(&result).expect("json");
        assert_eq!(output["language"], "python");
        assert_eq!(output["exitCode"], 0);
        assert!(output["stdout"].as_str().expect("stdout").contains('2'));
    }

    #[test]
    fn given_empty_code_when_repl_then_rejects_with_error() {
        let _guard = env_guard();
        let result = execute_tool("REPL", &json!({"language": "python", "code": "   "}));

        let error = result.expect_err("empty REPL code should fail");
        assert!(error.contains("code must not be empty"));
    }

    #[test]
    fn given_unsupported_language_when_repl_then_rejects_with_error() {
        let _guard = env_guard();
        let result = execute_tool("REPL", &json!({"language": "ruby", "code": "puts 1"}));

        let error = result.expect_err("unsupported REPL language should fail");
        assert!(error.contains("unsupported REPL language: ruby"));
    }

    #[test]
    fn given_timeout_ms_when_repl_blocks_then_returns_timeout_error() {
        let _guard = env_guard();
        let result = execute_tool(
            "REPL",
            &json!({
                "language": "python",
                "code": "import time\ntime.sleep(1)",
                "timeout_ms": 10
            }),
        );

        let error = result.expect_err("timed out REPL execution should fail");
        assert!(error.contains("REPL execution exceeded timeout of 10 ms"));
    }

    #[test]
    fn powershell_runs_via_stub_shell() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = std::env::temp_dir().join(format!(
            "aosd-pwsh-bin-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("create dir");
        let script = dir.join("pwsh");
        std::fs::write(
            &script,
            r#"#!/bin/sh
while [ $# -gt 0 ] && [ "$1" != "-Command" ]; do shift; done
[ "$1" = "-Command" ] && shift
printf 'pwsh:%s' "$1"
"#,
        )
        .expect("write script");
        std::process::Command::new("/bin/chmod")
            .arg("+x")
            .arg(&script)
            .status()
            .expect("chmod");
        let original_path = std::env::var("PATH").unwrap_or_default();
        std::env::set_var("PATH", format!("{}:{}", dir.display(), original_path));

        let result = execute_tool(
            "PowerShell",
            &json!({"command": "Write-Output hello", "timeout": 10_000}),
        )
        .expect("PowerShell should succeed");

        let background = execute_tool(
            "PowerShell",
            &json!({"command": "Write-Output hello", "run_in_background": true}),
        )
        .expect("PowerShell background should succeed");

        std::env::set_var("PATH", original_path);
        let _ = std::fs::remove_dir_all(dir);

        let output: serde_json::Value = serde_json::from_str(&result).expect("json");
        assert_eq!(output["stdout"], "pwsh:Write-Output hello");
        assert!(output["stderr"].as_str().expect("stderr").is_empty());

        let background_output: serde_json::Value = serde_json::from_str(&background).expect("json");
        assert!(background_output["backgroundTaskId"].as_str().is_some());
        assert_eq!(background_output["backgroundedByUser"], true);
        assert_eq!(background_output["assistantAutoBackgrounded"], false);
    }

    #[test]
    fn powershell_errors_when_shell_is_missing() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let original_path = std::env::var("PATH").unwrap_or_default();
        let empty_dir = std::env::temp_dir().join(format!(
            "aosd-empty-bin-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ));
        std::fs::create_dir_all(&empty_dir).expect("create empty dir");
        std::env::set_var("PATH", empty_dir.display().to_string());

        let err = execute_tool("PowerShell", &json!({"command": "Write-Output hello"}))
            .expect_err("PowerShell should fail when shell is missing");

        std::env::set_var("PATH", original_path);
        let _ = std::fs::remove_dir_all(empty_dir);

        assert!(err.contains("PowerShell executable not found"));
    }

    fn read_only_registry() -> super::GlobalToolRegistry {
        use runtime::permission_enforcer::PermissionEnforcer;
        use runtime::PermissionPolicy;

        let policy = mvp_tool_specs().into_iter().fold(
            PermissionPolicy::new(runtime::PermissionMode::ReadOnly),
            |policy, spec| policy.with_tool_requirement(spec.name, spec.required_permission),
        );
        let mut registry = super::GlobalToolRegistry::builtin();
        registry.set_enforcer(PermissionEnforcer::new(policy));
        registry
    }

    #[test]
    fn given_read_only_enforcer_when_bash_then_denied() {
        let registry = read_only_registry();
        // Use a command that requires DangerFullAccess (rm) to ensure it's blocked in read-only mode
        let err = registry
            .execute("bash", &json!({ "command": "rm -rf /" }))
            .expect_err("bash should be denied in read-only mode");
        assert!(
            err.contains("current mode is 'read-only'"),
            "should cite active mode: {err}"
        );
    }

    #[test]
    fn given_read_only_enforcer_when_write_file_then_denied() {
        let registry = read_only_registry();
        let err = registry
            .execute(
                "write_file",
                &json!({ "path": "/tmp/x.txt", "content": "x" }),
            )
            .expect_err("write_file should be denied in read-only mode");
        assert!(
            err.contains("current mode is 'read-only'"),
            "should cite active mode: {err}"
        );
    }

    #[test]
    fn given_read_only_enforcer_when_edit_file_then_denied() {
        let registry = read_only_registry();
        let err = registry
            .execute(
                "edit_file",
                &json!({ "path": "/tmp/x.txt", "old_string": "a", "new_string": "b" }),
            )
            .expect_err("edit_file should be denied in read-only mode");
        assert!(
            err.contains("current mode is 'read-only'"),
            "should cite active mode: {err}"
        );
    }

    #[test]
    fn given_read_only_enforcer_when_read_file_then_not_permission_denied() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let root = temp_path("perm-read");
        fs::create_dir_all(&root).expect("create root");
        let file = root.join("readable.txt");
        fs::write(&file, "content\n").expect("write test file");
        let original_dir = std::env::current_dir().expect("cwd");
        std::env::set_current_dir(&root).expect("set cwd");

        let registry = read_only_registry();
        let result = registry.execute("read_file", &json!({ "path": file.display().to_string() }));
        assert!(result.is_ok(), "read_file should be allowed: {result:?}");

        std::env::set_current_dir(&original_dir).expect("restore cwd");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn given_read_only_enforcer_when_glob_search_then_not_permission_denied() {
        let registry = read_only_registry();
        let result = registry.execute("glob_search", &json!({ "pattern": "*.rs" }));
        assert!(
            result.is_ok(),
            "glob_search should be allowed in read-only mode: {result:?}"
        );
    }

    #[test]
    fn given_no_enforcer_when_bash_then_executes_normally() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let registry = super::GlobalToolRegistry::builtin();
        let result = registry
            .execute("bash", &json!({ "command": "printf 'ok'" }))
            .expect("bash should succeed without enforcer");
        let output: serde_json::Value = serde_json::from_str(&result).expect("json");
        assert_eq!(output["stdout"], "ok");
    }

    #[test]
    fn provider_runtime_client_chain_uses_only_primary_when_no_fallbacks_configured() {
        // given
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let original_anthropic = std::env::var_os("ANTHROPIC_API_KEY");
        std::env::set_var("ANTHROPIC_API_KEY", "anthropic-test-key");
        let fallback_config = ProviderFallbackConfig::default();

        // when
        let client = ProviderRuntimeClient::new_with_fallback_config(
            "claude-sonnet-4-6".to_string(),
            BTreeSet::new(),
            &fallback_config,
        )
        .expect("primary-only chain should construct");

        // then
        assert_eq!(client.chain.len(), 1);
        assert_eq!(client.chain[0].model, "claude-sonnet-4-6");

        match original_anthropic {
            Some(value) => std::env::set_var("ANTHROPIC_API_KEY", value),
            None => std::env::remove_var("ANTHROPIC_API_KEY"),
        }
    }

    #[test]
    fn provider_runtime_client_chain_appends_configured_fallbacks_in_order() {
        // given
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let original_anthropic = std::env::var_os("ANTHROPIC_API_KEY");
        let original_xai = std::env::var_os("XAI_API_KEY");
        std::env::set_var("ANTHROPIC_API_KEY", "anthropic-test-key");
        std::env::set_var("XAI_API_KEY", "xai-test-key");
        let fallback_config = ProviderFallbackConfig::new(
            None,
            vec!["grok-3".to_string(), "grok-3-mini".to_string()],
        );

        // when
        let client = ProviderRuntimeClient::new_with_fallback_config(
            "claude-sonnet-4-6".to_string(),
            BTreeSet::new(),
            &fallback_config,
        )
        .expect("chain with fallbacks should construct");

        // then
        assert_eq!(client.chain.len(), 3);
        assert_eq!(client.chain[0].model, "claude-sonnet-4-6");
        assert_eq!(client.chain[1].model, "grok-3");
        assert_eq!(client.chain[2].model, "grok-3-mini");

        match original_anthropic {
            Some(value) => std::env::set_var("ANTHROPIC_API_KEY", value),
            None => std::env::remove_var("ANTHROPIC_API_KEY"),
        }
        match original_xai {
            Some(value) => std::env::set_var("XAI_API_KEY", value),
            None => std::env::remove_var("XAI_API_KEY"),
        }
    }

    #[test]
    fn provider_runtime_client_chain_primary_override_replaces_constructor_model() {
        // given
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let original_anthropic = std::env::var_os("ANTHROPIC_API_KEY");
        let original_xai = std::env::var_os("XAI_API_KEY");
        std::env::set_var("ANTHROPIC_API_KEY", "anthropic-test-key");
        std::env::set_var("XAI_API_KEY", "xai-test-key");
        let fallback_config = ProviderFallbackConfig::new(
            Some("grok-3".to_string()),
            vec!["claude-sonnet-4-6".to_string()],
        );

        // when
        let client = ProviderRuntimeClient::new_with_fallback_config(
            "claude-haiku-4-5-20251213".to_string(),
            BTreeSet::new(),
            &fallback_config,
        )
        .expect("chain with primary override should construct");

        // then
        assert_eq!(client.chain.len(), 2);
        assert_eq!(client.chain[0].model, "grok-3");
        assert_eq!(client.chain[1].model, "claude-sonnet-4-6");

        match original_anthropic {
            Some(value) => std::env::set_var("ANTHROPIC_API_KEY", value),
            None => std::env::remove_var("ANTHROPIC_API_KEY"),
        }
        match original_xai {
            Some(value) => std::env::set_var("XAI_API_KEY", value),
            None => std::env::remove_var("XAI_API_KEY"),
        }
    }

    #[test]
    fn provider_runtime_client_chain_skips_fallbacks_missing_credentials() {
        // given
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let original_anthropic = std::env::var_os("ANTHROPIC_API_KEY");
        let original_xai = std::env::var_os("XAI_API_KEY");
        std::env::set_var("ANTHROPIC_API_KEY", "anthropic-test-key");
        std::env::remove_var("XAI_API_KEY");
        let fallback_config = ProviderFallbackConfig::new(
            None,
            vec![
                "grok-3".to_string(),
                "claude-haiku-4-5-20251213".to_string(),
            ],
        );

        // when
        let client = ProviderRuntimeClient::new_with_fallback_config(
            "claude-sonnet-4-6".to_string(),
            BTreeSet::new(),
            &fallback_config,
        )
        .expect("chain construction should not fail when only some fallbacks are unavailable");

        // then
        assert_eq!(client.chain.len(), 2);
        assert_eq!(client.chain[0].model, "claude-sonnet-4-6");
        assert_eq!(client.chain[1].model, "claude-haiku-4-5-20251213");

        match original_anthropic {
            Some(value) => std::env::set_var("ANTHROPIC_API_KEY", value),
            None => std::env::remove_var("ANTHROPIC_API_KEY"),
        }
        if let Some(value) = original_xai {
            std::env::set_var("XAI_API_KEY", value);
        }
    }

    #[test]
    fn run_task_packet_creates_packet_backed_task() {
        let result = run_task_packet(TaskPacket {
            objective: "Ship packetized runtime task".to_string(),
            scope: "runtime/task system".to_string(),
            repo: "aos-parity".to_string(),
            branch_policy: "origin/main only".to_string(),
            acceptance_tests: vec![
                "cargo build --workspace".to_string(),
                "cargo test --workspace".to_string(),
            ],
            commit_policy: "single commit".to_string(),
            reporting_contract: "print build/test result and sha".to_string(),
            escalation_policy: "manual escalation".to_string(),
        })
        .expect("task packet should create a task");

        let output: serde_json::Value = serde_json::from_str(&result).expect("json");
        assert_eq!(output["status"], "created");
        assert_eq!(output["prompt"], "Ship packetized runtime task");
        assert_eq!(output["description"], "runtime/task system");
        assert_eq!(output["task_packet"]["repo"], "aos-parity");
        assert_eq!(
            output["task_packet"]["acceptance_tests"][1],
            "cargo test --workspace"
        );
    }

    struct TestServer {
        addr: SocketAddr,
        shutdown: Option<std::sync::mpsc::Sender<()>>,
        handle: Option<thread::JoinHandle<()>>,
    }

    impl TestServer {
        fn spawn(handler: Arc<dyn Fn(&str, &str) -> HttpResponse + Send + Sync + 'static>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
            listener
                .set_nonblocking(true)
                .expect("set nonblocking listener");
            let addr = listener.local_addr().expect("local addr");
            let (tx, rx) = std::sync::mpsc::channel::<()>();

            let handle = thread::spawn(move || loop {
                if rx.try_recv().is_ok() {
                    break;
                }

                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream
                            .set_nonblocking(false)
                            .expect("set accepted stream blocking");
                        let mut buffer = [0_u8; 4096];
                        let size = stream.read(&mut buffer).expect("read request");
                        let request = String::from_utf8_lossy(&buffer[..size]).into_owned();
                        let request_line = request.lines().next().unwrap_or_default().to_string();
                        let response = handler(&request_line, &request);
                        stream
                            .write_all(response.to_bytes().as_slice())
                            .expect("write response");
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("server accept failed: {error}"),
                }
            });

            Self {
                addr,
                shutdown: Some(tx),
                handle: Some(handle),
            }
        }

        fn addr(&self) -> SocketAddr {
            self.addr
        }
    }

    impl Drop for TestServer {
        fn drop(&mut self) {
            if let Some(tx) = self.shutdown.take() {
                let _ = tx.send(());
            }
            if let Some(handle) = self.handle.take() {
                handle.join().expect("join test server");
            }
        }
    }

    struct HttpResponse {
        status: u16,
        reason: &'static str,
        content_type: &'static str,
        body: String,
        headers: Vec<(String, String)>,
    }

    impl HttpResponse {
        fn html(status: u16, reason: &'static str, body: &str) -> Self {
            Self {
                status,
                reason,
                content_type: "text/html; charset=utf-8",
                body: body.to_string(),
                headers: Vec::new(),
            }
        }

        fn text(status: u16, reason: &'static str, body: &str) -> Self {
            Self {
                status,
                reason,
                content_type: "text/plain; charset=utf-8",
                body: body.to_string(),
                headers: Vec::new(),
            }
        }

        fn json(status: u16, reason: &'static str, body: &str) -> Self {
            Self {
                status,
                reason,
                content_type: "application/json; charset=utf-8",
                body: body.to_string(),
                headers: Vec::new(),
            }
        }

        fn with_header(mut self, key: &str, value: &str) -> Self {
            self.headers.push((key.to_string(), value.to_string()));
            self
        }

        fn to_bytes(&self) -> Vec<u8> {
            let extra_headers = self
                .headers
                .iter()
                .map(|(k, v)| format!("{k}: {v}\r\n"))
                .collect::<String>();
            format!(
                "HTTP/1.1 {} {}\r\nContent-Type: {}\r\n{}Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                self.status,
                self.reason,
                self.content_type,
                extra_headers,
                self.body.len(),
                self.body
            )
            .into_bytes()
        }
    }
}
