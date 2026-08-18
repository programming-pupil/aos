use std::collections::{BTreeSet, HashSet};

use crate::query_hygiene::sanitize_pm_search_queries;

pub fn build_pm_query_variants(question: &str) -> Vec<String> {
    let base = question.trim();
    if base.is_empty() {
        return Vec::new();
    }
    let mut variants = vec![base.to_string()];
    let expansions = [
        ("难点", "阻力"),
        ("痛点", "用户反馈"),
        ("痛点", "转化阻力"),
        ("需求", "用户反馈"),
        ("需求", "待满足场景"),
        ("留存", "流失"),
        ("问题", "反馈"),
        ("complaint", "pain point"),
        ("retention", "churn"),
        ("issue", "friction"),
    ];
    for (from, to) in expansions {
        if base.contains(from) {
            variants.push(base.replace(from, to));
        }
    }
    variants.sort();
    variants.dedup();
    sanitize_pm_search_queries(variants, Some(base), 5)
}

#[derive(Debug, Clone, Copy)]
struct PmRouteCapabilities {
    search: bool,
    forum: bool,
    social: bool,
    review_evidence: bool,
    code: bool,
    news: bool,
}

fn detect_pm_route_capabilities(mcp_servers: &[String], skills: &[String]) -> PmRouteCapabilities {
    let names: Vec<String> = mcp_servers
        .iter()
        .chain(skills.iter())
        .map(|name| name.to_ascii_lowercase())
        .collect();
    let has = |tokens: &[&str]| -> bool {
        names
            .iter()
            .any(|name| tokens.iter().any(|token| name.contains(token)))
    };
    // Built-in WebSearch is always available; MCP/skills extend route coverage rather than gate it.
    let builtin_search_available = true;
    PmRouteCapabilities {
        search: builtin_search_available
            || has(&[
                "search", "serp", "tavily", "exa", "google", "bing", "duck", "jina", "brave",
            ]),
        forum: has(&["reddit", "forum", "community", "linuxdo", "discourse", "hn"]),
        social: has(&["social", "weibo", "twitter", "x", "trend"]),
        review_evidence: has(&["review", "rating", "feedback", "voice", "customer"]),
        code: has(&["github", "gitlab", "repo", "issue"]),
        news: has(&["news", "rss", "feed"]),
    }
}

