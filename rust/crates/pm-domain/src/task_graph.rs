use std::collections::{HashMap, HashSet};

use crate::json_utils::extract_named_json_object;
use crate::planner::PmExecConstraints;
use crate::query_hygiene::{sanitize_pm_search_queries, sanitize_pm_search_query};
use crate::report_strategy::{attach_pm_report_strategy_hint, pm_is_report_strategy_mode};
use crate::route_plan::{
    build_pm_query_variants, build_pm_source_routes, pm_question_likely_requires_external_evidence,
    resolve_pm_plan_channels,
};

fn normalize_claim_key(input: &str) -> String {
    input
        .trim()
        .trim_matches(|ch: char| ch == '"' || ch == '\'' || ch == '`')
        .to_ascii_lowercase()
}

pub fn contains_cjk(text: &str) -> bool {
    text.chars()
        .any(|ch| ('\u{4e00}'..='\u{9fff}').contains(&ch))
}

pub fn sanitize_pm_task_graph_text(raw: &str, max_chars: usize) -> Option<String> {
    let compact = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = compact.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.chars().take(max_chars).collect())
}

pub fn sanitize_pm_task_graph_queries(
    value: Option<&serde_json::Value>,
    fallback: &str,
    max_items: usize,
) -> Vec<String> {
    let mut raw_items = Vec::<String>::new();
    if let Some(arr) = value.and_then(|v| v.as_array()) {
        for item in arr.iter().take(max_items * 2) {
            let Some(raw) = item.as_str() else {
                continue;
            };
            if let Some(cleaned) = sanitize_pm_task_graph_text(raw, 180) {
                raw_items.push(cleaned);
            }
        }
    }
    sanitize_pm_search_queries(raw_items, Some(fallback), max_items)
}

pub fn normalize_task_graph_priority(raw: Option<&str>) -> String {
    let normalized = raw.unwrap_or("medium").trim().to_ascii_lowercase();
    if normalized == "high" || normalized == "low" {
        normalized
    } else {
        "medium".to_string()
    }
}

pub fn normalize_pm_required_evidence_type(raw: Option<&str>) -> Option<String> {
    let normalized = raw?
        .trim()
        .replace('-', "_")
        .replace(' ', "_")
        .to_ascii_lowercase();
    let normalized = match normalized.as_str() {
        "external" | "web" | "public" | "public_web" | "source" | "source_backed" | "market"
        | "benchmark" | "case" | "competitive" | "competitor" => "external",
        "mixed" | "both" | "hybrid" | "first_party_plus_external" | "internal_plus_external" => {
            "mixed"
        }
        "first_party" | "firstparty" | "user_data" | "provided_data" | "provided_context"
        | "internal" | "private" | "model_reasoning" | "reasoning" | "analysis" => "first_party",
        _ => return None,
    };
    Some(normalized.to_string())
}

pub fn infer_pm_subtask_required_evidence_type(
    title: Option<&str>,
    goal: Option<&str>,
    deliverable: Option<&str>,
    queries: &[String],
) -> String {
    let text = [
        title.unwrap_or(""),
        goal.unwrap_or(""),
        deliverable.unwrap_or(""),
    ]
    .into_iter()
    .chain(queries.iter().map(String::as_str))
    .collect::<Vec<_>>()
    .join(" ");
    let lower = text.to_ascii_lowercase();
    let has_external_signal = pm_subtask_text_has_external_signal(&text, &lower);
    let has_first_party_signal = pm_subtask_text_has_first_party_signal(&text, &lower);

    if has_external_signal && has_first_party_signal {
        "mixed".to_string()
    } else if has_external_signal {
        "external".to_string()
    } else if has_first_party_signal || queries.is_empty() {
        "first_party".to_string()
    } else {
        "external".to_string()
    }
}

pub fn pm_subtask_allows_external_probe(required_evidence_type: Option<&str>) -> bool {
    match normalize_pm_required_evidence_type(required_evidence_type)
        .as_deref()
        .unwrap_or("external")
    {
        "first_party" => false,
        "external" | "mixed" => true,
        _ => true,
    }
}

