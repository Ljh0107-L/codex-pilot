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
const ACE_CONVERSATION_CONTEXT_PREFIX: &str = "__PROMPT_PILOT_ACE_CONVERSATION_CONTEXT__\n";
const ACE_AGENT_LOOP_CONTEXT_PREFIX: &str = "__PROMPT_PILOT_ACE_AGENT_LOOP__\n";
const ACE_GUARDRAIL_CONTEXT_PREFIX: &str = "__PROMPT_PILOT_ACE_GUARDRAIL__\n";

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

const ACE_PROMPT_PILOT_INSTRUCTIONS: &str = r#"You are PromptPilot ACE, the Agent Context Engine for OpenAI Codex CLI.

Your job is to enhance the user's draft prompt using the supplied Context Pack. ACE prepares a better prompt for the user to review; it does not execute the task, make a plan, or submit anything.

Return exactly the JSON object requested by the schema.

Rules:
- likely_understanding describes how Codex would likely interpret the original prompt if submitted unchanged.
- Write likely_understanding in the same primary language as the original prompt. If the prompt mixes languages, use the user's dominant language from the original prompt, not from the Context Pack.
- enhanced_prompt must be written as the user and remain in the same primary language as the original prompt unless the user explicitly asks for translation or another language.
- Use the Context Pack only as read-only project evidence.
- Use conversation_distillation only as read-only evidence from the current chat.
- Only cite project facts, rules, commands, and file paths that appear in the Context Pack.
- Do not invent files, APIs, frameworks, root causes, errors, tests, commands, tickets, or implementation details.
- Preserve explicit user constraints and paths.
- If context is incomplete or uncertain, phrase that uncertainty directly instead of filling gaps.
- Do not solve the task.
- Do not produce an implementation plan, numbered steps, phases, approval language, or Plan mode instructions.
- Keep the enhancement scoped to the user's request. Do not expand local PromptPilot/TUI work into auth, sandboxing, tool execution, approvals, model providers, Plan mode, or the main agent loop unless the original prompt explicitly asks for that.
- The output should be a natural coding-agent prompt that the user can review and send to Codex.
"#;

const ACE_AGENT_LOOP_INSTRUCTIONS: &str = r#"You are PromptPilot ACE, a read-only context agent for OpenAI Codex CLI.

Your job is to inspect the supplied ACE state and choose exactly one next action. You are not solving the user's task. You are gathering enough context to write a better prompt, then finishing with that enhanced prompt.

Return exactly the JSON object requested by the schema.

Allowed actions:
- search_files: request filename/path search for one or more queries.
- read_small_snippets: request small read-only snippets from paths already seen in ACE state.
- finish: return the final prompt preview.

Rules:
- Use only facts in ACE state and tool observations.
- Treat conversation_distillation as user/chat context, not project evidence.
- Do not invent files, APIs, frameworks, root causes, commands, or test names.
- If you need more context, choose search_files or read_small_snippets.
- If the original prompt is vague and ACE state does not already contain relevant files, prefer search_files before finish.
- For bugfix prompts, search for the user's domain terms plus common code terms such as login, auth, signin, session, token, error, handler, component, test, or their language-specific equivalents when relevant.
- Choose finish when the context is sufficient to write a useful enhanced prompt, or when more searching is unlikely to help.
- For finish, write likely_understanding and enhanced_prompt in the original prompt's primary language.
- For finish, enhanced_prompt must be written as the user and must not be an implementation plan.
- For finish, mention only file paths that are present in ACE state.
- Never request shell, tests, edits, approvals, or execution.
"#;

const ACE_CONVERSATION_DISTILLATION_INSTRUCTIONS: &str = r#"You are PromptPilot ACE, a conversation context distiller for OpenAI Codex CLI.

Your job is to read the user's current draft prompt and the supplied recent conversation transcript, then extract only the conversation details that are useful for later prompt enhancement.

Return exactly the JSON object requested by the schema.

