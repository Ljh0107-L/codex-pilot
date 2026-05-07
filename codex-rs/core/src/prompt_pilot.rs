use std::sync::Arc;
use std::time::Duration;

use crate::Prompt;
use crate::client_common::ResponseEvent;
use crate::config::PromptPilotConfig;
use crate::session::session::Session;
use codex_protocol::config_types::ReasoningSummary;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::models::BaseInstructions;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::openai_models::ReasoningEffort;
use codex_rollout_trace::InferenceTraceContext;
use futures::StreamExt;
use reqwest::StatusCode;
use serde::Deserialize;
use serde::Serialize;
use serde_json::json;
use url::Url;

const DEFAULT_OPENAI_COMPATIBLE_BASE_URL: &str = "https://api.openai.com/v1";
const DEFAULT_OPENAI_COMPATIBLE_API_KEY_ENV: &str = "OPENAI_API_KEY";
const OPENAI_COMPATIBLE_REQUEST_TIMEOUT: Duration = Duration::from_secs(90);

const PROMPT_PILOT_INSTRUCTIONS: &str = r#"You are PromptPilot, a prompt refinement helper for OpenAI Codex CLI.

Your job is to predict how Codex would likely understand the user's draft, then improve the draft only when that materially helps Codex execute the user's intent.

Return exactly the JSON object requested by the schema.

Rules:
- likely_understanding describes how Codex would likely interpret the original prompt if submitted unchanged.
- Write likely_understanding in the same primary language as the original prompt. If the prompt mixes languages, use the user's dominant language.
- enhanced_prompt must be the original prompt exactly when the original prompt is already clear, conversational, trivial, or should not be expanded.
- Keep enhanced_prompt in the same primary language as the original prompt unless the user explicitly asks for translation or a different language.
- Do not translate the user's intent into English by default.
- Do not invent requirements, files, tests, tools, PR steps, git steps, or implementation plans.
- Do not turn casual conversation into a coding task.
- Add constraints, context, or verification only when the original prompt implies that kind of work and the draft is materially ambiguous.
- Keep enhanced_prompt concise and written as the user, not as commentary about the user.

Examples:
- Original: hello
  likely_understanding: Codex should respond to the greeting.
  enhanced_prompt: hello
  changed: false
- Original: Summarize recent commits
  likely_understanding: Codex should inspect the repository's recent Git history and summarize the main changes.
  enhanced_prompt: Summarize the recent commits in this repository. Group the main changes by theme, mention the commit range reviewed, and call out notable risks or follow-up questions.
  changed: true
- Original: fix login bug
  likely_understanding: Codex should investigate and fix a login-related bug in the current codebase.
  enhanced_prompt: Investigate and fix the login bug in this codebase. First identify the relevant login flow, then make the smallest scoped change needed and run the most relevant local checks.
  changed: true
- Original: 修复登录 bug
  likely_understanding: Codex 应该在当前代码库中调查并修复登录相关的问题。
  enhanced_prompt: 在这个代码库中调查并修复登录 bug。先定位相关登录流程，然后进行最小范围修改，并运行最相关的本地检查。
  changed: true
"#;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromptEnhancement {
    pub likely_understanding: String,
    pub enhanced_prompt: String,
    pub changed: bool,
}

#[derive(Deserialize)]
struct ModelPromptEnhancement {
    likely_understanding: String,
    enhanced_prompt: String,
    changed: bool,
}

pub(crate) async fn enhance_prompt(
    session: Arc<Session>,
    original_prompt: String,
) -> CodexResult<PromptEnhancement> {
    let turn_context = session.new_default_turn().await;
    if let Some(model) = turn_context.config.prompt_pilot.model.as_deref() {
        return enhance_prompt_openai_compatible(
            &turn_context.config.prompt_pilot,
            model,
            &original_prompt,
        )
        .await;
    }

    session
        .maybe_emit_unknown_model_warning_for_turn(turn_context.as_ref())
        .await;

    let prompt = Prompt {
        input: vec![ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: format!("Original prompt:\n{original_prompt}"),
            }],
            phase: None,
        }],
        tools: Vec::new(),
        parallel_tool_calls: false,
        tool_choice: Some("none".to_string()),
        store: Some(false),
        base_instructions: BaseInstructions {
            text: PROMPT_PILOT_INSTRUCTIONS.to_string(),
        },
        personality: None,
        output_schema: Some(prompt_enhancement_schema()),
        output_schema_strict: true,
    };

    let turn_metadata_header = turn_context.turn_metadata_state.current_header_value();
    let mut client_session = session.services.model_client.new_session();
    let stream = client_session
        .stream(
            &prompt,
            &turn_context.model_info,
            &turn_context.session_telemetry,
            Some(ReasoningEffort::Medium),
            ReasoningSummary::None,
            turn_context.config.service_tier.clone(),
            turn_metadata_header.as_deref(),
            &InferenceTraceContext::disabled(),
        )
        .await?;

    parse_prompt_enhancement_output(stream, &original_prompt).await
}

