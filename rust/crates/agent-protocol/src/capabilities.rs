use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolCandidate {
    pub name: String,
    pub schema_version: String,
    pub schema_tokens: u64,
    pub required_capability: String,
    pub domain: String,
    pub relevance: u8,
    pub side_effect: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ToolDecision {
    Active {
        reason: String,
        valid_for_turns: u32,
    },
    Deferred {
        reason: String,
    },
    Rejected {
        reason: String,
    },
}
#[derive(Debug, Clone, Default)]
pub struct ToolCapabilityRouter;
impl ToolCapabilityRouter {
    pub fn route(
        &self,
        candidates: &[ToolCandidate],
        authorized_capabilities: &[String],
        domain: &str,
        max_schema_tokens: u64,
    ) -> BTreeMap<String, ToolDecision> {
        let mut decisions = BTreeMap::new();
        let mut used = 0;
        let mut ordered: Vec<_> = candidates
            .iter()
            .filter(|c| c.domain == domain || c.domain == "common")
            .collect();
        ordered.sort_by_key(|c| std::cmp::Reverse(c.relevance));
        for candidate in ordered {
            if !authorized_capabilities
                .iter()
                .any(|cap| cap == &candidate.required_capability)
            {
                decisions.insert(
                    candidate.name.clone(),
                    ToolDecision::Rejected {
                        reason: "capability not authorized".into(),
                    },
                );
                continue;
            }
            if used + candidate.schema_tokens <= max_schema_tokens || candidate.relevance >= 90 {
                used += candidate.schema_tokens;
                decisions.insert(
                    candidate.name.clone(),
                    ToolDecision::Active {
                        reason: if candidate.relevance >= 90 {
                            "high relevance preactivation"
                        } else {
                            "budget-admitted"
                        }
                        .into(),
                        valid_for_turns: 1,
                    },
                );
            } else {
                decisions.insert(
                    candidate.name.clone(),
                    ToolDecision::Deferred {
                        reason: "schema budget exceeded; use tool_search before activation".into(),
                    },
                );
            }
        }
        decisions
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PromptVariant {
    pub prompt_id: String,
    pub version: String,
    pub owner: String,
    pub model_pattern: String,
    pub stable_system: String,
    pub domain_contract: String,
    pub section_sources: Vec<String>,
    pub priority: u16,
    pub scope: String,
    pub trust_level: String,
    pub input_schema_hash: String,
    pub output_schema_hash: String,
    pub model_capabilities: Vec<String>,
    pub tool_schema_version: String,
    pub max_input_tokens: u64,
    pub max_output_tokens: u64,
    pub cache_class: String,
    pub eval_suite: String,
    pub rollout_percent: u8,
    pub rollback_version: Option<String>,
    pub evaluation_passed: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PromptManifest {
    pub prompt_id: String,
    pub version: String,
    pub variant: String,
    pub stable_prefix_hash: String,
    pub tool_schema_hash: String,
    pub input_budget: u64,
    pub output_budget: u64,
    pub eval_suite: String,
    pub owner: String,
    pub scope: String,
    pub trust_level: String,
    pub input_schema_hash: String,
    pub output_schema_hash: String,
    pub tool_schema_version: String,
    pub cache_class: String,
    pub rollback_version: Option<String>,
    pub section_sources: Vec<String>,
    pub priority: u16,
    pub model_capabilities: Vec<String>,
    pub evaluation_passed: bool,
}
#[derive(Debug, Clone, Default)]
pub struct PromptRegistry {
    variants: Vec<PromptVariant>,
}
impl PromptRegistry {
    pub fn register(&mut self, variant: PromptVariant) {
        self.variants.retain(|v| {
            !(v.prompt_id == variant.prompt_id
                && v.version == variant.version
                && v.model_pattern == variant.model_pattern)
        });
        self.variants.push(variant);
    }
    pub fn resolve(&self, prompt_id: &str, model: &str) -> Option<PromptVariant> {
        self.resolve_for_request(prompt_id, model, model)
    }

    pub fn resolve_for_request(
        &self,
        prompt_id: &str,
        model: &str,
        rollout_key: &str,
    ) -> Option<PromptVariant> {
        self.variants
            .iter()
            .filter(|v| {
                v.prompt_id == prompt_id
                    && v.evaluation_passed
                    && (v.model_pattern == "*" || model.contains(&v.model_pattern))
                    && rollout_bucket(prompt_id, &v.version, rollout_key) < v.rollout_percent
            })
            .max_by_key(|v| {
                (
                    u8::from(v.model_pattern != "*"),
                    v.priority,
                    semantic_version_key(&v.version),
                )
            })
            .cloned()
    }
    pub fn manifest(
        variant: &PromptVariant,
        tool_schema_hash: &str,
        input_budget: u64,
        output_budget: u64,
    ) -> PromptManifest {
        let mut hasher = Sha256::new();
        hasher.update(variant.stable_system.as_bytes());
        hasher.update(variant.domain_contract.as_bytes());
        PromptManifest {
            prompt_id: variant.prompt_id.clone(),
            version: variant.version.clone(),
            variant: variant.model_pattern.clone(),
            stable_prefix_hash: hex::encode(hasher.finalize()),
            tool_schema_hash: tool_schema_hash.into(),
            input_budget,
            output_budget,
            eval_suite: variant.eval_suite.clone(),
            owner: variant.owner.clone(),
            scope: variant.scope.clone(),
            trust_level: variant.trust_level.clone(),
            input_schema_hash: variant.input_schema_hash.clone(),
            output_schema_hash: variant.output_schema_hash.clone(),
            tool_schema_version: variant.tool_schema_version.clone(),
            cache_class: variant.cache_class.clone(),
            rollback_version: variant.rollback_version.clone(),
            section_sources: variant.section_sources.clone(),
            priority: variant.priority,
            model_capabilities: variant.model_capabilities.clone(),
            evaluation_passed: variant.evaluation_passed,
        }
    }
}

fn semantic_version_key(version: &str) -> (u64, u64, u64, String) {
    let normalized = version.trim_start_matches('v');
    let mut parts = normalized.splitn(2, '-');
    let core = parts.next().unwrap_or_default();
    let suffix = parts.next().unwrap_or_default().to_string();
    let mut numbers = core.split('.').map(|part| part.parse::<u64>().unwrap_or(0));
    (
        numbers.next().unwrap_or(0),
        numbers.next().unwrap_or(0),
        numbers.next().unwrap_or(0),
        suffix,
    )
}

fn rollout_bucket(prompt_id: &str, version: &str, rollout_key: &str) -> u8 {
    let mut hasher = Sha256::new();
    hasher.update(prompt_id.as_bytes());
    hasher.update([0]);
    hasher.update(version.as_bytes());
    hasher.update([0]);
    hasher.update(rollout_key.as_bytes());
    hasher.finalize()[0] % 100
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tool_router_intersects_authority_and_budget() {
        let router = ToolCapabilityRouter;
        let tools = vec![
            ToolCandidate {
                name: "search".into(),
                schema_version: "1".into(),
                schema_tokens: 20,
                required_capability: "web".into(),
                domain: "common".into(),
                relevance: 90,
                side_effect: false,
            },
            ToolCandidate {
                name: "delete".into(),
                schema_version: "1".into(),
                schema_tokens: 20,
                required_capability: "delete".into(),
                domain: "common".into(),
                relevance: 99,
                side_effect: true,
            },
            ToolCandidate {
                name: "read".into(),
                schema_version: "1".into(),
                schema_tokens: 100,
                required_capability: "repo".into(),
                domain: "code".into(),
                relevance: 50,
                side_effect: false,
            },
        ];
        let result = router.route(&tools, &["web".into()], "common", 20);
        assert!(matches!(result["search"], ToolDecision::Active { .. }));
        assert!(matches!(result["delete"], ToolDecision::Rejected { .. }));
    }
    #[test]
    fn prompt_registry_manifest_records_stable_prefix_and_schema_lineage() {
        let mut registry = PromptRegistry::default();
        registry.register(PromptVariant {
            prompt_id: "pm".into(),
            version: "1.2.0".into(),
            owner: "pm-team".into(),
            model_pattern: "deepseek".into(),
            stable_system: "safe".into(),
            domain_contract: "json".into(),
            section_sources: vec!["pm-domain".into()],
            priority: 10,
            scope: "pm".into(),
            trust_level: "system".into(),
            input_schema_hash: "input-schema".into(),
            output_schema_hash: "schema".into(),
            model_capabilities: vec!["tools".into()],
            tool_schema_version: "v1".into(),
            max_input_tokens: 1_000,
            max_output_tokens: 500,
            cache_class: "stable_prefix".into(),
            eval_suite: "pm-golden".into(),
            rollout_percent: 100,
            rollback_version: Some("1.1.0".into()),
            evaluation_passed: true,
        });
        let variant = registry.resolve("pm", "deepseek-v4-flash").unwrap();
        let manifest = PromptRegistry::manifest(&variant, "tools-hash", 1000, 500);
        assert_eq!(manifest.tool_schema_hash, "tools-hash");
        assert_eq!(manifest.stable_prefix_hash.len(), 64);
        assert_eq!(manifest.owner, "pm-team");
    }

    #[test]
    fn prompt_registry_rejects_unevaluated_variants_and_prefers_model_specific_semver() {
        let base = |version: &str, model_pattern: &str, evaluated| PromptVariant {
            prompt_id: "chat".into(),
            version: version.into(),
            owner: "runtime".into(),
            model_pattern: model_pattern.into(),
            stable_system: "stable".into(),
            domain_contract: version.into(),
            section_sources: vec!["runtime".into()],
            priority: 10,
            scope: "chat".into(),
            trust_level: "system".into(),
            input_schema_hash: "input".into(),
            output_schema_hash: "output".into(),
            model_capabilities: vec![],
            tool_schema_version: "v1".into(),
            max_input_tokens: 1_000,
            max_output_tokens: 500,
            cache_class: "stable_prefix".into(),
            eval_suite: "chat-golden".into(),
            rollout_percent: 100,
            rollback_version: None,
            evaluation_passed: evaluated,
        };
        let mut registry = PromptRegistry::default();
        registry.register(base("1.9.0", "*", true));
        registry.register(base("1.10.0", "deepseek", true));
        registry.register(base("2.0.0", "deepseek", false));
        let resolved = registry
            .resolve_for_request("chat", "deepseek-v4-flash", "session-1")
            .unwrap();
        assert_eq!(resolved.version, "1.10.0");
        assert_eq!(resolved.model_pattern, "deepseek");
    }
}