Rules:
- Use only facts present in the original prompt or recent conversation transcript.
- Extract the user's actual intent, hard constraints, prior decisions, rejected approaches, mentioned files or modules, and search hints.
- Preserve the original prompt's primary language for likely_understanding and concise_summary.
- Do not solve the task.
- Do not invent files, APIs, commands, root causes, bugs, requirements, or implementation details.
- Do not produce an implementation plan.
- If the recent conversation does not add useful context, return empty arrays and a short concise_summary saying no extra context was found.
"#;

const ACE_GUARDRAIL_INSTRUCTIONS: &str = r#"You are PromptPilot ACE Guardrails, a semantic reviewer and repair pass for PromptPilot ACE.

Your job is to review a candidate enhanced prompt before it is shown to the user. You may pass it, repair it, or fail it.

Return exactly the JSON object requested by the schema.

Review for semantic guardrails:
- The enhanced prompt preserves the user's primary intent and explicit constraints.
- The enhanced prompt stays in the original prompt's primary language unless the user explicitly asked otherwise.
- The enhanced prompt does not become an implementation plan, numbered step-by-step plan, Plan mode prompt, or approval/execution instruction.
- The enhanced prompt does not expand scope into authentication, sandboxing, tool execution, approval logic, model providers, Plan mode, or the core agent loop unless the original prompt explicitly asks for that.
- The enhanced prompt does not claim root causes, APIs, commands, files, tests, tickets, or implementation details that are not supported by the supplied Context Pack.
- Conversation-derived constraints and prior decisions may be used only when supplied in conversation_distillation.
- The enhanced prompt reads naturally as something the user can review and send to Codex.

Decision rules:
- status = "pass" when the candidate is already acceptable. Return the candidate enhanced_prompt unchanged.
- status = "repair" when a small rewrite can fix the issue. Return the repaired enhanced_prompt.
- status = "fail" only when the candidate cannot be repaired without inventing missing facts or changing the user's intent.
- Repairs must use only facts from the input.
- Do not solve the user's task.
- Do not request more context, tools, tests, shell commands, edits, or approvals.
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

struct AceHiddenPromptRequest {
    instructions: &'static str,
    user_content: String,
    output_schema: fn() -> serde_json::Value,
}

