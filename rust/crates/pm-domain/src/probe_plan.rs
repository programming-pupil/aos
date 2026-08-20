use std::collections::{HashMap, HashSet};

use crate::query_hygiene::sanitize_pm_search_queries;
use crate::task_graph::{
    build_pm_subtask_key, infer_pm_subtask_required_evidence_type,
    normalize_pm_required_evidence_type, normalize_task_graph_priority,
    pm_subtask_allows_external_probe, sanitize_pm_task_graph_queries, sanitize_pm_task_graph_text,
};

#[derive(Debug, Clone)]
pub struct PmProbeCandidate {
    pub variant: String,
    pub route: Option<serde_json::Value>,
    pub subtask_key: Option<String>,
    pub subtask_id: Option<String>,
    pub subtask_title: Option<String>,
    pub subtask_goal: Option<String>,
    pub subtask_deliverable: Option<String>,
    pub subtask_required_evidence_type: Option<String>,
    pub subtask_priority: Option<String>,
}

fn normalize_claim_key(input: &str) -> String {
    input
        .trim()
        .trim_matches(|ch: char| ch == '"' || ch == '\'' || ch == '`')
        .to_ascii_lowercase()
}

fn pm_env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|raw| raw.trim().parse::<usize>().ok())
        .unwrap_or(default)
}

fn interleave_pm_probe_candidate_groups(
    groups: Vec<Vec<PmProbeCandidate>>,
    cap: usize,
) -> Vec<PmProbeCandidate> {
    let mut iterators = groups.into_iter().map(Vec::into_iter).collect::<Vec<_>>();
    let mut selected = Vec::new();
    while selected.len() < cap {
        let mut made_progress = false;
        for iterator in &mut iterators {
            if selected.len() >= cap {
                break;
            }
            if let Some(candidate) = iterator.next() {
                selected.push(candidate);
                made_progress = true;
            }
        }
        if !made_progress {
            break;
        }
    }
    selected
}

