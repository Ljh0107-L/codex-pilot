use std::sync::Arc;

use crate::Prompt;
use crate::client_common::ResponseEvent;
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
use serde::Deserialize;
use serde_json::json;

const PROMPT_PILOT_INSTRUCTIONS: &str = r#"You are PromptPilot, a prompt refinement helper for OpenAI Codex CLI.

Your job is to predict how Codex would likely understand the user's draft, then improve the draft only when that materially helps Codex execute the user's intent.

Return exactly the JSON object requested by the schema.

Rules:
- likely_understanding describes how Codex would likely interpret the original prompt if submitted unchanged.
- enhanced_prompt must be the original prompt exactly when the original prompt is already clear, conversational, trivial, or should not be expanded.
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
            Some(ReasoningEffort::Minimal),
            ReasoningSummary::None,
            turn_context.config.service_tier.clone(),
            turn_metadata_header.as_deref(),
            &InferenceTraceContext::disabled(),
        )
        .await?;

    parse_prompt_enhancement_output(stream, &original_prompt).await
}

fn prompt_enhancement_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "likely_understanding": {
                "type": "string"
            },
            "enhanced_prompt": {
                "type": "string"
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
}