fn pm_subtask_text_has_external_signal(text: &str, lower: &str) -> bool {
    const EN: &[&str] = &[
        "external",
        "public",
        "web",
        "source",
        "citation",
        "benchmark",
        "case study",
        "comparable",
        "competitor",
        "competitive",
        "market",
        "industry",
        "latest",
        "current",
        "policy",
        "regulation",
        "best practice",
        "playbook",
    ];
    const ZH: &[&str] = &[
        "外部",
        "公开",
        "联网",
        "检索",
        "搜索",
        "来源",
        "引用",
        "案例",
        "可比",
        "竞品",
        "对标",
        "市场",
        "行业",
        "最新",
        "实时",
        "政策",
        "监管",
        "法规",
        "标杆",
        "基准",
        "最佳实践",
        "玩法参考",
    ];
    EN.iter().any(|token| lower.contains(token)) || ZH.iter().any(|token| text.contains(token))
}

fn pm_subtask_text_has_first_party_signal(text: &str, lower: &str) -> bool {
    const EN: &[&str] = &[
        "first-party",
        "first party",
        "user-provided",
        "provided data",
        "provided report",
        "internal data",
        "internal report",
        "private data",
        "cohort from the report",
        "based on the user's data",
    ];
    const ZH: &[&str] = &[
        "一手",
        "用户提供",
        "用户报告",
        "内部数据",
        "内部报告",
        "我给",
        "给定数据",
        "已提供",
        "基于数据",
        "基于报告",
        "报告中",
        "人群分层",
        "指标拆解",
        "归因",
    ];
    EN.iter().any(|token| lower.contains(token)) || ZH.iter().any(|token| text.contains(token))
}

pub fn build_pm_subtask_key(
    subtask_id: Option<&str>,
    title: Option<&str>,
    goal: Option<&str>,
    index: usize,
) -> String {
    if let Some(id) = subtask_id.and_then(|raw| sanitize_pm_task_graph_text(raw, 80)) {
        let key = normalize_claim_key(&id);
        if !key.is_empty() {
            return key;
        }
    }
    if let Some(title) = title.and_then(|raw| sanitize_pm_task_graph_text(raw, 120)) {
        let key = normalize_claim_key(&title);
        if !key.is_empty() {
            return key;
        }
    }
    if let Some(goal) = goal.and_then(|raw| sanitize_pm_task_graph_text(raw, 120)) {
        let key = normalize_claim_key(&goal);
        if !key.is_empty() {
            return key;
        }
    }
    format!("subtask_{}", index.saturating_add(1))
}

pub fn parse_pm_complexity_score(
    raw: Option<&serde_json::Value>,
    default: u64,
) -> (u64, Option<f64>) {
    let Some(value) = raw else {
        return (default.clamp(0, 100), None);
    };
    if let Some(v) = value.as_u64() {
        return (v.clamp(0, 100), Some(v as f64));
    }
    if let Some(v) = value.as_i64() {
        if v >= 0 {
            let as_u64 = u64::try_from(v).unwrap_or(0);
            return (as_u64.clamp(0, 100), Some(v as f64));
        }
    }
    let parse_float = |num: f64| -> Option<(u64, f64)> {
        if !num.is_finite() || num < 0.0 {
            return None;
        }
        // Accept either 0..10 (model-style complexity) or 0..100 (runtime-style score).
        let normalized = if num <= 10.0 {
            (num * 10.0).round() as u64
        } else {
            num.round() as u64
        };
        Some((normalized.clamp(0, 100), num))
    };
    if let Some(v) = value.as_f64() {
        if let Some((normalized, raw_float)) = parse_float(v) {
            return (normalized, Some(raw_float));
        }
    }
    if let Some(raw_text) = value.as_str() {
        if let Ok(parsed) = raw_text.trim().parse::<f64>() {
            if let Some((normalized, raw_float)) = parse_float(parsed) {
                return (normalized, Some(raw_float));
            }
        }
    }
    (default.clamp(0, 100), None)
}

