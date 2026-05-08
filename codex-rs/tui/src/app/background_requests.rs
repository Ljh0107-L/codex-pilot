//! Background app-server requests launched by the TUI app.
//!
//! This module owns fire-and-forget fetch/write helpers for MCP inventory, skills, plugins, rate
//! limits, add-credit nudges, and feedback uploads. Results are routed back through `AppEvent` so
//! the main event loop remains single-threaded.

use super::*;
use crate::bottom_pane::PromptPilotPreview;
use crate::legacy_core::config::Config;
use crate::prompt_pilot::AceProgress;
use crate::prompt_pilot::AceProgressStep;
use crate::prompt_pilot::collect_bootstrap_context_pack;
use crate::prompt_pilot::context_pack_json;
use crate::prompt_pilot::context_pack_repo_root;
use crate::prompt_pilot::relative_path;
use crate::prompt_pilot::retrieve_relevant_files_for_queries;
use codex_app_server_protocol::HookTrustStatus;
use codex_app_server_protocol::MarketplaceAddParams;
use codex_app_server_protocol::MarketplaceAddResponse;
use codex_app_server_protocol::MarketplaceRemoveParams;
use codex_app_server_protocol::MarketplaceRemoveResponse;
use codex_app_server_protocol::MarketplaceUpgradeParams;
use codex_app_server_protocol::MarketplaceUpgradeResponse;
use codex_exec_server::LOCAL_FS;
use serde::Deserialize;
use serde::Serialize;

use codex_app_server_protocol::RequestId;

use codex_utils_absolute_path::AbsolutePathBuf;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;

const ACE_CONVERSATION_CONTEXT_PREFIX: &str = "__PROMPT_PILOT_ACE_CONVERSATION_CONTEXT__\n";
const ACE_AGENT_LOOP_CONTEXT_PREFIX: &str = "__PROMPT_PILOT_ACE_AGENT_LOOP__\n";
const ACE_GUARDRAIL_CONTEXT_PREFIX: &str = "__PROMPT_PILOT_ACE_GUARDRAIL__\n";
const MAX_PROMPT_PILOT_CONVERSATION_BLOCKS: usize = 24;
const MAX_PROMPT_PILOT_CONVERSATION_CHARS: usize = 12_000;
const MAX_ACE_SNIPPET_BYTES: usize = 2_000;

impl App {
    pub(super) fn prompt_pilot_enhance(
        &mut self,
        app_server: &AppServerSession,
        thread_id: ThreadId,
        request_id: u64,
        prompt: String,
        context_aware: bool,
    ) {
        let request_handle = app_server.request_handle();
        let app_event_tx = self.app_event_tx.clone();
        let config = self.config.clone();
        let conversation_context = if context_aware {
            self.prompt_pilot_conversation_context()
        } else {
            None
        };
        tokio::spawn(async move {
            let result = if context_aware {
                run_prompt_pilot_ace_agent_loop(
                    request_handle,
                    thread_id,
                    config,
                    prompt,
                    conversation_context,
                    request_id,
                    app_event_tx.clone(),
                )
                .await
            } else {
                fetch_prompt_pilot_enhancement(
                    request_handle,
                    thread_id,
                    prompt,
                    /*context_pack_json*/ None,
                )
                .await
                .map_err(|err| format!("{err:#}"))
            };
            app_event_tx.send(AppEvent::PromptPilotEnhanceResult { request_id, result });
        });
    }

    fn prompt_pilot_conversation_context(&self) -> Option<String> {
        let mut blocks = Vec::new();
        let mut chars = 0usize;
        for cell in self.transcript_cells.iter().rev() {
            let block = lines_to_plain_text(cell.transcript_lines(/*width*/ 120));
            if block.trim().is_empty() {
                continue;
            }

            chars = chars.saturating_add(block.chars().count());
            blocks.push(block);
            if blocks.len() >= MAX_PROMPT_PILOT_CONVERSATION_BLOCKS
                || chars >= MAX_PROMPT_PILOT_CONVERSATION_CHARS
            {
                break;
            }
        }
        blocks.reverse();

        if let Some(lines) = self.chat_widget.active_cell_transcript_lines(/*width*/ 120) {
            let active = lines_to_plain_text(lines);
            if !active.trim().is_empty() {
                blocks.push(active);
            }
        }

        let context = truncate_chars(
            blocks.join("\n\n").trim(),
            MAX_PROMPT_PILOT_CONVERSATION_CHARS,
        );
        (!context.trim().is_empty()).then_some(context)
    }

    pub(super) fn fetch_mcp_inventory(
        &mut self,
        app_server: &AppServerSession,
        detail: McpServerStatusDetail,
    ) {
        let request_handle = app_server.request_handle();
        let app_event_tx = self.app_event_tx.clone();
        tokio::spawn(async move {
            let result = fetch_all_mcp_server_statuses(request_handle, detail)
                .await
                .map_err(|err| err.to_string());
            app_event_tx.send(AppEvent::McpInventoryLoaded { result, detail });
        });
    }

    /// Spawns a background task to fetch account rate limits and deliver the
    /// result as a `RateLimitsLoaded` event.
    ///
    /// The `origin` is forwarded to the completion handler so it can distinguish
    /// a startup prefetch (which only updates cached snapshots and schedules a
    /// frame) from a `/status`-triggered refresh (which must finalize the
    /// corresponding status card).
    pub(super) fn refresh_rate_limits(
        &mut self,
        app_server: &AppServerSession,
        origin: RateLimitRefreshOrigin,
    ) {
        let request_handle = app_server.request_handle();
        let app_event_tx = self.app_event_tx.clone();
        tokio::spawn(async move {
            let result = fetch_account_rate_limits(request_handle)
                .await
                .map_err(|err| err.to_string());
            app_event_tx.send(AppEvent::RateLimitsLoaded { origin, result });
        });
    }

    pub(super) fn send_add_credits_nudge_email(
        &mut self,
        app_server: &AppServerSession,
        credit_type: AddCreditsNudgeCreditType,
    ) {
        let request_handle = app_server.request_handle();
        let app_event_tx = self.app_event_tx.clone();
        tokio::spawn(async move {
            let result = send_add_credits_nudge_email(request_handle, credit_type)
                .await
                .map_err(|err| err.to_string());
            app_event_tx.send(AppEvent::AddCreditsNudgeEmailFinished { result });
        });
    }

    /// Starts the initial skills refresh without delaying the first interactive frame.
    ///
    /// Startup only needs skill metadata to populate skill mentions and the skills UI; the prompt can be
    /// rendered before that metadata arrives. The result is routed through the normal app event queue so
    /// the same response handler updates the chat widget and emits invalid `SKILL.md` warnings once the
    /// app-server RPC finishes. User-initiated skills refreshes still use the blocking app command path so
    /// callers that explicitly asked for fresh skill state do not race ahead of their own refresh.
    pub(super) fn refresh_startup_skills(&mut self, app_server: &AppServerSession) {
        let request_handle = app_server.request_handle();
        let app_event_tx = self.app_event_tx.clone();
        let cwd = self.config.cwd.to_path_buf();
        tokio::spawn(async move {
            let result = fetch_skills_list(request_handle, cwd)
                .await
                .map_err(|err| format!("{err:#}"));
            app_event_tx.send(AppEvent::SkillsListLoaded { result });
        });
    }