pub(crate) async fn enhance_prompt(
    session: Arc<Session>,
    original_prompt: String,
    context_pack_json: Option<String>,
) -> CodexResult<PromptEnhancement> {
    let turn_context = session.new_default_turn().await;
    if let Some(hidden_request) =
        ace_hidden_prompt_request(context_pack_json.as_deref(), &original_prompt)
    {
        if let Some(model) = turn_context.config.prompt_pilot.model.as_deref() {
            return enhance_prompt_openai_compatible_raw_json(
                &turn_context.config.prompt_pilot,
                model,
                hidden_request.instructions,
                &hidden_request.user_content,
            )
            .await;
        }

        session
            .maybe_emit_unknown_model_warning_for_turn(turn_context.as_ref())
            .await;

        let instructions = hidden_request.instructions;
        let output_schema = hidden_request.output_schema;
        let user_content = hidden_request.user_content;
        let prompt = Prompt {
            input: vec![ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText { text: user_content }],
                phase: None,
            }],
            tools: Vec::new(),
            parallel_tool_calls: false,
            tool_choice: Some("none".to_string()),
            store: Some(false),
            base_instructions: BaseInstructions {
                text: instructions.to_string(),
            },
            personality: None,
            output_schema: Some(output_schema()),
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

        return parse_raw_json_output(stream).await;
    }

    let user_content = prompt_user_content(&original_prompt, context_pack_json.as_deref());
    let instructions = if context_pack_json.is_some() {
        ACE_PROMPT_PILOT_INSTRUCTIONS
    } else {
        PROMPT_PILOT_INSTRUCTIONS
    };
    if let Some(model) = turn_context.config.prompt_pilot.model.as_deref() {
        return enhance_prompt_openai_compatible(
            &turn_context.config.prompt_pilot,
            model,
            &original_prompt,
            instructions,
            &user_content,
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
            content: vec![ContentItem::InputText { text: user_content }],
            phase: None,
        }],
        tools: Vec::new(),
        parallel_tool_calls: false,
        tool_choice: Some("none".to_string()),
        store: Some(false),
        base_instructions: BaseInstructions {
            text: instructions.to_string(),
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
    instructions: &str,
    user_content: &str,
) -> CodexResult<PromptEnhancement> {
    let base_url = config
        .base_url
        .as_deref()
        .unwrap_or(DEFAULT_OPENAI_COMPATIBLE_BASE_URL);
    let url = chat_completions_url(base_url)?;
    let request = ChatCompletionsRequest {
        model,
        messages: vec![
            ChatCompletionMessage {
                role: "system",
                content: instructions,
            },
            ChatCompletionMessage {
                role: "user",
                content: user_content,
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

async fn enhance_prompt_openai_compatible_raw_json(
    config: &PromptPilotConfig,
    model: &str,
    instructions: &str,
    user_content: &str,
) -> CodexResult<PromptEnhancement> {
    let base_url = config
        .base_url
        .as_deref()
        .unwrap_or(DEFAULT_OPENAI_COMPATIBLE_BASE_URL);
    let url = chat_completions_url(base_url)?;
    let request = ChatCompletionsRequest {
        model,
        messages: vec![
            ChatCompletionMessage {
                role: "system",
                content: instructions,
            },
            ChatCompletionMessage {
                role: "user",
                content: user_content,
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

    parse_raw_json(raw_output)
}

fn prompt_user_content(original_prompt: &str, context_pack_json: Option<&str>) -> String {
    match context_pack_json {
        Some(context_pack_json) => format!(
            "Original prompt:\n{original_prompt}\n\nContext Pack JSON:\n{context_pack_json}"
        ),
        None => format!("Original prompt:\n{original_prompt}"),
    }
}

fn ace_agent_loop_user_content(original_prompt: &str, state_json: &str) -> String {
    format!("Original prompt:\n{original_prompt}\n\nACE state JSON:\n{state_json}")
}

fn ace_conversation_distillation_user_content(original_prompt: &str, state_json: &str) -> String {
    format!("Original prompt:\n{original_prompt}\n\nACE conversation context JSON:\n{state_json}")
}

fn ace_guardrail_user_content(original_prompt: &str, state_json: &str) -> String {
    format!("Original prompt:\n{original_prompt}\n\nACE guardrail review JSON:\n{state_json}")
}

fn ace_hidden_prompt_request(
    context_pack_json: Option<&str>,
    original_prompt: &str,
) -> Option<AceHiddenPromptRequest> {
    let context_pack_json = context_pack_json?;
    if let Some(state_json) = context_pack_json.strip_prefix(ACE_CONVERSATION_CONTEXT_PREFIX) {
        return Some(AceHiddenPromptRequest {
            instructions: ACE_CONVERSATION_DISTILLATION_INSTRUCTIONS,
            user_content: ace_conversation_distillation_user_content(original_prompt, state_json),
            output_schema: ace_conversation_distillation_schema,
        });
    }
    if let Some(state_json) = context_pack_json.strip_prefix(ACE_AGENT_LOOP_CONTEXT_PREFIX) {
        return Some(AceHiddenPromptRequest {
            instructions: ACE_AGENT_LOOP_INSTRUCTIONS,
            user_content: ace_agent_loop_user_content(original_prompt, state_json),
            output_schema: ace_agent_step_schema,
        });
    }
    context_pack_json
        .strip_prefix(ACE_GUARDRAIL_CONTEXT_PREFIX)
        .map(|state_json| AceHiddenPromptRequest {
            instructions: ACE_GUARDRAIL_INSTRUCTIONS,
            user_content: ace_guardrail_user_content(original_prompt, state_json),
            output_schema: ace_guardrail_review_schema,
        })
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

fn ace_conversation_distillation_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "likely_understanding": {
                "type": "string"
            },
            "concise_summary": {
                "type": "string"
            },
            "hard_constraints": {
                "type": "array",
                "items": { "type": "string" }
            },
            "prior_decisions": {
                "type": "array",
                "items": { "type": "string" }
            },
            "mentioned_paths": {
                "type": "array",
                "items": { "type": "string" }
            },
            "search_hints": {
                "type": "array",
                "items": { "type": "string" }
            },
            "uncertainties": {
                "type": "array",
                "items": { "type": "string" }
            }
        },
        "required": [
            "likely_understanding",
            "concise_summary",
            "hard_constraints",
            "prior_decisions",
            "mentioned_paths",
            "search_hints",
            "uncertainties"
        ]
    })
}

fn ace_agent_step_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "likely_understanding": {
                "type": "string"
            },
            "action": {
                "type": "string",
                "enum": ["search_files", "read_small_snippets", "finish"]
            },
            "reason": {
                "type": "string"
            },
            "queries": {
                "type": "array",
                "items": { "type": "string" }
            },
            "paths": {
                "type": "array",
                "items": { "type": "string" }
            },
            "enhanced_prompt": {
                "type": "string"
            },
            "changed": {
                "type": "boolean"
            },
            "context_used": {
                "type": "array",
                "items": { "type": "string" }
            },
            "unresolved_questions": {
                "type": "array",
                "items": { "type": "string" }
            }
        },
        "required": [
            "likely_understanding",
            "action",
            "reason",
            "queries",
            "paths",
            "enhanced_prompt",
            "changed",
            "context_used",
            "unresolved_questions"
        ]
    })
}

