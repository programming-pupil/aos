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
    pub model_pattern: String,
    pub stable_system: String,
    pub domain_contract: String,
    pub output_schema_hash: String,
    pub eval_suite: String,
    pub rollout_percent: u8,
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
        self.variants
            .iter()
            .filter(|v| {
                v.prompt_id == prompt_id
                    && (v.model_pattern == "*" || model.contains(&v.model_pattern))
            })
            .max_by_key(|v| (v.rollout_percent, v.version.clone()))
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
        }
    }
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
            model_pattern: "deepseek".into(),
            stable_system: "safe".into(),
            domain_contract: "json".into(),
            output_schema_hash: "schema".into(),
            eval_suite: "pm-golden".into(),
            rollout_percent: 100,
        });
        let variant = registry.resolve("pm", "deepseek-v4-flash").unwrap();
        let manifest = PromptRegistry::manifest(&variant, "tools-hash", 1000, 500);
        assert_eq!(manifest.tool_schema_hash, "tools-hash");
        assert_eq!(manifest.stable_prefix_hash.len(), 64);
    }
}