    /// Emits the initial hook review warning without delaying the first interactive frame.
    pub(super) fn refresh_startup_hooks(&mut self, app_server: &AppServerSession) {
        let request_handle = app_server.request_handle();
        let app_event_tx = self.app_event_tx.clone();
        let cwd = self.config.cwd.to_path_buf();
        tokio::spawn(async move {
            let result = fetch_hooks_list(request_handle, cwd.clone()).await;
            let response = match result {
                Ok(response) => response,
                Err(err) => {
                    tracing::warn!("failed to load startup hook review state: {err:#}");
                    return;
                }
            };
            let hooks_needing_review = response
                .data
                .into_iter()
                .find(|entry| entry.cwd.as_path() == cwd.as_path())
                .map(|entry| {
                    entry
                        .hooks
                        .into_iter()
                        .filter(|hook| {
                            matches!(
                                hook.trust_status,
                                HookTrustStatus::Untrusted | HookTrustStatus::Modified
                            )
                        })
                        .count()
                })
                .unwrap_or_default();
            if let Some(message) =
                startup_prompts::hooks_needing_review_warning(hooks_needing_review)
            {
                app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
                    history_cell::new_warning_event(message),
                )));
            }
        });
    }

    pub(super) fn fetch_plugins_list(&mut self, app_server: &AppServerSession, cwd: PathBuf) {
        let request_handle = app_server.request_handle();
        let app_event_tx = self.app_event_tx.clone();
        tokio::spawn(async move {
            let result = fetch_plugins_list(request_handle, cwd.clone())
                .await
                .map_err(|err| err.to_string());
            app_event_tx.send(AppEvent::PluginsLoaded { cwd, result });
        });
    }

    pub(super) fn fetch_hooks_list(&mut self, app_server: &AppServerSession, cwd: PathBuf) {
        let request_handle = app_server.request_handle();
        let app_event_tx = self.app_event_tx.clone();
        tokio::spawn(async move {
            let result = fetch_hooks_list(request_handle, cwd.clone())
                .await
                .map_err(|err| err.to_string());
            app_event_tx.send(AppEvent::HooksLoaded { cwd, result });
        });
    }

    pub(super) fn fetch_plugin_detail(
        &mut self,
        app_server: &AppServerSession,
        cwd: PathBuf,
        params: PluginReadParams,
    ) {
        let request_handle = app_server.request_handle();
        let app_event_tx = self.app_event_tx.clone();
        tokio::spawn(async move {
            let result = fetch_plugin_detail(request_handle, params)
                .await
                .map_err(|err| err.to_string());
            app_event_tx.send(AppEvent::PluginDetailLoaded { cwd, result });
        });
    }

    pub(super) fn fetch_marketplace_add(
        &mut self,
        app_server: &AppServerSession,
        cwd: PathBuf,
        source: String,
    ) {
        let request_handle = app_server.request_handle();
        let app_event_tx = self.app_event_tx.clone();
        tokio::spawn(async move {
            let cwd_for_event = cwd.clone();
            let source_for_event = source.clone();
            let result = fetch_marketplace_add(request_handle, cwd, source)
                .await
                .map_err(|err| format!("Failed to add marketplace: {err}"));
            app_event_tx.send(AppEvent::MarketplaceAddLoaded {
                cwd: cwd_for_event,
                source: source_for_event,
                result,
            });
        });
    }

    pub(super) fn fetch_marketplace_remove(
        &mut self,
        app_server: &AppServerSession,
        cwd: PathBuf,
        marketplace_name: String,
        marketplace_display_name: String,
    ) {
        let request_handle = app_server.request_handle();
        let app_event_tx = self.app_event_tx.clone();
        tokio::spawn(async move {
            let cwd_for_event = cwd.clone();
            let marketplace_name_for_event = marketplace_name.clone();
            let result = fetch_marketplace_remove(request_handle, marketplace_name)
                .await
                .map_err(|err| format!("Failed to remove marketplace: {err}"));
            app_event_tx.send(AppEvent::MarketplaceRemoveLoaded {
                cwd: cwd_for_event,
                marketplace_name: marketplace_name_for_event,
                marketplace_display_name,
                result,
            });
        });
    }

    pub(super) fn fetch_marketplace_upgrade(
        &mut self,
        app_server: &AppServerSession,
        cwd: PathBuf,
        marketplace_name: Option<String>,
    ) {
        let request_handle = app_server.request_handle();
        let app_event_tx = self.app_event_tx.clone();
        tokio::spawn(async move {
            let cwd_for_event = cwd.clone();
            let result = fetch_marketplace_upgrade(request_handle, marketplace_name)
                .await
                .map_err(|err| format!("Failed to upgrade marketplace: {err}"));
            app_event_tx.send(AppEvent::MarketplaceUpgradeLoaded {
                cwd: cwd_for_event,
                result,
            });
        });
    }

    pub(super) fn fetch_plugin_install(
        &mut self,
        app_server: &AppServerSession,
        cwd: PathBuf,
        marketplace_path: AbsolutePathBuf,
        plugin_name: String,
        plugin_display_name: String,
    ) {
        let request_handle = app_server.request_handle();
        let app_event_tx = self.app_event_tx.clone();
        tokio::spawn(async move {
            let cwd_for_event = cwd.clone();
            let marketplace_path_for_event = marketplace_path.clone();
            let plugin_name_for_event = plugin_name.clone();
            let result = fetch_plugin_install(request_handle, marketplace_path, plugin_name)
                .await
                .map_err(|err| format!("Failed to install plugin: {err}"));
            app_event_tx.send(AppEvent::PluginInstallLoaded {
                cwd: cwd_for_event,
                marketplace_path: marketplace_path_for_event,
                plugin_name: plugin_name_for_event,
                plugin_display_name,
                result,
            });
        });
    }

    pub(super) fn fetch_plugin_uninstall(
        &mut self,
        app_server: &AppServerSession,
        cwd: PathBuf,
        plugin_id: String,
        plugin_display_name: String,
    ) {
        let request_handle = app_server.request_handle();
        let app_event_tx = self.app_event_tx.clone();
        tokio::spawn(async move {
            let cwd_for_event = cwd.clone();
            let plugin_id_for_event = plugin_id.clone();
            let result = fetch_plugin_uninstall(request_handle, plugin_id)
                .await
                .map_err(|err| format!("Failed to uninstall plugin: {err}"));
            app_event_tx.send(AppEvent::PluginUninstallLoaded {
                cwd: cwd_for_event,
                plugin_id: plugin_id_for_event,
                plugin_display_name,
                result,
            });
        });
    }

    pub(super) fn set_plugin_enabled(
        &mut self,
        app_server: &AppServerSession,
        cwd: PathBuf,
        plugin_id: String,
        enabled: bool,
    ) {
        if let Some(queued_enabled) = self.pending_plugin_enabled_writes.get_mut(&plugin_id) {
            *queued_enabled = Some(enabled);
            return;
        }

        self.pending_plugin_enabled_writes
            .insert(plugin_id.clone(), None);
        self.spawn_plugin_enabled_write(app_server, cwd, plugin_id, enabled);
    }

    pub(super) fn spawn_plugin_enabled_write(
        &mut self,
        app_server: &AppServerSession,
        cwd: PathBuf,
        plugin_id: String,
        enabled: bool,
    ) {
        let request_handle = app_server.request_handle();
        let app_event_tx = self.app_event_tx.clone();
        tokio::spawn(async move {
            let cwd_for_event = cwd.clone();
            let plugin_id_for_event = plugin_id.clone();
            let result = write_plugin_enabled(request_handle, plugin_id, enabled)
                .await
                .map(|_| ())
                .map_err(|err| format!("Failed to update plugin config: {err}"));
            app_event_tx.send(AppEvent::PluginEnabledSet {
                cwd: cwd_for_event,
                plugin_id: plugin_id_for_event,
                enabled,
                result,
            });
        });
    }

    pub(super) fn set_hook_enabled(
        &mut self,
        app_server: &AppServerSession,
        key: String,
        enabled: bool,
    ) {
        if let Some(queued_enabled) = self.pending_hook_enabled_writes.get_mut(&key) {
            *queued_enabled = Some(enabled);
            return;
        }

        self.pending_hook_enabled_writes.insert(key.clone(), None);
        self.spawn_hook_enabled_write(app_server, key, enabled);
    }

    pub(super) fn spawn_hook_enabled_write(
        &mut self,
        app_server: &AppServerSession,
        key: String,
        enabled: bool,
    ) {
        let request_handle = app_server.request_handle();
        let app_event_tx = self.app_event_tx.clone();
        tokio::spawn(async move {
            let key_for_event = key.clone();
            let result = write_hook_enabled(request_handle, key, enabled)
                .await
                .map(|_| ())
                .map_err(|err| format!("Failed to update hook config: {err}"));
            app_event_tx.send(AppEvent::HookEnabledSet {
                key: key_for_event,
                enabled,
                result,
            });
        });
    }

    pub(super) fn trust_hook(
        &mut self,
        app_server: &AppServerSession,
        key: String,
        current_hash: String,
    ) {
        let request_handle = app_server.request_handle();
        let app_event_tx = self.app_event_tx.clone();
        tokio::spawn(async move {
            let result = write_hook_trust(request_handle, key, current_hash)
                .await
                .map(|_| ())
                .map_err(|err| format!("Failed to trust hook: {err}"));
            app_event_tx.send(AppEvent::HookTrusted { result });
        });
    }

    pub(super) fn refresh_plugin_mentions(&mut self) {
        let config = self.config.clone();
        let app_event_tx = self.app_event_tx.clone();
        if !config.features.enabled(Feature::Plugins) {
            app_event_tx.send(AppEvent::PluginMentionsLoaded { plugins: None });
            return;
        }

        tokio::spawn(async move {
            let plugins_input = config.plugins_config_input();
            let plugins = PluginsManager::new(config.codex_home.to_path_buf())
                .plugins_for_config(&plugins_input)
                .await
                .capability_summaries()
                .to_vec();
            app_event_tx.send(AppEvent::PluginMentionsLoaded {
                plugins: Some(plugins),
            });
        });
    }

    pub(super) fn submit_feedback(
        &mut self,
        app_server: &AppServerSession,
        category: FeedbackCategory,
        reason: Option<String>,
        turn_id: Option<String>,
        include_logs: bool,
    ) {
        let request_handle = app_server.request_handle();
        let app_event_tx = self.app_event_tx.clone();
        let origin_thread_id = self.chat_widget.thread_id();
        let rollout_path = if include_logs {
            self.chat_widget.rollout_path()
        } else {
            None
        };
        let params = build_feedback_upload_params(
            origin_thread_id,
            rollout_path,
            category,
            reason,
            turn_id,
            include_logs,
        );
        tokio::spawn(async move {
            let result = fetch_feedback_upload(request_handle, params)
                .await
                .map(|response| response.thread_id)
                .map_err(|err| err.to_string());
            app_event_tx.send(AppEvent::FeedbackSubmitted {
                origin_thread_id,
                category,
                include_logs,
                result,
            });
        });
    }

    pub(super) fn handle_feedback_thread_event(&mut self, event: FeedbackThreadEvent) {
        match event.result {
            Ok(thread_id) => {
                self.chat_widget
                    .add_to_history(crate::bottom_pane::feedback_success_cell(
                        event.category,
                        event.include_logs,
                        &thread_id,
                        event.feedback_audience,
                    ))
            }
            Err(err) => self
                .chat_widget
                .add_to_history(history_cell::new_error_event(format!(
                    "Failed to upload feedback: {err}"
                ))),
        }
    }

    pub(super) async fn enqueue_thread_feedback_event(
        &mut self,
        thread_id: ThreadId,
        event: FeedbackThreadEvent,
    ) {
        let (sender, store) = {
            let channel = self.ensure_thread_channel(thread_id);
            (channel.sender.clone(), Arc::clone(&channel.store))
        };

        let should_send = {
            let mut guard = store.lock().await;
            guard
                .buffer
                .push_back(ThreadBufferedEvent::FeedbackSubmission(event.clone()));
            if guard.buffer.len() > guard.capacity
                && let Some(removed) = guard.buffer.pop_front()
                && let ThreadBufferedEvent::Request(request) = &removed
            {
                guard
                    .pending_interactive_replay
                    .note_evicted_server_request(request);
            }
            guard.active
        };

        if should_send {
            match sender.try_send(ThreadBufferedEvent::FeedbackSubmission(event)) {
                Ok(()) => {}
                Err(TrySendError::Full(event)) => {
                    tokio::spawn(async move {
                        if let Err(err) = sender.send(event).await {
                            tracing::warn!("thread {thread_id} event channel closed: {err}");
                        }
                    });
                }
                Err(TrySendError::Closed(_)) => {
                    tracing::warn!("thread {thread_id} event channel closed");
                }
            }
        }
    }

    pub(super) async fn handle_feedback_submitted(
        &mut self,
        origin_thread_id: Option<ThreadId>,
        category: FeedbackCategory,
        include_logs: bool,
        result: Result<String, String>,
    ) {
        let event = FeedbackThreadEvent {
            category,
            include_logs,
            feedback_audience: self.feedback_audience,
            result,
        };
        if let Some(thread_id) = origin_thread_id {
            self.enqueue_thread_feedback_event(thread_id, event).await;
        } else {
            self.handle_feedback_thread_event(event);
        }
    }

    /// Process the completed MCP inventory fetch: clear the loading spinner, then
    /// render either the full tool/resource listing or an error into chat history.
    ///
    /// When both the local config and the app-server report zero servers, a special
    /// "empty" cell is shown instead of the full table.
    pub(super) fn handle_mcp_inventory_result(
        &mut self,
        result: Result<Vec<McpServerStatus>, String>,
        detail: McpServerStatusDetail,
    ) {
        let config = self.chat_widget.config_ref().clone();
        self.chat_widget.clear_mcp_inventory_loading();
        self.clear_committed_mcp_inventory_loading();

        let statuses = match result {
            Ok(statuses) => statuses,
            Err(err) => {
                self.chat_widget
                    .add_error_message(format!("Failed to load MCP inventory: {err}"));
                return;
            }
        };

        if config.mcp_servers.get().is_empty() && statuses.is_empty() {
            self.chat_widget
                .add_to_history(history_cell::empty_mcp_output());
            return;
        }

        self.chat_widget
            .add_to_history(history_cell::new_mcp_tools_output_from_statuses(
                &config, &statuses, detail,
            ));
    }

    pub(super) fn clear_committed_mcp_inventory_loading(&mut self) {
        let Some(index) = self
            .transcript_cells
            .iter()
            .rposition(|cell| cell.as_any().is::<history_cell::McpInventoryLoadingCell>())
        else {
            return;
        };

        self.transcript_cells.remove(index);
        if let Some(Overlay::Transcript(overlay)) = &mut self.overlay {
            overlay.replace_cells(self.transcript_cells.clone());
        }
    }
}

