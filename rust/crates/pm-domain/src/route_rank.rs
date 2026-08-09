use std::collections::HashMap;

pub fn route_priority_weight(priority: &str) -> f64 {
    match priority.to_ascii_lowercase().as_str() {
        "high" => 1.0,
        "medium" => 0.7,
        "low" => 0.45,
        _ => 0.6,
    }
}

#[derive(Debug, Clone)]
pub struct PmRouteRankComponents {
    pub route_id: String,
    pub channel: String,
    pub execution_channel: String,
    pub priority: String,
    pub enabled: bool,
    pub learned_score: f64,
    pub health_score: f64,
    pub health_penalty: f64,
    pub topic_affinity: f64,
    pub channel_weight: f64,
    pub priority_weight: f64,
    pub final_score: f64,
    pub health_signal: Option<PmRouteHealthSignal>,
}

pub fn pm_route_channel_weight(
    execution_channel: &str,
    search_score: f64,
    browser_score: f64,
) -> f64 {
    if execution_channel.eq_ignore_ascii_case("browser") {
        browser_score
    } else {
        search_score
    }
}

pub fn pm_route_rank_components(
    route: &serde_json::Value,
    search_score: f64,
    browser_score: f64,
    learned_scores: &HashMap<String, f64>,
    route_health_scores: &HashMap<String, PmRouteHealthSignal>,
    user_question: &str,
) -> PmRouteRankComponents {
    let route_id = route
        .get("routeId")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let channel = route
        .get("channel")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let execution_channel = route
        .get("executionChannel")
        .and_then(|v| v.as_str())
        .unwrap_or("search")
        .to_string();
    let priority = route
        .get("priority")
        .and_then(|v| v.as_str())
        .unwrap_or("medium")
        .to_string();
    let enabled = route
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let learned_score = learned_scores.get(&route_id).copied().unwrap_or(0.5);
    let health_key = pm_route_health_key(&route_id, &channel);
    let health_signal = route_health_scores.get(&health_key).cloned();
    let health_score = health_signal
        .as_ref()
        .map(|x| x.health_score)
        .unwrap_or(0.5);
    let health_penalty = health_signal
        .as_ref()
        .map(pm_route_health_penalty)
        .unwrap_or(0.0);
    let topic_affinity = pm_route_topic_affinity(&route_id, &channel, user_question);
    let channel_weight = pm_route_channel_weight(&execution_channel, search_score, browser_score);
    let priority_weight = route_priority_weight(&priority);
    let enabled_weight = if enabled { 1.0 } else { 0.0 };
    let final_score = enabled_weight
        * (learned_score * 0.35
            + health_score * 0.26
            + topic_affinity * 0.16
            + channel_weight * 0.15
            + priority_weight * 0.08)
        - health_penalty;
    PmRouteRankComponents {
        route_id,
        channel,
        execution_channel,
        priority,
        enabled,
        learned_score,
        health_score,
        health_penalty,
        topic_affinity,
        channel_weight,
        priority_weight,
        final_score,
        health_signal,
    }
}

pub fn pm_route_rank_breakdown_json(
    rank: usize,
    components: &PmRouteRankComponents,
) -> serde_json::Value {
    let signal = components.health_signal.as_ref();
    serde_json::json!({
        "rank": rank,
        "routeId": components.route_id,
        "channel": components.channel,
        "executionChannel": components.execution_channel,
        "priority": components.priority,
        "enabled": components.enabled,
        "finalScore": components.final_score,
        "weights": {
            "learned": 0.35,
            "health": 0.26,
            "topicAffinity": 0.16,
            "channel": 0.15,
            "priority": 0.08,
        },
        "components": {
            "learnedScore": components.learned_score,
            "healthScore": components.health_score,
            "topicAffinity": components.topic_affinity,
            "channelWeight": components.channel_weight,
            "priorityWeight": components.priority_weight,
            "healthPenalty": components.health_penalty,
        },
        "healthSignal": {
            "runCount": signal.map(|x| x.run_count).unwrap_or(0),
            "failureCount": signal.map(|x| x.failure_count).unwrap_or(0),
            "timeoutCount": signal.map(|x| x.timeout_count).unwrap_or(0),
            "avgLatencyMs": signal.and_then(|x| x.avg_latency_ms),
            "lastErrorCode": signal.and_then(|x| x.last_error_code.clone()),
        }
    })
}

#[derive(Debug, Clone, Default)]
pub struct PmRouteHealthSignal {
    pub health_score: f64,
    pub run_count: u64,
    pub failure_count: u64,
    pub timeout_count: u64,
    pub avg_latency_ms: Option<f64>,
    pub last_error_code: Option<String>,
}

pub fn pm_route_health_key(route_id: &str, channel: &str) -> String {
    format!(
        "{}#{}",
        route_id.trim().to_ascii_lowercase(),
        channel.trim().to_ascii_lowercase()
    )
}