async fn enhance_prompt_openai_compatible(
    config: &PromptPilotConfig,
    model: &str,
    original_prompt: &str,
) -> CodexResult<PromptEnhancement> {
    let base_url = config
        .base_url
        .as_deref()
        .unwrap_or(DEFAULT_OPENAI_COMPATIBLE_BASE_URL);
    let url = chat_completions_url(base_url)?;
    let user_content = format!("Original prompt:\n{original_prompt}");
    let request = ChatCompletionsRequest {
        model,
        messages: vec![
            ChatCompletionMessage {
                role: "system",
                content: PROMPT_PILOT_INSTRUCTIONS,
            },
            ChatCompletionMessage {
                role: "user",
                content: &user_content,
            },
        ],
        response_format: ChatCompletionResponseFormat {
            kind: "json_object",
        },
        stream: false,
    };

    let client = reqwest::Client::builder()
        .timeout(OPENAI_COMPATIBLE_REQUEST_TIMEOUT)
        .build()
        .map_err(|err| {
            CodexErr::Fatal(format!("failed to build PromptPilot HTTP client: {err}"))
        })?;
    let mut request_builder = client.post(url).json(&request);
    if let Some(api_key) = prompt_pilot_api_key(config)? {
        request_builder = request_builder.bearer_auth(api_key);
    }

    let response = request_builder.send().await.map_err(|err| {
        CodexErr::Fatal(format!(
            "PromptPilot OpenAI-compatible request failed: {err}"
        ))
    })?;
    let status = response.status();
    let body = response.text().await.map_err(|err| {
        CodexErr::Fatal(format!(
            "failed to read PromptPilot OpenAI-compatible response: {err}"
        ))
    })?;
    if !status.is_success() {
        return Err(openai_compatible_status_error(status, &body));
    }

    let response: ChatCompletionsResponse = serde_json::from_str(&body).map_err(|err| {
        CodexErr::Fatal(format!(
            "PromptPilot OpenAI-compatible response was not valid JSON: {err}"
        ))
    })?;
    let raw_output = response
        .choices
        .first()
        .and_then(|choice| choice.message.content.as_deref())
        .map(str::trim)
        .filter(|content| !content.is_empty())
        .ok_or_else(|| {
            CodexErr::Fatal(
                "PromptPilot OpenAI-compatible response did not include message content"
                    .to_string(),
            )
        })?;

    parse_prompt_enhancement_json(raw_output, original_prompt)
}

#[derive(Serialize)]
struct ChatCompletionsRequest<'a> {
    model: &'a str,
    messages: Vec<ChatCompletionMessage<'a>>,
    response_format: ChatCompletionResponseFormat<'a>,
    stream: bool,
}

#[derive(Serialize)]
struct ChatCompletionMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Serialize)]
struct ChatCompletionResponseFormat<'a> {
    #[serde(rename = "type")]
    kind: &'a str,
}

#[derive(Deserialize)]
struct ChatCompletionsResponse {
    choices: Vec<ChatCompletionChoice>,
}

#[derive(Deserialize)]
struct ChatCompletionChoice {
    message: ChatCompletionResponseMessage,
}

#[derive(Deserialize)]
struct ChatCompletionResponseMessage {
    content: Option<String>,
}