pub fn build_pm_probe_candidates(plan: &serde_json::Value) -> Vec<PmProbeCandidate> {
    let decomposition_mode = plan
        .get("taskGraph")
        .and_then(|value| value.get("decompositionMode"))
        .and_then(|value| value.as_str())
        .map(|raw| raw.trim().to_ascii_lowercase())
        .unwrap_or_else(|| "light".to_string());
    if decomposition_mode == "none" {
        return Vec::new();
    }

    let variants: Vec<String> = plan
        .get("queryVariants")
        .and_then(|value| value.as_array())
        .map(|items| {
            let raw = items
                .iter()
                .filter_map(|item| item.as_str())
                .map(std::string::ToString::to_string)
                .collect::<Vec<_>>();
            sanitize_pm_search_queries(raw, None, 24)
        })
        .filter(|items| !items.is_empty())
        .unwrap_or_default();
    let routes: Vec<serde_json::Value> = plan
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
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let parallel = plan.get("parallelism").and_then(|value| value.as_object());
    let route_fanout = parallel
        .and_then(|obj| obj.get("probeRouteFanoutMax"))
        .and_then(|value| value.as_u64())
        .unwrap_or(2) as usize;
    let candidate_max = parallel
        .and_then(|obj| obj.get("probeCandidateMax"))
        .and_then(|value| value.as_u64())
        .unwrap_or(5) as usize;
    let task_graph_subtasks = plan
        .get("taskGraph")
        .and_then(|value| value.get("subtasks"))
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();
    let max_probe_per_subtask = parallel
        .and_then(|obj| obj.get("maxProbePerSubtask"))
        .and_then(|value| value.as_u64())
        .unwrap_or(2) as usize;
    let max_probe_per_subtask = max_probe_per_subtask.max(1);
    let max_concurrent_subtasks = parallel
        .and_then(|obj| obj.get("maxConcurrentSubtasks"))
        .and_then(|value| value.as_u64())
        .unwrap_or(3) as usize;
    let min_sources_per_subtask = parallel
        .and_then(|obj| obj.get("minSourcesPerSubtask"))
        .and_then(|value| value.as_u64())
        .unwrap_or(1) as usize;
    let min_sources_per_subtask = min_sources_per_subtask.max(1);
    let subtask_candidate_cap = pm_env_usize("PM_SUBTASK_CANDIDATE_CAP", 6).max(1);
    let query_only_variant_fanout = pm_env_usize("PM_QUERY_ONLY_VARIANT_FANOUT", 5).max(1);
    let coverage_ratio_raw = pm_env_usize("PM_SUBTASK_PROBE_COVERAGE_PERCENT", 70);
    let coverage_ratio = coverage_ratio_raw.clamp(10, 95);

    let make_query_only_candidates = |all_variants: &[String],
                                      route_pool: &[serde_json::Value],
                                      query_cap: usize|
     -> Vec<PmProbeCandidate> {
        let chosen_variants: Vec<String> = all_variants
            .iter()
            .take(query_only_variant_fanout)
            .cloned()
            .collect();
        if chosen_variants.is_empty() {
            return Vec::new();
        }
        let chosen_routes: Vec<serde_json::Value> =
            route_pool.iter().take(route_fanout).cloned().collect();
        let mut out = Vec::new();
        if chosen_routes.is_empty() {
            for variant in chosen_variants {
                out.push(PmProbeCandidate {
                    variant,
                    route: None,
                    subtask_key: None,
                    subtask_id: None,
                    subtask_title: None,
                    subtask_goal: None,
                    subtask_deliverable: None,
                    subtask_required_evidence_type: None,
                    subtask_priority: None,
                });
                if out.len() >= query_cap {
                    break;
                }
            }
            return out;
        }
        for variant in chosen_variants {
            for route in &chosen_routes {
                out.push(PmProbeCandidate {
                    variant: variant.clone(),
                    route: Some(route.clone()),
                    subtask_key: None,
                    subtask_id: None,
                    subtask_title: None,
                    subtask_goal: None,
                    subtask_deliverable: None,
                    subtask_required_evidence_type: None,
                    subtask_priority: None,
                });
                if out.len() >= query_cap {
                    return out;
                }
            }
        }
        out
    };

    if !task_graph_subtasks.is_empty() {
        let route_limit = route_fanout.max(min_sources_per_subtask.max(1)).max(1);
        let chosen_routes: Vec<serde_json::Value> =
            routes.iter().take(route_limit).cloned().collect();
        let effective_min_sources = min_sources_per_subtask
            .max(1)
            .min(chosen_routes.len().max(1));
        let mut sorted_subtasks = task_graph_subtasks;
        sorted_subtasks.sort_by_key(|subtask| {
            let priority = subtask
                .get("priority")
                .and_then(|v| v.as_str())
                .unwrap_or("medium")
                .trim()
                .to_ascii_lowercase();
            match priority.as_str() {
                "high" => 0usize,
                "medium" => 1usize,
                _ => 2usize,
            }
        });
        let selected_subtasks = sorted_subtasks;
        let raw_selected_subtasks_count = selected_subtasks.len().max(1);
        let initial_subtask_floor =
            effective_min_sources.saturating_mul(raw_selected_subtasks_count);
        let effective_candidate_max = candidate_max
            .max(initial_subtask_floor)
            .max(1)
            .clamp(1, 480);

        let mut accepted_external_subtask_count = 0usize;
        let mut candidate_groups = Vec::<Vec<PmProbeCandidate>>::new();
        for (subtask_idx, subtask) in selected_subtasks.into_iter().enumerate() {
            let Some(task_obj) = subtask.as_object() else {
                continue;
            };
            let subtask_id = task_obj
                .get("id")
                .and_then(|v| v.as_str())
                .and_then(|raw| sanitize_pm_task_graph_text(raw, 60));
            let subtask_title = task_obj
                .get("title")
                .and_then(|v| v.as_str())
                .and_then(|raw| sanitize_pm_task_graph_text(raw, 90));
            let subtask_goal = task_obj
                .get("goal")
                .and_then(|v| v.as_str())
                .and_then(|raw| sanitize_pm_task_graph_text(raw, 180))
                .or_else(|| subtask_title.clone());
            let subtask_deliverable = task_obj
                .get("deliverable")
                .and_then(|v| v.as_str())
                .and_then(|raw| sanitize_pm_task_graph_text(raw, 180));
            let raw_required_evidence_type = task_obj
                .get("requiredEvidenceType")
                .or_else(|| task_obj.get("required_evidence_type"))
                .or_else(|| task_obj.get("evidenceType"))
                .or_else(|| task_obj.get("evidence_type"))
                .and_then(|v| v.as_str());
            let subtask_priority = Some(normalize_task_graph_priority(
                task_obj.get("priority").and_then(|v| v.as_str()),
            ));
            let subtask_key = Some(build_pm_subtask_key(
                subtask_id.as_deref(),
                subtask_title.as_deref(),
                subtask_goal.as_deref(),
                subtask_idx,
            ));
            let queries = sanitize_pm_task_graph_queries(
                task_obj.get("queries"),
                subtask_goal.as_deref().unwrap_or(""),
                max_probe_per_subtask.max(1),
            );
            let subtask_required_evidence_type = normalize_pm_required_evidence_type(
                raw_required_evidence_type,
            )
            .unwrap_or_else(|| {
                infer_pm_subtask_required_evidence_type(
                    subtask_title.as_deref(),
                    subtask_goal.as_deref(),
                    subtask_deliverable.as_deref(),
                    &queries,
                )
            });
            if !pm_subtask_allows_external_probe(Some(&subtask_required_evidence_type)) {
                continue;
            }
            if decomposition_mode == "light"
                && accepted_external_subtask_count >= max_concurrent_subtasks.max(1)
            {
                break;
            }
            accepted_external_subtask_count = accepted_external_subtask_count.saturating_add(1);
            let subtask_required_evidence_type = Some(subtask_required_evidence_type);
            let query_pool = if queries.is_empty() {
                vec![subtask_goal
                    .clone()
                    .unwrap_or_else(|| "research".to_string())]
            } else {
                queries
            };
            let per_subtask_cap = max_probe_per_subtask.max(effective_min_sources).max(1);
            let mut subtask_candidates = Vec::<PmProbeCandidate>::new();
            for query in &query_pool {
                if chosen_routes.is_empty() {
                    subtask_candidates.push(PmProbeCandidate {
                        variant: query.clone(),
                        route: None,
                        subtask_key: subtask_key.clone(),
                        subtask_id: subtask_id.clone(),
                        subtask_title: subtask_title.clone(),
                        subtask_goal: subtask_goal.clone(),
                        subtask_deliverable: subtask_deliverable.clone(),
                        subtask_required_evidence_type: subtask_required_evidence_type.clone(),
                        subtask_priority: subtask_priority.clone(),
                    });
                } else {
                    for route in &chosen_routes {
                        subtask_candidates.push(PmProbeCandidate {
                            variant: query.clone(),
                            route: Some(route.clone()),
                            subtask_key: subtask_key.clone(),
                            subtask_id: subtask_id.clone(),
                            subtask_title: subtask_title.clone(),
                            subtask_goal: subtask_goal.clone(),
                            subtask_deliverable: subtask_deliverable.clone(),
                            subtask_required_evidence_type: subtask_required_evidence_type.clone(),
                            subtask_priority: subtask_priority.clone(),
                        });
                        if subtask_candidates.len() >= per_subtask_cap {
                            break;
                        }
                    }
                }
                if subtask_candidates.len() >= per_subtask_cap {
                    break;
                }
            }
            if !chosen_routes.is_empty() && subtask_candidates.len() < effective_min_sources {
                let seed_variant = query_pool.first().cloned().unwrap_or_else(|| {
                    subtask_goal
                        .clone()
                        .unwrap_or_else(|| "research".to_string())
                });
                for route in chosen_routes.iter().take(effective_min_sources) {
                    if subtask_candidates.len() >= effective_min_sources {
                        break;
                    }
                    let duplicate = subtask_candidates.iter().any(|candidate| {
                        candidate.variant == seed_variant && candidate.route.as_ref() == Some(route)
                    });
                    if duplicate {
                        continue;
                    }
                    subtask_candidates.push(PmProbeCandidate {
                        variant: seed_variant.clone(),
                        route: Some(route.clone()),
                        subtask_key: subtask_key.clone(),
                        subtask_id: subtask_id.clone(),
                        subtask_title: subtask_title.clone(),
                        subtask_goal: subtask_goal.clone(),
                        subtask_deliverable: subtask_deliverable.clone(),
                        subtask_required_evidence_type: subtask_required_evidence_type.clone(),
                        subtask_priority: subtask_priority.clone(),
                    });
                }
            }
            if !subtask_candidates.is_empty() {
                candidate_groups.push(subtask_candidates);
            }
        }
        let fair_candidate_cap = effective_candidate_max
            .max(candidate_groups.len())
            .clamp(1, 480);
        let mut candidates =
            interleave_pm_probe_candidate_groups(candidate_groups, fair_candidate_cap);
        if !candidates.is_empty() {
            // A light plan has already selected its highest-value external subtasks.
            // Adding unbound query variants here defeats the light-plan budget and
            // can dilute the evidence set with searches unrelated to those tasks.
            // Full decomposition keeps the hybrid backfill lane below.
            if decomposition_mode == "light" {
                candidates.truncate(effective_candidate_max);
                return candidates;
            }
            // Hybrid scheduling when subtasks exist:
            // Keep taskGraph retrieval anchored to LLM-selected external/mixed subtasks,
            // but reserve spare capacity for query-only backfill. The raw user
            // variants often carry wording or entities that the task graph omits;
            // keeping a small backfill lane prevents over-narrow decompositions
            // from starving first-pass evidence collection.
            let selected_subtasks_count = accepted_external_subtask_count.max(1);
            let subtask_floor = effective_min_sources.saturating_mul(selected_subtasks_count);
            let total_cap = effective_candidate_max.max(subtask_floor).max(1);
            let query_only = make_query_only_candidates(&variants, &routes, total_cap);
            let query_only_budget = if query_only.is_empty() || total_cap <= subtask_floor {
                0
            } else {
                total_cap.saturating_sub(subtask_floor).max(1)
            };
            let mut subtask_budget = total_cap
                .saturating_mul(coverage_ratio)
                .saturating_div(100)
                .max(subtask_floor)
                .max(selected_subtasks_count.min(subtask_candidate_cap))
                .min(total_cap)
                .min(subtask_candidate_cap.max(subtask_floor));
            if query_only_budget > 0 {
                subtask_budget = subtask_budget.min(total_cap.saturating_sub(query_only_budget));
            }
            if subtask_budget > candidates.len() {
                subtask_budget = candidates.len();
            }
            if subtask_budget == 0 {
                subtask_budget = candidates.len().min(total_cap);
            }
            let mut merged = candidates
                .into_iter()
                .take(subtask_budget)
                .collect::<Vec<_>>();
            if query_only_budget > 0 {
                let remaining = total_cap.saturating_sub(merged.len());
                merged.extend(
                    query_only
                        .into_iter()
                        .take(query_only_budget.min(remaining)),
                );
            }
            merged.truncate(total_cap);
            return merged;
        }
        return Vec::new();
    }

    make_query_only_candidates(&variants, &routes, candidate_max)
}

