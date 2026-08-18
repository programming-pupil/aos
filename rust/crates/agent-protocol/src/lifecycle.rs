use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ToolLifecycleState {
    Proposed,
    AwaitingAuthorization,
    Authorized,
    Started,
    Streaming,
    Suspended,
    Resumed,
    Completed,
    Failed,
    Cancelled,
    Expired,
    OutcomeUnknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolLifecycle {
    state: ToolLifecycleState,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum LifecycleError {
    #[error("invalid tool lifecycle transition from {from:?} to {to:?}")]
    Invalid {
        from: ToolLifecycleState,
        to: ToolLifecycleState,
    },
}

impl ToolLifecycle {
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: ToolLifecycleState::Proposed,
        }
    }
    #[must_use]
    pub fn state(&self) -> ToolLifecycleState {
        self.state
    }
    pub fn transition(&mut self, next: ToolLifecycleState) -> Result<(), LifecycleError> {
        let valid = matches!(
            (self.state, next),
            (
                ToolLifecycleState::Proposed,
                ToolLifecycleState::AwaitingAuthorization
                    | ToolLifecycleState::Authorized
                    | ToolLifecycleState::Cancelled
                    | ToolLifecycleState::Expired,
            ) | (
                ToolLifecycleState::AwaitingAuthorization,
                ToolLifecycleState::Authorized
                    | ToolLifecycleState::Cancelled
                    | ToolLifecycleState::Expired,
            ) | (
                ToolLifecycleState::Authorized,
                ToolLifecycleState::Started
                    | ToolLifecycleState::Cancelled
                    | ToolLifecycleState::Expired,
            ) | (
                ToolLifecycleState::Started,
                ToolLifecycleState::Streaming
                    | ToolLifecycleState::Completed
                    | ToolLifecycleState::Failed
                    | ToolLifecycleState::Cancelled
                    | ToolLifecycleState::OutcomeUnknown
                    | ToolLifecycleState::Suspended,
            ) | (
                ToolLifecycleState::Streaming,
                ToolLifecycleState::Completed
                    | ToolLifecycleState::Failed
                    | ToolLifecycleState::Cancelled
                    | ToolLifecycleState::OutcomeUnknown
                    | ToolLifecycleState::Suspended,
            ) | (
                ToolLifecycleState::Suspended,
                ToolLifecycleState::Resumed
                    | ToolLifecycleState::Cancelled
                    | ToolLifecycleState::Expired
                    | ToolLifecycleState::OutcomeUnknown,
            ) | (
                ToolLifecycleState::Resumed,
                ToolLifecycleState::Streaming
                    | ToolLifecycleState::Completed
                    | ToolLifecycleState::Failed
                    | ToolLifecycleState::Cancelled
                    | ToolLifecycleState::OutcomeUnknown,
            )
        );
        if !valid {
            return Err(LifecycleError::Invalid {
                from: self.state,
                to: next,
            });
        }
        self.state = next;
        Ok(())
    }
}

impl Default for ToolLifecycle {
    fn default() -> Self {
        Self::new()
    }
}