#[derive(Serialize)]
struct AceAgentState<'a> {
    iteration: usize,
    max_iterations: usize,
    force_finish: bool,
    original_prompt: &'a str,
    conversation_distillation: Option<&'a AceConversationDistillation>,
    context_pack: &'a crate::prompt_pilot::ContextPack,
    observations: &'a [AceAgentObservation],
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct AceConversationDistillation {
    likely_understanding: String,
    concise_summary: String,
    #[serde(default)]
    hard_constraints: Vec<String>,
    #[serde(default)]
    prior_decisions: Vec<String>,
    #[serde(default)]
    mentioned_paths: Vec<String>,
    #[serde(default)]
    search_hints: Vec<String>,
    #[serde(default)]
    uncertainties: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
struct AceAgentObservation {
    action: String,
    summary: String,
    paths: Vec<String>,
    snippets: Vec<AceSnippetObservation>,
}

#[derive(Clone, Debug, Serialize)]
struct AceSnippetObservation {
    path: String,
    content: String,
    truncated: bool,
}

#[derive(Debug, Deserialize)]
struct AceAgentDecision {
    likely_understanding: String,
    action: String,
    reason: String,
    #[serde(default)]
    queries: Vec<String>,
    #[serde(default)]
    paths: Vec<String>,
    enhanced_prompt: String,
}

#[derive(Serialize)]
struct AceGuardrailInput<'a> {
    original_prompt: &'a str,
    conversation_distillation: Option<&'a AceConversationDistillation>,
    context_pack: &'a crate::prompt_pilot::ContextPack,
    candidate_likely_understanding: &'a str,
    candidate_enhanced_prompt: &'a str,
}

#[derive(Debug, Deserialize)]
struct AceGuardrailReview {
    status: String,
    reason: String,
    likely_understanding: String,
    enhanced_prompt: String,
}