pub fn prioritize_pm_probe_candidates_for_subtasks(
    candidates: Vec<PmProbeCandidate>,
    target_subtasks: &[String],
    strict_only: bool,
) -> Vec<PmProbeCandidate> {
    if candidates.is_empty() || target_subtasks.is_empty() {
        return candidates;
    }
    let target_keys: HashSet<String> = target_subtasks
        .iter()
        .map(|title| normalize_claim_key(title))
        .filter(|key| !key.is_empty())
        .collect();
    if target_keys.is_empty() {
        return candidates;
    }
    let mut matched = Vec::<PmProbeCandidate>::new();
    let mut others = Vec::<PmProbeCandidate>::new();
    for candidate in candidates {
        let mut keys = Vec::<String>::new();
        for raw in [
            candidate.subtask_key.as_deref().unwrap_or(""),
            candidate.subtask_id.as_deref().unwrap_or(""),
            candidate.subtask_title.as_deref().unwrap_or(""),
            candidate.subtask_goal.as_deref().unwrap_or(""),
            candidate.subtask_deliverable.as_deref().unwrap_or(""),
        ] {
            let key = normalize_claim_key(raw);
            if !key.is_empty() && !keys.iter().any(|existing| existing == &key) {
                keys.push(key);
            }
        }
        if keys.iter().any(|key| target_keys.contains(key)) {
            matched.push(candidate);
        } else {
            others.push(candidate);
        }
    }
    if matched.is_empty() {
        return others;
    }
    if strict_only {
        return matched;
    }
    matched.extend(others);
    matched
}

