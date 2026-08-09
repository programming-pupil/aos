use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PmSearchLayer {
    FirstPartyEvidence,
    BuiltinWebSearch,
    NativeModelSearch,
    McpSearch,
    ConfiguredSearchProvider,
    RagLocal,
}

impl PmSearchLayer {
    pub const fn key(self) -> &'static str {
        match self {
            Self::FirstPartyEvidence => "first_party_report_evidence",
            Self::BuiltinWebSearch => "aos_builtin_web_search",
            Self::NativeModelSearch => "native_model_search",
            Self::McpSearch => "mcp_search",
            Self::ConfiguredSearchProvider => "configured_search_provider",
            Self::RagLocal => "rag_local",
        }
    }

    pub const fn adapter(self) -> &'static str {
        match self {
            Self::FirstPartyEvidence => "FirstPartyEvidenceAdapter",
            Self::BuiltinWebSearch => "AosBuiltinWebSearchAdapter",
            Self::NativeModelSearch => "ProviderNativeSearchAdapter",
            Self::McpSearch => "McpSearchAdapter",
            Self::ConfiguredSearchProvider => "ConfiguredSearchProviderAdapter",
            Self::RagLocal => "RagAdapter/LocalEvidenceAdapter",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::FirstPartyEvidence => "First-party report evidence",
            Self::BuiltinWebSearch => "AOS built-in web search",
            Self::NativeModelSearch => "Model native search",
            Self::McpSearch => "MCP search/browser/fetch",
            Self::ConfiguredSearchProvider => "Search Extension",
            Self::RagLocal => "RAG/local fallback",
        }
    }
}

pub const PM_SEARCH_FALLBACK_ORDER: [PmSearchLayer; 6] = [
    PmSearchLayer::FirstPartyEvidence,
    PmSearchLayer::ConfiguredSearchProvider,
    PmSearchLayer::BuiltinWebSearch,
    PmSearchLayer::NativeModelSearch,
    PmSearchLayer::McpSearch,
    PmSearchLayer::RagLocal,
];