pub fn build_pm_source_routes(
    channels: &[String],
    mcp_servers: &[String],
    skills: &[String],
) -> Vec<serde_json::Value> {
    #[allow(clippy::too_many_arguments)]
    fn push_pm_source_route(
        routes: &mut Vec<serde_json::Value>,
        route_id: &str,
        channel: &str,
        enabled: bool,
        reason: &str,
        quota: u64,
        priority: &str,
        execution_channel: &str,
        _tool_hints: &[&str],
    ) {
        routes.push(serde_json::json!({
            "routeId": route_id,
            "channel": channel,
            "enabled": enabled,
            "priority": priority,
            "executionChannel": execution_channel,
            "quota": quota,
            "reason": reason,
            "toolHints": ["mcp_search"],
        }));
    }

    let caps = detect_pm_route_capabilities(mcp_servers, skills);
    let mut routes: Vec<serde_json::Value> = Vec::new();

    for channel in channels {
        match channel.as_str() {
            "reddit" => {
                push_pm_source_route(
                    &mut routes,
                    "reddit.community.search",
                    "reddit",
                    caps.forum || caps.search,
                    "Reddit and community discussions",
                    4,
                    "high",
                    "search",
                    &["mcp_search"],
                );
            }
            "forum" | "forums" => {
                push_pm_source_route(
                    &mut routes,
                    "community.forums.search",
                    channel,
                    caps.forum || caps.search,
                    "Forum and community evidence",
                    4,
                    "high",
                    "search",
                    &["mcp_search"],
                );
            }
            "social" => {
                push_pm_source_route(
                    &mut routes,
                    "social.trends.search",
                    "social",
                    caps.social || caps.search,
                    "Social trends and hot topics",
                    3,
                    "high",
                    "search",
                    &["mcp_search"],
                );
            }
            "review_evidence" | "app_store_reviews" | "reviews" => {
                push_pm_source_route(
                    &mut routes,
                    "reviews.evidence.search",
                    "review_evidence",
                    caps.review_evidence || caps.search,
                    "Review and feedback evidence",
                    4,
                    "high",
                    "search",
                    &["mcp_search"],
                );
            }
            "github_issues" => {
                push_pm_source_route(
                    &mut routes,
                    "github.issues.search",
                    "github_issues",
                    caps.code || caps.search,
                    "Public issue trackers",
                    3,
                    "medium",
                    "search",
                    &["mcp_search"],
                );
            }
            "news_sites" => {
                push_pm_source_route(
                    &mut routes,
                    "news.sites.search",
                    "news_sites",
                    caps.news || caps.search,
                    "News and editorial sources",
                    3,
                    "medium",
                    "search",
                    &["mcp_search"],
                );
            }
            "web_search" => {
                push_pm_source_route(
                    &mut routes,
                    "web.search.general",
                    "web_search",
                    caps.search,
                    "General web retrieval",
                    4,
                    "high",
                    "search",
                    &["mcp_search"],
                );
            }
            _ => {
                push_pm_source_route(
                    &mut routes,
                    &format!("generic.{channel}.search"),
                    channel,
                    caps.search,
                    "Generic retrieval route",
                    2,
                    "medium",
                    "search",
                    &["mcp_search"],
                );
            }
        }
    }

    if routes.is_empty() {
        push_pm_source_route(
            &mut routes,
            "fallback.web.search",
            "web_search",
            caps.search || mcp_servers.is_empty(),
            "Fallback route when channel detection misses",
            3,
            "high",
            "search",
            &["mcp_search"],
        );
    }

    let has_enabled_route = routes.iter().any(|route| {
        route
            .get("enabled")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
    });
    if !has_enabled_route {
        let mut flipped = false;
        for route in &mut routes {
            let route_id = route.get("routeId").and_then(serde_json::Value::as_str);
            if route_id != Some("fallback.web.search") {
                continue;
            }
            if let Some(obj) = route.as_object_mut() {
                obj.insert("enabled".to_string(), serde_json::json!(true));
                obj.insert(
                    "reason".to_string(),
                    serde_json::json!("Forced fallback route when all planned routes are disabled"),
                );
            }
            flipped = true;
            break;
        }
        if !flipped {
            push_pm_source_route(
                &mut routes,
                "fallback.web.search",
                "web_search",
                true,
                "Forced fallback route when all planned routes are disabled",
                3,
                "high",
                "search",
                &["mcp_search"],
            );
        }
    }

    let mut dedup = BTreeSet::new();
    routes.retain(|route| {
        let route_id = route
            .get("routeId")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        dedup.insert(route_id.to_string())
    });
    routes
}

pub fn normalize_pm_channel_name(raw: &str) -> Option<String> {
    let normalized = raw.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return None;
    }
    let canonical = match normalized.as_str() {
        "web" | "web_search" | "search" => "web_search",
        "forum" | "forums" | "community" | "reddit" => "forum",
        "social" | "social_trends" => "social",
        "review" | "reviews" | "feedback" | "ratings" | "review_evidence" | "app_store_reviews"
        | "app_reviews" | "store_reviews" => "review_evidence",
        "github" | "issues" | "github_issues" | "code" => "github_issues",
        "news" | "news_sites" => "news_sites",
        other => other,
    };
    Some(canonical.to_string())
}

pub fn parse_pm_route_channels_env(raw: &str) -> Vec<String> {
    let mut channels = Vec::<String>::new();
    let mut seen = HashSet::<String>::new();
    for token in raw
        .split([',', ';', '|'])
        .map(str::trim)
        .filter(|token| !token.is_empty())
    {
        let Some(channel) = normalize_pm_channel_name(token) else {
            continue;
        };
        if seen.insert(channel.clone()) {
            channels.push(channel);
        }
    }
    channels
}

