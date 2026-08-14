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
pub struct ContextBlock {
    pub block_id: String,
    pub source: String,
    pub content: String,
    pub tokens: u64,
    pub truncated: bool,
    pub source_hash: String,
    pub policy_version: String,
    pub layer: PromptLayer,
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
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContextSelection {
    pub objective: String,
    pub blocks: Vec<ContextBlock>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContextPacket {
    pub objective: String,
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
                })
                .collect(),
            snapshot_version: None,
        };
        Ok(ContextPacket {
            objective: selection.objective,
            blocks: selection.blocks,
            manifest,
        })
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