pub fn pm_search_fallback_keys() -> Vec<&'static str> {
    PM_SEARCH_FALLBACK_ORDER
        .iter()
        .map(|layer| layer.key())
        .collect()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PmSearchProviderDescriptor {
    pub id: String,
    pub name: String,
    pub provider_type: String,
    pub enabled: bool,
    pub priority: i32,
    pub health_status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PmSearchLayerAvailability {
    pub layer: PmSearchLayer,
    pub key: String,
    pub adapter: String,
    pub label: String,
    pub available: bool,
    pub status: String,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PmSearchOrchestratorSnapshot {
    pub orchestrator: String,
    pub fallback_order: Vec<String>,
    pub adapters: Vec<String>,
    pub layers: Vec<PmSearchLayerAvailability>,
    pub effective_order: Vec<String>,
    pub degraded_reason: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct PmSearchOrchestratorInput {
    pub first_party_available: bool,
    pub builtin_web_search_available: bool,
    pub builtin_web_search_detail: Option<String>,
    pub native_available: bool,
    pub native_detail: Option<String>,
    pub mcp_available: bool,
    pub mcp_detail: Option<String>,
    pub configured_providers: Vec<PmSearchProviderDescriptor>,
    pub rag_local_available: bool,
    pub rag_local_detail: Option<String>,
}

pub struct PmSearchOrchestrator;

impl PmSearchOrchestrator {
    pub fn snapshot(input: PmSearchOrchestratorInput) -> PmSearchOrchestratorSnapshot {
        let configured_available = input
            .configured_providers
            .iter()
            .any(|provider| provider.enabled && provider.health_status != "unhealthy");
        let layers = PM_SEARCH_FALLBACK_ORDER
            .iter()
            .map(|layer| {
                let (available, status, detail) = match layer {
                    PmSearchLayer::FirstPartyEvidence => (
                        input.first_party_available,
                        if input.first_party_available {
                            "available"
                        } else {
                            "not_present"
                        },
                        if input.first_party_available {
                            "user-provided report/evidence is available"
                        } else {
                            "no first-party report evidence detected for this turn"
                        }
                        .to_string(),
                    ),
                    PmSearchLayer::BuiltinWebSearch => (
                        input.builtin_web_search_available,
                        if input.builtin_web_search_available {
                            "available"
                        } else {
                            "unavailable"
                        },
                        input.builtin_web_search_detail.clone().unwrap_or_else(|| {
                            if input.builtin_web_search_available {
                                "zero-configuration AOS web search is available".to_string()
                            } else {
                                "AOS built-in web search is unavailable".to_string()
                            }
                        }),
                    ),
                    PmSearchLayer::NativeModelSearch => (
                        input.native_available,
                        if input.native_available {
                            "available"
                        } else {
                            "not_configured"
                        },
                        input.native_detail.clone().unwrap_or_else(|| {
                            if input.native_available {
                                "model-native search path is available".to_string()
                            } else {
                                "no model-native search path detected".to_string()
                            }
                        }),
                    ),
                    PmSearchLayer::McpSearch => (
                        input.mcp_available,
                        if input.mcp_available {
                            "available"
                        } else {
                            "not_configured"
                        },
                        input.mcp_detail.clone().unwrap_or_else(|| {
                            if input.mcp_available {
                                "MCP search/browser/fetch server discovered".to_string()
                            } else {
                                "no enabled MCP search/browser/fetch server discovered".to_string()
                            }
                        }),
                    ),
                    PmSearchLayer::ConfiguredSearchProvider => (
                        configured_available,
                        if configured_available {
                            "available"
                        } else {
                            "not_configured"
                        },
                        if configured_available {
                            format!(
                                "{} enabled configured provider(s)",
                                input
                                    .configured_providers
                                    .iter()
                                    .filter(|provider| provider.enabled)
                                    .count()
                            )
                        } else {
                            "no healthy enabled Search Extension".to_string()
                        },
                    ),
                    PmSearchLayer::RagLocal => (
                        input.rag_local_available,
                        if input.rag_local_available {
                            "available"
                        } else {
                            "not_configured"
                        },
                        input.rag_local_detail.clone().unwrap_or_else(|| {
                            if input.rag_local_available {
                                "local/RAG evidence fallback is available through PM attachments, history, and local context".to_string()
                            } else {
                                "local/RAG evidence fallback is not configured".to_string()
                            }
                        }),
                    ),
                };
                PmSearchLayerAvailability {
                    layer: *layer,
                    key: layer.key().to_string(),
                    adapter: layer.adapter().to_string(),
                    label: layer.label().to_string(),
                    available,
                    status: status.to_string(),
                    detail,
                }
            })
            .collect::<Vec<_>>();

        let mut effective_order = Vec::new();
        effective_order.push(PmSearchLayer::FirstPartyEvidence.key().to_string());
        let configured_provider_order = input
            .configured_providers
            .iter()
            .filter(|provider| provider.enabled && provider.health_status != "unhealthy")
            .map(|provider| {
                format!(
                    "{}:{}",
                    PmSearchLayer::ConfiguredSearchProvider.key(),
                    provider.name
                )
            })
            .collect::<Vec<_>>();
        effective_order.extend(configured_provider_order);
        if input.builtin_web_search_available {
            effective_order.push(PmSearchLayer::BuiltinWebSearch.key().to_string());
        }
        if input.native_available {
            effective_order.push(PmSearchLayer::NativeModelSearch.key().to_string());
        }
        if input.mcp_available {
            effective_order.push(PmSearchLayer::McpSearch.key().to_string());
        }
        if input.rag_local_available {
            effective_order.push(PmSearchLayer::RagLocal.key().to_string());
        }

        let degraded_reason = if input.builtin_web_search_available
            || input.native_available
            || input.mcp_available
            || configured_available
        {
            None
        } else {
            Some(
                    "external search is unavailable; PM will use first-party evidence and local/RAG fallback"
                        .to_string(),
                )
        };

        PmSearchOrchestratorSnapshot {
            orchestrator: "PmSearchOrchestrator".to_string(),
            fallback_order: pm_search_fallback_keys()
                .into_iter()
                .map(ToOwned::to_owned)
                .collect(),
            adapters: PM_SEARCH_FALLBACK_ORDER
                .iter()
                .map(|layer| layer.adapter().to_string())
                .collect(),
            layers,
            effective_order,
            degraded_reason,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_order_matches_v5_contract() {
        assert_eq!(
            pm_search_fallback_keys(),
            vec![
                "first_party_report_evidence",
                "configured_search_provider",
                "aos_builtin_web_search",
                "native_model_search",
                "mcp_search",
                "rag_local",
            ]
        );
    }

    #[test]
    fn snapshot_degrades_when_external_layers_are_unavailable() {
        let snapshot = PmSearchOrchestrator::snapshot(PmSearchOrchestratorInput {
            first_party_available: true,
            rag_local_available: true,
            ..PmSearchOrchestratorInput::default()
        });
        assert!(snapshot.degraded_reason.is_some());
        assert_eq!(
            snapshot.effective_order,
            vec!["first_party_report_evidence", "rag_local"]
        );
    }

    #[test]
    fn snapshot_includes_configured_provider_by_priority_order() {
        let snapshot = PmSearchOrchestrator::snapshot(PmSearchOrchestratorInput {
            first_party_available: true,
            configured_providers: vec![PmSearchProviderDescriptor {
                id: "p1".to_string(),
                name: "Internal Search".to_string(),
                provider_type: "internal_http".to_string(),
                enabled: true,
                priority: 1,
                health_status: "unknown".to_string(),
            }],
            rag_local_available: true,
            ..PmSearchOrchestratorInput::default()
        });
        assert!(snapshot.degraded_reason.is_none());
        assert!(snapshot
            .effective_order
            .iter()
            .any(|item| item == "configured_search_provider:Internal Search"));
        assert_eq!(
            snapshot
                .effective_order
                .iter()
                .position(|item| item == "configured_search_provider:Internal Search"),
            Some(1)
        );
    }

    #[test]
    fn snapshot_exposes_zero_configuration_builtin_search() {
        let snapshot = PmSearchOrchestrator::snapshot(PmSearchOrchestratorInput {
            builtin_web_search_available: true,
            builtin_web_search_detail: Some("AOS runtime search".to_string()),
            ..PmSearchOrchestratorInput::default()
        });
        assert!(snapshot.degraded_reason.is_none());
        assert!(snapshot
            .effective_order
            .iter()
            .any(|item| item == "aos_builtin_web_search"));
    }

    #[test]
    fn snapshot_orders_configured_provider_before_native_when_both_available() {
        let snapshot = PmSearchOrchestrator::snapshot(PmSearchOrchestratorInput {
            first_party_available: true,
            native_available: true,
            configured_providers: vec![PmSearchProviderDescriptor {
                id: "p1".to_string(),
                name: "Brave".to_string(),
                provider_type: "brave".to_string(),
                enabled: true,
                priority: 1,
                health_status: "healthy".to_string(),
            }],
            mcp_available: true,
            rag_local_available: true,
            ..PmSearchOrchestratorInput::default()
        });
        assert_eq!(
            snapshot.effective_order,
            vec![
                "first_party_report_evidence",
                "configured_search_provider:Brave",
                "native_model_search",
                "mcp_search",
                "rag_local",
            ]
        );
    }
}
