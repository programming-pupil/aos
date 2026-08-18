use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InteractionKind {
    Approval,
    UserQuestion,
    CredentialRequest,
    ExternalAuthorization,
}

impl InteractionKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Approval => "approval",
            Self::UserQuestion => "user_question",
            Self::CredentialRequest => "credential_request",
            Self::ExternalAuthorization => "external_authorization",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InteractionState {
    Pending,
    Responded,
    Granted,
    Rejected,
    Expired,
    Cancelled,
    Consumed,
}

impl InteractionState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Responded => "responded",
            Self::Granted => "granted",
            Self::Rejected => "rejected",
            Self::Expired => "expired",
            Self::Cancelled => "cancelled",
            Self::Consumed => "consumed",
        }
    }

    #[must_use]
    pub const fn can_resume(self) -> bool {
        matches!(
            self,
            Self::Responded | Self::Granted | Self::Rejected | Self::Expired | Self::Cancelled
        )
    }

    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Rejected | Self::Expired | Self::Cancelled | Self::Consumed
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InteractionScope {
    pub tenant_id: String,
    pub user_id: String,
    pub session_id: String,
    pub turn_id: String,
    pub invocation_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DurableInteraction {
    pub interaction_id: String,
    pub kind: InteractionKind,
    pub state: InteractionState,
    pub scope: InteractionScope,
    pub owner_user_id: String,
    pub allowed_responder_ids: Vec<String>,
    pub capability_requirement: Option<String>,
    pub request_schema_hash: String,
    pub choice_schema_hash: Option<String>,
    pub display_projection: serde_json::Value,
    pub response_projection: Option<serde_json::Value>,
    pub encrypted_secret_ref: Option<String>,
    pub idempotency_key: String,
    pub expected_turn_revision: u64,
    pub expires_at: Option<DateTime<Utc>>,
    pub created_event_id: Option<String>,
    pub response_event_id: Option<String>,
    pub consumed_event_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InteractionResponse {
    pub responder_user_id: String,
    pub state: InteractionState,
    pub response_projection: Option<serde_json::Value>,
    pub encrypted_secret_ref: Option<String>,
    pub response_event_id: String,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum InteractionError {
    #[error("interaction is not pending (state={0})")]
    NotPending(&'static str),
    #[error("interaction response state is invalid")]
    InvalidResponseState,
    #[error("interaction responder is not authorized")]
    UnauthorizedResponder,
    #[error("interaction expired")]
    Expired,
    #[error("credential responses must contain only an encrypted secret reference")]
    PlaintextCredential,
    #[error("interaction cannot be consumed from state={0}")]
    NotConsumable(&'static str),
    #[error("interaction was already consumed")]
    AlreadyConsumed,
}

impl DurableInteraction {
    pub fn respond(
        &mut self,
        response: InteractionResponse,
        now: DateTime<Utc>,
    ) -> Result<InteractionState, InteractionError> {
        if self.state != InteractionState::Pending {
            return if self.state == InteractionState::Consumed {
                Err(InteractionError::AlreadyConsumed)
            } else {
                Err(InteractionError::NotPending(self.state.as_str()))
            };
        }
        if self.expires_at.is_some_and(|deadline| deadline <= now) {
            self.state = InteractionState::Expired;
            self.response_event_id = Some(response.response_event_id);
            return Ok(self.state);
        }
        if response.responder_user_id != self.owner_user_id
            && !self
                .allowed_responder_ids
                .iter()
                .any(|id| id == &response.responder_user_id)
        {
            return Err(InteractionError::UnauthorizedResponder);
        }
        if !matches!(
            response.state,
            InteractionState::Responded
                | InteractionState::Granted
                | InteractionState::Rejected
                | InteractionState::Cancelled
        ) {
            return Err(InteractionError::InvalidResponseState);
        }
        if self.kind == InteractionKind::CredentialRequest
            && (response.encrypted_secret_ref.is_none() || response.response_projection.is_some())
        {
            return Err(InteractionError::PlaintextCredential);
        }
        self.state = response.state;
        self.response_projection = response.response_projection;
        self.encrypted_secret_ref = response.encrypted_secret_ref;
        self.response_event_id = Some(response.response_event_id);
        Ok(self.state)
    }

    pub fn consume(&mut self, event_id: String) -> Result<(), InteractionError> {
        if self.state == InteractionState::Consumed {
            return Err(InteractionError::AlreadyConsumed);
        }
        if !self.state.can_resume() {
            return Err(InteractionError::NotConsumable(self.state.as_str()));
        }
        self.state = InteractionState::Consumed;
        self.consumed_event_id = Some(event_id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn interaction(kind: InteractionKind) -> DurableInteraction {
        DurableInteraction {
            interaction_id: "interaction-1".into(),
            kind,
            state: InteractionState::Pending,
            scope: InteractionScope {
                tenant_id: "tenant".into(),
                user_id: "owner".into(),
                session_id: "session".into(),
                turn_id: "turn".into(),
                invocation_id: "invocation".into(),
            },
            owner_user_id: "owner".into(),
            allowed_responder_ids: vec!["delegate".into()],
            capability_requirement: None,
            request_schema_hash: "request".into(),
            choice_schema_hash: None,
            display_projection: serde_json::json!({"question":"continue?"}),
            response_projection: None,
            encrypted_secret_ref: None,
            idempotency_key: "once".into(),
            expected_turn_revision: 1,
            expires_at: Some(Utc::now() + Duration::minutes(5)),
            created_event_id: None,
            response_event_id: None,
            consumed_event_id: None,
        }
    }

    #[test]
    fn response_then_consume_is_exactly_once() {
        let mut value = interaction(InteractionKind::UserQuestion);
        value
            .respond(
                InteractionResponse {
                    responder_user_id: "owner".into(),
                    state: InteractionState::Responded,
                    response_projection: Some(serde_json::json!({"answer":"yes"})),
                    encrypted_secret_ref: None,
                    response_event_id: "response-event".into(),
                },
                Utc::now(),
            )
            .unwrap();
        value.consume("consume-event".into()).unwrap();
        assert_eq!(value.state, InteractionState::Consumed);
        assert_eq!(
            value.consume("duplicate".into()),
            Err(InteractionError::AlreadyConsumed)
        );
    }

    #[test]
    fn unauthorized_responses_fail_and_expiry_is_durably_terminal() {
        let mut value = interaction(InteractionKind::Approval);
        let unauthorized = value.respond(
            InteractionResponse {
                responder_user_id: "stranger".into(),
                state: InteractionState::Granted,
                response_projection: None,
                encrypted_secret_ref: None,
                response_event_id: "response".into(),
            },
            Utc::now(),
        );
        assert_eq!(unauthorized, Err(InteractionError::UnauthorizedResponder));
        let after_expiry = value.expires_at.unwrap() + Duration::seconds(1);
        assert_eq!(
            value
                .respond(
                    InteractionResponse {
                        responder_user_id: "owner".into(),
                        state: InteractionState::Granted,
                        response_projection: None,
                        encrypted_secret_ref: None,
                        response_event_id: "late".into(),
                    },
                    after_expiry,
                )
                .unwrap(),
            InteractionState::Expired
        );
        assert_eq!(value.state, InteractionState::Expired);
        value.consume("expiry-consumed".into()).unwrap();
        assert_eq!(value.state, InteractionState::Consumed);
    }

    #[test]
    fn credential_plaintext_never_enters_the_protocol() {
        let mut value = interaction(InteractionKind::CredentialRequest);
        assert_eq!(
            value.respond(
                InteractionResponse {
                    responder_user_id: "owner".into(),
                    state: InteractionState::Responded,
                    response_projection: Some(serde_json::json!({"password":"secret"})),
                    encrypted_secret_ref: None,
                    response_event_id: "response".into(),
                },
                Utc::now(),
            ),
            Err(InteractionError::PlaintextCredential)
        );
        assert_eq!(value.state, InteractionState::Pending);
    }
}