pub fn resolve_pm_plan_channels(question: &str) -> Vec<String> {
    // Guardrail: never exceed 3 planned routes to prevent tool overuse.
    // PM_ROUTE_MAX_CHANNELS may lower this ceiling, but cannot raise above 3.
    let max_channels = std::env::var("PM_ROUTE_MAX_CHANNELS")
        .ok()
        .and_then(|raw| raw.trim().parse::<usize>().ok())
        .unwrap_or(3)
        .clamp(1, 3);
    let env_value = std::env::var("PM_ROUTE_CHANNELS").unwrap_or_else(|_| "auto".to_string());
    let mode = env_value.trim();
    if !mode.is_empty() && !mode.eq_ignore_ascii_case("auto") {
        let mut manual_channels = parse_pm_route_channels_env(mode);
        if manual_channels.len() > max_channels {
            manual_channels.truncate(max_channels);
        }
        if !manual_channels.is_empty() {
            return manual_channels;
        }
    }
    build_pm_auto_channels(question, max_channels)
}

fn push_pm_channel_if_needed(channels: &mut Vec<String>, seen: &mut HashSet<String>, value: &str) {
    let Some(channel) = normalize_pm_channel_name(value) else {
        return;
    };
    if seen.insert(channel.clone()) {
        channels.push(channel);
    }
}

pub fn build_pm_auto_channels(question: &str, max_channels: usize) -> Vec<String> {
    let mut channels = Vec::<String>::new();
    let mut seen = HashSet::<String>::new();
    push_pm_channel_if_needed(&mut channels, &mut seen, "web_search");

    if !pm_question_likely_requires_external_evidence(question) {
        return channels;
    }

    let q = question.trim();
    if pm_text_contains_any(
        q,
        &[
            "review",
            "rating",
            "customer review",
            "user review",
            "app store review",
            "play store review",
            "feedback",
            "complaint",
        ],
    ) || q.contains("评论")
        || q.contains("评分")
        || q.contains("差评")
        || q.contains("反馈")
    {
        push_pm_channel_if_needed(&mut channels, &mut seen, "review_evidence");
    }
    if pm_text_contains_any(
        q,
        &[
            "forum",
            "community",
            "reddit",
            "discord",
            "telegram",
            "user voice",
        ],
    ) || q.contains("论坛")
        || q.contains("社区")
        || q.contains("贴吧")
        || q.contains("讨论")
    {
        push_pm_channel_if_needed(&mut channels, &mut seen, "forum");
    }
    if pm_text_contains_any(
        q,
        &[
            "policy",
            "regulation",
            "law",
            "news",
            "market",
            "benchmark",
            "report",
            "forecast",
            "trend",
        ],
    ) || q.contains("政策")
        || q.contains("法规")
        || q.contains("新闻")
        || q.contains("市场")
        || q.contains("报告")
        || q.contains("趋势")
    {
        push_pm_channel_if_needed(&mut channels, &mut seen, "news_sites");
    }
    if pm_text_contains_any(
        q,
        &[
            "social",
            "twitter",
            "tiktok",
            "youtube",
            "hot topic",
            "x.com",
        ],
    ) || q.contains("社媒")
        || q.contains("热搜")
        || q.contains("话题")
    {
        push_pm_channel_if_needed(&mut channels, &mut seen, "social");
    }
    if pm_text_contains_any(
        q,
        &[
            "github",
            "issue",
            "sdk",
            "api",
            "integration",
            "repository",
            "open source",
            "bug",
            "crash",
        ],
    ) || q.contains("开源")
        || q.contains("代码")
        || q.contains("技术实现")
        || q.contains("报错")
    {
        push_pm_channel_if_needed(&mut channels, &mut seen, "github_issues");
    }

    if channels.len() > max_channels {
        channels.truncate(max_channels);
    }
    channels
}

pub fn pm_text_contains_any(text: &str, tokens: &[&str]) -> bool {
    let lower = text.to_ascii_lowercase();
    tokens.iter().any(|token| lower.contains(token))
}