pub fn pick_pm_subtask_focus_for_repair(
    pending_queue: &mut Vec<String>,
    repair_attempts: &mut HashMap<String, usize>,
    gap_titles: &[String],
    max_attempts_per_subtask: usize,
) -> Option<String> {
    if gap_titles.is_empty() {
        pending_queue.clear();
        repair_attempts.clear();
        return None;
    }
    let mut normalized_gaps = Vec::<(String, String)>::new();
    for title in gap_titles {
        let key = normalize_claim_key(title);
        if key.is_empty() {
            continue;
        }
        if normalized_gaps.iter().any(|(existing, _)| existing == &key) {
            continue;
        }
        normalized_gaps.push((key, title.clone()));
    }
    if normalized_gaps.is_empty() {
        pending_queue.clear();
        repair_attempts.clear();
        return None;
    }
    let gap_keys: HashSet<String> = normalized_gaps.iter().map(|(key, _)| key.clone()).collect();
    pending_queue.retain(|title| gap_keys.contains(&normalize_claim_key(title)));
    repair_attempts.retain(|key, _| gap_keys.contains(key));

    for (key, title) in &normalized_gaps {
        let exists = pending_queue
            .iter()
            .any(|item| normalize_claim_key(item) == *key);
        if !exists {
            pending_queue.push(title.clone());
        }
    }

    let max_attempts_per_subtask = max_attempts_per_subtask.max(1);
    while let Some(front) = pending_queue.first() {
        let key = normalize_claim_key(front);
        let used = repair_attempts.get(&key).copied().unwrap_or(0);
        if used >= max_attempts_per_subtask {
            pending_queue.remove(0);
            continue;
        }
        return Some(front.clone());
    }

    None
}

