use std::collections::HashSet;

use crate::task_graph::{
    infer_pm_subtask_required_evidence_type, normalize_pm_required_evidence_type,
    sanitize_pm_task_graph_queries,
};

#[derive(Debug, Clone)]
pub struct PmSubtaskRuntimeMeta {
    pub key: String,
    pub subtask_id: Option<String>,
    pub title: String,
    pub goal: Option<String>,
    pub deliverable: Option<String>,
    pub required_evidence_type: Option<String>,
    pub priority: String,
}

fn normalize_claim_key(input: &str) -> String {
    input
        .trim()
        .trim_matches(|ch: char| ch == '"' || ch == '\'' || ch == '`')
        .to_ascii_lowercase()
}

pub trait PmSubtaskOutcomeLike {
    fn subtask_key(&self) -> Option<&str>;
    fn subtask_id(&self) -> Option<&str>;
    fn subtask_title(&self) -> Option<&str>;
}

pub fn collect_pm_subtask_runtime_metas(plan: &serde_json::Value) -> Vec<PmSubtaskRuntimeMeta> {
    let Some(subtasks) = plan
        .get("taskGraph")
        .and_then(|value| value.get("subtasks"))
        .and_then(|value| value.as_array())
    else {
        return Vec::new();
    };
    let mut out = Vec::<PmSubtaskRuntimeMeta>::new();
    let mut seen = HashSet::<String>::new();
    for (idx, subtask) in subtasks.iter().enumerate() {
        let Some(obj) = subtask.as_object() else {
            continue;
        };
        let subtask_id = obj
            .get("id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(std::string::ToString::to_string);
        let title = obj
            .get("title")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(std::string::ToString::to_string)
            .unwrap_or_else(|| format!("subtask-{}", idx + 1));
        let goal = obj
            .get("goal")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(std::string::ToString::to_string);
        let deliverable = obj
            .get("deliverable")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(std::string::ToString::to_string);
        let required_evidence_type = obj
            .get("requiredEvidenceType")
            .or_else(|| obj.get("required_evidence_type"))
            .or_else(|| obj.get("evidenceType"))
            .or_else(|| obj.get("evidence_type"))
            .and_then(|v| v.as_str())
            .and_then(|value| normalize_pm_required_evidence_type(Some(value)))
            .or_else(|| {
                let queries = sanitize_pm_task_graph_queries(
                    obj.get("queries"),
                    goal.as_deref().unwrap_or(title.as_str()),
                    6,
                );
                Some(infer_pm_subtask_required_evidence_type(
                    Some(&title),
                    goal.as_deref(),
                    deliverable.as_deref(),
                    &queries,
                ))
            });
        let priority = obj
            .get("priority")
            .and_then(|v| v.as_str())
            .map(|value| value.trim().to_ascii_lowercase())
            .filter(|value| matches!(value.as_str(), "high" | "medium" | "low"))
            .unwrap_or_else(|| "medium".to_string());
        let mut key = subtask_id
            .clone()
            .map(|raw| normalize_claim_key(&raw))
            .filter(|value| !value.is_empty())
            .unwrap_or_default();
        if key.is_empty() {
            key = normalize_claim_key(&title);
        }
        if key.is_empty() {
            key = format!("subtask_{}", idx + 1);
        }
        if !seen.insert(key.clone()) {
            continue;
        }
        out.push(PmSubtaskRuntimeMeta {
            key,
            subtask_id,
            title,
            goal,
            deliverable,
            required_evidence_type,
            priority,
        });
    }
    out
}

pub fn resolve_subtask_runtime_key<T: PmSubtaskOutcomeLike>(outcome: &T) -> Option<String> {
    if let Some(key) = outcome
        .subtask_key()
        .map(normalize_claim_key)
        .filter(|value| !value.is_empty())
    {
        return Some(key);
    }
    if let Some(key) = outcome
        .subtask_id()
        .map(normalize_claim_key)
        .filter(|value| !value.is_empty())
    {
        return Some(key);
    }
    outcome
        .subtask_title()
        .map(normalize_claim_key)
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_metas_infer_required_evidence_type_when_missing() {
        let plan = serde_json::json!({
            "taskGraph": {
                "subtasks": [
                    {
                        "id": "first",
                        "title": "一手数据诊断与人群优先级排序",
                        "goal": "基于用户提供的数据做指标拆解",
                        "queries": [],
                        "deliverable": "内部诊断"
                    },
                    {
                        "id": "external",
                        "title": "外部案例参考",
                        "goal": "检索可比案例和行业最佳实践",
                        "queries": ["rewarded ads benchmark case study"],
                        "deliverable": "外部证据"
                    }
                ]
            }
        });

        let metas = collect_pm_subtask_runtime_metas(&plan);

        assert_eq!(
            metas
                .iter()
                .find(|meta| meta.key == "first")
                .and_then(|meta| meta.required_evidence_type.as_deref()),
            Some("first_party")
        );
        assert_eq!(
            metas
                .iter()
                .find(|meta| meta.key == "external")
                .and_then(|meta| meta.required_evidence_type.as_deref()),
            Some("external")
        );
    }
}