pub fn extract_pm_task_graph(preface_text: &str) -> Option<serde_json::Value> {
    let raw_graph = extract_named_json_object(preface_text, "TASK_GRAPH_V2")
        .or_else(|| extract_named_json_object(preface_text, "TASK_GRAPH"))
        .or_else(|| extract_named_json_object(preface_text, "TASK_DECOMPOSITION"))?;
    let obj = raw_graph.as_object()?;
    let intent = obj
        .get("intent")
        .and_then(|v| v.as_str())
        .and_then(|raw| sanitize_pm_task_graph_text(raw, 40))
        .unwrap_or_else(|| "research".to_string());
    let (complexity_score, complexity_score_raw) = parse_pm_complexity_score(
        obj.get("complexityScore")
            .or_else(|| obj.get("complexity_score")),
        60,
    );
    let decomposition_mode_raw = obj
        .get("decompositionMode")
        .or_else(|| obj.get("decomposition_mode"))
        .and_then(|v| v.as_str())
        .map(|raw| raw.trim().to_ascii_lowercase())
        .filter(|raw| matches!(raw.as_str(), "none" | "light" | "full"));
    let decomposition_mode = decomposition_mode_raw.unwrap_or_else(|| {
        if complexity_score >= 72 {
            "full".to_string()
        } else {
            "light".to_string()
        }
    });
    let subtask_items = obj
        .get("subtasks")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    if decomposition_mode != "none" && subtask_items.is_empty() {
        return None;
    }

    let mut subtasks = Vec::<serde_json::Value>::new();
    let subtask_limit = if complexity_score >= 72 { 24 } else { 12 };
    for (idx, subtask) in subtask_items.iter().take(subtask_limit).enumerate() {
        let Some(task_obj) = subtask.as_object() else {
            continue;
        };
        let title = task_obj
            .get("title")
            .and_then(|v| v.as_str())
            .and_then(|raw| sanitize_pm_task_graph_text(raw, 90))
            .unwrap_or_else(|| format!("subtask-{}", idx + 1));
        let goal = task_obj
            .get("goal")
            .and_then(|v| v.as_str())
            .and_then(|raw| sanitize_pm_task_graph_text(raw, 180))
            .unwrap_or_else(|| title.clone());
        let id = task_obj
            .get("id")
            .and_then(|v| v.as_str())
            .and_then(|raw| sanitize_pm_task_graph_text(raw, 60))
            .unwrap_or_else(|| format!("task-{}", idx + 1));
        let queries = sanitize_pm_task_graph_queries(task_obj.get("queries"), &goal, 4);
        let deliverable = task_obj
            .get("deliverable")
            .and_then(|v| v.as_str())
            .and_then(|raw| sanitize_pm_task_graph_text(raw, 180))
            .unwrap_or_else(|| goal.clone());
        let required_evidence_type = normalize_pm_required_evidence_type(
            task_obj
                .get("requiredEvidenceType")
                .or_else(|| task_obj.get("required_evidence_type"))
                .or_else(|| task_obj.get("evidenceType"))
                .or_else(|| task_obj.get("evidence_type"))
                .and_then(|v| v.as_str()),
        )
        .unwrap_or_else(|| {
            infer_pm_subtask_required_evidence_type(
                Some(&title),
                Some(&goal),
                Some(&deliverable),
                &queries,
            )
        });
        let priority =
            normalize_task_graph_priority(task_obj.get("priority").and_then(|v| v.as_str()));
        subtasks.push(serde_json::json!({
            "id": id,
            "title": title,
            "goal": goal,
            "queries": queries,
            "deliverable": deliverable,
            "requiredEvidenceType": required_evidence_type,
            "priority": priority,
        }));
    }

    if decomposition_mode != "none" && subtasks.is_empty() {
        return None;
    }
    let mut output = serde_json::json!({
        "intent": intent,
        "complexityScore": complexity_score,
        "decompositionMode": decomposition_mode,
        "subtasks": subtasks,
    });
    if let Some(raw_value) = complexity_score_raw {
        if (raw_value - complexity_score as f64).abs() > f64::EPSILON {
            if let Some(obj) = output.as_object_mut() {
                obj.insert(
                    "complexityScoreRaw".to_string(),
                    serde_json::json!(raw_value),
                );
            }
        }
    }
    Some(output)
}