fn ace_guardrail_review_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "status": {
                "type": "string",
                "enum": ["pass", "repair", "fail"]
            },
            "reason": {
                "type": "string"
            },
            "likely_understanding": {
                "type": "string"
            },
            "enhanced_prompt": {
                "type": "string"
            }
        },
        "required": [
            "status",
            "reason",
            "likely_understanding",
            "enhanced_prompt"
        ]
    })
}

async fn parse_raw_json_output(
    mut stream: crate::ResponseStream,
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
            "PromptPilot ACE agent stream closed before response.completed".to_string(),
        ));
    }

    let raw_output = if message_text.trim().is_empty() {
        delta_text.trim()
    } else {
        message_text.trim()
    };
    if raw_output.is_empty() {
        return Err(CodexErr::Fatal(
            "PromptPilot ACE agent response was empty".to_string(),
        ));
    }

    parse_raw_json(raw_output)
}

fn parse_raw_json(raw_output: &str) -> CodexResult<PromptEnhancement> {
    let value: serde_json::Value = serde_json::from_str(raw_output).map_err(|err| {
        CodexErr::Fatal(format!(
            "PromptPilot ACE agent response was not valid JSON: {err}"
        ))
    })?;
    let likely_understanding = value
        .get("likely_understanding")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("PromptPilot ACE is selecting context.")
        .to_string();
    let raw_output = serde_json::to_string(&value).map_err(|err| {
        CodexErr::Fatal(format!(
            "PromptPilot ACE agent response could not be serialized: {err}"
        ))
    })?;

    Ok(PromptEnhancement {
        likely_understanding,
        enhanced_prompt: raw_output,
        changed: true,
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
            ..PromptPilotConfig::default()
        };

        let user_content = prompt_user_content("fix login bug", /*context_pack_json*/ None);
        let enhancement = enhance_prompt_openai_compatible(
            &config,
            "prompt-optimizer",
            "fix login bug",
            PROMPT_PILOT_INSTRUCTIONS,
            &user_content,
        )
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
        assert!(ACE_PROMPT_PILOT_INSTRUCTIONS.contains("Context Pack"));
        assert!(ACE_PROMPT_PILOT_INSTRUCTIONS.contains("Do not produce an implementation plan"));

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
