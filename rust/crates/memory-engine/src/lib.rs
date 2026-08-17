//! Memory 2.0 pure engine.  It deliberately stores evidence references and
//! versioned candidates instead of silently rewriting a single summary.

use chrono::{DateTime, Utc};
use regex::Regex;
use semantic_core::{
    AssertionScope, CalibratedScore, EntityRef, EvidenceRef, Sensitivity, TypedValue,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use thiserror::Error;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum MemoryChannel {
    ContinuityState,
    LongTermMemory,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum MemoryKind {
    Fact,
    Preference,
    Constraint,
    Decision,
    OpenQuestion,
    Entity,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemoryCandidate {
    pub id: String,
    pub tenant_id: String,
    pub scope: AssertionScope,
    pub channel: MemoryChannel,
    pub kind: MemoryKind,
    pub subject: EntityRef,
    pub predicate: String,
    pub value: TypedValue,
    pub text: String,
    pub source: EvidenceRef,
    pub observed_at: DateTime<Utc>,
    pub valid_until: Option<DateTime<Utc>>,
    pub confidence: CalibratedScore,
    pub sensitivity: Sensitivity,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DualChannelExtraction {
    pub continuity_state: Vec<MemoryCandidate>,
    pub long_term_memory: Vec<MemoryCandidate>,
}
impl DualChannelExtraction {
    pub fn all(&self) -> impl Iterator<Item = &MemoryCandidate> {
        self.continuity_state
            .iter()
            .chain(self.long_term_memory.iter())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemoryHit {
    pub candidate: MemoryCandidate,
    pub score: f32,
    pub score_breakdown: ScoreBreakdown,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScoreBreakdown {
    pub lexical: f32,
    pub entity: f32,
    pub scope: f32,
    pub authority: f32,
    pub recency: f32,
    pub contradiction_penalty: f32,
    pub redundancy_penalty: f32,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConflictBundle {
    pub subject: EntityRef,
    pub predicate: String,
    pub current: Vec<MemoryCandidate>,
    pub superseded: Vec<MemoryCandidate>,
}
#[derive(Debug, Error, PartialEq, Eq)]
pub enum MemoryError {
    #[error("sensitive memory candidate was rejected: {0}")]
    Sensitive(String),
    #[error("memory candidate has no evidence")]
    MissingEvidence,
    #[error("invalid confidence: {0}")]
    Confidence(String),
    #[error("invalid temporal memory relation: {0}")]
    InvalidRelation(String),
}

/// Stateless production policy kernel. Durable adapters own storage, while
/// every admission, lexical signal, hybrid score and temporal relation passes
/// through this type so SQLite and test repositories cannot drift.
#[derive(Debug, Clone, Copy, Default)]
pub struct MemoryEngine;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RetrievalSignals {
    pub lexical: f64,
    pub semantic: Option<f64>,
    pub semantic_min_relevance: f64,
    pub semantic_weight: f64,
    pub lexical_weight: f64,
    pub confidence: f64,
    pub confidence_weight: f64,
    pub recency: f64,
    pub recency_weight: f64,
    pub pinned: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemporalRelation {
    Supersedes,
    ConflictsWith,
}

/// Reference repository for deterministic unit tests and compatibility
/// bridges. Production state lives in the tenant-scoped durable adapter.
#[derive(Debug, Clone, Default)]
pub struct InMemoryMemoryRepository {
    records: BTreeMap<String, MemoryCandidate>,
}

impl MemoryEngine {
    /// Shared deterministic admission policy used by durable adapters.  The
    /// adapter may choose its own repository, but it must pass this gate
    /// before a candidate can enter production Memory.
    pub fn admit_text(text: &str) -> Result<(), MemoryError> {
        validate_memory_text(text)
    }

    #[must_use]
    pub fn is_sensitive(text: &str) -> bool {
        contains_secret(text)
    }

    /// Canonical lexical component for hybrid retrieval. Keeping tokenization
    /// here prevents the SQLite adapter and the in-memory engine from silently
    /// ranking the same candidate differently.
    #[must_use]
    pub fn lexical_relevance(query: &str, text: &str) -> f64 {
        let terms = query
            .to_lowercase()
            .split_whitespace()
            .filter(|term| !term.is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>();
        Self::lexical_relevance_terms(&terms, text)
    }

    #[must_use]
    pub fn lexical_relevance_terms(terms: &[String], text: &str) -> f64 {
        if terms.is_empty() {
            return 0.0;
        }
        let haystack = text.to_lowercase();
        terms
            .iter()
            .filter(|term| haystack.contains(term.as_str()) || text.contains(term.as_str()))
            .count() as f64
            / terms.len() as f64
    }

    #[must_use]
    pub fn retrieval_score(signals: RetrievalSignals) -> f64 {
        if signals.pinned {
            return 1.0;
        }
        let semantic_relevance = signals
            .semantic
            .filter(|score| *score >= signals.semantic_min_relevance)
            .unwrap_or(0.0);
        let relevance = match signals.semantic {
            Some(_) => {
                semantic_relevance * signals.semantic_weight
                    + signals.lexical * signals.lexical_weight
            }
            None => signals.lexical * (signals.semantic_weight + signals.lexical_weight),
        };
        if relevance <= 0.0 {
            return 0.0;
        }
        (relevance
            + signals.confidence.clamp(0.0, 1.0) * signals.confidence_weight
            + signals.recency.clamp(0.0, 1.0) * signals.recency_weight)
            .clamp(0.0, 1.0)
    }

    pub fn temporal_relation(value: &str) -> Result<TemporalRelation, MemoryError> {
        match value.trim().to_ascii_lowercase().as_str() {
            "supersedes" => Ok(TemporalRelation::Supersedes),
            "conflicts_with" => Ok(TemporalRelation::ConflictsWith),
            other => Err(MemoryError::InvalidRelation(other.to_string())),
        }
    }
}

impl InMemoryMemoryRepository {
    pub fn ingest(&mut self, candidate: MemoryCandidate) -> Result<bool, MemoryError> {
        if candidate.source.evidence_id.is_empty() {
            return Err(MemoryError::MissingEvidence);
        }
        if candidate.sensitivity == Sensitivity::Secret || contains_secret(&candidate.text) {
            return Err(MemoryError::Sensitive(candidate.id));
        }
        if self.records.contains_key(&candidate.id) {
            return Ok(false);
        }
        self.records.insert(candidate.id.clone(), candidate);
        Ok(true)
    }
    pub fn ingest_channels(
        &mut self,
        extraction: DualChannelExtraction,
    ) -> Result<usize, MemoryError> {
        let mut inserted = 0;
        for candidate in extraction.all().cloned() {
            if self.ingest(candidate)? {
                inserted += 1;
            }
        }
        Ok(inserted)
    }
    pub fn search(
        &self,
        query: &str,
        scope: &AssertionScope,
        now: DateTime<Utc>,
        limit: usize,
    ) -> Vec<MemoryHit> {
        let terms: Vec<_> = query
            .to_lowercase()
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        let mut hits: Vec<_> = self
            .records
            .values()
            .filter(|c| &c.scope == scope && c.valid_until.is_none_or(|until| until > now))
            .map(|candidate| {
                let text = format!(
                    "{} {} {:?}",
                    candidate.text, candidate.predicate, candidate.value
                )
                .to_lowercase();
                let lexical = terms.iter().filter(|t| text.contains(t.as_str())).count() as f32
                    / terms.len().max(1) as f32;
                let entity = if query.contains(&candidate.subject.id) {
                    1.0
                } else {
                    0.0
                };
                let authority = match candidate.source.authority {
                    semantic_core::EvidenceAuthority::User
                    | semantic_core::EvidenceAuthority::Owner => 1.0,
                    semantic_core::EvidenceAuthority::Document
                    | semantic_core::EvidenceAuthority::Tool => 0.8,
                    _ => 0.3,
                };
                let age_days = (now - candidate.observed_at).num_days().max(0) as f32;
                let recency = 1.0 / (1.0 + age_days / 30.0);
                let breakdown = ScoreBreakdown {
                    lexical,
                    entity,
                    scope: 1.0,
                    authority,
                    recency,
                    contradiction_penalty: 0.0,
                    redundancy_penalty: 0.0,
                };
                MemoryHit {
                    candidate: candidate.clone(),
                    score: lexical * 0.4 + entity * 0.2 + authority * 0.2 + recency * 0.2,
                    score_breakdown: breakdown,
                }
            })
            .collect();
        hits.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then_with(|| a.candidate.id.cmp(&b.candidate.id))
        });
        hits.truncate(limit);
        hits
    }
    pub fn conflicts(&self, scope: &AssertionScope) -> Vec<ConflictBundle> {
        let mut groups: BTreeMap<(EntityRef, String), Vec<MemoryCandidate>> = BTreeMap::new();
        for candidate in self.records.values().filter(|c| &c.scope == scope) {
            groups
                .entry((candidate.subject.clone(), candidate.predicate.clone()))
                .or_default()
                .push(candidate.clone());
        }
        groups
            .into_iter()
            .filter_map(|((subject, predicate), mut entries)| {
                let mut values = entries
                    .iter()
                    .map(|e| serde_json::to_string(&e.value).unwrap_or_default())
                    .collect::<Vec<_>>();
                values.sort();
                values.dedup();
                if values.len() <= 1 {
                    return None;
                }
                entries.sort_by(|a, b| b.observed_at.cmp(&a.observed_at));
                let current = entries.iter().take(1).cloned().collect();
                let superseded = entries.into_iter().skip(1).collect();
                Some(ConflictBundle {
                    subject,
                    predicate,
                    current,
                    superseded,
                })
            })
            .collect()
    }
    pub fn len(&self) -> usize {
        self.records.len()
    }
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
}
fn contains_secret(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    let sensitive_needles = [
        "api_key",
        "apikey",
        "secret",
        "password",
        "passwd",
        "token=",
        "bearer ",
        "private key",
        "access key",
        "authorization:",
        "cookie:",
        "set-cookie:",
        "sk-",
    ];
    if sensitive_needles
        .iter()
        .any(|needle| lower.contains(needle))
    {
        return true;
    }
    if Regex::new(r"(?i)(sk-[A-Za-z0-9]{12,}|password\s*=|authorization:\s*bearer)")
        .expect("static secret regex")
        .is_match(text)
    {
        return true;
    }
    let digit_count = text
        .chars()
        .filter(|character| character.is_ascii_digit())
        .count();
    digit_count >= 14 && digit_count * 2 >= text.chars().count()
}

/// Shared production admission guard for adapters that still persist through
/// the legacy Unified Memory repository.  This keeps compaction and the pure
/// engine on one secret/no-evidence policy during the staged migration.
pub fn validate_memory_text(text: &str) -> Result<(), MemoryError> {
    if text.trim().is_empty() {
        return Err(MemoryError::MissingEvidence);
    }
    if contains_secret(text) {
        return Err(MemoryError::Sensitive(stable_source_hash(text)));
    }
    Ok(())
}
pub fn stable_source_hash(text: &str) -> String {
    hex::encode(Sha256::digest(text.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use semantic_core::{EvidenceAuthority, EvidenceSourceType};
    fn candidate(id: &str, value: &str, days_ago: i64) -> MemoryCandidate {
        MemoryCandidate {
            id: id.into(),
            tenant_id: "t".into(),
            scope: AssertionScope::Session("s".into()),
            channel: MemoryChannel::LongTermMemory,
            kind: MemoryKind::Fact,
            subject: EntityRef::new("user", "u"),
            predicate: "theme".into(),
            value: TypedValue::String(value.into()),
            text: format!("theme {value}"),
            source: EvidenceRef {
                evidence_id: format!("e-{id}"),
                source_type: EvidenceSourceType::Message,
                source_locator: "msg://1".into(),
                content_hash: stable_source_hash(value),
                event_seq: Some(1),
                byte_or_line_range: None,
                collected_at: Utc::now(),
                authority: EvidenceAuthority::User,
            },
            observed_at: Utc::now() - Duration::days(days_ago),
            valid_until: None,
            confidence: CalibratedScore::new(0.8).unwrap(),
            sensitivity: Sensitivity::Internal,
        }
    }
    #[test]
    fn dual_channels_and_sensitive_filter_are_independent() {
        let mut engine = InMemoryMemoryRepository::default();
        let mut continuity = candidate("c", "pending step", 0);
        continuity.channel = MemoryChannel::ContinuityState;
        let mut long_term = candidate("l", "dark", 0);
        long_term.channel = MemoryChannel::LongTermMemory;
        assert_eq!(
            engine
                .ingest_channels(DualChannelExtraction {
                    continuity_state: vec![continuity],
                    long_term_memory: vec![long_term]
                })
                .unwrap(),
            2
        );
        let mut secret = candidate("s", "x", 0);
        secret.text = "password=secret".into();
        assert!(matches!(
            engine.ingest(secret),
            Err(MemoryError::Sensitive(_))
        ));
    }
    #[test]
    fn search_returns_current_scope_and_conflict_package() {
        let mut engine = InMemoryMemoryRepository::default();
        engine.ingest(candidate("old", "dark", 30)).unwrap();
        engine.ingest(candidate("new", "light", 1)).unwrap();
        let scope = AssertionScope::Session("s".into());
        let hits = engine.search("theme light", &scope, Utc::now(), 10);
        assert_eq!(hits[0].candidate.id, "new");
        let conflicts = engine.conflicts(&scope);
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].current[0].id, "new");
        assert_eq!(conflicts[0].superseded[0].id, "old");
    }

    #[test]
    fn production_policy_combines_retrieval_signals_and_validates_relations() {
        let score = MemoryEngine::retrieval_score(RetrievalSignals {
            lexical: 0.5,
            semantic: Some(0.8),
            semantic_min_relevance: 0.4,
            semantic_weight: 0.68,
            lexical_weight: 0.22,
            confidence: 0.9,
            confidence_weight: 0.05,
            recency: 1.0,
            recency_weight: 0.05,
            pinned: false,
        });
        assert!(score > 0.7 && score < 1.0);
        assert_eq!(
            MemoryEngine::temporal_relation("supersedes").unwrap(),
            TemporalRelation::Supersedes
        );
        assert!(MemoryEngine::temporal_relation("overwrites").is_err());
    }
}
