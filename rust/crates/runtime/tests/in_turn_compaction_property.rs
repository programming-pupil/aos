#![allow(clippy::doc_markdown, clippy::uninlined_format_args)]
//! Property-based test for In_Turn_Compaction (spec: codex-parity-gaps).
//!
//! Task 1.5 — drives `ConversationRuntime::run_turn` (which invokes the
//! in-turn `maybe_auto_compact` trigger point) with an in-memory session and a
//! mock `ApiClient` whose reported usage crosses the auto-compaction threshold.
//! The property asserts that once the gate is crossed, compaction happens
//! *and* the turn continues to natural completion using the compacted context
//! (the turn is not terminated / does not error out).

use proptest::prelude::*;

use runtime::{
    ApiClient, ApiRequest, AssistantEvent, ContentBlock, ConversationMessage, ConversationRuntime,
    MessageRole, PermissionMode, PermissionPolicy, RuntimeError, Session, StaticToolExecutor,
    TokenUsage,
};

/// Mock `ApiClient` that emits a single, tool-free assistant response whose
/// usage crosses the configured auto-compaction threshold. Because it requests
/// no tools, the reasoning loop finishes in one iteration — so if the turn is
/// *not* terminated by compaction it reaches natural completion.
struct ThresholdCrossingApi {
    input_tokens: u32,
}

#[::async_trait::async_trait]
impl ApiClient for ThresholdCrossingApi {
    async fn stream(&mut self, _request: ApiRequest) -> Result<Vec<AssistantEvent>, RuntimeError> {
        Ok(vec![
            AssistantEvent::TextDelta("done".to_string()),
            AssistantEvent::Usage(TokenUsage {
                input_tokens: self.input_tokens,
                output_tokens: 4,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
            }),
            AssistantEvent::MessageStop,
        ])
    }
}

/// Builds an in-memory session with alternating user/assistant messages whose
/// archived window is large enough to shrink after continuation framing. The
/// runtime must fail closed instead of replacing tiny source messages with a
/// larger synthetic checkpoint.
fn build_session(contents: &[String]) -> Session {
    let mut session = Session::new();
    session.messages = contents
        .iter()
        .enumerate()
        .map(|(index, text)| {
            let text = format!("{text}: {}", "important context ".repeat(48));
            if index % 2 == 0 {
                ConversationMessage::user_text(text)
            } else {
                ConversationMessage::assistant(vec![ContentBlock::Text { text }])
            }
        })
        .collect();
    session
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    // Feature: codex-parity-gaps, Property 1: In-turn 压缩后使用压缩后上下文继续本 turn 且不终止
    //
    // For any session whose cumulative input tokens reach or exceed the
    // Auto_Compact_Threshold while the turn's reasoning loop is still running,
    // In_Turn_Compaction MUST replace the session with the compacted version and
    // continue the rest of this turn (not return terminated/failed), so the turn
    // ends naturally rather than being interrupted.
    //
    // Validates: Requirements 1.1, 1.2
    #[test]
    fn in_turn_compaction_continues_turn_without_termination(
        // >= 6 messages guarantees a non-empty removable window (the runtime
        // preserves a recent tail of 4), so removed_message_count > 0.
        contents in proptest::collection::vec("[a-zA-Z0-9 ]{1,20}", 6..=24),
        threshold in 1_000u32..=200_000,
        // `extra` guarantees the mock's reported usage strictly clears the gate.
        extra in 0u32..=100_000,
    ) {
        let input_tokens = threshold.saturating_add(extra);

        let runtime_result = tokio::runtime::Runtime::new()
            .expect("tokio runtime should build")
            .block_on(async move {
                let session = build_session(&contents);
                let mut runtime = ConversationRuntime::new(
                    session,
                    ThresholdCrossingApi { input_tokens },
                    StaticToolExecutor::new(),
                    PermissionPolicy::new(PermissionMode::DangerFullAccess),
                    vec!["system".to_string()],
                )
                .with_auto_compaction_input_tokens_threshold(threshold);

                let summary = runtime.run_turn("trigger", None, ()).await?;
                // Return everything the property needs to inspect after the turn.
                let session_first_role = runtime.session().messages.first().map(|m| m.role);
                let session_tail = runtime.session().messages.get(1..).map(<[_]>::to_vec);
                let compaction_recorded = runtime.session().compaction.is_some();
                Ok::<_, RuntimeError>((
                    summary,
                    session_first_role,
                    session_tail,
                    compaction_recorded,
                ))
            });

        // (1.2) The turn continues to natural completion: run_turn returns Ok,
        // i.e. compaction did not terminate the turn with an error.
        let (summary, session_first_role, session_tail, compaction_recorded) =
            runtime_result.map_err(|error| {
                TestCaseError::fail(format!("turn was terminated with an error: {error}"))
            })?;

        // (1.1) In_Turn_Compaction actually happened during the turn.
        let compaction = summary
            .auto_compaction
            .as_ref()
            .ok_or_else(|| TestCaseError::fail("expected in-turn compaction to be reported"))?;
        prop_assert!(
            compaction.removed_message_count > 0,
            "compaction must remove at least one message"
        );

        // (1.1) The session was replaced with the compacted version: a leading
        // synthesized continuation `System` message + a recorded compaction
        // checkpoint.
        prop_assert_eq!(
            session_first_role,
            Some(MessageRole::System),
            "compacted session must start with the continuation system message"
        );
        prop_assert!(
            compaction_recorded,
            "a compaction checkpoint must be recorded on the session"
        );

        // (1.2) The turn continued *using the compacted context*: the retained
        // tail carried in the event is preserved verbatim as the live compacted
        // session's tail (everything after the leading continuation message).
        prop_assert_eq!(
            session_tail.as_deref(),
            Some(compaction.retained_tail.as_slice()),
            "the event's retained tail must match the live compacted session tail"
        );

        // (1.2) The turn reached natural completion: at least one reasoning
        // iteration ran and produced the assistant response after the gate was
        // crossed (rather than being cut short).
        prop_assert!(summary.iterations >= 1, "the turn must complete at least one iteration");
        prop_assert!(
            !summary.assistant_messages.is_empty(),
            "the turn must produce its assistant response and finish naturally"
        );
    }
}
