use super::*;

fn validate_contract_allowed_keys(
    value: &serde_json::Value,
    allowed_keys: &[&str],
    contract_name: &str,
) -> Result<(), String> {
    let Some(object) = value.as_object() else {
        return Err(format!("{contract_name} must be a JSON object"));
    };
    for key in object.keys() {
        if !allowed_keys.iter().any(|allowed| *allowed == key) {
            return Err(format!(
                "{contract_name}.{key} is not allowed (strict contract mode)"
            ));
        }
    }
    Ok(())
}

fn value_array_of_strings(value: Option<&serde_json::Value>) -> Option<Vec<String>> {
    let items = value?.as_array()?;
    let out = items
        .iter()
        .filter_map(|item| item.as_str())
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(std::string::ToString::to_string)
        .collect::<Vec<_>>();
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

fn value_u64(value: Option<&serde_json::Value>) -> Option<u64> {
    value.and_then(|v| {
        if let Some(x) = v.as_u64() {
            Some(x)
        } else if let Some(x) = v.as_i64() {
            u64::try_from(x).ok()
        } else {
            None
        }
    })
}

fn value_f64(value: Option<&serde_json::Value>) -> Option<f64> {
    value.and_then(|v| {
        if let Some(x) = v.as_f64() {
            Some(x)
        } else if let Some(x) = v.as_i64() {
            Some(x as f64)
        } else if let Some(x) = v.as_u64() {
            Some(x as f64)
        } else {
            None
        }
    })
}

fn value_array_of_strings_allow_empty(value: Option<&serde_json::Value>) -> Option<Vec<String>> {
    let items = value?.as_array()?;
    let out = items
        .iter()
        .filter_map(|item| item.as_str())
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(std::string::ToString::to_string)
        .collect::<Vec<_>>();
    Some(out)
}

fn normalize_exec_constraints_text(preface_text: &str) -> String {
    preface_text
        .replace("<EXEC_CONSTRAINTS>", "EXEC_CONSTRAINTS ")
        .replace("</EXEC_CONSTRAINTS>", "")
        .replace("```json", "")
        .replace("```JSON", "")
        .replace("```", "")
}

pub(super) fn extract_pm_exec_constraints(
    preface_text: &str,
    runtime_budget: &PmTimeoutBudget,
) -> Result<PmExecConstraints, String> {
    let normalized = normalize_exec_constraints_text(preface_text);
    let contract_json = extract_named_json_object(&normalized, "EXEC_CONSTRAINTS")
        .or_else(|| {
            extract_first_json_object(&normalized).and_then(|raw| parse_json_object_relaxed(&raw))
        })
        .ok_or_else(|| "missing EXEC_CONSTRAINTS".to_string())?;
    validate_exec_constraints_contract(&contract_json, runtime_budget)?;

    let allowlist = value_array_of_strings(contract_json.get("routeAllowlist"))
        .ok_or_else(|| "EXEC_CONSTRAINTS.routeAllowlist missing/empty".to_string())?;
    let route_priority_raw = value_array_of_strings(contract_json.get("routePriority"))
        .ok_or_else(|| "EXEC_CONSTRAINTS.routePriority missing/empty".to_string())?;
    let stop_conditions = value_array_of_strings(contract_json.get("stopConditions"))
        .ok_or_else(|| "EXEC_CONSTRAINTS.stopConditions missing/empty".to_string())?;
    let source_slot_budget_secs = value_u64(contract_json.get("sourceSlotBudgetSecs"))
        .ok_or_else(|| "EXEC_CONSTRAINTS.sourceSlotBudgetSecs missing".to_string())?;
    let tool_budget_per_attempt = value_u64(contract_json.get("toolBudgetPerAttempt"))
        .ok_or_else(|| "EXEC_CONSTRAINTS.toolBudgetPerAttempt missing".to_string())?;
    let pipeline_timeout_secs = value_u64(contract_json.get("pipelineTimeoutSecs"))
        .ok_or_else(|| "EXEC_CONSTRAINTS.pipelineTimeoutSecs missing".to_string())?;

    let mut allowlist_seen = HashSet::<String>::new();
    let mut normalized_allowlist: Vec<String> = Vec::new();
    for item in allowlist {
        let key = item.trim().to_ascii_lowercase();
        if key.is_empty() || !allowlist_seen.insert(key) {
            continue;
        }
        normalized_allowlist.push(item);
    }

    let mut priority_seen = HashSet::<String>::new();
    let mut normalized_priority: Vec<String> = Vec::new();
    for item in route_priority_raw {
        let key = item.trim().to_ascii_lowercase();
        if key.is_empty() || !allowlist_seen.contains(&key) || !priority_seen.insert(key) {
            continue;
        }
        normalized_priority.push(item);
    }
    for allow in &normalized_allowlist {
        let key = allow.trim().to_ascii_lowercase();
        if priority_seen.insert(key) {
            normalized_priority.push(allow.clone());
        }
    }

    Ok(PmExecConstraints {
        route_allowlist: normalized_allowlist,
        route_priority: normalized_priority,
        stop_conditions,
        source_slot_budget_secs,
        tool_budget_per_attempt: usize::try_from(tool_budget_per_attempt).unwrap_or(usize::MAX),
        pipeline_timeout_secs,
    })
}

pub(super) fn validate_exec_constraints_contract(
    value: &serde_json::Value,
    _runtime_budget: &PmTimeoutBudget,
) -> Result<(), String> {
    validate_contract_allowed_keys(
        value,
        &[
            "routeAllowlist",
            "routePriority",
            "stopConditions",
            "sourceSlotBudgetSecs",
            "toolBudgetPerAttempt",
            "pipelineTimeoutSecs",
        ],
        "EXEC_CONSTRAINTS",
    )?;
    let allowlist = value_array_of_strings(value.get("routeAllowlist"))
        .ok_or_else(|| "EXEC_CONSTRAINTS.routeAllowlist missing/empty".to_string())?;
    if allowlist.len() > 20 {
        return Err("EXEC_CONSTRAINTS.routeAllowlist too long".to_string());
    }
    if value_array_of_strings(value.get("routePriority")).is_none() {
        return Err("EXEC_CONSTRAINTS.routePriority missing/empty".to_string());
    }
    if value_array_of_strings(value.get("stopConditions")).is_none() {
        return Err("EXEC_CONSTRAINTS.stopConditions missing/empty".to_string());
    }
    let slot = value_u64(value.get("sourceSlotBudgetSecs"))
        .ok_or_else(|| "EXEC_CONSTRAINTS.sourceSlotBudgetSecs missing".to_string())?;
    let tool = value_u64(value.get("toolBudgetPerAttempt"))
        .ok_or_else(|| "EXEC_CONSTRAINTS.toolBudgetPerAttempt missing".to_string())?;
    let pipe = value_u64(value.get("pipelineTimeoutSecs"))
        .ok_or_else(|| "EXEC_CONSTRAINTS.pipelineTimeoutSecs missing".to_string())?;
    if slot == 0 {
        return Err("EXEC_CONSTRAINTS.sourceSlotBudgetSecs must be > 0".to_string());
    }
    if tool == 0 {
        return Err("EXEC_CONSTRAINTS.toolBudgetPerAttempt must be > 0".to_string());
    }
    if pipe == 0 {
        return Err("EXEC_CONSTRAINTS.pipelineTimeoutSecs must be > 0".to_string());
    }
    Ok(())
}

pub(super) fn detect_exec_constraints_issue(
    preface_text: &str,
    runtime_budget: &PmTimeoutBudget,
) -> Option<String> {
    extract_pm_exec_constraints(preface_text, runtime_budget)
        .err()
        .map(|err| err.to_string())
}

fn validate_retrieve_constraints_contract(
    value: &serde_json::Value,
    runtime_budget: &PmTimeoutBudget,
) -> Result<(), String> {
    validate_contract_allowed_keys(
        value,
        &[
            "route",
            "variant",
            "toolBudget",
            "sourceSlotBudgetSecs",
            "maxCallsPerSource",
            "blockedDomains",
        ],
        "RETRIEVE_CONSTRAINTS",
    )?;
    let route = value
        .get("route")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("");
    if route.is_empty() {
        return Err("RETRIEVE_CONSTRAINTS.route missing".to_string());
    }
    let variant = value
        .get("variant")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("");
    if variant.is_empty() {
        return Err("RETRIEVE_CONSTRAINTS.variant missing".to_string());
    }
    let tool_budget = value_u64(value.get("toolBudget"))
        .ok_or_else(|| "RETRIEVE_CONSTRAINTS.toolBudget missing".to_string())?;
    let slot = value_u64(value.get("sourceSlotBudgetSecs"))
        .ok_or_else(|| "RETRIEVE_CONSTRAINTS.sourceSlotBudgetSecs missing".to_string())?;
    let max_calls = value_u64(value.get("maxCallsPerSource"))
        .ok_or_else(|| "RETRIEVE_CONSTRAINTS.maxCallsPerSource missing".to_string())?;
    if value
        .get("blockedDomains")
        .and_then(|v| v.as_array())
        .is_none()
    {
        return Err("RETRIEVE_CONSTRAINTS.blockedDomains missing".to_string());
    }
    if tool_budget > u64::try_from(runtime_budget.retrieve_max_tool_calls).unwrap_or(u64::MAX) {
        return Err("RETRIEVE_CONSTRAINTS.toolBudget exceeds budget".to_string());
    }
    if slot
        > runtime_budget
            .source_slot_search_secs
            .max(runtime_budget.source_slot_browser_secs)
    {
        return Err("RETRIEVE_CONSTRAINTS.sourceSlotBudgetSecs exceeds budget".to_string());
    }
    if max_calls > u64::try_from(runtime_budget.max_calls_per_source).unwrap_or(u64::MAX) {
        return Err("RETRIEVE_CONSTRAINTS.maxCallsPerSource exceeds budget".to_string());
    }
    Ok(())
}

fn validate_retrieve_result_contract(value: &serde_json::Value) -> Result<(), String> {
    validate_contract_allowed_keys(
        value,
        &["route", "attempt", "toolCalls", "citationUrls", "domains"],
        "RETRIEVE_RESULT",
    )?;
    let route = value
        .get("route")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("");
    if route.is_empty() {
        return Err("RETRIEVE_RESULT.route missing".to_string());
    }
    if value_u64(value.get("attempt")).is_none() {
        return Err("RETRIEVE_RESULT.attempt missing".to_string());
    }
    if value_u64(value.get("toolCalls")).is_none() {
        return Err("RETRIEVE_RESULT.toolCalls missing".to_string());
    }
    if value_u64(value.get("citationUrls")).is_none() {
        return Err("RETRIEVE_RESULT.citationUrls missing".to_string());
    }
    if value_u64(value.get("domains")).is_none() {
        return Err("RETRIEVE_RESULT.domains missing".to_string());
    }
    Ok(())
}

fn validate_repair_scope_contract(
    value: &serde_json::Value,
    runtime_budget: &PmTimeoutBudget,
    retry_budget_cap_secs: u64,
) -> Result<(), String> {
    validate_contract_allowed_keys(
        value,
        &[
            "repairOnly",
            "forbidden",
            "blockedDomains",
            "toolBudget",
            "sourceSlotBudgetSecs",
            "maxCallsPerSource",
            "retryBudgetSecs",
        ],
        "REPAIR_SCOPE",
    )?;
    if value_array_of_strings(value.get("repairOnly")).is_none() {
        return Err("REPAIR_SCOPE.repairOnly missing/empty".to_string());
    }
    if value_array_of_strings_allow_empty(value.get("forbidden")).is_none() {
        return Err("REPAIR_SCOPE.forbidden missing".to_string());
    }
    if value_array_of_strings_allow_empty(value.get("blockedDomains")).is_none() {
        return Err("REPAIR_SCOPE.blockedDomains missing".to_string());
    }
    let tool_budget = value_u64(value.get("toolBudget"))
        .ok_or_else(|| "REPAIR_SCOPE.toolBudget missing".to_string())?;
    let slot = value_u64(value.get("sourceSlotBudgetSecs"))
        .ok_or_else(|| "REPAIR_SCOPE.sourceSlotBudgetSecs missing".to_string())?;
    let max_calls = value_u64(value.get("maxCallsPerSource"))
        .ok_or_else(|| "REPAIR_SCOPE.maxCallsPerSource missing".to_string())?;
    let retry_budget = value_u64(value.get("retryBudgetSecs"))
        .ok_or_else(|| "REPAIR_SCOPE.retryBudgetSecs missing".to_string())?;

    if tool_budget > u64::try_from(runtime_budget.retrieve_max_tool_calls).unwrap_or(u64::MAX) {
        return Err("REPAIR_SCOPE.toolBudget exceeds budget".to_string());
    }
    if slot
        > runtime_budget
            .source_slot_search_secs
            .max(runtime_budget.source_slot_browser_secs)
            .max(runtime_budget.source_slot_api_fetch_secs)
    {
        return Err("REPAIR_SCOPE.sourceSlotBudgetSecs exceeds budget".to_string());
    }
    if max_calls > u64::try_from(runtime_budget.max_calls_per_source).unwrap_or(u64::MAX) {
        return Err("REPAIR_SCOPE.maxCallsPerSource exceeds budget".to_string());
    }
    if retry_budget > retry_budget_cap_secs {
        return Err("REPAIR_SCOPE.retryBudgetSecs exceeds budget".to_string());
    }
    Ok(())
}

fn validate_repair_result_contract(value: &serde_json::Value) -> Result<(), String> {
    validate_contract_allowed_keys(
        value,
        &[
            "attempt",
            "strategy",
            "repairedClaims",
            "remainingGaps",
            "toolCalls",
            "citationUrls",
        ],
        "REPAIR_RESULT",
    )?;
    if value_u64(value.get("attempt")).is_none() {
        return Err("REPAIR_RESULT.attempt missing".to_string());
    }
    let strategy = value
        .get("strategy")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("");
    if strategy.is_empty() {
        return Err("REPAIR_RESULT.strategy missing".to_string());
    }
    if value_array_of_strings_allow_empty(value.get("repairedClaims")).is_none() {
        return Err("REPAIR_RESULT.repairedClaims missing".to_string());
    }
    if value_array_of_strings_allow_empty(value.get("remainingGaps")).is_none() {
        return Err("REPAIR_RESULT.remainingGaps missing".to_string());
    }
    if value_u64(value.get("toolCalls")).is_none() {
        return Err("REPAIR_RESULT.toolCalls missing".to_string());
    }
    if value_u64(value.get("citationUrls")).is_none() {
        return Err("REPAIR_RESULT.citationUrls missing".to_string());
    }
    Ok(())
}

fn validate_synthesis_meta_contract(value: &serde_json::Value) -> Result<(), String> {
    validate_contract_allowed_keys(
        value,
        &[
            "attempt",
            "mode",
            "evidenceConfidence",
            "confirmedCount",
            "unverifiedCount",
            "riskCount",
        ],
        "SYNTHESIS_META",
    )?;
    if value_u64(value.get("attempt")).is_none() {
        return Err("SYNTHESIS_META.attempt missing".to_string());
    }
    let mode = value
        .get("mode")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("");
    if mode != "degraded" && mode != "normal" {
        return Err("SYNTHESIS_META.mode invalid".to_string());
    }
    let confidence = value_f64(value.get("evidenceConfidence"))
        .ok_or_else(|| "SYNTHESIS_META.evidenceConfidence missing".to_string())?;
    if !(0.0..=1.0).contains(&confidence) {
        return Err("SYNTHESIS_META.evidenceConfidence out of range".to_string());
    }
    if value_u64(value.get("confirmedCount")).is_none() {
        return Err("SYNTHESIS_META.confirmedCount missing".to_string());
    }
    if value_u64(value.get("unverifiedCount")).is_none() {
        return Err("SYNTHESIS_META.unverifiedCount missing".to_string());
    }
    if value_u64(value.get("riskCount")).is_none() {
        return Err("SYNTHESIS_META.riskCount missing".to_string());
    }
    Ok(())
}

fn validate_report_json_contract(value: &serde_json::Value) -> Result<(), String> {
    let obj = value
        .as_object()
        .ok_or_else(|| "REPORT_JSON must be a JSON object".to_string())?;
    let summary = obj
        .get("summary")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("");
    if summary.is_empty() {
        return Err("REPORT_JSON.summary missing".to_string());
    }
    let highlights = obj
        .get("highlights")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "REPORT_JSON.highlights missing".to_string())?;
    if highlights.is_empty() {
        return Err("REPORT_JSON.highlights empty".to_string());
    }
    let sections = obj
        .get("sections")
        .and_then(|v| v.as_object())
        .ok_or_else(|| "REPORT_JSON.sections missing".to_string())?;
    for key in ["confirmed", "pending", "risks", "actions"] {
        if sections.get(key).and_then(|v| v.as_array()).is_none() {
            return Err(format!("REPORT_JSON.sections.{key} missing"));
        }
    }
    let triads = obj
        .get("evidenceTriads")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "REPORT_JSON.evidenceTriads missing".to_string())?;
    if triads.is_empty() {
        return Err("REPORT_JSON.evidenceTriads empty".to_string());
    }
    let quant = obj
        .get("quant")
        .and_then(|v| v.as_object())
        .ok_or_else(|| "REPORT_JSON.quant missing".to_string())?;
    if quant.get("enabled").and_then(|v| v.as_bool()).is_none() {
        return Err("REPORT_JSON.quant.enabled missing".to_string());
    }
    if let Some(layers) = obj.get("deepResearchLayers") {
        let layer_obj = layers
            .as_object()
            .ok_or_else(|| "REPORT_JSON.deepResearchLayers must be object".to_string())?;
        if layer_obj
            .get("breadthScan")
            .and_then(|v| v.as_array())
            .is_none()
        {
            return Err("REPORT_JSON.deepResearchLayers.breadthScan missing".to_string());
        }
        if layer_obj
            .get("priorityDeepDives")
            .and_then(|v| v.as_array())
            .is_none()
        {
            return Err("REPORT_JSON.deepResearchLayers.priorityDeepDives missing".to_string());
        }
        if layer_obj
            .get("counterEvidenceChecks")
            .and_then(|v| v.as_array())
            .is_none()
        {
            return Err("REPORT_JSON.deepResearchLayers.counterEvidenceChecks missing".to_string());
        }
        if let Some(action_plan) = layer_obj.get("actionPlan") {
            let action_obj = action_plan
                .as_object()
                .ok_or_else(|| "REPORT_JSON.deepResearchLayers.actionPlan invalid".to_string())?;
            for key in ["now", "next", "later"] {
                if action_obj.get(key).and_then(|v| v.as_array()).is_none() {
                    return Err(format!(
                        "REPORT_JSON.deepResearchLayers.actionPlan.{key} missing"
                    ));
                }
            }
        }
    }
    if let Some(metric_model) = obj.get("metricModel") {
        let metric_obj = metric_model
            .as_object()
            .ok_or_else(|| "REPORT_JSON.metricModel must be object".to_string())?;
        if metric_obj
            .get("metrics")
            .and_then(|v| v.as_array())
            .is_none()
        {
            return Err("REPORT_JSON.metricModel.metrics missing".to_string());
        }
        if let Some(coverage) = metric_obj.get("coverage") {
            let coverage_obj = coverage
                .as_object()
                .ok_or_else(|| "REPORT_JSON.metricModel.coverage must be object".to_string())?;
            for key in [
                "structuredMetricCount",
                "timeSeriesCount",
                "sourceTraceCount",
            ] {
                if value_u64(coverage_obj.get(key)).is_none() {
                    return Err(format!("REPORT_JSON.metricModel.coverage.{key} missing"));
                }
            }
        }
    }
    if let Some(strategy) = obj.get("reportStrategy") {
        let strategy_obj = strategy
            .as_object()
            .ok_or_else(|| "REPORT_JSON.reportStrategy must be object".to_string())?;
        if strategy_obj
            .get("layout")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .unwrap_or("")
            .is_empty()
        {
            return Err("REPORT_JSON.reportStrategy.layout missing".to_string());
        }
        if let Some(order) = strategy_obj.get("sectionOrder") {
            if order.as_array().is_none() {
                return Err("REPORT_JSON.reportStrategy.sectionOrder must be array".to_string());
            }
        }
    }
    Ok(())
}