async fn run_prompt_pilot_ace_agent_loop(
    request_handle: AppServerRequestHandle,
    thread_id: ThreadId,
    config: Config,
    prompt: String,
    conversation_context: Option<String>,
    request_id: u64,
    app_event_tx: AppEventSender,
) -> Result<PromptPilotPreview, String> {
    let max_iterations = config.prompt_pilot.ace_max_iterations;
    let progress_tx = app_event_tx.clone();
    let mut context_pack = collect_bootstrap_context_pack(&config, &prompt, move |progress| {
        progress_tx.send(AppEvent::PromptPilotEnhanceProgress {
            request_id,
            progress,
        });
    })
    .await;
    let mut observations = Vec::new();
    let mut transcript = Vec::new();
    push_ace_loop_note(
        &mut transcript,
        "Collected project context from docs, rules, manifests, and git state",
    );
    let conversation_distillation = if let Some(conversation_context) = conversation_context {
        send_ace_loop_progress(
            &app_event_tx,
            request_id,
            AceProgressStep::GeneratingEnhancedPrompt,
            &transcript,
            Some("Distilling conversation context".to_string()),
        );
        match fetch_prompt_pilot_ace_conversation_distillation(
            request_handle.clone(),
            thread_id,
            prompt.clone(),
            conversation_context,
        )
        .await
        {
            Ok(distillation) => {
                if conversation_distillation_has_context(&distillation) {
                    push_ace_loop_note(&mut transcript, "Extracted context from the conversation");
                } else {
                    push_ace_loop_note(
                        &mut transcript,
                        "No additional conversation context was needed",
                    );
                }
                Some(distillation)
            }
            Err(err) => {
                tracing::warn!(error = %err, "PromptPilot ACE conversation distillation failed");
                push_ace_loop_note(
                    &mut transcript,
                    "Could not extract conversation context; continuing with project context",
                );
                None
            }
        }
    } else {
        None
    };

    for iteration in 0..max_iterations {
        send_ace_loop_progress(
            &app_event_tx,
            request_id,
            AceProgressStep::GeneratingEnhancedPrompt,
            &transcript,
            Some(format!(
                "Checking whether more context is needed ({}/{})",
                iteration + 1,
                max_iterations
            )),
        );

        let state = AceAgentState {
            iteration,
            max_iterations,
            force_finish: iteration + 1 == max_iterations,
            original_prompt: &prompt,
            conversation_distillation: conversation_distillation.as_ref(),
            context_pack: &context_pack,
            observations: &observations,
        };
        let state_json = serde_json::to_string_pretty(&state).map_err(|err| format!("{err:#}"))?;
        let decision = fetch_prompt_pilot_ace_agent_step(
            request_handle.clone(),
            thread_id,
            prompt.clone(),
            state_json,
        )
        .await?;

        push_ace_loop_note(
            &mut transcript,
            assessment_note_for_action(&decision.action),
        );
        match decision.action.as_str() {
            "finish" => {
                push_ace_loop_note(&mut transcript, "Drafted an enhanced prompt");
                send_ace_loop_progress(
                    &app_event_tx,
                    request_id,
                    AceProgressStep::GeneratingEnhancedPrompt,
                    &transcript,
                    Some("Building final prompt draft".to_string()),
                );
                let enhanced_prompt = decision.enhanced_prompt.trim().to_string();
                if enhanced_prompt.is_empty() {
                    return Err("PromptPilot ACE returned an empty enhanced prompt".to_string());
                }
                let likely_understanding = if decision.likely_understanding.trim().is_empty() {
                    prompt.clone()
                } else {
                    decision.likely_understanding.trim().to_string()
                };
                let preview =
                    PromptPilotPreview::new(prompt, likely_understanding, enhanced_prompt);
                return review_prompt_pilot_ace_guardrails(
                    request_handle,
                    thread_id,
                    &context_pack,
                    conversation_distillation.as_ref(),
                    preview,
                    request_id,
                    &app_event_tx,
                    &transcript,
                )
                .await;
            }
            "search_files" => {
                let repo_root = context_pack_repo_root(&context_pack).to_path_buf();
                let queries = non_empty_limited(decision.queries, 6);
                if queries.is_empty() {
                    observations.push(AceAgentObservation {
                        action: decision.action,
                        summary: "Model requested file search without queries.".to_string(),
                        paths: Vec::new(),
                        snippets: Vec::new(),
                    });
                    push_ace_loop_note(
                        &mut transcript,
                        "Skipped file search because no query was available",
                    );
                    send_ace_loop_progress(
                        &app_event_tx,
                        request_id,
                        AceProgressStep::GeneratingEnhancedPrompt,
                        &transcript,
                        None,
                    );
                    continue;
                }

                push_ace_loop_note(&mut transcript, "Searching the codebase for relevant files");
                send_ace_loop_progress(
                    &app_event_tx,
                    request_id,
                    AceProgressStep::GeneratingEnhancedPrompt,
                    &transcript,
                    Some("Searching project files".to_string()),
                );

                let files = retrieve_relevant_files_for_queries(&repo_root, &queries).await;
                let paths = files
                    .iter()
                    .map(|file| file.path.display().to_string())
                    .collect::<Vec<_>>();
                merge_relevant_files(&mut context_pack.relevant_files, files);
                observations.push(AceAgentObservation {
                    action: decision.action,
                    summary: fallback_reason(&decision.reason),
                    paths,
                    snippets: Vec::new(),
                });
                let observation = observations
                    .last()
                    .expect("search observation should have been recorded");
                push_ace_loop_note(&mut transcript, search_result_note(observation));
                send_ace_loop_progress(
                    &app_event_tx,
                    request_id,
                    AceProgressStep::GeneratingEnhancedPrompt,
                    &transcript,
                    None,
                );
            }
            "read_small_snippets" => {
                let repo_root = context_pack_repo_root(&context_pack).to_path_buf();
                let paths = non_empty_limited(decision.paths, 5);
                push_ace_loop_note(&mut transcript, "Reading snippets from likely files");
                send_ace_loop_progress(
                    &app_event_tx,
                    request_id,
                    AceProgressStep::GeneratingEnhancedPrompt,
                    &transcript,
                    Some("Reading source snippets".to_string()),
                );
                let snippets = read_small_snippets(&repo_root, &paths).await;
                let observed_paths = snippets
                    .iter()
                    .map(|snippet| snippet.path.clone())
                    .collect::<Vec<_>>();
                for path in &observed_paths {
                    add_observed_relevant_file(&mut context_pack.relevant_files, path);
                }
                observations.push(AceAgentObservation {
                    action: decision.action,
                    summary: decision.reason.trim().to_string(),
                    paths: observed_paths,
                    snippets,
                });
                let observation = observations
                    .last()
                    .expect("snippet observation should have been recorded");
                push_ace_loop_note(&mut transcript, snippet_result_note(observation));
                send_ace_loop_progress(
                    &app_event_tx,
                    request_id,
                    AceProgressStep::GeneratingEnhancedPrompt,
                    &transcript,
                    None,
                );
            }
            other => {
                return Err(format!("PromptPilot ACE returned unknown action `{other}`"));
            }
        }
    }

    send_ace_loop_progress(
        &app_event_tx,
        request_id,
        AceProgressStep::GeneratingEnhancedPrompt,
        &transcript,
        Some("Reached iteration limit; drafting from collected context".to_string()),
    );
    let context_json =
        ace_compiler_context_json(&context_pack, conversation_distillation.as_ref())?;
    let preview = fetch_prompt_pilot_enhancement(
        request_handle.clone(),
        thread_id,
        prompt,
        Some(context_json),
    )
    .await
    .map_err(|err| format!("{err:#}"))?;
    review_prompt_pilot_ace_guardrails(
        request_handle,
        thread_id,
        &context_pack,
        conversation_distillation.as_ref(),
        preview,
        request_id,
        &app_event_tx,
        &transcript,
    )
    .await
}