pub fn pm_question_likely_requires_external_evidence(question: &str) -> bool {
    let trimmed = question.trim();
    if trimmed.is_empty() {
        return false;
    }
    let lower = trimmed.to_ascii_lowercase();
    let explicit_no_search_tokens = [
        "no web",
        "without web",
        "without search",
        "do not search",
        "don't search",
        "no external lookup",
        "no external search",
        "only use the provided data",
        "only use provided data",
        "based only on the provided data",
        "不需要联网",
        "不要联网",
        "无需联网",
        "不用联网",
        "别联网",
        "不要搜索",
        "不需要搜索",
        "无需搜索",
        "不用搜索",
        "不要检索",
        "不需要检索",
        "无需检索",
        "不用检索",
        "只基于",
        "仅基于",
        "只根据",
        "仅根据",
        "不要外部",
        "不需要外部",
    ];
    if explicit_no_search_tokens
        .iter()
        .any(|token| lower.contains(&token.to_ascii_lowercase()) || trimmed.contains(token))
    {
        return false;
    }
    let explicit_search_tokens = [
        "online",
        "web search",
        "search for",
        "search the web",
        "look up",
        "lookup",
        "find sources",
        "source-backed",
        "上网",
        "联网",
        "搜索",
        "检索",
        "查一下",
        "查下",
        "搜一下",
        "搜下",
        "找一下",
        "帮我找",
        "查找",
    ];
    if explicit_search_tokens
        .iter()
        .any(|token| lower.contains(&token.to_ascii_lowercase()) || trimmed.contains(token))
    {
        return true;
    }
    let en_tokens = [
        "latest",
        "most recent",
        "today",
        "tomorrow",
        "yesterday",
        "this week",
        "this month",
        "news",
        "external report",
        "policy",
        "regulation",
        "law",
        "price",
        "stock price",
        "gold price",
        "exchange rate",
        "weather",
    ];
    let zh_tokens = [
        "最新",
        "最近",
        "今天",
        "今日",
        "明天",
        "昨天",
        "本周",
        "本月",
        "新闻",
        "行情",
        "外部报告",
        "政策变化",
        "政策更新",
        "法规",
        "监管",
        "法律",
        "价格",
        "金价",
        "油价",
        "股价",
        "汇率",
        "天气",
        "预报",
        "实时",
    ];
    if pm_text_contains_any(trimmed, &en_tokens) || zh_tokens.iter().any(|t| trimmed.contains(t)) {
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_data_summary_does_not_require_external_evidence_by_default() {
        let question = "我给你一组数据和一份内部报告，请基于这些数据汇总对比各项指标，输出结论。";
        assert!(!pm_question_likely_requires_external_evidence(question));
    }

    #[test]
    fn first_party_business_context_words_do_not_force_external_evidence() {
        let question = "当前产品是印尼网赚单机休闲 App 矩阵，请基于我给的数据和竞品方向给出策略。";
        assert!(!pm_question_likely_requires_external_evidence(question));
    }

    #[test]
    fn explicit_fresh_or_external_requests_require_external_evidence() {
        assert!(pm_question_likely_requires_external_evidence(
            "查下今天北京天气预报"
        ));
        assert!(pm_question_likely_requires_external_evidence(
            "帮我找一下这个行业的市场规模和竞品案例"
        ));
        assert!(pm_question_likely_requires_external_evidence(
            "look up the latest pricing benchmark"
        ));
        assert!(!pm_question_likely_requires_external_evidence(
            "分析这个行业的市场规模和竞品案例应该怎么拆解"
        ));
    }

    #[test]
    fn explicit_no_search_request_overrides_search_words() {
        assert!(!pm_question_likely_requires_external_evidence(
            "我给你一组内部数据，请只基于这组数据做ROI对比和投放建议，不需要联网。"
        ));
        assert!(!pm_question_likely_requires_external_evidence(
            "Only use the provided data; do not search the web."
        ));
    }
}