pub(super) fn apply_pm_contract_gate(
    quality: &mut PmAnswerQualityDto,
    text: &str,
    runtime_budget: &PmTimeoutBudget,
) {
    let mut contract_issues: Vec<String> = Vec::new();
    let mut contract_warnings: Vec<String> = Vec::new();
    let upper = text.to_ascii_uppercase();
    let has_retrieve_contract =
        upper.contains("RETRIEVE_CONSTRAINTS") || upper.contains("RETRIEVE_RESULT");
    let has_repair_contract = upper.contains("REPAIR_SCOPE") || upper.contains("REPAIR_RESULT");
    let has_synthesis_contract = upper.contains("SYNTHESIS_META");
    let has_report_contract = upper.contains("REPORT_JSON");
    let mut checked_contract = false;

    if has_retrieve_contract {
        checked_contract = true;
        match extract_named_json_object(text, "RETRIEVE_CONSTRAINTS") {
            Some(value) => {
                if let Err(err) = validate_retrieve_constraints_contract(&value, runtime_budget) {
                    contract_issues.push(err);
                }
            }
            None => contract_issues.push("missing RETRIEVE_CONSTRAINTS".to_string()),
        }
        match extract_named_json_object(text, "RETRIEVE_RESULT") {
            Some(value) => {
                if let Err(err) = validate_retrieve_result_contract(&value) {
                    contract_issues.push(err);
                }
            }
            None => contract_issues.push("missing RETRIEVE_RESULT".to_string()),
        }
    }

    if has_repair_contract {
        checked_contract = true;
        match extract_named_json_object(text, "REPAIR_SCOPE") {
            Some(value) => {
                if let Err(err) = validate_repair_scope_contract(
                    &value,
                    runtime_budget,
                    runtime_budget.retry_step_budget_secs.max(1),
                ) {
                    contract_issues.push(err);
                }
            }
            None => contract_issues.push("missing REPAIR_SCOPE".to_string()),
        }
        match extract_named_json_object(text, "REPAIR_RESULT") {
            Some(value) => {
                if let Err(err) = validate_repair_result_contract(&value) {
                    contract_issues.push(err);
                }
            }
            None => contract_issues.push("missing REPAIR_RESULT".to_string()),
        }
    }

    if has_synthesis_contract {
        checked_contract = true;
        match extract_named_json_object(text, "SYNTHESIS_META") {
            Some(value) => {
                if let Err(err) = validate_synthesis_meta_contract(&value) {
                    contract_issues.push(err);
                }
            }
            None => contract_issues.push("missing SYNTHESIS_META".to_string()),
        }
    }

    if has_report_contract {
        checked_contract = true;
        match extract_named_json_object(text, "REPORT_JSON") {
            Some(value) => {
                if let Err(err) = validate_report_json_contract(&value) {
                    contract_issues.push(err);
                }
            }
            None => contract_issues.push("missing REPORT_JSON".to_string()),
        }
    } else if has_synthesis_contract {
        contract_warnings.push("missing REPORT_JSON".to_string());
    }

    if !checked_contract {
        return;
    }

    if !contract_issues.is_empty() {
        quality.passed = false;
        quality.claim_alignment_ok = false;
        for issue in contract_issues {
            let entry = format!("contract_invalid:{issue}");
            if !quality.missing.iter().any(|x| x == &entry) {
                quality.missing.push(entry);
            }
        }
        quality.suggestions.push(
            "Re-run synthesis with clearer natural-language conclusions and stronger evidence alignment."
                .to_string(),
        );
    }
    for warning in contract_warnings {
        let entry = format!("contract_warn:{warning}");
        if !quality.missing.iter().any(|x| x == &entry) {
            quality.missing.push(entry);
        }
    }
}