pub fn contains_cjk(text: &str) -> bool {
    text.chars()
        .any(|ch| ('\u{4e00}'..='\u{9fff}').contains(&ch))
}

pub fn pm_route_topic_affinity(route_id: &str, channel: &str, question: &str) -> f64 {
    let route = format!(
        "{} {}",
        route_id.to_ascii_lowercase(),
        channel.to_ascii_lowercase()
    );
    let q = question.to_ascii_lowercase();
    let mut score: f64 = 0.5;

    let topic_hints: [(&str, &[&str]); 7] = [
        (
            "reviews",
            &["review", "rating", "评分", "评价", "comment", "feedback"],
        ),
        (
            "forums",
            &["forum", "reddit", "community", "discussion", "帖子", "社区"],
        ),
        ("social", &["social", "x.com", "twitter", "weibo", "tiktok"]),
        (
            "news",
            &["news", "press", "trend", "report", "资讯", "新闻"],
        ),
        ("github", &["github", "issue", "repo", "open source"]),
        ("browser", &["blocked", "anti-bot", "动态", "js渲染"]),
        (
            "search",
            &["overview", "market", "global", "research", "分析"],
        ),
    ];

    for (route_hint, tokens) in topic_hints {
        if !route.contains(route_hint) {
            continue;
        }
        if tokens.iter().any(|t| q.contains(t)) {
            score += 0.22;
        }
    }

    if contains_cjk(question) {
        // CJK queries usually benefit from wider web search first, then forums.
        if route.contains("web.search") || route.contains("search") {
            score += 0.08;
        }
        if route.contains("forums") || route.contains("reddit") {
            score += 0.05;
        }
    }

    score.clamp(0.0, 1.0)
}

pub fn pm_route_health_penalty(signal: &PmRouteHealthSignal) -> f64 {
    let mut penalty = 0.0;
    if signal.run_count >= 2 {
        let fail_rate = if signal.run_count == 0 {
            0.0
        } else {
            signal.failure_count as f64 / signal.run_count as f64
        };
        penalty += fail_rate.clamp(0.0, 1.0) * 0.20;
    }
    if signal.timeout_count > 0 {
        penalty += (signal.timeout_count as f64).min(4.0) * 0.05;
    }
    if let Some(latency) = signal.avg_latency_ms {
        if latency > 9_000.0 {
            penalty += 0.12;
        } else if latency > 6_000.0 {
            penalty += 0.08;
        } else if latency > 3_500.0 {
            penalty += 0.04;
        }
    }
    if signal
        .last_error_code
        .as_deref()
        .is_some_and(|x| x.contains("timeout") || x.contains("network"))
    {
        penalty += 0.06;
    }
    penalty.clamp(0.0, 0.45)
}

pub fn rank_pm_plan_routes_with_scores(
    plan: &mut serde_json::Value,
    search_score: f64,
    browser_score: f64,
    learned_scores: &HashMap<String, f64>,
    route_health_scores: &HashMap<String, PmRouteHealthSignal>,
    user_question: &str,
) {
    let (selected_route_ids, route_score_breakdown): (
        Vec<serde_json::Value>,
        Vec<serde_json::Value>,
    ) = {
        let Some(routes) = plan
            .get_mut("sourceRoutes")
            .and_then(|value| value.as_array_mut())
        else {
            return;
        };

        routes.sort_by(|a, b| {
            let score_a = pm_route_rank_components(
                a,
                search_score,
                browser_score,
                learned_scores,
                route_health_scores,
                user_question,
            )
            .final_score;
            let score_b = pm_route_rank_components(
                b,
                search_score,
                browser_score,
                learned_scores,
                route_health_scores,
                user_question,
            )
            .final_score;
            score_b
                .partial_cmp(&score_a)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let selected_route_ids = routes
            .iter()
            .filter(|route| {
                route
                    .get("enabled")
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false)
            })
            .filter_map(|route| route.get("routeId").and_then(|value| value.as_str()))
            .map(|route_id| serde_json::Value::String(route_id.to_string()))
            .collect::<Vec<_>>();
        let route_score_breakdown = routes
            .iter()
            .enumerate()
            .map(|(idx, route)| {
                let components = pm_route_rank_components(
                    route,
                    search_score,
                    browser_score,
                    learned_scores,
                    route_health_scores,
                    user_question,
                );
                pm_route_rank_breakdown_json(idx.saturating_add(1), &components)
            })
            .collect::<Vec<_>>();
        (selected_route_ids, route_score_breakdown)
    };

    if let Some(route_ids) = plan
        .get_mut("selectedRouteIds")
        .and_then(|value| value.as_array_mut())
    {
        route_ids.clear();
        route_ids.extend(selected_route_ids);
    }
    if let Some(obj) = plan.as_object_mut() {
        obj.insert(
            "routeScoreBreakdown".to_string(),
            serde_json::Value::Array(route_score_breakdown),
        );
    }
}