pub fn detect_pm_task_graph_issue(preface_text: &str) -> Option<String> {
    let has_graph_block = extract_named_json_object(preface_text, "TASK_GRAPH_V2")
        .or_else(|| extract_named_json_object(preface_text, "TASK_GRAPH"))
        .or_else(|| extract_named_json_object(preface_text, "TASK_DECOMPOSITION"))
        .is_some();
    if !has_graph_block {
        return Some("missing TASK_GRAPH_V2".to_string());
    }
    if extract_pm_task_graph(preface_text).is_some() {
        None
    } else {
        Some("TASK_GRAPH_V2 invalid: check decompositionMode/subtasks schema".to_string())
    }
}

pub fn apply_pm_task_graph_to_plan(plan: &mut serde_json::Value, task_graph: &serde_json::Value) {
    let Some(plan_obj) = plan.as_object_mut() else {
        return;
    };
    let subtasks = task_graph
        .get("subtasks")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    if subtasks.is_empty() {
        plan_obj.insert("taskGraph".to_string(), task_graph.clone());
        return;
    }

    let mut merged_variants = plan_obj
        .get("queryVariants")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(std::string::ToString::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    for subtask in &subtasks {
        let Some(obj) = subtask.as_object() else {
            continue;
        };
        let required_evidence_type = obj
            .get("requiredEvidenceType")
            .or_else(|| obj.get("required_evidence_type"))
            .or_else(|| obj.get("evidenceType"))
            .or_else(|| obj.get("evidence_type"))
            .and_then(|v| v.as_str());
        if !pm_subtask_allows_external_probe(required_evidence_type) {
            continue;
        }
        if let Some(queries) = obj.get("queries").and_then(|v| v.as_array()) {
            for query in queries.iter().take(4) {
                let Some(raw) = query.as_str() else {
                    continue;
                };
                if let Some(cleaned) = sanitize_pm_search_query(raw, 140) {
                    merged_variants.push(cleaned);
                }
            }
        }
    }
    let mut seen = HashSet::<String>::new();
    merged_variants.retain(|variant| seen.insert(variant.to_ascii_lowercase()));
    if merged_variants.len() > 24 {
        merged_variants.truncate(24);
    }
    if !merged_variants.is_empty() {
        plan_obj.insert(
            "queryVariants".to_string(),
            serde_json::Value::Array(
                merged_variants
                    .into_iter()
                    .map(serde_json::Value::String)
                    .collect(),
            ),
        );
    }

    let mut parallelism = plan_obj
        .get("parallelism")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    let graph_parallel = task_graph
        .get("parallelism")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    let enabled_route_count = plan_obj
        .get("sourceRoutes")
        .and_then(|v| v.as_array())
        .map(|routes| {
            routes
                .iter()
                .filter(|route| {
                    route
                        .get("enabled")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false)
                })
                .count()
        })
        .unwrap_or(0) as u64;
    let subtasks_count = subtasks.len() as u64;
    let (complexity_score, _complexity_score_raw) = parse_pm_complexity_score(
        task_graph
            .get("complexityScore")
            .or_else(|| task_graph.get("complexity_score")),
        55,
    );
    let decomposition_mode = task_graph
        .get("decompositionMode")
        .or_else(|| task_graph.get("decomposition_mode"))
        .and_then(|v| v.as_str())
        .map(|raw| raw.trim().to_ascii_lowercase())
        .unwrap_or_else(|| "light".to_string());
    let planned_probe_variant_fanout = parallelism
        .get("probeVariantFanoutMax")
        .and_then(|v| v.as_u64());
    let planned_probe_route_fanout = parallelism
        .get("probeRouteFanoutMax")
        .and_then(|v| v.as_u64());
    let planned_probe_candidate_max = parallelism
        .get("probeCandidateMax")
        .and_then(|v| v.as_u64());
    let max_concurrent_subtasks = graph_parallel
        .get("maxConcurrentSubtasks")
        .and_then(|v| v.as_u64())
        .unwrap_or(3)
        .clamp(1, 6);
    let max_probe_per_subtask = graph_parallel
        .get("maxProbePerSubtask")
        .and_then(|v| v.as_u64())
        .unwrap_or(2)
        .clamp(1, 2);
    let min_sources_per_subtask = graph_parallel
        .get("minSourcesPerSubtask")
        .and_then(|v| v.as_u64())
        .unwrap_or(1)
        .clamp(1, 2);
    let route_fanout_default = enabled_route_count.clamp(1, 2);
    let route_fanout = planned_probe_route_fanout
        .unwrap_or(route_fanout_default)
        .clamp(1, 3);
    let complexity_probe_candidate_cap = if decomposition_mode == "full" || complexity_score >= 85 {
        10
    } else if complexity_score >= 68 {
        8
    } else {
        2
    };
    let complexity_variant_fanout_cap = if decomposition_mode == "full" || complexity_score >= 85 {
        12
    } else if complexity_score >= 68 {
        8
    } else {
        4
    };
    let variant_fanout_default = subtasks_count.saturating_mul(max_probe_per_subtask).max(1);
    let variant_fanout = planned_probe_variant_fanout
        .unwrap_or(variant_fanout_default)
        .clamp(1, complexity_variant_fanout_cap);
    let candidate_max_default = subtasks_count
        .saturating_mul(max_probe_per_subtask.max(1))
        .saturating_mul(min_sources_per_subtask.max(1))
        .max(1)
        .clamp(1, complexity_probe_candidate_cap);
    let candidate_max = planned_probe_candidate_max
        .unwrap_or(candidate_max_default)
        .clamp(1, complexity_probe_candidate_cap);
    parallelism.insert(
        "probeVariantFanoutMax".to_string(),
        serde_json::json!(variant_fanout),
    );
    parallelism.insert(
        "probeRouteFanoutMax".to_string(),
        serde_json::json!(route_fanout),
    );
    parallelism.insert(
        "probeCandidateMax".to_string(),
        serde_json::json!(candidate_max),
    );
    parallelism.insert(
        "maxConcurrentSubtasks".to_string(),
        serde_json::json!(max_concurrent_subtasks),
    );
    parallelism.insert(
        "maxProbePerSubtask".to_string(),
        serde_json::json!(max_probe_per_subtask),
    );
    parallelism.insert(
        "minSourcesPerSubtask".to_string(),
        serde_json::json!(min_sources_per_subtask),
    );
    plan_obj.insert(
        "parallelism".to_string(),
        serde_json::Value::Object(parallelism),
    );
    plan_obj.insert("taskGraph".to_string(), task_graph.clone());
}