async fn fetch_prompt_pilot_ace_agent_step(
    request_handle: AppServerRequestHandle,
    thread_id: ThreadId,
    prompt: String,
    state_json: String,
) -> Result<AceAgentDecision, String> {
    let preview = fetch_prompt_pilot_enhancement(
        request_handle,
        thread_id,
        prompt,
        Some(format!("{ACE_AGENT_LOOP_CONTEXT_PREFIX}{state_json}")),
    )
    .await
    .map_err(|err| format!("{err:#}"))?;
    serde_json::from_str::<AceAgentDecision>(&preview.enhanced_prompt)
        .map_err(|err| format!("PromptPilot ACE agent response was invalid: {err}"))
}

async fn fetch_prompt_pilot_ace_conversation_distillation(
    request_handle: AppServerRequestHandle,
    thread_id: ThreadId,
    prompt: String,
    conversation_context: String,
) -> Result<AceConversationDistillation, String> {
    #[derive(Serialize)]
    struct ConversationDistillationInput<'a> {
        original_prompt: &'a str,
        recent_conversation: &'a str,
    }

    let input = ConversationDistillationInput {
        original_prompt: &prompt,
        recent_conversation: &conversation_context,
    };
    let input_json = serde_json::to_string_pretty(&input).map_err(|err| format!("{err:#}"))?;
    let preview = fetch_prompt_pilot_enhancement(
        request_handle,
        thread_id,
        prompt,
        Some(format!("{ACE_CONVERSATION_CONTEXT_PREFIX}{input_json}")),
    )
    .await
    .map_err(|err| format!("{err:#}"))?;
    serde_json::from_str::<AceConversationDistillation>(&preview.enhanced_prompt)
        .map_err(|err| format!("PromptPilot ACE conversation response was invalid: {err}"))
}

async fn review_prompt_pilot_ace_guardrails(
    request_handle: AppServerRequestHandle,
    thread_id: ThreadId,
    context_pack: &crate::prompt_pilot::ContextPack,
    conversation_distillation: Option<&AceConversationDistillation>,
    preview: PromptPilotPreview,
    request_id: u64,
    app_event_tx: &AppEventSender,
    transcript: &[String],
) -> Result<PromptPilotPreview, String> {
    send_ace_loop_progress(
        app_event_tx,
        request_id,
        AceProgressStep::ReviewingGuardrails,
        transcript,
        Some("Checking the draft before preview".to_string()),
    );
    let input = AceGuardrailInput {
        original_prompt: &preview.original_prompt,
        conversation_distillation,
        context_pack,
        candidate_likely_understanding: &preview.understanding,
        candidate_enhanced_prompt: &preview.enhanced_prompt,
    };
    let input_json = serde_json::to_string_pretty(&input).map_err(|err| format!("{err:#}"))?;
    let review = fetch_prompt_pilot_ace_guardrail_review(
        request_handle,
        thread_id,
        preview.original_prompt.clone(),
        input_json,
    )
    .await?;

    let reviewed_preview = match review.status.as_str() {
        "pass" | "repair" => {
            let likely_understanding = if review.likely_understanding.trim().is_empty() {
                preview.understanding
            } else {
                review.likely_understanding.trim().to_string()
            };
            let enhanced_prompt = if review.enhanced_prompt.trim().is_empty() {
                preview.enhanced_prompt
            } else {
                review.enhanced_prompt.trim().to_string()
            };
            PromptPilotPreview::new(
                preview.original_prompt,
                likely_understanding,
                enhanced_prompt,
            )
        }
        "fail" => {
            return Err(format!(
                "PromptPilot ACE skipped enhancement because semantic guardrails failed: {}",
                fallback_reason(&review.reason)
            ));
        }
        other => {
            return Err(format!(
                "PromptPilot ACE guardrail review returned unknown status `{other}`"
            ));
        }
    };

    if reviewed_preview.enhanced_prompt.trim().is_empty() {
        return Err("PromptPilot ACE guardrail review returned an empty prompt".to_string());
    }
    Ok(reviewed_preview)
}

async fn fetch_prompt_pilot_ace_guardrail_review(
    request_handle: AppServerRequestHandle,
    thread_id: ThreadId,
    prompt: String,
    input_json: String,
) -> Result<AceGuardrailReview, String> {
    let preview = fetch_prompt_pilot_enhancement(
        request_handle,
        thread_id,
        prompt,
        Some(format!("{ACE_GUARDRAIL_CONTEXT_PREFIX}{input_json}")),
    )
    .await
    .map_err(|err| format!("{err:#}"))?;
    serde_json::from_str::<AceGuardrailReview>(&preview.enhanced_prompt)
        .map_err(|err| format!("PromptPilot ACE guardrail response was invalid: {err}"))
}

fn send_ace_loop_progress(
    app_event_tx: &AppEventSender,
    request_id: u64,
    step: AceProgressStep,
    transcript: &[String],
    active_note: Option<String>,
) {
    let mut progress = AceProgress::running(step);
    for note in transcript {
        progress.add_note(note.clone());
    }
    if let Some(active_note) = active_note {
        progress.add_note(active_note);
    }
    app_event_tx.send(AppEvent::PromptPilotEnhanceProgress {
        request_id,
        progress,
    });
}

fn ace_compiler_context_json(
    context_pack: &crate::prompt_pilot::ContextPack,
    conversation_distillation: Option<&AceConversationDistillation>,
) -> Result<String, String> {
    match conversation_distillation {
        Some(conversation_distillation) => serde_json::to_string_pretty(&serde_json::json!({
            "conversation_distillation": conversation_distillation,
            "context_pack": context_pack,
        }))
        .map_err(|err| format!("{err:#}")),
        None => context_pack_json(context_pack).map_err(|err| format!("{err:#}")),
    }
}

fn conversation_distillation_has_context(distillation: &AceConversationDistillation) -> bool {
    !distillation.hard_constraints.is_empty()
        || !distillation.prior_decisions.is_empty()
        || !distillation.mentioned_paths.is_empty()
        || !distillation.search_hints.is_empty()
        || !distillation.uncertainties.is_empty()
}

fn push_ace_loop_note(transcript: &mut Vec<String>, note: impl Into<String>) {
    let note = trim_progress_text(&note.into());
    if transcript.last() != Some(&note) {
        transcript.push(note);
    }
}

fn assessment_note_for_action(action: &str) -> &'static str {
    match action {
        "search_files" => "Assessed context: more file context is needed",
        "read_small_snippets" => "Assessed context: likely files should be inspected",
        "finish" => "Assessed context: enough context to draft",
        _ => "Assessed collected context",
    }
}

fn search_result_note(observation: &AceAgentObservation) -> String {
    if observation.paths.is_empty() {
        return "No matching files found yet".to_string();
    }

    let paths = high_signal_paths(&observation.paths, 3);
    if paths.is_empty() {
        return format!(
            "Found {} candidate file(s); still looking for higher-signal source files",
            observation.paths.len()
        );
    }

    let remaining = observation.paths.len().saturating_sub(paths.len());
    if remaining == 0 {
        format!("Found likely files: {}", paths.join(", "))
    } else {
        format!(
            "Found likely files: {} (+{remaining} more)",
            paths.join(", ")
        )
    }
}

fn snippet_result_note(observation: &AceAgentObservation) -> String {
    if observation.paths.is_empty() {
        return "No snippets were read from the requested files".to_string();
    }

    let paths = display_paths(&observation.paths, 3);
    let remaining = observation.paths.len().saturating_sub(paths.len());
    if remaining == 0 {
        format!("Read snippets from: {}", paths.join(", "))
    } else {
        format!(
            "Read snippets from: {} (+{remaining} more)",
            paths.join(", ")
        )
    }
}

fn high_signal_paths(paths: &[String], limit: usize) -> Vec<String> {
    paths
        .iter()
        .filter(|path| !is_low_signal_display_path(path))
        .take(limit)
        .cloned()
        .collect()
}

fn display_paths(paths: &[String], limit: usize) -> Vec<String> {
    paths.iter().take(limit).cloned().collect()
}

fn is_low_signal_display_path(path: &str) -> bool {
    let path = path.replace('\\', "/").to_ascii_lowercase();
    path.contains("/snapshots/")
        || path.contains("/fixtures/")
        || path.contains("/testdata/")
        || path.contains("/schema/")
        || path.ends_with(".snap")
        || path.ends_with(".snap.new")
}

