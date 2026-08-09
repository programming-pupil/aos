use crate::session::{ContentBlock, ConversationMessage, Session};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenizerKind {
    O200kBase,
    Cl100kBase,
    UnicodeHeuristic,
}

impl TokenizerKind {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::O200kBase => "tiktoken:o200k_base",
            Self::Cl100kBase => "tiktoken:cl100k_base",
            Self::UnicodeHeuristic => "unicode_heuristic",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenEstimateOptions {
    pub tokenizer: TokenizerKind,
}

impl Default for TokenEstimateOptions {
    fn default() -> Self {
        Self {
            tokenizer: TokenizerKind::UnicodeHeuristic,
        }
    }
}

#[must_use]
pub fn estimate_session_tokens(session: &Session) -> usize {
    estimate_session_tokens_with_options(session, TokenEstimateOptions::default())
}

#[must_use]
pub fn estimate_session_tokens_with_options(
    session: &Session,
    options: TokenEstimateOptions,
) -> usize {
    session
        .messages
        .iter()
        .map(|message| estimate_message_tokens_with_options(message, options))
        .sum()
}

#[must_use]
pub fn estimate_message_tokens(message: &ConversationMessage) -> usize {
    estimate_message_tokens_with_options(message, TokenEstimateOptions::default())
}

#[must_use]
pub fn estimate_message_tokens_with_options(
    message: &ConversationMessage,
    options: TokenEstimateOptions,
) -> usize {
    let block_tokens: usize = message
        .blocks
        .iter()
        .map(|block| match block {
            ContentBlock::Text { text } => estimate_text_tokens_with_options(text, options),
            ContentBlock::ToolUse { name, input, .. } => {
                estimate_text_tokens_with_options(name, options)
                    + estimate_text_tokens_with_options(input, options)
                    + 6
            }
            ContentBlock::ToolResult {
                tool_name, output, ..
            } => {
                estimate_text_tokens_with_options(tool_name, options)
                    + estimate_text_tokens_with_options(output, options)
                    + 6
            }
        })
        .sum();

    let thinking_tokens = message.thinking.as_ref().map_or(0, |thinking| {
        estimate_text_tokens_with_options(thinking, options)
    });
    let signature_tokens = message.thinking_signature.as_ref().map_or(0, |signature| {
        estimate_text_tokens_with_options(signature, options)
    });

    // Include a small role/framing allowance per message. Exact chat framing
    // varies by provider, but this keeps compaction thresholds conservative.
    block_tokens + thinking_tokens + signature_tokens + 4
}

#[must_use]
pub fn estimate_text_tokens(text: &str) -> usize {
    estimate_text_tokens_with_options(text, TokenEstimateOptions::default())
}

#[must_use]
pub fn estimate_text_tokens_with_options(text: &str, options: TokenEstimateOptions) -> usize {
    if text.is_empty() {
        return 0;
    }
    match options.tokenizer {
        TokenizerKind::O200kBase => tiktoken_rs::o200k_base_singleton()
            .count_with_special_tokens(text)
            .max(1),
        TokenizerKind::Cl100kBase => tiktoken_rs::cl100k_base_singleton()
            .count_with_special_tokens(text)
            .max(1),
        TokenizerKind::UnicodeHeuristic => estimate_unicode_heuristic_tokens(text),
    }
}

fn estimate_unicode_heuristic_tokens(text: &str) -> usize {
    let mut ascii_run = 0usize;
    let mut tokens = 0usize;
    for ch in text.chars() {
        if ch.is_ascii() {
            ascii_run = ascii_run.saturating_add(1);
        } else {
            if ascii_run > 0 {
                tokens = tokens.saturating_add(ascii_run.div_ceil(4));
                ascii_run = 0;
            }
            tokens = tokens.saturating_add(non_ascii_char_token_weight(ch));
        }
    }
    if ascii_run > 0 {
        tokens = tokens.saturating_add(ascii_run.div_ceil(4));
    }
    tokens.max(1)
}

fn non_ascii_char_token_weight(ch: char) -> usize {
    if is_cjk_char(ch) {
        1
    } else {
        // Accented Latin, emoji, and other scripts vary a lot. Two tokens is a
        // safer fallback for compaction than the old UTF-8-bytes/4 estimate.
        2
    }
}

fn is_cjk_char(ch: char) -> bool {
    matches!(
        ch as u32,
        0x3400..=0x4DBF
            | 0x4E00..=0x9FFF
            | 0xF900..=0xFAFF
            | 0x20000..=0x2A6DF
            | 0x2A700..=0x2B73F
            | 0x2B740..=0x2B81F
            | 0x2B820..=0x2CEAF
            | 0x2CEB0..=0x2EBEF
            | 0x30000..=0x3134F
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiktoken_counts_known_openai_text() {
        let tokens = estimate_text_tokens_with_options(
            "hello world",
            TokenEstimateOptions {
                tokenizer: TokenizerKind::O200kBase,
            },
        );
        assert!((1..=4).contains(&tokens));
    }

    #[test]
    fn unicode_heuristic_does_not_count_utf8_bytes_as_tokens() {
        assert_eq!(estimate_unicode_heuristic_tokens("你好"), 2);
        assert_eq!(estimate_unicode_heuristic_tokens("hello"), 2);
    }
}
