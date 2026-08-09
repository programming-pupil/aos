use api::{
    ContentBlockDelta, ContentBlockDeltaEvent, InputMessage, MessageRequest, OpenAiCompatClient,
    OpenAiCompatConfig, OutputContentBlock, StreamEvent, ToolChoice, ToolDefinition,
};
use serde_json::json;

fn deepseek_client() -> OpenAiCompatClient {
    let api_key = std::env::var("DEEPSEEK_API_KEY")
        .expect("DEEPSEEK_API_KEY is required for the ignored live test");
    OpenAiCompatClient::new(api_key, OpenAiCompatConfig::openai())
        .with_base_url("https://api.deepseek.com/v1")
        .with_retry_policy(
            0,
            std::time::Duration::from_millis(100),
            std::time::Duration::from_millis(100),
        )
}

#[tokio::test]
#[ignore = "uses the official DeepSeek API and consumes quota"]
async fn deepseek_v4_thinking_accepts_aos_tools_without_tool_choice_rejection() {
    let response = deepseek_client()
        .send_message(&MessageRequest {
            model: "deepseek-v4-pro".to_string(),
            max_tokens: 512,
            messages: vec![InputMessage::user_text("北京海淀天气预报")],
            system: Some(
                "Use WebSearch when current information is needed. Return a tool call or a concise answer."
                    .to_string(),
            ),
            tools: Some(vec![ToolDefinition {
                name: "WebSearch".to_string(),
                description: Some("Search the public web for current information".to_string()),
                input_schema: json!({
                    "type": "object",
                    "properties": {"query": {"type": "string"}},
                    "required": ["query"]
                }),
            }]),
            tool_choice: Some(ToolChoice::Auto),
            stream: false,
            ..Default::default()
        })
        .await
        .expect("official DeepSeek V4 should accept AOS tools in thinking mode");

    assert!(response.content.iter().any(|block| matches!(
        block,
        OutputContentBlock::Text { .. } | OutputContentBlock::ToolUse { .. }
    )));
}

#[tokio::test]
#[ignore = "uses the official DeepSeek API and consumes quota"]
async fn deepseek_flash_responses_web_search_stream_returns_usable_text() {
    let mut stream = deepseek_client()
        .stream_responses_web_search_message(&MessageRequest {
            model: "deepseek-v4-flash".to_string(),
            max_tokens: 1_024,
            messages: vec![InputMessage::user_text("北京海淀天气预报")],
            stream: false,
            ..Default::default()
        })
        .await
        .expect("official DeepSeek Flash Responses web search stream should start");

    let mut text = String::new();
    while let Some(event) = stream
        .next_event()
        .await
        .expect("DeepSeek Responses stream event should parse")
    {
        if let StreamEvent::ContentBlockDelta(ContentBlockDeltaEvent {
            delta: ContentBlockDelta::TextDelta { text: delta },
            ..
        }) = event
        {
            text.push_str(&delta);
        }
    }
    assert!(
        !text.trim().is_empty(),
        "Responses stream must contain answer text"
    );
}