fn chat_completions_url(base_url: &str) -> CodexResult<Url> {
    let base_url = base_url.trim();
    if base_url.is_empty() {
        return Err(CodexErr::Fatal(
            "prompt_pilot.base_url must not be empty".to_string(),
        ));
    }
    let base_url = if base_url.ends_with('/') {
        base_url.to_string()
    } else {
        format!("{base_url}/")
    };
    Url::parse(&base_url)
        .and_then(|url| url.join("chat/completions"))
        .map_err(|err| CodexErr::Fatal(format!("prompt_pilot.base_url must be a valid URL: {err}")))
}

fn prompt_pilot_api_key(config: &PromptPilotConfig) -> CodexResult<Option<String>> {
    let api_key_env = config
        .api_key_env
        .as_deref()
        .unwrap_or(DEFAULT_OPENAI_COMPATIBLE_API_KEY_ENV);
    if api_key_env.is_empty() {
        return Ok(None);
    }

    let api_key = std::env::var(api_key_env).map_err(|_| {
        CodexErr::Fatal(format!(
            "PromptPilot OpenAI-compatible model requires ${api_key_env}; set prompt_pilot.api_key_env = \"\" to omit Authorization"
        ))
    })?;
    let api_key = api_key.trim().to_string();
    if api_key.is_empty() {
        return Err(CodexErr::Fatal(format!(
            "PromptPilot OpenAI-compatible API key from ${api_key_env} is empty"
        )));
    }
    Ok(Some(api_key))
}

fn openai_compatible_status_error(status: StatusCode, body: &str) -> CodexErr {
    let body = body.trim();
    if body.is_empty() {
        CodexErr::Fatal(format!(
            "PromptPilot OpenAI-compatible request failed with HTTP {status}"
        ))
    } else {
        CodexErr::Fatal(format!(
            "PromptPilot OpenAI-compatible request failed with HTTP {status}: {body}"
        ))
    }
}

fn prompt_enhancement_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "likely_understanding": {
                "type": "string",
                "description": "How Codex would likely interpret the original prompt if submitted unchanged. Use the same primary language as the original prompt."
            },
            "enhanced_prompt": {
                "type": "string",
                "description": "The refined prompt, written as the user. Use the same primary language as the original prompt unless the user explicitly asks for translation or a different language."
            },
            "changed": {
                "type": "boolean"
            }
        },
        "required": ["likely_understanding", "enhanced_prompt", "changed"]
    })
}

async fn parse_prompt_enhancement_output(
    mut stream: crate::ResponseStream,
    original_prompt: &str,
) -> CodexResult<PromptEnhancement> {
    let mut completed = false;
    let mut message_text = String::new();
    let mut delta_text = String::new();

    while let Some(event) = stream.next().await {
        match event? {
            ResponseEvent::OutputItemDone(item) => {
                message_text.push_str(&response_item_text(&item));
            }
            ResponseEvent::OutputTextDelta(delta) => {
                delta_text.push_str(&delta);
            }
            ResponseEvent::Completed { .. } => {
                completed = true;
                break;
            }
            _ => {}
        }
    }

    if !completed {
        return Err(CodexErr::Fatal(
            "prompt enhancement stream closed before response.completed".to_string(),
        ));
    }

    let raw_output = if message_text.trim().is_empty() {
        delta_text.trim()
    } else {
        message_text.trim()
    };
    if raw_output.is_empty() {
        return Err(CodexErr::Fatal(
            "prompt enhancement response was empty".to_string(),
        ));
    }

    parse_prompt_enhancement_json(raw_output, original_prompt)
}

fn parse_prompt_enhancement_json(
    raw_output: &str,
    original_prompt: &str,
) -> CodexResult<PromptEnhancement> {
    let parsed: ModelPromptEnhancement = serde_json::from_str(raw_output).map_err(|err| {
        CodexErr::Fatal(format!(
            "prompt enhancement response was not valid JSON: {err}"
        ))
    })?;
    let likely_understanding = parsed.likely_understanding.trim().to_string();
    let enhanced_prompt = parsed.enhanced_prompt.trim().to_string();
    if likely_understanding.is_empty() || enhanced_prompt.is_empty() {
        return Err(CodexErr::Fatal(
            "prompt enhancement response must include non-empty fields".to_string(),
        ));
    }

    Ok(PromptEnhancement {
        likely_understanding,
        changed: parsed.changed && enhanced_prompt != original_prompt,
        enhanced_prompt,
    })
}