fn fallback_reason(reason: &str) -> String {
    let reason = reason.trim();
    if reason.is_empty() {
        "no reason provided".to_string()
    } else {
        trim_progress_text(reason)
    }
}

fn trim_progress_text(text: &str) -> String {
    const MAX_PROGRESS_TEXT_CHARS: usize = 220;
    let mut chars = text.chars();
    let trimmed = chars
        .by_ref()
        .take(MAX_PROGRESS_TEXT_CHARS)
        .collect::<String>();
    if chars.next().is_some() {
        format!("{trimmed}...")
    } else {
        trimmed
    }
}

fn lines_to_plain_text(lines: Vec<ratatui::text::Line<'static>>) -> String {
    lines
        .into_iter()
        .map(|line| {
            line.spans
                .into_iter()
                .map(|span| span.content.into_owned())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let truncated = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{truncated}\n[conversation truncated]")
    } else {
        truncated
    }
}

fn non_empty_limited(values: Vec<String>, limit: usize) -> Vec<String> {
    values
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .take(limit)
        .collect()
}

fn merge_relevant_files(
    existing: &mut Vec<crate::prompt_pilot::RelevantFileBlock>,
    files: Vec<crate::prompt_pilot::RelevantFileBlock>,
) {
    for file in files {
        if let Some(current) = existing
            .iter_mut()
            .find(|current| current.path == file.path)
        {
            current.confidence = current.confidence.max(file.confidence);
            for evidence in file.evidence {
                if !current.evidence.contains(&evidence) {
                    current.evidence.push(evidence);
                }
            }
        } else {
            existing.push(file);
        }
    }
}

fn add_observed_relevant_file(
    relevant_files: &mut Vec<crate::prompt_pilot::RelevantFileBlock>,
    path: &str,
) {
    let path = PathBuf::from(path);
    if let Some(file) = relevant_files.iter_mut().find(|file| file.path == path) {
        let evidence = "small snippet read by ACE agent".to_string();
        if !file.evidence.contains(&evidence) {
            file.evidence.push(evidence);
        }
        file.confidence = file.confidence.max(0.65);
        return;
    }

    relevant_files.push(crate::prompt_pilot::RelevantFileBlock {
        path,
        reason: "small snippet read by ACE agent".to_string(),
        confidence: 0.65,
        evidence: vec!["small snippet read by ACE agent".to_string()],
    });
}

async fn read_small_snippets(repo_root: &Path, paths: &[String]) -> Vec<AceSnippetObservation> {
    let mut snippets = Vec::new();
    for path in paths {
        let Some(relative) = safe_relative_path(path) else {
            continue;
        };
        let absolute_path = repo_root.join(&relative);
        let Ok(absolute) = AbsolutePathBuf::from_absolute_path(&absolute_path) else {
            continue;
        };
        let Ok(mut bytes) = LOCAL_FS.read_file(&absolute, /*sandbox*/ None).await else {
            continue;
        };
        if bytes.contains(&0) {
            continue;
        }
        let truncated = bytes.len() > MAX_ACE_SNIPPET_BYTES;
        if truncated {
            bytes.truncate(MAX_ACE_SNIPPET_BYTES);
        }
        snippets.push(AceSnippetObservation {
            path: relative_path(&absolute_path, repo_root)
                .display()
                .to_string(),
            content: String::from_utf8_lossy(&bytes).trim().to_string(),
            truncated,
        });
    }
    snippets
}

fn safe_relative_path(path: &str) -> Option<PathBuf> {
    let path = path.trim_matches(|ch| matches!(ch, '`' | '"' | '\'' | ' '));
    if path.is_empty() {
        return None;
    }
    let path = PathBuf::from(path);
    if path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return None;
    }
    Some(path)
}

pub(super) async fn fetch_all_mcp_server_statuses(
    request_handle: AppServerRequestHandle,
    detail: McpServerStatusDetail,
) -> Result<Vec<McpServerStatus>> {
    let mut cursor = None;
    let mut statuses = Vec::new();

    loop {
        let request_id = RequestId::String(format!("mcp-inventory-{}", Uuid::new_v4()));
        let response: ListMcpServerStatusResponse = request_handle
            .request_typed(ClientRequest::McpServerStatusList {
                request_id,
                params: ListMcpServerStatusParams {
                    cursor: cursor.clone(),
                    limit: Some(100),
                    detail: Some(detail),
                },
            })
            .await
            .wrap_err("mcpServerStatus/list failed in TUI")?;
        statuses.extend(response.data);
        if let Some(next_cursor) = response.next_cursor {
            cursor = Some(next_cursor);
        } else {
            break;
        }
    }

    Ok(statuses)
}

pub(super) async fn fetch_account_rate_limits(
    request_handle: AppServerRequestHandle,
) -> Result<Vec<RateLimitSnapshot>> {
    let request_id = RequestId::String(format!("account-rate-limits-{}", Uuid::new_v4()));
    let response: GetAccountRateLimitsResponse = request_handle
        .request_typed(ClientRequest::GetAccountRateLimits {
            request_id,
            params: None,
        })
        .await
        .wrap_err("account/rateLimits/read failed in TUI")?;

    Ok(app_server_rate_limit_snapshots(response))
}

pub(super) async fn fetch_prompt_pilot_enhancement(
    request_handle: AppServerRequestHandle,
    thread_id: ThreadId,
    prompt: String,
    context_pack_json: Option<String>,
) -> Result<PromptPilotPreview> {
    let request_id = RequestId::String(format!("prompt-pilot-{}", Uuid::new_v4()));
    let response: ThreadPromptEnhanceResponse = request_handle
        .request_typed(ClientRequest::ThreadPromptEnhance {
            request_id,
            params: ThreadPromptEnhanceParams {
                thread_id: thread_id.to_string(),
                prompt: prompt.clone(),
                context_pack_json,
            },
        })
        .await
        .wrap_err("thread/prompt/enhance failed in TUI")?;

    Ok(PromptPilotPreview::new(
        prompt,
        response.likely_understanding,
        response.enhanced_prompt,
    ))
}

pub(super) async fn send_add_credits_nudge_email(
    request_handle: AppServerRequestHandle,
    credit_type: AddCreditsNudgeCreditType,
) -> Result<codex_app_server_protocol::AddCreditsNudgeEmailStatus> {
    let request_id = RequestId::String(format!("add-credits-nudge-{}", Uuid::new_v4()));
    let response: codex_app_server_protocol::SendAddCreditsNudgeEmailResponse = request_handle
        .request_typed(ClientRequest::SendAddCreditsNudgeEmail {
            request_id,
            params: SendAddCreditsNudgeEmailParams { credit_type },
        })
        .await
        .wrap_err("account/sendAddCreditsNudgeEmail failed in TUI")?;

    Ok(response.status)
}

pub(super) async fn fetch_skills_list(
    request_handle: AppServerRequestHandle,
    cwd: PathBuf,
) -> Result<SkillsListResponse> {
    let request_id = RequestId::String(format!("startup-skills-list-{}", Uuid::new_v4()));
    // Use the cloneable request handle so startup can issue this RPC from a background task without
    // extending a borrow of `AppServerSession` across the first frame render.
    request_handle
        .request_typed(ClientRequest::SkillsList {
            request_id,
            params: SkillsListParams {
                cwds: vec![cwd],
                force_reload: true,
                per_cwd_extra_user_roots: None,
            },
        })
        .await
        .wrap_err("skills/list failed in TUI")
}

pub(super) async fn fetch_plugins_list(
    request_handle: AppServerRequestHandle,
    cwd: PathBuf,
) -> Result<PluginListResponse> {
    let cwd = AbsolutePathBuf::try_from(cwd).wrap_err("plugin list cwd must be absolute")?;
    let request_id = RequestId::String(format!("plugin-list-{}", Uuid::new_v4()));
    let mut response = request_handle
        .request_typed(ClientRequest::PluginList {
            request_id,
            params: PluginListParams {
                cwds: Some(vec![cwd]),
                marketplace_kinds: None,
            },
        })
        .await
        .wrap_err("plugin/list failed in TUI")?;
    hide_cli_only_plugin_marketplaces(&mut response);
    Ok(response)
}

