use crate::SemanticSnapshot;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum PromptLayer {
    StableSystem,
    DomainContract,
    TaskPacket,
    RecentInteraction,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContextTrust {
    Instruction,
    GovernedState,
    UntrustedData,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ContextReference {
    pub id: String,
    pub version: Option<u64>,
    pub content_hash: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ContextOutputContract {
    pub contract_id: String,
    pub schema_version: String,
    pub media_type: String,
}
impl Default for ContextOutputContract {
    fn default() -> Self {
        Self {
            contract_id: "aos-answer".into(),
            schema_version: "v1".into(),
            media_type: "text/markdown".into(),
        }
    }
}
/// Typed semantic envelope carried by every compiled model context. The
/// referenced payloads remain in governed storage; selected excerpts are
/// represented by [`ContextBlock`] values in the same packet.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ContextEnvelope {
    pub domain: String,
    pub current_state: Option<ContextReference>,
    pub confirmed_constraints: Vec<ContextReference>,
    pub unresolved_conflicts: Vec<ContextReference>,
    pub relevant_memories: Vec<ContextReference>,
    pub evidence_index: Vec<ContextReference>,
    pub exact_artifacts: Vec<ContextReference>,
    pub recent_messages: Vec<ContextReference>,
    pub output_contract: ContextOutputContract,
}
impl Default for ContextEnvelope {
    fn default() -> Self {
        Self {
            domain: "general".into(),
            current_state: None,
            confirmed_constraints: Vec::new(),
            unresolved_conflicts: Vec::new(),
            relevant_memories: Vec::new(),
            evidence_index: Vec::new(),
            exact_artifacts: Vec::new(),
            recent_messages: Vec::new(),
            output_contract: ContextOutputContract::default(),
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContextBlock {
    pub block_id: String,
    pub source: String,
    pub content: String,
    pub tokens: u64,
    pub truncated: bool,
    pub source_hash: String,
    pub policy_version: String,
    pub layer: PromptLayer,
    pub selection_reason: String,
    pub trust: ContextTrust,
}
impl ContextBlock {
    pub fn new(
        id: impl Into<String>,
        source: impl Into<String>,
        content: impl Into<String>,
        tokens: u64,
        truncated: bool,
        hash: impl Into<String>,
    ) -> Self {
        Self {
            block_id: id.into(),
            source: source.into(),
            content: content.into(),
            tokens,
            truncated,
            source_hash: hash.into(),
            policy_version: "v1".into(),
            layer: PromptLayer::TaskPacket,
            selection_reason: "explicit context selection".into(),
            trust: ContextTrust::UntrustedData,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContextSelection {
    pub objective: String,
    pub envelope: ContextEnvelope,
    pub blocks: Vec<ContextBlock>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContextPacket {
    pub objective: String,
    #[serde(flatten)]
    pub envelope: ContextEnvelope,
    pub blocks: Vec<ContextBlock>,
    pub manifest: ContextManifest,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContextBlockManifest {
    pub block_id: String,
    pub source: String,
    pub tokens: u64,
    pub truncated: bool,
    pub source_hash: String,
    pub policy_version: String,
    pub selection_reason: String,
    pub trust: ContextTrust,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContextManifest {
    pub max_tokens: u64,
    pub used_tokens: u64,
    pub blocks: Vec<ContextBlockManifest>,
    pub snapshot_version: Option<u64>,
}
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ContextError {
    #[error("context budget exceeded: requested {requested}, maximum {maximum}")]
    BudgetExceeded { requested: u64, maximum: u64 },
}
#[derive(Debug, Clone, Default)]
pub struct ContextCompiler;
impl ContextCompiler {
    pub fn compile(
        &self,
        selection: ContextSelection,
        max_tokens: u64,
    ) -> Result<ContextPacket, ContextError> {
        let used = selection.blocks.iter().map(|b| b.tokens).sum();
        if used > max_tokens {
            return Err(ContextError::BudgetExceeded {
                requested: used,
                maximum: max_tokens,
            });
        }
        let manifest = ContextManifest {
            max_tokens,
            used_tokens: used,
            blocks: selection
                .blocks
                .iter()
                .map(|b| ContextBlockManifest {
                    block_id: b.block_id.clone(),
                    source: b.source.clone(),
                    tokens: b.tokens,
                    truncated: b.truncated,
                    source_hash: b.source_hash.clone(),
                    policy_version: b.policy_version.clone(),
                    selection_reason: b.selection_reason.clone(),
                    trust: b.trust.clone(),
                })
                .collect(),
            snapshot_version: None,
        };
        Ok(ContextPacket {
            objective: selection.objective,
            envelope: selection.envelope,
            blocks: selection.blocks,
            manifest,
        })
    }

    /// Select a model-visible packet under a hard input budget.
    ///
    /// Stable system and task blocks are mandatory; domain contracts precede
    /// recent history, and recent history is kept newest-first. The returned
    /// packet is the selection decision used to build the provider request,
    /// rather than an audit-only shadow.
    pub fn compile_for_model(
        &self,
        selection: ContextSelection,
        max_tokens: u64,
    ) -> Result<ContextPacket, ContextError> {
        let ContextSelection {
            objective,
            envelope,
            blocks,
        } = selection;
        let total: u64 = blocks.iter().map(|block| block.tokens).sum();
        if total <= max_tokens {
            return self.compile(
                ContextSelection {
                    objective,
                    envelope,
                    blocks,
                },
                max_tokens,
            );
        }

        let mut mandatory = Vec::new();
        let mut optional = Vec::new();
        for (index, block) in blocks.into_iter().enumerate() {
            if matches!(
                block.layer,
                PromptLayer::StableSystem | PromptLayer::DomainContract | PromptLayer::TaskPacket
            ) {
                mandatory.push((index, block));
            } else {
                optional.push((index, block));
            }
        }
        let mandatory_tokens = mandatory.iter().map(|(_, block)| block.tokens).sum::<u64>();
        if mandatory_tokens > max_tokens {
            return Err(ContextError::BudgetExceeded {
                requested: mandatory_tokens,
                maximum: max_tokens,
            });
        }

        // Higher-priority layers win. Within a layer, newer blocks win so a
        // long session sheds its oldest recent context first.
        optional.sort_by(|(left_index, left), (right_index, right)| {
            prompt_layer_priority(&right.layer)
                .cmp(&prompt_layer_priority(&left.layer))
                .then_with(|| right_index.cmp(left_index))
        });
        let mut remaining = max_tokens - mandatory_tokens;
        let mut selected = mandatory;
        for (index, block) in optional {
            if block.tokens <= remaining {
                remaining -= block.tokens;
                selected.push((index, block));
            }
        }
        selected.sort_by_key(|(index, _)| *index);
        let selected_blocks = selected
            .into_iter()
            .map(|(_, block)| block)
            .collect::<Vec<_>>();
        self.compile(
            ContextSelection {
                objective,
                envelope,
                blocks: selected_blocks,
            },
            max_tokens,
        )
    }
    pub fn compile_from_snapshot(
        &self,
        objective: impl Into<String>,
        snapshot: &SemanticSnapshot,
        blocks: Vec<ContextBlock>,
        max_tokens: u64,
    ) -> Result<ContextPacket, ContextError> {
        let mut packet = self.compile(
            ContextSelection {
                objective: objective.into(),
                envelope: ContextEnvelope::default(),
                blocks,
            },
            max_tokens,
        )?;
        packet.manifest.snapshot_version = Some(snapshot.version);
        Ok(packet)
    }
    pub fn hash(packet: &ContextPacket) -> String {
        let bytes = serde_json::to_vec(packet).expect("context is serializable");
        hex::encode(Sha256::digest(bytes))
    }
}

fn prompt_layer_priority(layer: &PromptLayer) -> u8 {
    match layer {
        PromptLayer::StableSystem => 4,
        PromptLayer::DomainContract => 3,
        PromptLayer::TaskPacket => 2,
        PromptLayer::RecentInteraction => 1,
    }
}
