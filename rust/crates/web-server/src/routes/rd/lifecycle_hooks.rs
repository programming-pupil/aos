//! RD lifecycle hook dispatch helpers.

use crate::routes::hooks::{run_lifecycle_hooks, HookEventType};

use super::*;

pub(super) async fn run_rd_hook(
    state: &AppState,
    claims: &Claims,
    event_type: HookEventType,
    subject: &str,
    input_json: Value,
    output_json: Option<Value>,
    is_error: bool,
    fail_on_block: bool,
) -> Result<(), AppError> {
    let result = run_lifecycle_hooks(
        state,
        &claims.tenant_id,
        RD_SCENARIO,
        event_type,
        subject,
        input_json,
        output_json,
        is_error,
    )
    .await?;
    if fail_on_block && (result.is_denied() || result.is_failed() || result.is_cancelled()) {
        let reason = if result.messages().is_empty() {
            "hook blocked execution".to_string()
        } else {
            result.messages().join("; ")
        };
        return Err(AppError::ValidationError(format!(
            "RD hook blocked '{subject}': {reason}"
        )));
    }
    Ok(())
}