pub(super) async fn fetch_hooks_list(
    request_handle: AppServerRequestHandle,
    cwd: PathBuf,
) -> Result<HooksListResponse> {
    let request_id = RequestId::String(format!("hooks-list-{}", Uuid::new_v4()));
    request_handle
        .request_typed(ClientRequest::HooksList {
            request_id,
            params: HooksListParams { cwds: vec![cwd] },
        })
        .await
        .wrap_err("hooks/list failed in TUI")
}

const CLI_HIDDEN_PLUGIN_MARKETPLACES: &[&str] = &["openai-bundled"];

pub(super) fn hide_cli_only_plugin_marketplaces(response: &mut PluginListResponse) {
    response
        .marketplaces
        .retain(|marketplace| !CLI_HIDDEN_PLUGIN_MARKETPLACES.contains(&marketplace.name.as_str()));
}

pub(super) async fn fetch_plugin_detail(
    request_handle: AppServerRequestHandle,
    params: PluginReadParams,
) -> Result<PluginReadResponse> {
    let request_id = RequestId::String(format!("plugin-read-{}", Uuid::new_v4()));
    request_handle
        .request_typed(ClientRequest::PluginRead { request_id, params })
        .await
        .wrap_err("plugin/read failed in TUI")
}

pub(super) async fn fetch_marketplace_add(
    request_handle: AppServerRequestHandle,
    cwd: PathBuf,
    source: String,
) -> Result<MarketplaceAddResponse> {
    let cwd = AbsolutePathBuf::try_from(cwd).wrap_err("marketplace/add cwd must be absolute")?;
    let source = marketplace_add_source_for_request(cwd.as_path(), source);
    let request_id = RequestId::String(format!("marketplace-add-{}", Uuid::new_v4()));
    request_handle
        .request_typed(ClientRequest::MarketplaceAdd {
            request_id,
            params: MarketplaceAddParams {
                source,
                ref_name: None,
                sparse_paths: None,
            },
        })
        .await
        .wrap_err("marketplace/add failed in TUI")
}

fn marketplace_add_source_for_request(cwd: &std::path::Path, source: String) -> String {
    let (base_source, suffix) = if let Some((base, ref_name)) = source.rsplit_once('#') {
        (base, Some(format!("#{ref_name}")))
    } else if let Some((base, ref_name)) = source.rsplit_once('@') {
        (base, Some(format!("@{ref_name}")))
    } else {
        (source.as_str(), None)
    };

    if matches!(base_source, "." | "..")
        || base_source.starts_with("./")
        || base_source.starts_with("../")
        || base_source.starts_with(".\\")
        || base_source.starts_with("..\\")
    {
        let mut resolved = AbsolutePathBuf::resolve_path_against_base(base_source, cwd)
            .to_string_lossy()
            .into_owned();
        if let Some(suffix) = suffix {
            resolved.push_str(&suffix);
        }
        return resolved;
    }

    source
}

pub(super) async fn fetch_marketplace_remove(
    request_handle: AppServerRequestHandle,
    marketplace_name: String,
) -> Result<MarketplaceRemoveResponse> {
    let request_id = RequestId::String(format!("marketplace-remove-{}", Uuid::new_v4()));
    request_handle
        .request_typed(ClientRequest::MarketplaceRemove {
            request_id,
            params: MarketplaceRemoveParams { marketplace_name },
        })
        .await
        .wrap_err("marketplace/remove failed in TUI")
}

pub(super) async fn fetch_marketplace_upgrade(
    request_handle: AppServerRequestHandle,
    marketplace_name: Option<String>,
) -> Result<MarketplaceUpgradeResponse> {
    let request_id = RequestId::String(format!("marketplace-upgrade-{}", Uuid::new_v4()));
    request_handle
        .request_typed(ClientRequest::MarketplaceUpgrade {
            request_id,
            params: MarketplaceUpgradeParams { marketplace_name },
        })
        .await
        .wrap_err("marketplace/upgrade failed in TUI")
}
pub(super) async fn fetch_plugin_install(
    request_handle: AppServerRequestHandle,
    marketplace_path: AbsolutePathBuf,
    plugin_name: String,
) -> Result<PluginInstallResponse> {
    let request_id = RequestId::String(format!("plugin-install-{}", Uuid::new_v4()));
    request_handle
        .request_typed(ClientRequest::PluginInstall {
            request_id,
            params: PluginInstallParams {
                marketplace_path: Some(marketplace_path),
                remote_marketplace_name: None,
                plugin_name,
            },
        })
        .await
        .wrap_err("plugin/install failed in TUI")
}

pub(super) async fn fetch_plugin_uninstall(
    request_handle: AppServerRequestHandle,
    plugin_id: String,
) -> Result<PluginUninstallResponse> {
    let request_id = RequestId::String(format!("plugin-uninstall-{}", Uuid::new_v4()));
    request_handle
        .request_typed(ClientRequest::PluginUninstall {
            request_id,
            params: PluginUninstallParams { plugin_id },
        })
        .await
        .wrap_err("plugin/uninstall failed in TUI")
}

pub(super) async fn write_plugin_enabled(
    request_handle: AppServerRequestHandle,
    plugin_id: String,
    enabled: bool,
) -> Result<ConfigWriteResponse> {
    let request_id = RequestId::String(format!("plugin-enable-{}", Uuid::new_v4()));
    request_handle
        .request_typed(ClientRequest::ConfigValueWrite {
            request_id,
            params: ConfigValueWriteParams {
                key_path: format!("plugins.{plugin_id}"),
                value: serde_json::json!({ "enabled": enabled }),
                merge_strategy: MergeStrategy::Upsert,
                file_path: None,
                expected_version: None,
            },
        })
        .await
        .wrap_err("config/value/write failed while updating plugin enablement in TUI")
}

pub(super) async fn write_hook_enabled(
    request_handle: AppServerRequestHandle,
    key: String,
    enabled: bool,
) -> Result<ConfigWriteResponse> {
    let request_id = RequestId::String(format!("hooks-config-write-{}", Uuid::new_v4()));
    request_handle
        .request_typed(ClientRequest::ConfigBatchWrite {
            request_id,
            params: ConfigBatchWriteParams {
                edits: vec![codex_app_server_protocol::ConfigEdit {
                    key_path: "hooks.state".to_string(),
                    value: serde_json::json!({
                        key: {
                            "enabled": enabled,
                        }
                    }),
                    merge_strategy: MergeStrategy::Upsert,
                }],
                file_path: None,
                expected_version: None,
                reload_user_config: true,
            },
        })
        .await
        .wrap_err("config/batchWrite failed while updating hook enablement in TUI")
}

pub(super) async fn write_hook_trust(
    request_handle: AppServerRequestHandle,
    key: String,
    current_hash: String,
) -> Result<ConfigWriteResponse> {
    let request_id = RequestId::String(format!("hooks-config-write-{}", Uuid::new_v4()));
    let value = serde_json::json!({
        key: {
            "trusted_hash": current_hash,
        }
    });
    request_handle
        .request_typed(ClientRequest::ConfigBatchWrite {
            request_id,
            params: ConfigBatchWriteParams {
                edits: vec![codex_app_server_protocol::ConfigEdit {
                    key_path: "hooks.state".to_string(),
                    value,
                    merge_strategy: MergeStrategy::Upsert,
                }],
                file_path: None,
                expected_version: None,
                reload_user_config: true,
            },
        })
        .await
        .wrap_err("config/batchWrite failed while updating hook trust in TUI")
}

pub(super) fn build_feedback_upload_params(
    origin_thread_id: Option<ThreadId>,
    rollout_path: Option<PathBuf>,
    category: FeedbackCategory,
    reason: Option<String>,
    turn_id: Option<String>,
    include_logs: bool,
) -> FeedbackUploadParams {
    let extra_log_files = if include_logs {
        rollout_path.map(|rollout_path| vec![rollout_path])
    } else {
        None
    };
    let tags = turn_id.map(|turn_id| BTreeMap::from([(String::from("turn_id"), turn_id)]));
    FeedbackUploadParams {
        classification: crate::bottom_pane::feedback_classification(category).to_string(),
        reason,
        thread_id: origin_thread_id.map(|thread_id| thread_id.to_string()),
        include_logs,
        extra_log_files,
        tags,
    }
}

