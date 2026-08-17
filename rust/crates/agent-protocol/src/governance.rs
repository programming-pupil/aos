use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CapabilityScope {
    pub tenant_id: String,
    pub user_id: String,
    pub session_id: Option<String>,
    pub tool_name: String,
    pub resources: BTreeSet<String>,
    pub actions: BTreeSet<String>,
    pub executor: Option<String>,
    pub child_thread: Option<String>,
}
impl CapabilityScope {
    /// Bind a server-selected executor before delegation. A child-supplied
    /// executor is never adopted by generic scope intersection.
    pub fn bind_executor(&self, executor: &str) -> Option<Self> {
        if executor.trim().is_empty() || self.executor.is_some() {
            return None;
        }
        let mut bound = self.clone();
        bound.executor = Some(executor.to_string());
        Some(bound)
    }

    /// Bind a server-generated child id at capability issuance time. Generic
    /// intersection is not allowed to adopt an id supplied by the child.
    pub fn bind_child_thread(&self, child_thread: &str) -> Option<Self> {
        if child_thread.trim().is_empty() || self.child_thread.is_some() {
            return None;
        }
        let mut bound = self.clone();
        bound.child_thread = Some(child_thread.to_string());
        Some(bound)
    }

    pub fn intersection(&self, child: &CapabilityScope) -> Option<Self> {
        if self.tenant_id != child.tenant_id
            || self.user_id != child.user_id
            || self.tool_name != child.tool_name
            // A child may narrow a session, but it can never move a
            // capability into another session.  A session-less parent is
            // deliberately not allowed to mint a session-bound child here;
            // callers must first bind the parent at issuance time.
            || self.session_id != child.session_id
        {
            return None;
        }
        let resources: BTreeSet<String> = self
            .resources
            .intersection(&child.resources)
            .cloned()
            .collect();
        let actions: BTreeSet<String> =
            self.actions.intersection(&child.actions).cloned().collect();
        if resources.is_empty() || actions.is_empty() {
            return None;
        }
        Some(Self {
            tenant_id: self.tenant_id.clone(),
            user_id: self.user_id.clone(),
            session_id: child.session_id.clone().or_else(|| self.session_id.clone()),
            tool_name: self.tool_name.clone(),
            resources,
            actions,
            executor: match (&self.executor, &child.executor) {
                (Some(a), Some(b)) if a == b => Some(a.clone()),
                (a, None) => a.clone(),
                (None, Some(_)) => return None,
                _ => return None,
            },
            child_thread: match (&self.child_thread, &child.child_thread) {
                (Some(parent), Some(child)) if parent == child => Some(child.clone()),
                (None, None) => None,
                _ => return None,
            },
        })
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CapabilityToken {
    pub id: String,
    pub scope: CapabilityScope,
    pub expires_at: DateTime<Utc>,
    pub remaining_uses: u32,
    pub revoked: bool,
}
#[derive(Debug, Error, PartialEq, Eq)]
pub enum CapabilityError {
    #[error("capability token is expired or revoked")]
    ExpiredOrRevoked,
    #[error("capability token has no remaining uses")]
    Exhausted,
    #[error("child capability is broader than its parent")]
    ScopeExpansion,
}
impl CapabilityToken {
    pub fn new(scope: CapabilityScope, expires_at: DateTime<Utc>, uses: u32) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            scope,
            expires_at,
            remaining_uses: uses,
            revoked: false,
        }
    }
    pub fn consume(&mut self, now: DateTime<Utc>) -> Result<(), CapabilityError> {
        if self.revoked || now >= self.expires_at {
            return Err(CapabilityError::ExpiredOrRevoked);
        }
        if self.remaining_uses == 0 {
            return Err(CapabilityError::Exhausted);
        }
        self.remaining_uses -= 1;
        Ok(())
    }
    pub fn derive_child(
        &mut self,
        child_scope: CapabilityScope,
        now: DateTime<Utc>,
    ) -> Result<Self, CapabilityError> {
        let scope = self
            .scope
            .intersection(&child_scope)
            .ok_or(CapabilityError::ScopeExpansion)?;
        let child_uses = self.remaining_uses.min(1);
        self.consume(now)?;
        Ok(Self::new(
            scope,
            self.expires_at.min(now + chrono::Duration::hours(1)),
            child_uses,
        ))
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ProjectionKind {
    RawEncrypted,
    ModelVisible,
    ClientVisible,
    TelemetryRedacted,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SensitiveProjectionPolicy {
    pub version: String,
    pub allow_raw_encrypted: bool,
    pub redact_credentials: bool,
    pub redact_pii: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectedPayload {
    pub kind: ProjectionKind,
    pub source_hash: String,
    pub policy_version: String,
    pub text: String,
    pub redaction_provenance: Vec<String>,
}
pub struct SensitiveProjector;
impl SensitiveProjector {
    pub fn project(
        text: &str,
        kind: ProjectionKind,
        policy: &SensitiveProjectionPolicy,
    ) -> ProjectedPayload {
        let source_hash = hex::encode(Sha256::digest(text.as_bytes()));
        if matches!(kind, ProjectionKind::RawEncrypted) {
            return ProjectedPayload {
                kind,
                text: if policy.allow_raw_encrypted {
                    format!("encrypted-artifact://{source_hash}")
                } else {
                    "[RAW_PAYLOAD_NOT_RETAINED]".into()
                },
                source_hash,
                policy_version: policy.version.clone(),
                redaction_provenance: vec![if policy.allow_raw_encrypted {
                    "raw-storage-envelope-required".into()
                } else {
                    "raw-retention-denied".into()
                }],
            };
        }
        let mut value = text.to_owned();
        let mut provenance = vec![];
        if policy.redact_credentials {
            for marker in ["sk-", "password=", "token=", "Authorization: Bearer "] {
                while let Some(start) = value.find(marker) {
                    let end = value[start..]
                        .find(|c: char| c.is_whitespace() || c == ',' || c == '}')
                        .map_or(value.len(), |offset| start + offset);
                    value.replace_range(start..end, "[REDACTED]");
                    provenance.push(format!("credential:{marker}"));
                }
            }
        }
        if policy.redact_pii && value.contains('@') {
            let words: Vec<_> = value
                .split_whitespace()
                .map(|word| {
                    if word.contains('@') {
                        "[REDACTED_EMAIL]"
                    } else {
                        word
                    }
                })
                .collect();
            value = words.join(" ");
            provenance.push("email".into());
        }
        ProjectedPayload {
            kind,
            source_hash,
            policy_version: policy.version.clone(),
            text: value,
            redaction_provenance: provenance,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactObject {
    pub id: String,
    pub tenant_id: String,
    pub owner_scope: String,
    pub content_hash: String,
    pub media_type: String,
    pub bytes: u64,
    pub locator: String,
    pub retention: String,
    pub deleted: bool,
}
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ArtifactError {
    #[error("artifact is not accessible in this tenant/scope")]
    NotAccessible,
    #[error("artifact has been deleted")]
    Deleted,
}
#[derive(Debug, Default)]
pub struct ArtifactPlane {
    objects: std::collections::BTreeMap<String, (ArtifactObject, Vec<u8>)>,
}
impl ArtifactPlane {
    pub fn put(
        &mut self,
        tenant_id: &str,
        owner_scope: &str,
        media_type: &str,
        payload: Vec<u8>,
        retention: &str,
    ) -> ArtifactObject {
        let hash = hex::encode(Sha256::digest(&payload));
        let id = Uuid::new_v4().to_string();
        let object = ArtifactObject {
            id: id.clone(),
            tenant_id: tenant_id.into(),
            owner_scope: owner_scope.into(),
            content_hash: hash,
            media_type: media_type.into(),
            bytes: payload.len() as u64,
            locator: format!("artifact://{id}"),
            retention: retention.into(),
            deleted: false,
        };
        self.objects.insert(id, (object.clone(), payload));
        object
    }
    pub fn read(
        &self,
        id: &str,
        tenant_id: &str,
        owner_scope: &str,
    ) -> Result<&[u8], ArtifactError> {
        let (object, payload) = self.objects.get(id).ok_or(ArtifactError::NotAccessible)?;
        if object.deleted {
            return Err(ArtifactError::Deleted);
        }
        if object.tenant_id != tenant_id || object.owner_scope != owner_scope {
            return Err(ArtifactError::NotAccessible);
        }
        Ok(payload)
    }
    pub fn delete(
        &mut self,
        id: &str,
        tenant_id: &str,
        owner_scope: &str,
    ) -> Result<(), ArtifactError> {
        let (object, _) = self
            .objects
            .get_mut(id)
            .ok_or(ArtifactError::NotAccessible)?;
        if object.tenant_id != tenant_id || object.owner_scope != owner_scope {
            return Err(ArtifactError::NotAccessible);
        }
        object.deleted = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn scope() -> CapabilityScope {
        CapabilityScope {
            tenant_id: "t".into(),
            user_id: "u".into(),
            session_id: Some("s".into()),
            tool_name: "read".into(),
            resources: ["repo:a".into()].into_iter().collect(),
            actions: ["read".into()].into_iter().collect(),
            executor: Some("native".into()),
            child_thread: None,
        }
    }
    #[test]
    fn child_capability_cannot_expand_scope_and_tokens_are_one_time() {
        let now = Utc::now();
        let mut token = CapabilityToken::new(scope(), now + chrono::Duration::minutes(5), 1);
        token.consume(now).unwrap();
        assert_eq!(token.consume(now), Err(CapabilityError::Exhausted));
        let mut broader = scope();
        broader.resources.insert("repo:b".into());
        let mut fresh = CapabilityToken::new(scope(), now + chrono::Duration::minutes(5), 1);
        let child = fresh.derive_child(broader, now).unwrap();
        assert!(!child.scope.resources.contains("repo:b"));
        assert_eq!(
            fresh.remaining_uses, 0,
            "child derivation consumes a delegation use"
        );
    }
    #[test]
    fn child_capability_cannot_cross_session_and_derivation_consumes_parent() {
        let now = Utc::now();
        let mut parent = CapabilityToken::new(scope(), now + chrono::Duration::minutes(5), 2);
        let mut cross_session = scope();
        cross_session.session_id = Some("other".into());
        assert_eq!(
            parent.derive_child(cross_session, now),
            Err(CapabilityError::ScopeExpansion)
        );
        assert_eq!(parent.remaining_uses, 2);
        let child = parent.derive_child(scope(), now).unwrap();
        assert_eq!(child.remaining_uses, 1);
        assert_eq!(parent.remaining_uses, 1);
    }
    #[test]
    fn server_must_bind_executor_and_child_id_before_delegation() {
        let mut unbound = scope();
        unbound.executor = None;
        unbound.child_thread = None;
        let mut child_claim = unbound.clone();
        child_claim.executor = Some("child-selected-executor".into());
        child_claim.child_thread = Some("child-selected-id".into());
        assert!(unbound.intersection(&child_claim).is_none());

        let bound = unbound
            .bind_executor("native")
            .unwrap()
            .bind_child_thread("server-child-id")
            .unwrap();
        let mut narrowed = bound.clone();
        narrowed.resources = ["repo:a".into()].into_iter().collect();
        narrowed.actions = ["read".into()].into_iter().collect();
        let delegated = bound.intersection(&narrowed).unwrap();
        assert_eq!(delegated.executor.as_deref(), Some("native"));
        assert_eq!(delegated.child_thread.as_deref(), Some("server-child-id"));
    }
    #[test]
    fn projections_keep_source_hash_without_leaking_secret() {
        let policy = SensitiveProjectionPolicy {
            version: "p1".into(),
            allow_raw_encrypted: false,
            redact_credentials: true,
            redact_pii: true,
        };
        let projected = SensitiveProjector::project(
            "key sk-secret password=hunter2 email a@b.com",
            ProjectionKind::TelemetryRedacted,
            &policy,
        );
        assert!(!projected.text.contains("sk-secret"));
        assert!(!projected.text.contains("hunter2"));
        assert_eq!(projected.source_hash.len(), 64);
    }
    #[test]
    fn raw_projection_requires_encrypted_storage_and_repeated_secrets_are_redacted() {
        let policy = SensitiveProjectionPolicy {
            version: "p1".into(),
            allow_raw_encrypted: true,
            redact_credentials: true,
            redact_pii: true,
        };
        let raw = SensitiveProjector::project(
            "sk-first sk-second",
            ProjectionKind::RawEncrypted,
            &policy,
        );
        assert!(raw.text.starts_with("encrypted-artifact://"));
        assert!(!raw.text.contains("sk-first"));
        let telemetry = SensitiveProjector::project(
            "sk-first sk-second",
            ProjectionKind::TelemetryRedacted,
            &policy,
        );
        assert!(!telemetry.text.contains("sk-first"));
        assert!(!telemetry.text.contains("sk-second"));
    }
    #[test]
    fn artifacts_are_tenant_and_owner_scoped_and_deletable() {
        let mut plane = ArtifactPlane::default();
        let object = plane.put("t", "u", "text/plain", b"full".to_vec(), "standard");
        assert!(plane.read(&object.id, "t", "other").is_err());
        assert_eq!(plane.read(&object.id, "t", "u").unwrap(), b"full");
        plane.delete(&object.id, "t", "u").unwrap();
        assert_eq!(
            plane.read(&object.id, "t", "u"),
            Err(ArtifactError::Deleted)
        );
    }
}
