//! Runtime tool allowlist and safety policy for RD agent sessions.

use std::collections::BTreeSet;

#[derive(Debug, Clone)]
pub struct RdRuntimeToolPolicy {
    pub allowed_tools: Vec<String>,
    pub source: &'static str,
    pub bash_enabled: bool,
    pub filtered_bash: bool,
    pub write_tools_enabled: bool,
    pub filtered_write_tools: Vec<String>,
}

pub fn ensure_rd_candidate_runtime_tools_with_bash(
    allowed_tools: Option<Vec<String>>,
    bash_enabled: bool,
) -> Vec<String> {
    let mut tools = allowed_tools.unwrap_or_else(default_rd_candidate_runtime_tools);
    tools.extend(
        [
            "read_file",
            "grep_search",
            "glob_search",
            "write_file",
            "edit_file",
            "rd_validate_diff",
        ]
        .into_iter()
        .map(ToOwned::to_owned),
    );
    if bash_enabled {
        tools.push("bash".to_string());
    } else {
        tools.retain(|tool| !rd_tool_is_bash(tool));
    }
    stable_rd_tool_list(tools)
}

pub fn resolve_rd_runtime_tool_policy_with_switches(
    mode: &str,
    allowed_tools: Option<Vec<String>>,
    bash_enabled: bool,
    write_tools_enabled: bool,
) -> RdRuntimeToolPolicy {
    let mut tools = match allowed_tools {
        Some(tools) if !tools.is_empty() => tools,
        _ => default_rd_runtime_tools_for_mode(mode),
    };
    let source = if tools.is_empty() {
        "default"
    } else if default_rd_runtime_tools_for_mode(mode) == tools {
        "default"
    } else {
        "agent_profile"
    };
    tools.push("rd_validate_diff".to_string());

    let had_bash = tools.iter().any(|tool| rd_tool_is_bash(tool));
    if bash_enabled {
        if had_bash || rd_runtime_default_bash_enabled(mode, bash_enabled) {
            tools.push("bash".to_string());
        }
    } else {
        tools.retain(|tool| !rd_tool_is_bash(tool));
    }

    let filtered_write_tools = if write_tools_enabled {
        Vec::new()
    } else {
        tools
            .iter()
            .filter(|tool| rd_tool_is_direct_write_tool(tool))
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
    };
    if !write_tools_enabled {
        tools.retain(|tool| !rd_tool_is_direct_write_tool(tool));
    }

    let allowed_tools = stable_rd_tool_list(tools);
    RdRuntimeToolPolicy {
        allowed_tools,
        source,
        bash_enabled,
        filtered_bash: had_bash && !bash_enabled,
        write_tools_enabled,
        filtered_write_tools,
    }
}

pub fn default_rd_runtime_tools_for_mode(mode: &str) -> Vec<String> {
    let tools: &[&str] = match mode {
        "modify" | "review" => &[
            "read_file",
            "grep_search",
            "glob_search",
            "git_status",
            "git_diff",
            "git_log",
            "git_show",
            "git_blame",
            "skill",
            "tool_search",
            "rd_validate_diff",
        ],
        "explain" => &[
            "read_file",
            "grep_search",
            "glob_search",
            "git_status",
            "git_log",
            "git_show",
            "git_blame",
            "skill",
            "tool_search",
            "rd_validate_diff",
        ],
        _ => &[
            "read_file",
            "grep_search",
            "glob_search",
            "git_status",
            "git_diff",
            "git_log",
            "git_show",
            "git_blame",
            "skill",
            "tool_search",
            "rd_validate_diff",
        ],
    };
    tools.into_iter().map(|tool| (*tool).to_string()).collect()
}

pub fn default_rd_candidate_runtime_tools() -> Vec<String> {
    [
        "read_file",
        "grep_search",
        "glob_search",
        "write_file",
        "edit_file",
        "git_status",
        "git_diff",
        "git_log",
        "git_show",
        "git_blame",
        "skill",
        "tool_search",
        "rd_validate_diff",
    ]
    .into_iter()
    .map(ToOwned::to_owned)
    .collect()
}

pub fn stable_rd_tool_list(tools: Vec<String>) -> Vec<String> {
    tools
        .into_iter()
        .map(|tool| tool.trim().to_string())
        .filter(|tool| !tool.is_empty())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

pub fn rd_tool_is_bash(tool: &str) -> bool {
    compact_rd_tool_name(tool).eq_ignore_ascii_case("bash")
}

pub fn rd_tool_is_direct_write_tool(tool: &str) -> bool {
    matches!(
        compact_rd_tool_name(tool).as_str(),
        "writefile" | "editfile" | "notebookedit"
    )
}

pub fn compact_rd_tool_name(tool: &str) -> String {
    tool.chars()
        .filter(|ch| !ch.is_whitespace() && *ch != '_' && *ch != '-')
        .collect::<String>()
        .to_ascii_lowercase()
}

pub const fn rd_runtime_default_bash_enabled(_mode: &str, bash_enabled: bool) -> bool {
    bash_enabled
}