pub fn build_pm_fallback_task_graph(
    original_question: &str,
    plan: &serde_json::Value,
) -> Option<serde_json::Value> {
    let question = original_question.trim();
    if question.is_empty() {
        return None;
    }

    let mut variants = plan
        .get("queryVariants")
        .and_then(|value| value.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str())
                .filter_map(|raw| sanitize_pm_search_query(raw, 140))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if variants.is_empty() {
        if let Some(cleaned_question) = sanitize_pm_search_query(question, 140) {
            variants.push(cleaned_question);
        }
    }
    if variants.is_empty() {
        return None;
    }

    let cjk = contains_cjk(question);
    let query_context = variants
        .first()
        .cloned()
        .unwrap_or_else(|| "product operations strategy".to_string());
    let mk_query = |idx: usize, fallback_suffix: &str| -> String {
        variants
            .get(idx)
            .cloned()
            .or_else(|| variants.first().cloned())
            .unwrap_or_else(|| format!("{query_context} {fallback_suffix}"))
    };
    let mk_queries = |base_a: String, base_b: String, fallback: &str| -> Vec<String> {
        let out = vec![base_a, base_b];
        let mut out = sanitize_pm_search_queries(out, Some(fallback), 2);
        let mut seen = HashSet::<String>::new();
        out.retain(|item| {
            let key = item.trim().to_ascii_lowercase();
            if key.is_empty() {
                return false;
            }
            seen.insert(key)
        });
        out
    };

    let (s1_title, s1_goal, s1_deliverable, s1_q2) = if cjk {
        (
            "可比案例与增长机制".to_string(),
            "围绕用户问题识别可比产品、行业案例、用户路径和增长机制".to_string(),
            "输出可迁移案例、机制启发和适用边界".to_string(),
            format!("{query_context} 可比案例 增长机制 用户路径 benchmark case study"),
        )
    } else {
        (
            "Comparable cases and growth mechanisms".to_string(),
            "Identify comparable products, industry cases, user paths, and growth mechanisms from the user's question".to_string(),
            "Deliver transferable cases, mechanism inspiration, and fit boundaries".to_string(),
            format!("{query_context} comparable cases growth mechanisms user journey benchmark"),
        )
    };
    let (s2_title, s2_goal, s2_deliverable, s2_q2) = if cjk {
        (
            "业务模型与关键指标".to_string(),
            "拆解问题中的收入、成本、转化、留存、效率、质量或其他已出现的核心指标路径".to_string(),
            "输出指标驱动路径、关键阈值和敏感性假设".to_string(),
            format!("{query_context} 业务模型 指标路径 单位经济 转化 留存 成本 质量"),
        )
    } else {
        (
            "Business model and key metrics".to_string(),
            "Break down the revenue, cost, conversion, retention, efficiency, quality, or other metric paths present in the question".to_string(),
            "Deliver metric movement paths, thresholds, and sensitivity assumptions".to_string(),
            format!("{query_context} business model metric path unit economics conversion retention cost quality"),
        )
    };
    let (s3_title, s3_goal, s3_deliverable, s3_q2) = if cjk {
        (
            "风险、约束与执行边界".to_string(),
            "识别与用户问题相关的体验、合规、运营、财务、数据或执行风险".to_string(),
            "输出可执行风险边界、保护指标和回滚规则".to_string(),
            format!("{query_context} 风险 约束 保护指标 实验 回滚 执行边界"),
        )
    } else {
        (
            "Risks, constraints, and execution boundaries".to_string(),
            "Identify experience, compliance, operational, financial, data, or execution risks relevant to the user's question".to_string(),
            "Deliver risk boundaries, guardrail metrics, and rollback rules".to_string(),
            format!("{query_context} risk constraints guardrail metrics experiment rollback execution boundary"),
        )
    };

    let subtasks = vec![
        serde_json::json!({
            "id": "fallback-s1",
            "title": s1_title,
            "goal": s1_goal,
            "queries": mk_queries(mk_query(0, "comparable cases mechanism"), s1_q2, &query_context),
            "deliverable": s1_deliverable,
            "requiredEvidenceType": "external",
            "priority": "high",
        }),
        serde_json::json!({
            "id": "fallback-s2",
            "title": s2_title,
            "goal": s2_goal,
            "queries": mk_queries(mk_query(1, "business model key metrics"), s2_q2, &query_context),
            "deliverable": s2_deliverable,
            "requiredEvidenceType": "mixed",
            "priority": "high",
        }),
        serde_json::json!({
            "id": "fallback-s3",
            "title": s3_title,
            "goal": s3_goal,
            "queries": mk_queries(mk_query(2, "risk constraints execution"), s3_q2, &query_context),
            "deliverable": s3_deliverable,
            "requiredEvidenceType": "mixed",
            "priority": "medium",
        }),
    ];

    let parallelism = plan
        .get("parallelism")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    let max_concurrency = parallelism
        .get("maxConcurrentSubtasks")
        .or_else(|| parallelism.get("maxConcurrency"))
        .and_then(|value| value.as_u64())
        .unwrap_or(3)
        .clamp(1, 6);
    let max_probe_per_subtask = parallelism
        .get("maxProbePerSubtask")
        .and_then(|value| value.as_u64())
        .unwrap_or(2)
        .clamp(1, 2);
    let min_sources_per_subtask = parallelism
        .get("minSourcesPerSubtask")
        .and_then(|value| value.as_u64())
        .unwrap_or(1)
        .clamp(1, 3);

    Some(serde_json::json!({
        "intent": "research",
        "complexityScore": 68,
        "decompositionMode": "light",
        "subtasks": subtasks,
        "parallelism": {
            "maxConcurrentSubtasks": max_concurrency,
            "maxProbePerSubtask": max_probe_per_subtask,
            "minSourcesPerSubtask": min_sources_per_subtask,
        }
    }))
}