pub fn pick_pm_subtask_gap_retry_variant(
    plan: &serde_json::Value,
    gap_titles: &[String],
) -> Option<String> {
    if gap_titles.is_empty() {
        return None;
    }
    let mut title_keys: Vec<String> = gap_titles
        .iter()
        .map(|title| normalize_claim_key(title))
        .filter(|key| !key.is_empty())
        .collect();
    title_keys.dedup();
    if title_keys.is_empty() {
        return None;
    }
    let title_key_set: HashSet<String> = title_keys.iter().cloned().collect();
    let subtasks = plan
        .get("taskGraph")
        .and_then(|value| value.get("subtasks"))
        .and_then(|value| value.as_array())?;

    let mut fallback_query: Option<String> = None;
    for preferred in &title_keys {
        for subtask in subtasks {
            let Some(obj) = subtask.as_object() else {
                continue;
            };
            let id = obj.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let title = obj.get("title").and_then(|v| v.as_str()).unwrap_or("");
            let goal = obj.get("goal").and_then(|v| v.as_str()).unwrap_or("");
            let deliverable = obj
                .get("deliverable")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let display = if !title.trim().is_empty() {
                title
            } else if !deliverable.trim().is_empty() {
                deliverable
            } else if !goal.trim().is_empty() {
                goal
            } else if !id.trim().is_empty() {
                id
            } else {
                ""
            };
            let mut keys = vec![normalize_claim_key(display)];
            for raw in [id, title, goal, deliverable] {
                let key = normalize_claim_key(raw);
                if !key.is_empty() && !keys.iter().any(|existing| existing == &key) {
                    keys.push(key);
                }
            }
            if !keys.iter().any(|key| key == preferred) {
                if fallback_query.is_none() && keys.iter().any(|key| title_key_set.contains(key)) {
                    let queries = sanitize_pm_task_graph_queries(obj.get("queries"), goal, 6);
                    if let Some(query) = queries.into_iter().find(|query| !query.trim().is_empty())
                    {
                        fallback_query = Some(query);
                    }
                }
                continue;
            }
            let queries = sanitize_pm_task_graph_queries(obj.get("queries"), goal, 6);
            if let Some(query) = queries.into_iter().find(|query| !query.trim().is_empty()) {
                return Some(query);
            }
        }
    }

    if fallback_query.is_some() {
        return fallback_query;
    }

    for subtask in subtasks {
        let Some(obj) = subtask.as_object() else {
            continue;
        };
        let id = obj.get("id").and_then(|v| v.as_str()).unwrap_or("");
        let title = obj.get("title").and_then(|v| v.as_str()).unwrap_or("");
        let goal = obj.get("goal").and_then(|v| v.as_str()).unwrap_or("");
        let deliverable = obj
            .get("deliverable")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let display = if !title.trim().is_empty() {
            title
        } else if !deliverable.trim().is_empty() {
            deliverable
        } else if !goal.trim().is_empty() {
            goal
        } else if !id.trim().is_empty() {
            id
        } else {
            ""
        };
        let mut keys = vec![normalize_claim_key(display)];
        for raw in [id, title, goal, deliverable] {
            let key = normalize_claim_key(raw);
            if !key.is_empty() && !keys.iter().any(|existing| existing == &key) {
                keys.push(key);
            }
        }
        let is_target = keys.iter().any(|key| title_key_set.contains(key));
        if !is_target {
            continue;
        }
        let queries = sanitize_pm_task_graph_queries(obj.get("queries"), goal, 6);
        if let Some(query) = queries.into_iter().find(|query| !query.trim().is_empty()) {
            return Some(query);
        }
    }
    None
}

pub fn pick_pm_subtask_gap_retry_variant_for_attempt(
    plan: &serde_json::Value,
    gap_titles: &[String],
    next_attempt: usize,
) -> Option<String> {
    if gap_titles.is_empty() {
        return None;
    }
    if gap_titles.len() == 1 {
        return pick_pm_subtask_gap_retry_variant(plan, gap_titles);
    }
    let safe_attempt = next_attempt.max(1);
    let start_idx = (safe_attempt - 1) % gap_titles.len();
    let mut rotated = Vec::with_capacity(gap_titles.len());
    for offset in 0..gap_titles.len() {
        rotated.push(gap_titles[(start_idx + offset) % gap_titles.len()].clone());
    }
    pick_pm_subtask_gap_retry_variant(plan, &rotated)
}