pub(super) async fn fetch_feedback_upload(
    request_handle: AppServerRequestHandle,
    params: FeedbackUploadParams,
) -> Result<FeedbackUploadResponse> {
    let request_id = RequestId::String(format!("feedback-upload-{}", Uuid::new_v4()));
    request_handle
        .request_typed(ClientRequest::FeedbackUpload { request_id, params })
        .await
        .wrap_err("feedback/upload failed in TUI")
}

/// Convert flat `McpServerStatus` responses into the per-server maps used by the
/// in-process MCP subsystem (tools keyed as `mcp__{server}__{tool}`, plus
/// per-server resource/template/auth maps). Test-only because the TUI
/// renders directly from `McpServerStatus` rather than these maps.
#[cfg(test)]
pub(super) type McpInventoryMaps = (
    HashMap<String, codex_protocol::mcp::Tool>,
    HashMap<String, Vec<codex_protocol::mcp::Resource>>,
    HashMap<String, Vec<codex_protocol::mcp::ResourceTemplate>>,
    HashMap<String, McpAuthStatus>,
);

#[cfg(test)]
pub(super) fn mcp_inventory_maps_from_statuses(statuses: Vec<McpServerStatus>) -> McpInventoryMaps {
    let mut tools = HashMap::new();
    let mut resources = HashMap::new();
    let mut resource_templates = HashMap::new();
    let mut auth_statuses = HashMap::new();

    for status in statuses {
        let server_name = status.name;
        auth_statuses.insert(
            server_name.clone(),
            match status.auth_status {
                codex_app_server_protocol::McpAuthStatus::Unsupported => McpAuthStatus::Unsupported,
                codex_app_server_protocol::McpAuthStatus::NotLoggedIn => McpAuthStatus::NotLoggedIn,
                codex_app_server_protocol::McpAuthStatus::BearerToken => McpAuthStatus::BearerToken,
                codex_app_server_protocol::McpAuthStatus::OAuth => McpAuthStatus::OAuth,
            },
        );
        resources.insert(server_name.clone(), status.resources);
        resource_templates.insert(server_name.clone(), status.resource_templates);
        for (tool_name, tool) in status.tools {
            tools.insert(format!("mcp__{server_name}__{tool_name}"), tool);
        }
    }

    (tools, resources, resource_templates, auth_statuses)
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_app_server_protocol::PluginMarketplaceEntry;
    use codex_protocol::mcp::Tool;
    use codex_utils_absolute_path::AbsolutePathBuf;
    use pretty_assertions::assert_eq;

    fn test_absolute_path(path: &str) -> AbsolutePathBuf {
        AbsolutePathBuf::try_from(PathBuf::from(path)).expect("absolute test path")
    }

    #[test]
    fn marketplace_add_source_for_request_resolves_relative_local_paths() {
        let cwd = if cfg!(windows) {
            PathBuf::from(r"C:\workspace\project")
        } else {
            PathBuf::from("/workspace/project")
        };

        let resolved = marketplace_add_source_for_request(&cwd, "./marketplace".to_string());
        assert!(std::path::Path::new(&resolved).is_absolute());
        assert_eq!(resolved, cwd.join("marketplace").display().to_string());
        assert_eq!(
            marketplace_add_source_for_request(&cwd, "./marketplace#main".to_string()),
            format!("{}#main", cwd.join("marketplace").display())
        );
        assert_eq!(
            marketplace_add_source_for_request(&cwd, "owner/repo".to_string()),
            "owner/repo"
        );
        assert_eq!(
            marketplace_add_source_for_request(&cwd, "~/marketplace".to_string()),
            "~/marketplace"
        );
    }

    #[test]
    fn hide_cli_only_plugin_marketplaces_removes_openai_bundled() {
        let mut response = PluginListResponse {
            marketplaces: vec![
                PluginMarketplaceEntry {
                    name: "openai-bundled".to_string(),
                    path: Some(test_absolute_path("/marketplaces/openai-bundled")),
                    interface: None,
                    plugins: Vec::new(),
                },
                PluginMarketplaceEntry {
                    name: "openai-curated".to_string(),
                    path: Some(test_absolute_path("/marketplaces/openai-curated")),
                    interface: None,
                    plugins: Vec::new(),
                },
            ],
            marketplace_load_errors: Vec::new(),
            featured_plugin_ids: Vec::new(),
        };

        hide_cli_only_plugin_marketplaces(&mut response);

        assert_eq!(
            response.marketplaces,
            vec![PluginMarketplaceEntry {
                name: "openai-curated".to_string(),
                path: Some(test_absolute_path("/marketplaces/openai-curated")),
                interface: None,
                plugins: Vec::new(),
            }]
        );
    }

    #[test]
    fn mcp_inventory_maps_prefix_tool_names_by_server() {
        let statuses = vec![
            McpServerStatus {
                name: "docs".to_string(),
                tools: HashMap::from([(
                    "list".to_string(),
                    Tool {
                        description: None,
                        name: "list".to_string(),
                        title: None,
                        input_schema: serde_json::json!({"type": "object"}),
                        output_schema: None,
                        annotations: None,
                        icons: None,
                        meta: None,
                    },
                )]),
                resources: Vec::new(),
                resource_templates: Vec::new(),
                auth_status: codex_app_server_protocol::McpAuthStatus::Unsupported,
            },
            McpServerStatus {
                name: "disabled".to_string(),
                tools: HashMap::new(),
                resources: Vec::new(),
                resource_templates: Vec::new(),
                auth_status: codex_app_server_protocol::McpAuthStatus::Unsupported,
            },
        ];

        let (tools, resources, resource_templates, auth_statuses) =
            mcp_inventory_maps_from_statuses(statuses);
        let mut resource_names = resources.keys().cloned().collect::<Vec<_>>();
        resource_names.sort();
        let mut template_names = resource_templates.keys().cloned().collect::<Vec<_>>();
        template_names.sort();

        assert_eq!(
            tools.keys().cloned().collect::<Vec<_>>(),
            vec!["mcp__docs__list".to_string()]
        );
        assert_eq!(resource_names, vec!["disabled", "docs"]);
        assert_eq!(template_names, vec!["disabled", "docs"]);
        assert_eq!(
            auth_statuses.get("disabled"),
            Some(&McpAuthStatus::Unsupported)
        );
    }

    #[test]
    fn build_feedback_upload_params_includes_thread_id_and_rollout_path() {
        let thread_id = ThreadId::new();
        let rollout_path = PathBuf::from("/tmp/rollout.jsonl");

        let params = build_feedback_upload_params(
            Some(thread_id),
            Some(rollout_path.clone()),
            FeedbackCategory::SafetyCheck,
            Some("needs follow-up".to_string()),
            Some("turn-123".to_string()),
            /*include_logs*/ true,
        );

        assert_eq!(params.classification, "safety_check");
        assert_eq!(params.reason, Some("needs follow-up".to_string()));
        assert_eq!(params.thread_id, Some(thread_id.to_string()));
        assert_eq!(
            params
                .tags
                .as_ref()
                .and_then(|tags| tags.get("turn_id"))
                .map(String::as_str),
            Some("turn-123")
        );
        assert_eq!(params.include_logs, true);
        assert_eq!(params.extra_log_files, Some(vec![rollout_path]));
    }

    #[test]
    fn build_feedback_upload_params_omits_rollout_path_without_logs() {
        let params = build_feedback_upload_params(
            /*origin_thread_id*/ None,
            Some(PathBuf::from("/tmp/rollout.jsonl")),
            FeedbackCategory::GoodResult,
            /*reason*/ None,
            /*turn_id*/ None,
            /*include_logs*/ false,
        );

        assert_eq!(params.classification, "good_result");
        assert_eq!(params.reason, None);
        assert_eq!(params.thread_id, None);
        assert_eq!(params.tags, None);
        assert_eq!(params.include_logs, false);
        assert_eq!(params.extra_log_files, None);
    }
}