fn response_item_text(item: &ResponseItem) -> String {
    let ResponseItem::Message { content, .. } = item else {
        return String::new();
    };

    content
        .iter()
        .filter_map(|item| match item {
            ContentItem::InputText { text } | ContentItem::OutputText { text } => {
                Some(text.as_str())
            }
            ContentItem::InputImage { .. } => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use tokio::sync::mpsc;
    use tokio_util::sync::CancellationToken;
    use wiremock::Mock;
    use wiremock::MockServer;
    use wiremock::ResponseTemplate;
    use wiremock::matchers::body_json;
    use wiremock::matchers::method;
    use wiremock::matchers::path;

    fn response_stream(
        events: Vec<codex_protocol::error::Result<ResponseEvent>>,
    ) -> crate::ResponseStream {
        let (tx_event, rx_event) = mpsc::channel(events.len().max(1));
        for event in events {
            tx_event
                .try_send(event)
                .expect("response stream test channel should have capacity");
        }
        drop(tx_event);
        crate::ResponseStream {
            rx_event,
            consumer_dropped: CancellationToken::new(),
        }
    }

    #[tokio::test]
    async fn parses_prompt_enhancement_json() {
        let output = r#"{"likely_understanding":"Codex should respond to the greeting.","enhanced_prompt":"hello","changed":false}"#;
        let stream = response_stream(vec![
            Ok(ResponseEvent::OutputItemDone(ResponseItem::Message {
                id: None,
                role: "assistant".to_string(),
                content: vec![ContentItem::OutputText {
                    text: output.to_string(),
                }],
                phase: None,
            })),
            Ok(ResponseEvent::Completed {
                response_id: "resp-1".to_string(),
                token_usage: None,
                end_turn: Some(true),
            }),
        ]);

        let enhancement = parse_prompt_enhancement_output(stream, "hello")
            .await
            .expect("enhancement should parse");

        assert_eq!(
            enhancement,
            PromptEnhancement {
                likely_understanding: "Codex should respond to the greeting.".to_string(),
                enhanced_prompt: "hello".to_string(),
                changed: false,
            }
        );
    }

    #[tokio::test]
    async fn openai_compatible_chat_request_uses_configured_model() {
        let server = MockServer::start().await;
        let output = r#"{"likely_understanding":"Codex should fix the login bug.","enhanced_prompt":"Investigate and fix the login bug in this codebase.","changed":true}"#;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(body_json(json!({
                "model": "prompt-optimizer",
                "messages": [
                    {
                        "role": "system",
                        "content": PROMPT_PILOT_INSTRUCTIONS,
                    },
                    {
                        "role": "user",
                        "content": "Original prompt:\nfix login bug",
                    },
                ],
                "response_format": {
                    "type": "json_object",
                },
                "stream": false,
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [
                    {
                        "message": {
                            "content": output,
                        },
                    },
                ],
            })))
            .mount(&server)
            .await;

        let config = PromptPilotConfig {
            model: Some("prompt-optimizer".to_string()),
            base_url: Some(format!("{}/v1", server.uri())),
            api_key_env: Some(String::new()),
        };

        let enhancement =
            enhance_prompt_openai_compatible(&config, "prompt-optimizer", "fix login bug")
                .await
                .expect("enhancement should parse");

        assert_eq!(
            enhancement,
            PromptEnhancement {
                likely_understanding: "Codex should fix the login bug.".to_string(),
                enhanced_prompt: "Investigate and fix the login bug in this codebase.".to_string(),
                changed: true,
            }
        );
    }

    #[test]
    fn prompt_instructions_require_original_prompt_language() {
        assert!(PROMPT_PILOT_INSTRUCTIONS.contains("same primary language"));
        assert!(PROMPT_PILOT_INSTRUCTIONS.contains("Do not translate"));

        let schema = prompt_enhancement_schema();
        assert_eq!(
            schema["properties"]["likely_understanding"]["description"],
            "How Codex would likely interpret the original prompt if submitted unchanged. Use the same primary language as the original prompt."
        );
        assert_eq!(
            schema["properties"]["enhanced_prompt"]["description"],
            "The refined prompt, written as the user. Use the same primary language as the original prompt unless the user explicitly asks for translation or a different language."
        );
    }
}
