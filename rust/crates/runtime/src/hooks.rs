use std::ffi::OsStr;
use std::fmt::Write as FmtWrite;
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread;
use std::time::Duration;

use serde_json::{json, Value};

use crate::config::{RuntimeFeatureConfig, RuntimeHookConfig, RuntimeHookEntry};
use crate::data_protection::{configured_data_protection_mode, protect_sensitive_text};
use crate::permissions::PermissionOverride;

const HOOK_PREVIEW_CHAR_LIMIT: usize = 160;

pub type HookPermissionDecision = PermissionOverride;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookEvent {
    PreToolUse,
    PostToolUse,
    PostToolUseFailure,
    MessageReceived,
    BeforeModelCall,
    AfterModelCall,
    BeforeRoute,
    AfterRoute,
    BeforeFinalAnswer,
    AfterFinalAnswer,
    TaskCompleted,
    BotMessageReceived,
}

impl HookEvent {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PreToolUse => "PreToolUse",
            Self::PostToolUse => "PostToolUse",
            Self::PostToolUseFailure => "PostToolUseFailure",
            Self::MessageReceived => "MessageReceived",
            Self::BeforeModelCall => "BeforeModelCall",
            Self::AfterModelCall => "AfterModelCall",
            Self::BeforeRoute => "BeforeRoute",
            Self::AfterRoute => "AfterRoute",
            Self::BeforeFinalAnswer => "BeforeFinalAnswer",
            Self::AfterFinalAnswer => "AfterFinalAnswer",
            Self::TaskCompleted => "TaskCompleted",
            Self::BotMessageReceived => "BotMessageReceived",
        }
    }

    #[must_use]
    pub fn db_key(self) -> &'static str {
        match self {
            Self::PreToolUse => "pre_tool_use",
            Self::PostToolUse => "post_tool_use",
            Self::PostToolUseFailure => "post_tool_use_failure",
            Self::MessageReceived => "message_received",
            Self::BeforeModelCall => "before_model_call",
            Self::AfterModelCall => "after_model_call",
            Self::BeforeRoute => "before_route",
            Self::AfterRoute => "after_route",
            Self::BeforeFinalAnswer => "before_final_answer",
            Self::AfterFinalAnswer => "after_final_answer",
            Self::TaskCompleted => "task_completed",
            Self::BotMessageReceived => "bot_message_received",
        }
    }

    #[must_use]
    pub fn from_db_key(value: &str) -> Option<Self> {
        match value {
            "pre_tool_use" => Some(Self::PreToolUse),
            "post_tool_use" => Some(Self::PostToolUse),
            "post_tool_use_failure" => Some(Self::PostToolUseFailure),
            "message_received" => Some(Self::MessageReceived),
            "before_model_call" => Some(Self::BeforeModelCall),
            "after_model_call" => Some(Self::AfterModelCall),
            "before_route" => Some(Self::BeforeRoute),
            "after_route" => Some(Self::AfterRoute),
            "before_final_answer" => Some(Self::BeforeFinalAnswer),
            "after_final_answer" => Some(Self::AfterFinalAnswer),
            "task_completed" => Some(Self::TaskCompleted),
            "bot_message_received" => Some(Self::BotMessageReceived),
            _ => None,
        }
    }

    #[must_use]
    pub fn from_event_name(value: &str) -> Option<Self> {
        match value {
            "PreToolUse" => Some(Self::PreToolUse),
            "PostToolUse" => Some(Self::PostToolUse),
            "PostToolUseFailure" => Some(Self::PostToolUseFailure),
            "MessageReceived" => Some(Self::MessageReceived),
            "BeforeModelCall" => Some(Self::BeforeModelCall),
            "AfterModelCall" => Some(Self::AfterModelCall),
            "BeforeRoute" => Some(Self::BeforeRoute),
            "AfterRoute" => Some(Self::AfterRoute),
            "BeforeFinalAnswer" => Some(Self::BeforeFinalAnswer),
            "AfterFinalAnswer" => Some(Self::AfterFinalAnswer),
            "TaskCompleted" => Some(Self::TaskCompleted),
            "BotMessageReceived" => Some(Self::BotMessageReceived),
            other => Self::from_db_key(other),
        }
    }

    #[must_use]
    pub fn is_tool_event(self) -> bool {
        matches!(
            self,
            Self::PreToolUse | Self::PostToolUse | Self::PostToolUseFailure
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookProgressEvent {
    Started {
        event: HookEvent,
        tool_name: String,
        command: String,
        hook_id: Option<String>,
        tenant_id: Option<String>,
    },
    Completed {
        event: HookEvent,
        tool_name: String,
        command: String,
        hook_id: Option<String>,
        tenant_id: Option<String>,
        status: String,
        exit_code: Option<i32>,
        duration_ms: u64,
        stdout: String,
        stderr: String,
        tool_input: String,
        tool_output: Option<String>,
    },
    Cancelled {
        event: HookEvent,
        tool_name: String,
        command: String,
        hook_id: Option<String>,
        tenant_id: Option<String>,
        duration_ms: u64,
        tool_input: String,
        tool_output: Option<String>,
    },
}

pub trait HookProgressReporter {
    fn on_event(&mut self, event: &HookProgressEvent);
}

#[derive(Debug, Clone, Default)]
pub struct HookAbortSignal {
    aborted: Arc<AtomicBool>,
}

impl HookAbortSignal {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn abort(&self) {
        self.aborted.store(true, Ordering::SeqCst);
    }

    #[must_use]
    pub fn is_aborted(&self) -> bool {
        self.aborted.load(Ordering::SeqCst)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookRunResult {
    denied: bool,
    failed: bool,
    cancelled: bool,
    messages: Vec<String>,
    permission_override: Option<PermissionOverride>,
    permission_reason: Option<String>,
    updated_input: Option<String>,
}

impl HookRunResult {
    #[must_use]
    pub fn allow(messages: Vec<String>) -> Self {
        Self {
            denied: false,
            failed: false,
            cancelled: false,
            messages,
            permission_override: None,
            permission_reason: None,
            updated_input: None,
        }
    }

    #[must_use]
    pub fn is_denied(&self) -> bool {
        self.denied
    }

    #[must_use]
    pub fn is_failed(&self) -> bool {
        self.failed
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled
    }

    #[must_use]
    pub fn messages(&self) -> &[String] {
        &self.messages
    }

    #[must_use]
    pub fn permission_override(&self) -> Option<PermissionOverride> {
        self.permission_override
    }

    #[must_use]
    pub fn permission_decision(&self) -> Option<HookPermissionDecision> {
        self.permission_override
    }

    #[must_use]
    pub fn permission_reason(&self) -> Option<&str> {
        self.permission_reason.as_deref()
    }

    #[must_use]
    pub fn updated_input(&self) -> Option<&str> {
        self.updated_input.as_deref()
    }

    #[must_use]
    pub fn updated_input_json(&self) -> Option<&str> {
        self.updated_input()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HookRunner {
    config: RuntimeHookConfig,
}

impl HookRunner {
    #[must_use]
    pub fn new(config: RuntimeHookConfig) -> Self {
        Self { config }
    }

    #[must_use]
    pub fn from_feature_config(feature_config: &RuntimeFeatureConfig) -> Self {
        Self::new(feature_config.hooks().clone())
    }

    #[must_use]
    pub fn run_pre_tool_use(&self, tool_name: &str, tool_input: &str) -> HookRunResult {
        self.run_pre_tool_use_with_context(tool_name, tool_input, None, None)
    }

    #[must_use]
    pub fn run_pre_tool_use_with_context(
        &self,
        tool_name: &str,
        tool_input: &str,
        abort_signal: Option<&HookAbortSignal>,
        reporter: Option<&mut dyn HookProgressReporter>,
    ) -> HookRunResult {
        Self::run_commands(
            HookEvent::PreToolUse,
            self.config.pre_tool_use(),
            tool_name,
            tool_input,
            None,
            false,
            abort_signal,
            reporter,
        )
    }

    #[must_use]
    pub fn run_pre_tool_use_with_signal(
        &self,
        tool_name: &str,
        tool_input: &str,
        abort_signal: Option<&HookAbortSignal>,
    ) -> HookRunResult {
        self.run_pre_tool_use_with_context(tool_name, tool_input, abort_signal, None)
    }

    #[must_use]
    pub fn run_post_tool_use(
        &self,
        tool_name: &str,
        tool_input: &str,
        tool_output: &str,
        is_error: bool,
    ) -> HookRunResult {
        self.run_post_tool_use_with_context(
            tool_name,
            tool_input,
            tool_output,
            is_error,
            None,
            None,
        )
    }

    #[must_use]
    pub fn run_post_tool_use_with_context(
        &self,
        tool_name: &str,
        tool_input: &str,
        tool_output: &str,
        is_error: bool,
        abort_signal: Option<&HookAbortSignal>,
        reporter: Option<&mut dyn HookProgressReporter>,
    ) -> HookRunResult {
        Self::run_commands(
            HookEvent::PostToolUse,
            self.config.post_tool_use(),
            tool_name,
            tool_input,
            Some(tool_output),
            is_error,
            abort_signal,
            reporter,
        )
    }

    #[must_use]
    pub fn run_post_tool_use_with_signal(
        &self,
        tool_name: &str,
        tool_input: &str,
        tool_output: &str,
        is_error: bool,
        abort_signal: Option<&HookAbortSignal>,
    ) -> HookRunResult {
        self.run_post_tool_use_with_context(
            tool_name,
            tool_input,
            tool_output,
            is_error,
            abort_signal,
            None,
        )
    }

    #[must_use]
    pub fn run_post_tool_use_failure(
        &self,
        tool_name: &str,
        tool_input: &str,
        tool_error: &str,
    ) -> HookRunResult {
        self.run_post_tool_use_failure_with_context(tool_name, tool_input, tool_error, None, None)
    }

    #[must_use]
    pub fn run_post_tool_use_failure_with_context(
        &self,
        tool_name: &str,
        tool_input: &str,
        tool_error: &str,
        abort_signal: Option<&HookAbortSignal>,
        reporter: Option<&mut dyn HookProgressReporter>,
    ) -> HookRunResult {
        Self::run_commands(
            HookEvent::PostToolUseFailure,
            self.config.post_tool_use_failure(),
            tool_name,
            tool_input,
            Some(tool_error),
            true,
            abort_signal,
            reporter,
        )
    }

    #[must_use]
    pub fn run_post_tool_use_failure_with_signal(
        &self,
        tool_name: &str,
        tool_input: &str,
        tool_error: &str,
        abort_signal: Option<&HookAbortSignal>,
    ) -> HookRunResult {
        self.run_post_tool_use_failure_with_context(
            tool_name,
            tool_input,
            tool_error,
            abort_signal,
            None,
        )
    }

    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn run_hook_event_with_context(
        &self,
        event: HookEvent,
        subject: &str,
        input_json: &str,
        output_json: Option<&str>,
        is_error: bool,
        abort_signal: Option<&HookAbortSignal>,
        reporter: Option<&mut dyn HookProgressReporter>,
    ) -> HookRunResult {
        let commands = match event {
            HookEvent::PreToolUse => self.config.pre_tool_use(),
            HookEvent::PostToolUse => self.config.post_tool_use(),
            HookEvent::PostToolUseFailure => self.config.post_tool_use_failure(),
            _ => self.config.lifecycle_event(event.db_key()),
        };
        Self::run_commands(
            event,
            commands,
            subject,
            input_json,
            output_json,
            is_error,
            abort_signal,
            reporter,
        )
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    fn run_commands(
        event: HookEvent,
        commands: &[RuntimeHookEntry],
        tool_name: &str,
        tool_input: &str,
        tool_output: Option<&str>,
        is_error: bool,
        abort_signal: Option<&HookAbortSignal>,
        mut reporter: Option<&mut dyn HookProgressReporter>,
    ) -> HookRunResult {
        if commands.is_empty() {
            return HookRunResult::allow(Vec::new());
        }

        if abort_signal.is_some_and(HookAbortSignal::is_aborted) {
            return HookRunResult {
                denied: false,
                failed: false,
                cancelled: true,
                messages: vec![format!(
                    "{} hook cancelled before execution",
                    event.as_str()
                )],
                permission_override: None,
                permission_reason: None,
                updated_input: None,
            };
        }

        let protection_mode = configured_data_protection_mode();
        let protected_input = protect_sensitive_text(tool_input, protection_mode);
        let protected_output =
            tool_output.map(|value| protect_sensitive_text(value, protection_mode));
        let mut protection_report = protected_input.report.clone();
        if let Some(output) = &protected_output {
            protection_report.merge(&output.report);
        }
        if protection_report.redacted {
            tracing::warn!(
                event = event.as_str(),
                tool_name,
                finding_count = protection_report.finding_count,
                categories = ?protection_report.categories,
                "hook payload data protection redacted sensitive values"
            );
        }
        let tool_input = protected_input.value.as_str();
        let tool_output = protected_output.as_ref().map(|value| value.value.as_str());
        let payload = hook_payload(event, tool_name, tool_input, tool_output, is_error).to_string();
        let mut result = HookRunResult::allow(Vec::new());

        for entry in commands {
            let raw_command = entry.command();
            let protected_command = protect_sensitive_text(raw_command, protection_mode);
            let command = protected_command.value.as_str();
            if let Some(reporter) = reporter.as_deref_mut() {
                reporter.on_event(&HookProgressEvent::Started {
                    event,
                    tool_name: tool_name.to_string(),
                    command: command.to_string(),
                    hook_id: entry.id().map(ToOwned::to_owned),
                    tenant_id: entry.tenant_id().map(ToOwned::to_owned),
                });
            }

            match Self::run_command(
                raw_command,
                entry.timeout_seconds(),
                event,
                tool_name,
                tool_input,
                tool_output,
                is_error,
                &payload,
                abort_signal,
            ) {
                HookCommandOutcome::Allow { parsed } => {
                    if let Some(reporter) = reporter.as_deref_mut() {
                        reporter.on_event(&HookProgressEvent::Completed {
                            event,
                            tool_name: tool_name.to_string(),
                            command: command.to_string(),
                            hook_id: entry.id().map(ToOwned::to_owned),
                            tenant_id: entry.tenant_id().map(ToOwned::to_owned),
                            status: "allow".to_string(),
                            exit_code: parsed.exit_code,
                            duration_ms: parsed.duration_ms,
                            stdout: parsed.stdout.clone(),
                            stderr: parsed.stderr.clone(),
                            tool_input: tool_input.to_string(),
                            tool_output: tool_output.map(ToOwned::to_owned),
                        });
                    }
                    merge_parsed_hook_output(&mut result, parsed);
                }
                HookCommandOutcome::Deny { parsed } => {
                    if let Some(reporter) = reporter.as_deref_mut() {
                        reporter.on_event(&HookProgressEvent::Completed {
                            event,
                            tool_name: tool_name.to_string(),
                            command: command.to_string(),
                            hook_id: entry.id().map(ToOwned::to_owned),
                            tenant_id: entry.tenant_id().map(ToOwned::to_owned),
                            status: "deny".to_string(),
                            exit_code: parsed.exit_code,
                            duration_ms: parsed.duration_ms,
                            stdout: parsed.stdout.clone(),
                            stderr: parsed.stderr.clone(),
                            tool_input: tool_input.to_string(),
                            tool_output: tool_output.map(ToOwned::to_owned),
                        });
                    }
                    merge_parsed_hook_output(&mut result, parsed);
                    result.denied = true;
                    return result;
                }
                HookCommandOutcome::Failed { parsed } => {
                    if let Some(reporter) = reporter.as_deref_mut() {
                        reporter.on_event(&HookProgressEvent::Completed {
                            event,
                            tool_name: tool_name.to_string(),
                            command: command.to_string(),
                            hook_id: entry.id().map(ToOwned::to_owned),
                            tenant_id: entry.tenant_id().map(ToOwned::to_owned),
                            status: "failed".to_string(),
                            exit_code: parsed.exit_code,
                            duration_ms: parsed.duration_ms,
                            stdout: parsed.stdout.clone(),
                            stderr: parsed.stderr.clone(),
                            tool_input: tool_input.to_string(),
                            tool_output: tool_output.map(ToOwned::to_owned),
                        });
                    }
                    merge_parsed_hook_output(&mut result, parsed);
                    if entry.fail_fast() {
                        result.failed = true;
                        return result;
                    }
                }
                HookCommandOutcome::Cancelled {
                    message,
                    duration_ms,
                } => {
                    if let Some(reporter) = reporter.as_deref_mut() {
                        reporter.on_event(&HookProgressEvent::Cancelled {
                            event,
                            tool_name: tool_name.to_string(),
                            command: command.to_string(),
                            hook_id: entry.id().map(ToOwned::to_owned),
                            tenant_id: entry.tenant_id().map(ToOwned::to_owned),
                            duration_ms,
                            tool_input: tool_input.to_string(),
                            tool_output: tool_output.map(ToOwned::to_owned),
                        });
                    }
                    result.cancelled = true;
                    result.messages.push(message);
                    return result;
                }
            }
        }

        result
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    fn run_command(
        command: &str,
        timeout_seconds: Option<u32>,
        event: HookEvent,
        tool_name: &str,
        tool_input: &str,
        tool_output: Option<&str>,
        is_error: bool,
        payload: &str,
        abort_signal: Option<&HookAbortSignal>,
    ) -> HookCommandOutcome {
        let mut child = shell_command(command);
        child.stdin(Stdio::piped());
        child.stdout(Stdio::piped());
        child.stderr(Stdio::piped());
        child.env("HOOK_EVENT", event.as_str());
        child.env("HOOK_EVENT_KEY", event.db_key());
        child.env("HOOK_SUBJECT", tool_name);
        child.env("HOOK_INPUT_JSON", tool_input);
        child.env("HOOK_OUTPUT", tool_output.unwrap_or_default());
        child.env("HOOK_TOOL_NAME", tool_name);
        child.env("HOOK_TOOL_INPUT", tool_input);
        child.env("HOOK_TOOL_IS_ERROR", if is_error { "1" } else { "0" });
        if let Some(tool_output) = tool_output {
            child.env("HOOK_TOOL_OUTPUT", tool_output);
        }

        let timeout = timeout_seconds.map(|seconds| Duration::from_secs(u64::from(seconds)));
        let started = std::time::Instant::now();
        match child.output_with_stdin(payload.as_bytes(), abort_signal, timeout) {
            Ok(CommandExecution::Finished(output)) => {
                let duration_ms = started.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
                let stdout = protect_sensitive_text(
                    String::from_utf8_lossy(&output.stdout).trim(),
                    configured_data_protection_mode(),
                )
                .value;
                let stderr = protect_sensitive_text(
                    String::from_utf8_lossy(&output.stderr).trim(),
                    configured_data_protection_mode(),
                )
                .value;
                let exit_code = output.status.code();
                let safe_command =
                    protect_sensitive_text(command, configured_data_protection_mode()).value;
                let parsed = parse_hook_output(event, tool_name, &safe_command, &stdout, &stderr)
                    .with_execution(exit_code, duration_ms, stdout.clone(), stderr.clone());
                let primary_message = parsed.primary_message().map(ToOwned::to_owned);
                match exit_code {
                    Some(0) => {
                        if parsed.deny {
                            HookCommandOutcome::Deny { parsed }
                        } else {
                            HookCommandOutcome::Allow { parsed }
                        }
                    }
                    Some(2) => HookCommandOutcome::Deny {
                        parsed: parsed.with_fallback_message(format!(
                            "{} hook denied tool `{tool_name}`",
                            event.as_str()
                        )),
                    },
                    Some(code) => HookCommandOutcome::Failed {
                        parsed: parsed.with_fallback_message(format_hook_failure(
                            &safe_command,
                            code,
                            primary_message.as_deref(),
                            stderr.as_str(),
                        )),
                    },
                    None => HookCommandOutcome::Failed {
                        parsed: parsed.with_fallback_message(format!(
                            "{} hook `{safe_command}` terminated by signal while handling `{}`",
                            event.as_str(),
                            tool_name
                        )),
                    },
                }
            }
            Ok(CommandExecution::Cancelled) => HookCommandOutcome::Cancelled {
                message: {
                    let safe_command =
                        protect_sensitive_text(command, configured_data_protection_mode()).value;
                    format!(
                        "{} hook `{safe_command}` cancelled while handling `{tool_name}`",
                        event.as_str()
                    )
                },
                duration_ms: started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
            },
            Ok(CommandExecution::TimedOut) => HookCommandOutcome::Failed {
                parsed: ParsedHookOutput {
                    messages: vec![format!(
                        "{} hook timed out after {}s while handling `{}`",
                        event.as_str(),
                        timeout_seconds.unwrap_or_default(),
                        tool_name
                    )],
                    exit_code: Some(124),
                    duration_ms: started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
                    ..ParsedHookOutput::default()
                },
            },
            Err(error) => HookCommandOutcome::Failed {
                parsed: ParsedHookOutput {
                    messages: vec![format!(
                        "{} hook failed to start for `{}`: {}",
                        event.as_str(),
                        tool_name,
                        protect_sensitive_text(
                            &error.to_string(),
                            configured_data_protection_mode(),
                        )
                        .value
                    )],
                    exit_code: None,
                    duration_ms: started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
                    ..ParsedHookOutput::default()
                },
            },
        }
    }
}

enum HookCommandOutcome {
    Allow { parsed: ParsedHookOutput },
    Deny { parsed: ParsedHookOutput },
    Failed { parsed: ParsedHookOutput },
    Cancelled { message: String, duration_ms: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct ParsedHookOutput {
    messages: Vec<String>,
    deny: bool,
    permission_override: Option<PermissionOverride>,
    permission_reason: Option<String>,
    updated_input: Option<String>,
    exit_code: Option<i32>,
    duration_ms: u64,
    stdout: String,
    stderr: String,
}

impl ParsedHookOutput {
    fn with_fallback_message(mut self, fallback: String) -> Self {
        if self.messages.is_empty() {
            self.messages.push(fallback);
        }
        self
    }

    fn primary_message(&self) -> Option<&str> {
        self.messages.first().map(String::as_str)
    }

    fn with_execution(
        mut self,
        exit_code: Option<i32>,
        duration_ms: u64,
        stdout: String,
        stderr: String,
    ) -> Self {
        self.exit_code = exit_code;
        self.duration_ms = duration_ms;
        self.stdout = stdout;
        self.stderr = stderr;
        self
    }
}

fn merge_parsed_hook_output(target: &mut HookRunResult, parsed: ParsedHookOutput) {
    target.messages.extend(parsed.messages);
    if parsed.permission_override.is_some() {
        target.permission_override = parsed.permission_override;
    }
    if parsed.permission_reason.is_some() {
        target.permission_reason = parsed.permission_reason;
    }
    if parsed.updated_input.is_some() {
        target.updated_input = parsed.updated_input;
    }
}

fn parse_hook_output(
    event: HookEvent,
    tool_name: &str,
    command: &str,
    stdout: &str,
    stderr: &str,
) -> ParsedHookOutput {
    if stdout.is_empty() {
        return ParsedHookOutput::default();
    }

    let root = match serde_json::from_str::<Value>(stdout) {
        Ok(Value::Object(root)) => root,
        Ok(value) => {
            return ParsedHookOutput {
                messages: vec![format_invalid_hook_output(
                    event,
                    tool_name,
                    command,
                    &format!(
                        "expected top-level JSON object, got {}",
                        json_type_name(&value)
                    ),
                    stdout,
                    stderr,
                )],
                ..ParsedHookOutput::default()
            };
        }
        Err(error) if looks_like_json_attempt(stdout) => {
            return ParsedHookOutput {
                messages: vec![format_invalid_hook_output(
                    event,
                    tool_name,
                    command,
                    &error.to_string(),
                    stdout,
                    stderr,
                )],
                ..ParsedHookOutput::default()
            };
        }
        Err(_) => {
            return ParsedHookOutput {
                messages: vec![stdout.to_string()],
                ..ParsedHookOutput::default()
            };
        }
    };

    let mut parsed = ParsedHookOutput::default();

    if let Some(message) = root.get("systemMessage").and_then(Value::as_str) {
        parsed.messages.push(message.to_string());
    }
    if let Some(message) = root.get("reason").and_then(Value::as_str) {
        parsed.messages.push(message.to_string());
    }
    if root.get("continue").and_then(Value::as_bool) == Some(false)
        || root.get("decision").and_then(Value::as_str) == Some("block")
    {
        parsed.deny = true;
    }

    if let Some(Value::Object(specific)) = root.get("hookSpecificOutput") {
        if let Some(Value::String(additional_context)) = specific.get("additionalContext") {
            parsed.messages.push(additional_context.clone());
        }
        if let Some(decision) = specific.get("permissionDecision").and_then(Value::as_str) {
            parsed.permission_override = match decision {
                "allow" => Some(PermissionOverride::Allow),
                "deny" => Some(PermissionOverride::Deny),
                "ask" => Some(PermissionOverride::Ask),
                _ => None,
            };
        }
        if let Some(reason) = specific
            .get("permissionDecisionReason")
            .and_then(Value::as_str)
        {
            parsed.permission_reason = Some(reason.to_string());
        }
        if let Some(updated_input) = specific.get("updatedInput") {
            parsed.updated_input = serde_json::to_string(updated_input).ok();
        }
    }

    if parsed.messages.is_empty() {
        parsed.messages.push(stdout.to_string());
    }

    parsed
}

fn hook_payload(
    event: HookEvent,
    tool_name: &str,
    tool_input: &str,
    tool_output: Option<&str>,
    is_error: bool,
) -> Value {
    match event {
        HookEvent::PostToolUseFailure => json!({
            "hook_event_name": event.as_str(),
            "hook_event_key": event.db_key(),
            "subject": tool_name,
            "tool_name": tool_name,
            "tool_input": parse_tool_input(tool_input),
            "tool_input_json": tool_input,
            "tool_error": tool_output,
            "tool_result_is_error": true,
        }),
        _ => json!({
            "hook_event_name": event.as_str(),
            "hook_event_key": event.db_key(),
            "subject": tool_name,
            "tool_name": tool_name,
            "tool_input": parse_tool_input(tool_input),
            "tool_input_json": tool_input,
            "tool_output": tool_output,
            "tool_result_is_error": is_error,
        }),
    }
}

fn parse_tool_input(tool_input: &str) -> Value {
    serde_json::from_str(tool_input).unwrap_or_else(|_| json!({ "raw": tool_input }))
}

fn format_invalid_hook_output(
    event: HookEvent,
    tool_name: &str,
    command: &str,
    detail: &str,
    stdout: &str,
    stderr: &str,
) -> String {
    let stdout_preview = bounded_hook_preview(stdout).unwrap_or_else(|| "<empty>".to_string());
    let stderr_preview = bounded_hook_preview(stderr).unwrap_or_else(|| "<empty>".to_string());
    let command_preview = bounded_hook_preview(command).unwrap_or_else(|| "<empty>".to_string());

    format!(
        "hook_invalid_json: phase={} tool={} command={} detail={} stdout_preview={} stderr_preview={}",
        event.as_str(),
        tool_name,
        command_preview,
        detail,
        stdout_preview,
        stderr_preview
    )
}

fn bounded_hook_preview(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }

    let mut preview = String::new();
    for (count, ch) in trimmed.chars().enumerate() {
        if count == HOOK_PREVIEW_CHAR_LIMIT {
            preview.push('…');
            break;
        }
        match ch {
            '\n' => preview.push_str("\\n"),
            '\r' => preview.push_str("\\r"),
            '\t' => preview.push_str("\\t"),
            control if control.is_control() => {
                let _ = write!(&mut preview, "\\u{{{:x}}}", control as u32);
            }
            _ => preview.push(ch),
        }
    }
    Some(preview)
}

fn json_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn looks_like_json_attempt(value: &str) -> bool {
    matches!(value.trim_start().chars().next(), Some('{' | '['))
}

fn format_hook_failure(command: &str, code: i32, stdout: Option<&str>, stderr: &str) -> String {
    let mut message = format!("Hook `{command}` exited with status {code}");
    if let Some(stdout) = stdout.filter(|stdout| !stdout.is_empty()) {
        message.push_str(": ");
        message.push_str(stdout);
    } else if !stderr.is_empty() {
        message.push_str(": ");
        message.push_str(stderr);
    }
    message
}

fn shell_command(command: &str) -> CommandWithStdin {
    #[cfg(windows)]
    let mut command_builder = {
        let mut command_builder = Command::new("cmd");
        command_builder.arg("/C").arg(command);
        CommandWithStdin::new(command_builder)
    };

    #[cfg(not(windows))]
    let command_builder = {
        let mut command_builder = Command::new("sh");
        command_builder.arg("-lc").arg(command);
        CommandWithStdin::new(command_builder)
    };

    command_builder
}

struct CommandWithStdin {
    command: Command,
}

impl CommandWithStdin {
    fn new(command: Command) -> Self {
        Self { command }
    }

    fn stdin(&mut self, cfg: Stdio) -> &mut Self {
        self.command.stdin(cfg);
        self
    }

    fn stdout(&mut self, cfg: Stdio) -> &mut Self {
        self.command.stdout(cfg);
        self
    }

    fn stderr(&mut self, cfg: Stdio) -> &mut Self {
        self.command.stderr(cfg);
        self
    }

    fn env<K, V>(&mut self, key: K, value: V) -> &mut Self
    where
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        self.command.env(key, value);
        self
    }

    fn output_with_stdin(
        &mut self,
        stdin: &[u8],
        abort_signal: Option<&HookAbortSignal>,
        timeout: Option<Duration>,
    ) -> std::io::Result<CommandExecution> {
        let mut child = self.command.spawn()?;
        if let Some(mut child_stdin) = child.stdin.take() {
            child_stdin.write_all(stdin)?;
        }
        let started = std::time::Instant::now();

        loop {
            if abort_signal.is_some_and(HookAbortSignal::is_aborted) {
                let _ = child.kill();
                let _ = child.wait_with_output();
                return Ok(CommandExecution::Cancelled);
            }
            if timeout.is_some_and(|limit| started.elapsed() >= limit) {
                let _ = child.kill();
                let _ = child.wait_with_output();
                return Ok(CommandExecution::TimedOut);
            }

            match child.try_wait()? {
                Some(_) => return child.wait_with_output().map(CommandExecution::Finished),
                None => thread::sleep(Duration::from_millis(20)),
            }
        }
    }
}

enum CommandExecution {
    Finished(std::process::Output),
    Cancelled,
    TimedOut,
}

#[cfg(test)]
mod tests {
    use std::thread;
    use std::time::Duration;

    use super::{
        HookAbortSignal, HookEvent, HookProgressEvent, HookProgressReporter, HookRunResult,
        HookRunner,
    };
    use crate::config::{RuntimeFeatureConfig, RuntimeHookConfig};
    use crate::permissions::PermissionOverride;

    struct RecordingReporter {
        events: Vec<HookProgressEvent>,
    }

    impl HookProgressReporter for RecordingReporter {
        fn on_event(&mut self, event: &HookProgressEvent) {
            self.events.push(event.clone());
        }
    }

    #[test]
    fn allows_exit_code_zero_and_captures_stdout() {
        let runner = HookRunner::new(RuntimeHookConfig::new(
            vec![shell_snippet("printf 'pre ok'")],
            Vec::new(),
            Vec::new(),
        ));

        let result = runner.run_pre_tool_use("Read", r#"{"path":"README.md"}"#);

        assert_eq!(result, HookRunResult::allow(vec!["pre ok".to_string()]));
    }

    #[test]
    fn denies_exit_code_two() {
        let runner = HookRunner::new(RuntimeHookConfig::new(
            vec![shell_snippet("printf 'blocked by hook'; exit 2")],
            Vec::new(),
            Vec::new(),
        ));

        let result = runner.run_pre_tool_use("Bash", r#"{"command":"pwd"}"#);

        assert!(result.is_denied());
        assert_eq!(result.messages(), &["blocked by hook".to_string()]);
    }

    #[test]
    fn propagates_other_non_zero_statuses_as_failures() {
        let runner = HookRunner::from_feature_config(&RuntimeFeatureConfig::default().with_hooks(
            RuntimeHookConfig::new(
                vec![shell_snippet("printf 'warning hook'; exit 1")],
                Vec::new(),
                Vec::new(),
            ),
        ));

        // given
        // when
        let result = runner.run_pre_tool_use("Edit", r#"{"file":"src/lib.rs"}"#);

        // then
        assert!(result.is_failed());
        assert!(result
            .messages()
            .iter()
            .any(|message| message.contains("warning hook")));
    }

    #[test]
    fn parses_pre_hook_permission_override_and_updated_input() {
        let runner = HookRunner::new(RuntimeHookConfig::new(
            vec![shell_snippet(
                r#"printf '%s' '{"systemMessage":"updated","hookSpecificOutput":{"permissionDecision":"allow","permissionDecisionReason":"hook ok","updatedInput":{"command":"git status"}}}'"#,
            )],
            Vec::new(),
            Vec::new(),
        ));

        let result = runner.run_pre_tool_use("bash", r#"{"command":"pwd"}"#);

        assert_eq!(
            result.permission_override(),
            Some(PermissionOverride::Allow)
        );
        assert_eq!(result.permission_reason(), Some("hook ok"));
        assert_eq!(result.updated_input(), Some(r#"{"command":"git status"}"#));
        assert!(result.messages().iter().any(|message| message == "updated"));
    }

    #[test]
    fn runs_post_tool_use_failure_hooks() {
        // given
        let runner = HookRunner::new(RuntimeHookConfig::new(
            Vec::new(),
            Vec::new(),
            vec![shell_snippet("printf 'failure hook ran'")],
        ));

        // when
        let result =
            runner.run_post_tool_use_failure("bash", r#"{"command":"false"}"#, "command failed");

        // then
        assert!(!result.is_denied());
        assert_eq!(result.messages(), &["failure hook ran".to_string()]);
    }

    #[test]
    fn runs_lifecycle_hooks_with_business_payload_env() {
        let mut lifecycle = std::collections::BTreeMap::new();
        lifecycle.insert(
            HookEvent::BeforeFinalAnswer.db_key().to_string(),
            vec![crate::config::RuntimeHookEntry::plain(
                r#"printf '%s:%s:%s' "$HOOK_EVENT_KEY" "$HOOK_SUBJECT" "$HOOK_INPUT_JSON""#
                    .to_string(),
            )],
        );
        let runner = HookRunner::new(RuntimeHookConfig::new_entries_with_lifecycle(
            Vec::new(),
            Vec::new(),
            Vec::new(),
            lifecycle,
        ));

        let result = runner.run_hook_event_with_context(
            HookEvent::BeforeFinalAnswer,
            "pm.final_answer",
            r#"{"answer":"draft"}"#,
            None,
            false,
            None,
            None,
        );

        assert!(!result.is_failed());
        assert_eq!(
            result.messages(),
            &["before_final_answer:pm.final_answer:{\"answer\":\"draft\"}".to_string()]
        );
    }

    #[test]
    fn hook_boundary_never_receives_or_reports_plaintext_credentials() {
        let secret = "sk-1234567890abcdef";
        let runner = HookRunner::new(RuntimeHookConfig::new(
            vec![shell_snippet(&format!(
                r#"printf '%s:%s' "$HOOK_TOOL_INPUT" "{secret}""#
            ))],
            Vec::new(),
            Vec::new(),
        ));
        let mut reporter = RecordingReporter { events: Vec::new() };

        let result = runner.run_pre_tool_use_with_context(
            "RemoteTrigger",
            &format!(r#"{{"api_key":"{secret}"}}"#),
            None,
            Some(&mut reporter),
        );

        let rendered = format!("{result:?}:{:?}", reporter.events);
        assert!(!rendered.contains(secret));
        assert!(rendered.contains("[REDACTED"));
    }

    #[test]
    fn stops_running_failure_hooks_after_failure() {
        // given
        let runner = HookRunner::new(RuntimeHookConfig::new(
            Vec::new(),
            Vec::new(),
            vec![
                shell_snippet("printf 'broken failure hook'; exit 1"),
                shell_snippet("printf 'later failure hook'"),
            ],
        ));

        // when
        let result =
            runner.run_post_tool_use_failure("bash", r#"{"command":"false"}"#, "command failed");

        // then
        assert!(result.is_failed());
        assert!(result
            .messages()
            .iter()
            .any(|message| message.contains("broken failure hook")));
        assert!(!result
            .messages()
            .iter()
            .any(|message| message == "later failure hook"));
    }

    #[test]
    fn executes_hooks_in_configured_order() {
        // given
        let runner = HookRunner::new(RuntimeHookConfig::new(
            vec![
                shell_snippet("printf 'first'"),
                shell_snippet("printf 'second'"),
            ],
            Vec::new(),
            Vec::new(),
        ));
        let mut reporter = RecordingReporter { events: Vec::new() };

        // when
        let result = runner.run_pre_tool_use_with_context(
            "Read",
            r#"{"path":"README.md"}"#,
            None,
            Some(&mut reporter),
        );

        // then
        assert_eq!(
            result,
            HookRunResult::allow(vec!["first".to_string(), "second".to_string()])
        );
        assert_eq!(reporter.events.len(), 4);
        assert!(matches!(
            &reporter.events[0],
            HookProgressEvent::Started {
                event: HookEvent::PreToolUse,
                command,
                ..
            } if command == "printf 'first'"
        ));
        assert!(matches!(
            &reporter.events[1],
            HookProgressEvent::Completed {
                event: HookEvent::PreToolUse,
                command,
                ..
            } if command == "printf 'first'"
        ));
        assert!(matches!(
            &reporter.events[2],
            HookProgressEvent::Started {
                event: HookEvent::PreToolUse,
                command,
                ..
            } if command == "printf 'second'"
        ));
        assert!(matches!(
            &reporter.events[3],
            HookProgressEvent::Completed {
                event: HookEvent::PreToolUse,
                command,
                ..
            } if command == "printf 'second'"
        ));
    }

    #[test]
    fn stops_running_hooks_after_failure() {
        // given
        let runner = HookRunner::new(RuntimeHookConfig::new(
            vec![
                shell_snippet("printf 'broken'; exit 1"),
                shell_snippet("printf 'later'"),
            ],
            Vec::new(),
            Vec::new(),
        ));

        // when
        let result = runner.run_pre_tool_use("Edit", r#"{"file":"src/lib.rs"}"#);

        // then
        assert!(result.is_failed());
        assert!(result
            .messages()
            .iter()
            .any(|message| message.contains("broken")));
        assert!(!result.messages().iter().any(|message| message == "later"));
    }

    #[test]
    fn malformed_nonempty_hook_output_reports_explicit_diagnostic_with_previews() {
        let runner = HookRunner::new(RuntimeHookConfig::new(
            vec![shell_snippet(
                "printf '{not-json\nsecond line'; printf 'stderr warning' >&2; exit 1",
            )],
            Vec::new(),
            Vec::new(),
        ));

        let result = runner.run_pre_tool_use("Edit", r#"{"file":"src/lib.rs"}"#);

        assert!(result.is_failed());
        let rendered = result.messages().join("\n");
        assert!(rendered.contains("hook_invalid_json:"));
        assert!(rendered.contains("phase=PreToolUse"));
        assert!(rendered.contains("tool=Edit"));
        assert!(rendered.contains("command=printf '{not-json"));
        assert!(rendered.contains("printf 'stderr warning' >&2; exit 1"));
        assert!(rendered.contains("detail=key must be a string"));
        assert!(rendered.contains("stdout_preview={not-json"));
        assert!(rendered.contains("second line stderr_preview=stderr warning"));
        assert!(rendered.contains("stderr_preview=stderr warning"));
    }

    #[test]
    fn abort_signal_cancels_long_running_hook_and_reports_progress() {
        let runner = HookRunner::new(RuntimeHookConfig::new(
            vec![shell_snippet("sleep 5")],
            Vec::new(),
            Vec::new(),
        ));
        let abort_signal = HookAbortSignal::new();
        let abort_signal_for_thread = abort_signal.clone();
        let mut reporter = RecordingReporter { events: Vec::new() };

        thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            abort_signal_for_thread.abort();
        });

        let result = runner.run_pre_tool_use_with_context(
            "bash",
            r#"{"command":"sleep 5"}"#,
            Some(&abort_signal),
            Some(&mut reporter),
        );

        assert!(result.is_cancelled());
        assert!(reporter.events.iter().any(|event| matches!(
            event,
            HookProgressEvent::Started {
                event: HookEvent::PreToolUse,
                ..
            }
        )));
        assert!(reporter.events.iter().any(|event| matches!(
            event,
            HookProgressEvent::Cancelled {
                event: HookEvent::PreToolUse,
                ..
            }
        )));
    }

    #[cfg(windows)]
    fn shell_snippet(script: &str) -> String {
        script.replace('\'', "\"")
    }

    #[cfg(not(windows))]
    fn shell_snippet(script: &str) -> String {
        script.to_string()
    }
}
