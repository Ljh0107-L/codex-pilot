use super::*;
use pretty_assertions::assert_eq;

fn enable_prompt_pilot(chat: &mut ChatWidget) -> ThreadId {
    let thread_id = ThreadId::new();
    chat.thread_id = Some(thread_id);
    thread_id
}

#[tokio::test]
async fn ctrl_x_starts_prompt_pilot_enhancement() {
    let (mut chat, mut rx, mut op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    let thread_id = enable_prompt_pilot(&mut chat);
    chat.set_composer_text("fix login bug".to_string(), Vec::new(), Vec::new());

    chat.handle_key_event(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL));

    assert_eq!(
        chat.bottom_pane.active_view_id(),
        Some("prompt_pilot_loading")
    );
    assert_matches!(
        rx.try_recv(),
        Ok(AppEvent::PromptPilotEnhance {
            thread_id: event_thread_id,
            request_id: 0,
            prompt,
            context_aware: false,
        }) if event_thread_id == thread_id && prompt == "fix login bug"
    );
    assert!(op_rx.try_recv().is_err());
}

#[tokio::test]
async fn ctrl_x_uses_ace_when_session_context_is_on() {
    let (mut chat, mut rx, mut op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    let thread_id = enable_prompt_pilot(&mut chat);
    chat.set_prompt_pilot_ace_enabled(/*enabled*/ true);
    chat.set_composer_text("fix login bug".to_string(), Vec::new(), Vec::new());

    chat.handle_key_event(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL));

    assert_eq!(
        chat.bottom_pane.active_view_id(),
        Some("prompt_pilot_loading")
    );
    assert_matches!(
        rx.try_recv(),
        Ok(AppEvent::PromptPilotEnhance {
            thread_id: event_thread_id,
            request_id: 0,
            prompt,
            context_aware: true,
        }) if event_thread_id == thread_id && prompt == "fix login bug"
    );
    assert!(op_rx.try_recv().is_err());
}

#[tokio::test]
async fn prompt_pilot_apply_replaces_composer_without_submitting() {
    let (mut chat, _rx, mut op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    enable_prompt_pilot(&mut chat);
    chat.set_composer_text("fix login bug".to_string(), Vec::new(), Vec::new());

    chat.handle_key_event(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL));
    chat.on_prompt_pilot_enhance_result(
        0,
        Ok(PromptPilotPreview::new(
            "fix login bug".to_string(),
            "Codex should investigate and fix the login bug.".to_string(),
            "Investigate and fix the login bug in this codebase.".to_string(),
        )),
    );
    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert_eq!(chat.bottom_pane.active_view_id(), None);
    assert_eq!(
        chat.composer_text_with_pending(),
        "Investigate and fix the login bug in this codebase."
    );
    assert!(op_rx.try_recv().is_err());
}

#[tokio::test]
async fn prompt_pilot_cancel_preserves_original_composer() {
    let (mut chat, _rx, mut op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    enable_prompt_pilot(&mut chat);
    chat.set_composer_text("fix login bug".to_string(), Vec::new(), Vec::new());

    chat.handle_key_event(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL));
    chat.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

    assert_eq!(chat.bottom_pane.active_view_id(), None);
    assert_eq!(chat.composer_text_with_pending(), "fix login bug");
    assert!(op_rx.try_recv().is_err());
}

#[tokio::test]
async fn prompt_pilot_ace_failure_restores_original_composer() {
    let (mut chat, mut rx, mut op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    enable_prompt_pilot(&mut chat);
    chat.set_prompt_pilot_ace_enabled(/*enabled*/ true);
    chat.set_composer_text("fix login bug".to_string(), Vec::new(), Vec::new());

    chat.handle_key_event(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL));
    chat.on_prompt_pilot_enhance_result(
        0,
        Err("PromptPilot ACE skipped enhancement because guardrails failed.".to_string()),
    );

    assert_eq!(chat.bottom_pane.active_view_id(), None);
    assert_eq!(chat.composer_text_with_pending(), "fix login bug");
    assert!(op_rx.try_recv().is_err());

    let cells = drain_insert_history(&mut rx);
    assert_eq!(cells.len(), 1);
    assert_chatwidget_snapshot!(
        "prompt_pilot_ace_failure_restores_original_composer",
        lines_to_single_string(&cells[0])
    );
}

#[tokio::test]
async fn prompt_pilot_result_is_ignored_after_cancel() {
    let (mut chat, _rx, mut op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    enable_prompt_pilot(&mut chat);
    chat.set_composer_text("hello".to_string(), Vec::new(), Vec::new());

    chat.handle_key_event(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL));
    chat.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    chat.on_prompt_pilot_enhance_result(
        0,
        Ok(PromptPilotPreview::new(
            "hello".to_string(),
            "Codex should respond to the greeting.".to_string(),
            "hello".to_string(),
        )),
    );

    assert_eq!(chat.bottom_pane.active_view_id(), None);
    assert_eq!(chat.composer_text_with_pending(), "hello");
    assert!(op_rx.try_recv().is_err());
}