#[derive(Debug, Clone)]
pub struct PmEnabledRoute {
    pub route_id: String,
    pub channel: String,
    pub execution_channel: String,
}

pub fn collect_enabled_pm_routes(plan: &serde_json::Value) -> Vec<PmEnabledRoute> {
    plan.get("sourceRoutes")
        .and_then(|value| value.as_array())
        .map(|items| {
            items
                .iter()
                .filter(|item| {
                    item.get("enabled")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false)
                })
                .filter_map(|item| {
                    let route_id = item.get("routeId").and_then(|v| v.as_str())?.trim();
                    if route_id.is_empty() {
                        return None;
                    }
                    let channel = item
                        .get("channel")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown")
                        .trim();
                    let execution_channel = item
                        .get("executionChannel")
                        .and_then(|v| v.as_str())
                        .unwrap_or("search")
                        .trim();
                    Some(PmEnabledRoute {
                        route_id: route_id.to_string(),
                        channel: channel.to_string(),
                        execution_channel: execution_channel.to_string(),
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

pub fn pick_pm_attempt_preferences(
    query_variants: &[String],
    enabled_routes: &[PmEnabledRoute],
    attempt: usize,
) -> (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
) {
    let safe_attempt = if attempt == 0 { 1 } else { attempt };
    let variant_order = prioritize_pm_query_variants_for_attempts(query_variants);
    let selected_variant = if query_variants.is_empty() {
        None
    } else {
        let idx = variant_order[(safe_attempt - 1) % variant_order.len()];
        Some(query_variants[idx].clone())
    };
    let (selected_route_id, selected_route_channel, selected_execution_channel) =
        if enabled_routes.is_empty() {
            (None, None, None)
        } else {
            let row = &enabled_routes[(safe_attempt - 1) % enabled_routes.len()];
            (
                Some(row.route_id.clone()),
                Some(row.channel.clone()),
                Some(row.execution_channel.clone()),
            )
        };
    (
        selected_variant,
        selected_route_id,
        selected_route_channel,
        selected_execution_channel,
    )
}

pub fn pm_route_usage_key(route_id: Option<&str>, route_channel: Option<&str>) -> Option<String> {
    route_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_ascii_lowercase())
        .or_else(|| {
            route_channel
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|value| format!("channel:{}", value.to_ascii_lowercase()))
        })
}

pub fn pm_should_consume_source_quota(probe_kernel_active: bool, best_turn_adopted: bool) -> bool {
    !probe_kernel_active || best_turn_adopted
}

pub fn is_pm_route_over_quota(
    route_usage_counts: &HashMap<String, usize>,
    route_id: Option<&str>,
    route_channel: Option<&str>,
    max_calls_per_source: usize,
) -> bool {
    let Some(key) = pm_route_usage_key(route_id, route_channel) else {
        return false;
    };
    route_usage_counts.get(&key).copied().unwrap_or(0) >= max_calls_per_source
}

pub fn is_pm_route_blocked(
    route_blocklist: &HashSet<String>,
    route_id: Option<&str>,
    route_channel: Option<&str>,
) -> bool {
    let Some(key) = pm_route_usage_key(route_id, route_channel) else {
        return false;
    };
    route_blocklist.contains(&key)
}

pub fn record_pm_route_failure_and_maybe_block(
    route_fail_streaks: &mut HashMap<String, usize>,
    route_blocklist: &mut HashSet<String>,
    route_id: Option<&str>,
    route_channel: Option<&str>,
    block_threshold: usize,
) -> Option<String> {
    let key = pm_route_usage_key(route_id, route_channel)?;
    let next = route_fail_streaks
        .get(&key)
        .copied()
        .unwrap_or(0)
        .saturating_add(1);
    route_fail_streaks.insert(key.clone(), next);
    if next >= block_threshold.max(1) {
        route_blocklist.insert(key.clone());
        return Some(key);
    }
    None
}

pub fn record_pm_route_success(
    route_fail_streaks: &mut HashMap<String, usize>,
    route_blocklist: &mut HashSet<String>,
    route_id: Option<&str>,
    route_channel: Option<&str>,
) {
    if let Some(key) = pm_route_usage_key(route_id, route_channel) {
        route_fail_streaks.remove(&key);
        route_blocklist.remove(&key);
    }
}

fn prioritize_pm_query_variants_for_attempts(query_variants: &[String]) -> Vec<usize> {
    fn is_noise_variant(value: &str) -> bool {
        let lower = value.to_ascii_lowercase();
        lower.contains("exec_constraints")
            || lower.contains("task_graph")
            || lower.contains("retrieve_constraints")
            || lower.contains("repair_scope")
            || lower.contains("report_json")
    }

    fn has_non_latin(value: &str) -> bool {
        value.chars().any(|ch| (ch as u32) > 0x024F)
    }

    fn score(value: &str) -> (u8, usize) {
        let trimmed = value.trim();
        let len = trimmed.chars().count();
        if trimmed.is_empty() {
            return (4, len);
        }
        if is_noise_variant(trimmed) {
            return (4, len);
        }
        if len > 220 {
            return (3, len);
        }
        if len > 140 {
            return (2, len);
        }
        if len > 90 {
            return (1, len);
        }
        if has_non_latin(trimmed) {
            return (0, len);
        }
        (0, len)
    }

    let mut scored: Vec<(usize, (u8, usize))> = query_variants
        .iter()
        .enumerate()
        .map(|(idx, item)| (idx, score(item)))
        .collect();
    scored.sort_by_key(|(_, key)| *key);
    scored.into_iter().map(|(idx, _)| idx).collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PmSourceExhaustionReason {
    QuotaOnly,
    BlockedOnly,
    QuotaAndBlocked,
    Unavailable,
}

pub fn classify_pm_source_exhaustion_reason(
    enabled_routes: &[PmEnabledRoute],
    route_usage_counts: &HashMap<String, usize>,
    route_blocklist: &HashSet<String>,
    max_calls_per_source: usize,
) -> PmSourceExhaustionReason {
    if enabled_routes.is_empty() {
        return PmSourceExhaustionReason::Unavailable;
    }

    let mut all_quota = true;
    let mut all_blocked = true;
    for row in enabled_routes {
        let over_quota = is_pm_route_over_quota(
            route_usage_counts,
            Some(row.route_id.as_str()),
            Some(row.channel.as_str()),
            max_calls_per_source,
        );
        let blocked = is_pm_route_blocked(
            route_blocklist,
            Some(row.route_id.as_str()),
            Some(row.channel.as_str()),
        );
        all_quota &= over_quota;
        all_blocked &= blocked;
    }

    match (all_quota, all_blocked) {
        (true, true) => PmSourceExhaustionReason::QuotaAndBlocked,
        (true, false) => PmSourceExhaustionReason::QuotaOnly,
        (false, true) => PmSourceExhaustionReason::BlockedOnly,
        (false, false) => PmSourceExhaustionReason::Unavailable,
    }
}

pub fn pm_source_exhaustion_reason_code(reason: PmSourceExhaustionReason) -> &'static str {
    match reason {
        PmSourceExhaustionReason::QuotaOnly => "source_quota_exhausted",
        PmSourceExhaustionReason::BlockedOnly => "source_route_blocked",
        PmSourceExhaustionReason::QuotaAndBlocked => "source_quota_exhausted_and_blocked",
        PmSourceExhaustionReason::Unavailable => "source_unavailable",
    }
}

pub fn pick_pm_attempt_preferences_with_source_quota_and_blocked(
    query_variants: &[String],
    enabled_routes: &[PmEnabledRoute],
    attempt: usize,
    route_usage_counts: &HashMap<String, usize>,
    route_blocklist: &HashSet<String>,
    max_calls_per_source: usize,
) -> (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    bool,
) {
    let (selected_variant, selected_route_id, selected_route_channel, selected_execution_channel) =
        pick_pm_attempt_preferences(query_variants, enabled_routes, attempt);
    if selected_route_id.is_none() || enabled_routes.is_empty() {
        return (
            selected_variant,
            selected_route_id,
            selected_route_channel,
            selected_execution_channel,
            false,
        );
    }
    if !is_pm_route_over_quota(
        route_usage_counts,
        selected_route_id.as_deref(),
        selected_route_channel.as_deref(),
        max_calls_per_source,
    ) && !is_pm_route_blocked(
        route_blocklist,
        selected_route_id.as_deref(),
        selected_route_channel.as_deref(),
    ) {
        return (
            selected_variant,
            selected_route_id,
            selected_route_channel,
            selected_execution_channel,
            false,
        );
    }

    let safe_attempt = if attempt == 0 { 1 } else { attempt };
    let start_idx = (safe_attempt - 1) % enabled_routes.len();
    for offset in 0..enabled_routes.len() {
        let row = &enabled_routes[(start_idx + offset) % enabled_routes.len()];
        if is_pm_route_over_quota(
            route_usage_counts,
            Some(row.route_id.as_str()),
            Some(row.channel.as_str()),
            max_calls_per_source,
        ) {
            continue;
        }
        if is_pm_route_blocked(
            route_blocklist,
            Some(row.route_id.as_str()),
            Some(row.channel.as_str()),
        ) {
            continue;
        }
        return (
            selected_variant,
            Some(row.route_id.clone()),
            Some(row.channel.clone()),
            Some(row.execution_channel.clone()),
            false,
        );
    }

    (
        selected_variant,
        selected_route_id,
        selected_route_channel,
        selected_execution_channel,
        true,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enabled_route() -> serde_json::Value {
        serde_json::json!({
            "routeId": "web.search.general",
            "channel": "web_search",
            "executionChannel": "search",
            "enabled": true
        })
    }

    #[test]
    fn first_party_only_task_graph_produces_no_probe_candidates() {
        let plan = serde_json::json!({
            "queryVariants": ["internal ROI cohort analysis should not be searched"],
            "sourceRoutes": [enabled_route()],
            "parallelism": {
                "probeRouteFanoutMax": 1,
                "probeCandidateMax": 4,
                "maxConcurrentSubtasks": 2,
                "maxProbePerSubtask": 2
            },
            "taskGraph": {
                "intent": "analysis",
                "decompositionMode": "light",
                "subtasks": [
                    {
                        "id": "cohort",
                        "title": "一手数据人群归因",
                        "goal": "基于用户提供的一手指标识别优先爆破人群",
                        "queries": [],
                        "deliverable": "内部指标归因",
                        "requiredEvidenceType": "first_party",
                        "priority": "high"
                    }
                ]
            }
        });

        let candidates = build_pm_probe_candidates(&plan);
        assert!(candidates.is_empty(), "{candidates:?}");
    }

    #[test]
    fn mixed_task_graph_filters_first_party_subtasks_before_light_cap() {
        let plan = serde_json::json!({
            "queryVariants": ["fallback variant"],
            "sourceRoutes": [enabled_route()],
            "parallelism": {
                "probeRouteFanoutMax": 1,
                "probeCandidateMax": 4,
                "maxConcurrentSubtasks": 1,
                "maxProbePerSubtask": 1
            },
            "taskGraph": {
                "intent": "decision_support",
                "decompositionMode": "light",
                "subtasks": [
                    {
                        "id": "internal",
                        "title": "一手数据拆解",
                        "goal": "基于用户提供的一手数据拆解 ROI 与留存",
                        "queries": [],
                        "deliverable": "内部指标结论",
                        "requiredEvidenceType": "first_party",
                        "priority": "high"
                    },
                    {
                        "id": "cases",
                        "title": "外部案例",
                        "goal": "检索可比案例与机制启发",
                        "queries": ["rewarded ads retention case study"],
                        "deliverable": "案例启发",
                        "requiredEvidenceType": "external",
                        "priority": "medium"
                    }
                ]
            }
        });

        let candidates = build_pm_probe_candidates(&plan);
        assert_eq!(candidates.len(), 1, "{candidates:?}");
        assert_eq!(candidates[0].subtask_id.as_deref(), Some("cases"));
        assert_eq!(candidates[0].variant, "rewarded ads retention case study");
    }

    #[test]
    fn probe_candidates_cover_each_external_subtask_before_second_probe() {
        let plan = serde_json::json!({
            "sourceRoutes": [enabled_route()],
            "parallelism": {
                "probeRouteFanoutMax": 1,
                "probeCandidateMax": 4,
                "maxConcurrentSubtasks": 4,
                "maxProbePerSubtask": 2,
                "minSourcesPerSubtask": 1
            },
            "taskGraph": {
                "decompositionMode": "light",
                "subtasks": [
                    {"id": "s1", "title": "市场", "queries": ["market q1", "market q2"], "requiredEvidenceType": "external", "priority": "high"},
                    {"id": "s2", "title": "用户", "queries": ["user q1", "user q2"], "requiredEvidenceType": "external", "priority": "high"},
                    {"id": "s3", "title": "竞品", "queries": ["competitor q1", "competitor q2"], "requiredEvidenceType": "external", "priority": "medium"},
                    {"id": "s4", "title": "风险", "queries": ["risk q1", "risk q2"], "requiredEvidenceType": "external", "priority": "medium"}
                ]
            }
        });

        let candidates = build_pm_probe_candidates(&plan);
        let first_ids = candidates
            .iter()
            .take(4)
            .filter_map(|candidate| candidate.subtask_id.as_deref())
            .collect::<Vec<_>>();

        assert_eq!(first_ids, vec!["s1", "s2", "s3", "s4"]);
    }
}