pub fn apply_pm_exec_constraints_to_plan(
    plan: &mut serde_json::Value,
    constraints: &PmExecConstraints,
) {
    let Some(plan_obj) = plan.as_object_mut() else {
        return;
    };
    let allowset: HashSet<String> = constraints
        .route_allowlist
        .iter()
        .map(|route| route.trim().to_ascii_lowercase())
        .filter(|route| !route.is_empty())
        .collect();
    let priority_rank: HashMap<String, usize> = constraints
        .route_priority
        .iter()
        .enumerate()
        .map(|(idx, route)| (route.trim().to_ascii_lowercase(), idx))
        .collect();

    if let Some(routes) = plan_obj
        .get_mut("sourceRoutes")
        .and_then(|v| v.as_array_mut())
    {
        let has_overlap = routes.iter().any(|route| {
            route
                .get("routeId")
                .and_then(|v| v.as_str())
                .map(|route_id| allowset.contains(&route_id.trim().to_ascii_lowercase()))
                .unwrap_or(false)
        });

        if has_overlap && !allowset.is_empty() {
            for route in routes.iter_mut() {
                let Some(route_obj) = route.as_object_mut() else {
                    continue;
                };
                let route_id = route_obj
                    .get("routeId")
                    .and_then(|v| v.as_str())
                    .map(|v| v.trim().to_ascii_lowercase())
                    .unwrap_or_default();
                let currently_enabled = route_obj
                    .get("enabled")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let allowed = allowset.contains(&route_id);
                if currently_enabled && !allowed {
                    route_obj.insert("enabled".to_string(), serde_json::json!(false));
                    route_obj.insert(
                        "reason".to_string(),
                        serde_json::json!("disabled_by_exec_constraints_allowlist"),
                    );
                }
            }
        }

        routes.sort_by_key(|route| {
            route
                .get("routeId")
                .and_then(|v| v.as_str())
                .map(|route_id| route_id.trim().to_ascii_lowercase())
                .and_then(|route_id| priority_rank.get(&route_id).copied())
                .unwrap_or(usize::MAX)
        });
    }

    let selected_route_ids: Vec<String> = plan_obj
        .get("sourceRoutes")
        .and_then(|value| value.as_array())
        .map(|items| {
            items
                .iter()
                .filter(|item| {
                    item.get("enabled")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false)
                })
                .filter_map(|item| item.get("routeId").and_then(|v| v.as_str()))
                .map(std::string::ToString::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    plan_obj.insert(
        "selectedRouteIds".to_string(),
        serde_json::Value::Array(
            selected_route_ids
                .into_iter()
                .map(serde_json::Value::String)
                .collect(),
        ),
    );
    plan_obj.insert(
        "execConstraints".to_string(),
        serde_json::json!({
            "routeAllowlist": constraints.route_allowlist.clone(),
            "routePriority": constraints.route_priority.clone(),
            "stopConditions": constraints.stop_conditions.clone(),
            "sourceSlotBudgetSecs": constraints.source_slot_budget_secs,
            "toolBudgetPerAttempt": constraints.tool_budget_per_attempt,
            "pipelineTimeoutSecs": constraints.pipeline_timeout_secs
        }),
    );
}

pub fn build_pm_stage_plan(
    question: &str,
    mcp_servers: &[String],
    skills: &[String],
) -> serde_json::Value {
    let channels = resolve_pm_plan_channels(question);
    let query_variants = build_pm_query_variants(question);
    let source_routes = build_pm_source_routes(&channels, mcp_servers, skills);
    let selected_route_ids: Vec<String> = source_routes
        .iter()
        .filter_map(|route| {
            let enabled = route
                .get("enabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if enabled {
                route
                    .get("routeId")
                    .and_then(|v| v.as_str())
                    .map(std::string::ToString::to_string)
            } else {
                None
            }
        })
        .collect();
    let mut plan = serde_json::json!({
        "mode": "auto",
        "query": question.trim(),
        "queryVariants": query_variants,
        "channels": channels,
        "sourceRoutes": source_routes,
        "selectedRouteIds": selected_route_ids,
        "targetEvidenceCount": 12,
        "mustCiteUrls": true,
        "maxRetry": 3,
        "parallelism": {
            "probeVariantFanoutMax": 2,
            "probeRouteFanoutMax": 2,
            "probeCandidateMax": 5,
            "maxConcurrentSubtasks": 3,
            "maxProbePerSubtask": 2,
            "minSourcesPerSubtask": 1,
            "runtimePerSessionTurn": 1
        }
    });
    attach_pm_report_strategy_hint(&mut plan, question);
    plan
}

pub fn pm_should_bypass_retrieval(plan: &serde_json::Value, question: &str) -> bool {
    if pm_is_report_strategy_mode(plan) {
        return false;
    }
    let intent = plan
        .get("taskGraph")
        .and_then(|value| value.get("intent"))
        .and_then(|value| value.as_str())
        .map(|raw| raw.trim().to_ascii_lowercase())
        .unwrap_or_else(|| "research".to_string());
    if pm_question_likely_requires_external_evidence(question) {
        return false;
    }
    if intent == "chat" {
        return true;
    }
    let decomposition_mode = plan
        .get("taskGraph")
        .and_then(|value| value.get("decompositionMode"))
        .and_then(|value| value.as_str())
        .map(|raw| raw.trim().to_ascii_lowercase())
        .unwrap_or_else(|| "light".to_string());
    let subtask_count = plan
        .get("taskGraph")
        .and_then(|value| value.get("subtasks"))
        .and_then(|value| value.as_array())
        .map(|arr| arr.len())
        .unwrap_or(0);
    if decomposition_mode != "none" || subtask_count > 0 {
        return false;
    }
    let complexity_score = plan
        .get("taskGraph")
        .and_then(|value| value.get("complexityScore"))
        .and_then(|value| value.as_u64())
        .unwrap_or(70);
    intent == "analysis" && complexity_score <= 60
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_graph_preserves_required_evidence_type_aliases() {
        let preface = r#"
TASK_GRAPH_V2 {"intent":"analysis","complexityScore":70,"decompositionMode":"light","subtasks":[{"id":"s1","title":"一手数据拆解","goal":"基于用户提供的数据做归因","queries":[],"deliverable":"内部结论","evidenceType":"first-party","priority":"high"},{"id":"s2","title":"外部案例","goal":"检索可比案例","queries":["pricing case study"],"deliverable":"案例","requiredEvidenceType":"web","priority":"medium"}]}
"#;
        let graph = extract_pm_task_graph(preface).expect("task graph should parse");
        let subtasks = graph
            .get("subtasks")
            .and_then(|value| value.as_array())
            .expect("subtasks should exist");
        assert_eq!(
            subtasks[0]
                .get("requiredEvidenceType")
                .and_then(|value| value.as_str()),
            Some("first_party")
        );
        assert_eq!(
            subtasks[1]
                .get("requiredEvidenceType")
                .and_then(|value| value.as_str()),
            Some("external")
        );
    }

    #[test]
    fn apply_task_graph_skips_first_party_queries_when_merging_search_variants() {
        let mut plan = serde_json::json!({
            "queryVariants": ["original external query"],
            "sourceRoutes": [],
            "parallelism": {}
        });
        let task_graph = serde_json::json!({
            "intent": "decision_support",
            "complexityScore": 82,
            "decompositionMode": "light",
            "subtasks": [
                {
                    "id": "internal",
                    "title": "一手数据拆解",
                    "goal": "基于用户提供的一手数据识别优先人群",
                    "queries": ["基于一手数据识别最值得优先爆破的人群"],
                    "deliverable": "内部指标结论",
                    "requiredEvidenceType": "first_party",
                    "priority": "high"
                },
                {
                    "id": "external",
                    "title": "外部案例",
                    "goal": "检索可比案例",
                    "queries": ["rewarded ads retention case study"],
                    "deliverable": "案例启发",
                    "requiredEvidenceType": "external",
                    "priority": "medium"
                }
            ]
        });

        apply_pm_task_graph_to_plan(&mut plan, &task_graph);
        let variants = plan
            .get("queryVariants")
            .and_then(|value| value.as_array())
            .expect("query variants should exist")
            .iter()
            .filter_map(|value| value.as_str())
            .collect::<Vec<_>>();
        assert!(variants.contains(&"rewarded ads retention case study"));
        assert!(!variants
            .iter()
            .any(|value| value.contains("一手数据识别最值得优先爆破")));
    }
}
