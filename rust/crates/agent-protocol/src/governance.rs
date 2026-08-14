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
    pub fn intersection(&self, child: &CapabilityScope) -> Option<Self> {
        if self.tenant_id != child.tenant_id
            || self.user_id != child.user_id
            || self.tool_name != child.tool_name
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
                (None, b) => b.clone(),
                (a, None) => a.clone(),
                _ => return None,
            },
            child_thread: child.child_thread.clone(),
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
        &self,
        child_scope: CapabilityScope,
        now: DateTime<Utc>,
    ) -> Result<Self, CapabilityError> {
        if self.revoked || now >= self.expires_at {
            return Err(CapabilityError::ExpiredOrRevoked);
        }
        let scope = self
            .scope
            .intersection(&child_scope)
            .ok_or(CapabilityError::ScopeExpansion)?;
        Ok(Self::new(
            scope,
            self.expires_at.min(now + chrono::Duration::hours(1)),
            self.remaining_uses,
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
        if matches!(kind, ProjectionKind::RawEncrypted) && policy.allow_raw_encrypted {
            return ProjectedPayload {
                kind,
                source_hash,
                policy_version: policy.version.clone(),
                text: text.to_owned(),
                redaction_provenance: vec!["raw-retention-authorized".into()],
            };
        }
        let mut value = text.to_owned();
        let mut provenance = vec![];
        if policy.redact_credentials {
            for marker in ["sk-", "password=", "token=", "Authorization: Bearer "] {
                if let Some(start) = value.find(marker) {
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
        let fresh = CapabilityToken::new(scope(), now + chrono::Duration::minutes(5), 1);
        let child = fresh.derive_child(broader, now).unwrap();
        assert!(!child.scope.resources.contains("repo:b"));
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
