use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use crossterm::event::MouseEvent;
use crossterm::event::MouseEventKind;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;

use crate::prompt_pilot::AceProgress;
use crate::prompt_pilot::AceProgressStep;
use crate::render::renderable::Renderable;

use super::CancellationEvent;
use super::bottom_pane_view::BottomPaneView;
use super::bottom_pane_view::ViewCompletion;
use super::selection_popup_common::menu_surface_inset;
use super::selection_popup_common::menu_surface_padding_height;
use super::selection_popup_common::render_menu_surface;

pub(crate) const LOADING_VIEW_ID: &str = "prompt_pilot_loading";
pub(crate) const PREVIEW_VIEW_ID: &str = "prompt_pilot_preview";
const MIN_HEIGHT: u16 = 8;
const MAX_HEIGHT: u16 = 28;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PromptPilotPreview {
    pub(crate) original_prompt: String,
    pub(crate) understanding: String,
    pub(crate) enhanced_prompt: String,
}

impl PromptPilotPreview {
    pub(crate) fn new(
        original_prompt: String,
        understanding: String,
        enhanced_prompt: String,
    ) -> Self {
        Self {
            original_prompt,
            understanding,
            enhanced_prompt,
        }
    }
}

#[cfg(test)]
fn enhance_prompt(prompt: &str) -> PromptPilotPreview {
    let original_prompt = prompt.trim().to_string();
    let (understanding, enhanced_prompt) = match detect_intent(&original_prompt) {
        PromptIntent::KeepOriginal => keep_original_prompt(&original_prompt),
        PromptIntent::SummarizeRecentCommits => summarize_recent_commits_prompt(&original_prompt),
        PromptIntent::CodingTask => coding_task_prompt(&original_prompt),
    };

    PromptPilotPreview {
        original_prompt,
        understanding,
        enhanced_prompt,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg(test)]
enum PromptIntent {
    KeepOriginal,
    SummarizeRecentCommits,
    CodingTask,
}

#[cfg(test)]
fn detect_intent(prompt: &str) -> PromptIntent {
    let normalized = prompt.to_ascii_lowercase();
    if is_prompt_clear_or_conversational(prompt, &normalized) {
        return PromptIntent::KeepOriginal;
    }

    let mentions_commit = normalized.contains("commit") || prompt.contains("提交");
    let asks_for_summary = normalized.contains("summarize")
        || normalized.contains("summary")
        || normalized.contains("recent")
        || prompt.contains("总结")
        || prompt.contains("最近");

    if mentions_commit && asks_for_summary {
        PromptIntent::SummarizeRecentCommits
    } else {
        PromptIntent::CodingTask
    }
}

#[cfg(test)]
fn is_prompt_clear_or_conversational(prompt: &str, normalized: &str) -> bool {
    let trimmed = prompt.trim();
    if trimmed.is_empty() {
        return true;
    }

    matches!(
        normalized,
        "hello" | "hi" | "hey" | "thanks" | "thank you" | "你好" | "谢谢"
    ) || trimmed.ends_with('?')
}

#[cfg(test)]
fn keep_original_prompt(original_prompt: &str) -> (String, String) {
    (
        format!(
            "Codex should respond directly to the user's prompt without expanding it into a coding task: \"{original_prompt}\"."
        ),
        original_prompt.to_string(),
    )
}

#[cfg(test)]
fn summarize_recent_commits_prompt(original_prompt: &str) -> (String, String) {
    let understanding = String::from(
        "Codex should review the repository's recent Git history and produce a concise summary of \
         the main changes, notable files, and any follow-up risks or questions.",
    );
    let enhanced_prompt = format!(
        "Task: {original_prompt}\n\n\
         Please summarize the recent Git history for this repository:\n\n\
         1. Inspect recent commits with `git log` and, when useful, `git show` or `git diff`.\n\
         2. Group the changes by theme or component instead of listing every commit mechanically.\n\
         3. Call out notable behavior changes, touched files, and any risks or follow-up questions.\n\
         4. Keep the summary concise and include the commit range or count you reviewed."
    );
    (understanding, enhanced_prompt)
}

#[cfg(test)]
fn coding_task_prompt(original_prompt: &str) -> (String, String) {
    let understanding = format!(
        "Codex should treat this as a focused coding task based on the user's draft: \
         \"{original_prompt}\". The agent should inspect the relevant code first, keep the change \
         scoped, and verify the result with the most relevant local checks."
    );
    let enhanced_prompt = format!(
        "Task: {original_prompt}\n\n\
         Please work through this as a focused coding-agent task:\n\n\
         1. Inspect the relevant code path and summarize the current behavior before editing.\n\
         2. Identify the smallest scoped change that satisfies the task.\n\
         3. Implement the change using existing project conventions.\n\
         4. Add or update focused tests when behavior changes.\n\
         5. Run the most relevant formatter and tests, then report what passed or could not be run."
    );
    (understanding, enhanced_prompt)
}

pub(crate) struct PromptPilotPreviewView {
    preview: PromptPilotPreview,
    completion: Option<ViewCompletion>,
    composer_replacement: Option<String>,
    scroll_top: usize,
}

pub(crate) struct PromptPilotLoadingView {
    original_prompt: String,
    mode: PromptPilotLoadingMode,
    completion: Option<ViewCompletion>,
    scroll_top: usize,
}

enum PromptPilotLoadingMode {
    Standard,
    Ace(AceProgress),
}

impl PromptPilotLoadingView {
    pub(crate) fn new(original_prompt: String) -> Self {
        Self {
            original_prompt,
            mode: PromptPilotLoadingMode::Standard,
            completion: None,
            scroll_top: 0,
        }
    }

    pub(crate) fn new_ace(original_prompt: String) -> Self {
        Self {
            original_prompt,
            mode: PromptPilotLoadingMode::Ace(AceProgress::new()),
            completion: None,
            scroll_top: 0,
        }
    }

    fn update_ace_progress(&mut self, progress: AceProgress) -> bool {
        let PromptPilotLoadingMode::Ace(current) = &mut self.mode else {
            return false;
        };
        if *current == progress {
            return false;
        }
        *current = progress;
        true
    }

    fn lines(&self, width: u16) -> Vec<Line<'static>> {
        match &self.mode {
            PromptPilotLoadingMode::Standard => self.standard_lines(width),
            PromptPilotLoadingMode::Ace(progress) => self.ace_lines(width, progress),
        }
    }

    fn standard_lines(&self, width: u16) -> Vec<Line<'static>> {
        let wrap_width = usize::from(width.max(1));
        let mut lines = vec![Line::from("PromptPilot".bold())];
        lines.push(Line::from(""));
        lines.push(Line::from("Enhancing prompt with Codex".cyan().bold()));
        lines.push(Line::from(
            "Waiting for the model to refine this draft.".dim(),
        ));
        lines.push(Line::from(""));
        lines.push(Line::from("Original prompt".cyan().bold()));
        for wrapped in textwrap::wrap(&self.original_prompt, wrap_width.saturating_sub(2).max(1)) {
            lines.push(Line::from(format!("> {}", wrapped.into_owned()).dim()));
        }
        lines.push(Line::from(""));
        lines.push(vec!["Esc".cyan(), " cancel".into()].into());
        lines
    }

    fn ace_lines(&self, width: u16, progress: &AceProgress) -> Vec<Line<'static>> {
        let wrap_width = usize::from(width.max(1));
        let mut lines = vec![Line::from("PromptPilot ACE".bold())];
        lines.push(Line::from(""));
        for step in AceProgressStep::ORDER {
            lines.push(progress_line(progress, step));
        }
        if !progress.notes().is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from("Context steps".cyan().bold()));
            for note in progress.notes() {
                push_wrapped_bullet(&mut lines, note, wrap_width);
            }
        }
        lines.push(Line::from(""));
        lines.push(Line::from("Original prompt".cyan().bold()));
        for wrapped in textwrap::wrap(&self.original_prompt, wrap_width.saturating_sub(2).max(1)) {
            lines.push(Line::from(format!("> {}", wrapped.into_owned()).dim()));
        }
        lines.push(Line::from(""));
        lines.push(
            vec![
                "Scroll: ".into(),
                "↑/↓".cyan(),
                " ".into(),
                "wheel".cyan(),
                " ".into(),
                "PgUp/PgDn".cyan(),
                "   ".into(),
                "Esc".cyan(),
                " cancel".into(),
            ]
            .into(),
        );
        lines
    }
}

fn push_wrapped_bullet(lines: &mut Vec<Line<'static>>, text: &str, width: usize) {
    let wrap_width = width.saturating_sub(2).max(1);
    for (index, wrapped) in textwrap::wrap(text, wrap_width).into_iter().enumerate() {
        let prefix = if index == 0 { "• " } else { "  " };
        lines.push(Line::from(
            format!("{prefix}{}", wrapped.into_owned()).dim(),
        ));
    }
}

fn progress_line(progress: &AceProgress, step: AceProgressStep) -> Line<'static> {
    if progress.is_complete(step) {
        vec!["✓ ".green(), step.label().green()].into()
    } else if progress.current() == Some(step) {
        vec!["… ".cyan(), step.label().cyan().bold()].into()
    } else {
        vec!["  ".dim(), step.label().dim()].into()
    }
}

impl BottomPaneView for PromptPilotLoadingView {
    fn handle_key_event(&mut self, key_event: KeyEvent) {
        match key_event.code {
            KeyCode::Esc => {
                self.on_ctrl_c();
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.scroll_top = self.scroll_top.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.scroll_top = self.scroll_top.saturating_add(1);
            }
            KeyCode::PageUp => {
                self.scroll_top = self.scroll_top.saturating_sub(8);
            }
            KeyCode::PageDown => {
                self.scroll_top = self.scroll_top.saturating_add(8);
            }
            KeyCode::Home => {
                self.scroll_top = 0;
            }
            KeyCode::End => {
                self.scroll_top = usize::MAX;
            }
            _ => {}
        }
    }

    fn is_complete(&self) -> bool {
        self.completion.is_some()
    }

    fn completion(&self) -> Option<ViewCompletion> {
        self.completion
    }

    fn on_ctrl_c(&mut self) -> CancellationEvent {
        self.completion = Some(ViewCompletion::Cancelled);
        CancellationEvent::Handled
    }

    fn view_id(&self) -> Option<&'static str> {
        Some(LOADING_VIEW_ID)
    }

    fn update_prompt_pilot_ace_progress(&mut self, progress: AceProgress) -> bool {
        self.update_ace_progress(progress)
    }

    fn handle_mouse_event(&mut self, mouse_event: MouseEvent) -> bool {
        match mouse_event.kind {
            MouseEventKind::ScrollUp => {
                self.scroll_top = self.scroll_top.saturating_sub(3);
                true
            }
            MouseEventKind::ScrollDown => {
                self.scroll_top = self.scroll_top.saturating_add(3);
                true
            }
            _ => false,
        }
    }

    fn wants_mouse_capture(&self) -> bool {
        true
    }
}

impl Renderable for PromptPilotLoadingView {
    fn desired_height(&self, width: u16) -> u16 {
        let outer = Rect::new(0, 0, width, u16::MAX);
        let inner = menu_surface_inset(outer);
        let content_height = self.lines(inner.width.max(1)).len() as u16;
        content_height
            .saturating_add(menu_surface_padding_height())
            .clamp(MIN_HEIGHT, MAX_HEIGHT)
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let content_area = render_menu_surface(area, buf);
        if content_area.width == 0 || content_area.height == 0 {
            return;
        }

        let lines = self.lines(content_area.width);
        let visible_height = usize::from(content_area.height);
        let max_scroll = lines.len().saturating_sub(visible_height);
        let scroll_top = self.scroll_top.min(max_scroll);

        for (offset, line) in lines
            .into_iter()
            .skip(scroll_top)
            .take(visible_height)
            .enumerate()
        {
            Paragraph::new(line).render(
                Rect {
                    x: content_area.x,
                    y: content_area.y.saturating_add(offset as u16),
                    width: content_area.width,
                    height: 1,
                },
                buf,
            );
        }
    }
}

impl PromptPilotPreviewView {
    pub(crate) fn new(preview: PromptPilotPreview) -> Self {
        Self {
            preview,
            completion: None,
            composer_replacement: None,
            scroll_top: 0,
        }
    }

    fn apply(&mut self) {
        self.composer_replacement = Some(self.preview.enhanced_prompt.clone());
        self.completion = Some(ViewCompletion::Accepted);
    }

    fn lines(&self, width: u16) -> Vec<Line<'static>> {
        let wrap_width = usize::from(width.max(1));
        let mut lines = vec![Line::from("PromptPilot".bold())];

        lines.push(Line::from(""));
        lines.push(Line::from("Interpreted intent".cyan().bold()));
        for wrapped in textwrap::wrap(&self.preview.understanding, wrap_width) {
            lines.push(Line::from(wrapped.into_owned().dim()));
        }

        lines.push(Line::from(""));
        lines.push(Line::from("Original prompt".cyan().bold()));
        for wrapped in textwrap::wrap(
            &self.preview.original_prompt,
            wrap_width.saturating_sub(2).max(1),
        ) {
            lines.push(Line::from(format!("> {}", wrapped.into_owned()).dim()));
        }

        lines.push(Line::from(""));
        lines.push(Line::from("Enhanced prompt".green().bold()));
        for wrapped in textwrap::wrap(&self.preview.enhanced_prompt, wrap_width) {
            lines.push(Line::from(wrapped.into_owned()));
        }

        lines.push(Line::from(""));
        lines.push(
            vec![
                "Scroll: ".into(),
                "↑/↓".cyan(),
                " ".into(),
                "wheel".cyan(),
                " ".into(),
                "PgUp/PgDn".cyan(),
                "   ".into(),
                "Enter/A".cyan(),
                " apply   ".into(),
                "Esc".cyan(),
                " cancel".into(),
            ]
            .into(),
        );
        lines
    }
}

impl BottomPaneView for PromptPilotPreviewView {
    fn handle_key_event(&mut self, key_event: KeyEvent) {
        match key_event {
            KeyEvent {
                code: KeyCode::Esc, ..
            } => {
                self.on_ctrl_c();
            }
            KeyEvent {
                code: KeyCode::Enter,
                ..
            } => {
                self.apply();
            }
            KeyEvent {
                code: KeyCode::Up | KeyCode::Char('k'),
                ..
            } => {
                self.scroll_top = self.scroll_top.saturating_sub(1);
            }
            KeyEvent {
                code: KeyCode::Down | KeyCode::Char('j'),
                ..
            } => {
                self.scroll_top = self.scroll_top.saturating_add(1);
            }
            KeyEvent {
                code: KeyCode::PageUp,
                ..
            } => {
                self.scroll_top = self.scroll_top.saturating_sub(8);
            }
            KeyEvent {
                code: KeyCode::PageDown,
                ..
            } => {
                self.scroll_top = self.scroll_top.saturating_add(8);
            }
            KeyEvent {
                code: KeyCode::Home,
                ..
            } => {
                self.scroll_top = 0;
            }
            KeyEvent {
                code: KeyCode::End, ..
            } => {
                self.scroll_top = usize::MAX;
            }
            KeyEvent {
                code: KeyCode::Char(c),
                modifiers,
                ..
            } if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                && c.eq_ignore_ascii_case(&'a') =>
            {
                self.apply();
            }
            KeyEvent {
                code: KeyCode::Char(c),
                modifiers,
                ..
            } if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                && (c.eq_ignore_ascii_case(&'c') || c.eq_ignore_ascii_case(&'q')) =>
            {
                self.on_ctrl_c();
            }
            _ => {}
        }
    }

    fn is_complete(&self) -> bool {
        self.completion.is_some()
    }

    fn completion(&self) -> Option<ViewCompletion> {
        self.completion
    }

    fn take_composer_replacement(&mut self) -> Option<String> {
        self.composer_replacement.take()
    }

    fn on_ctrl_c(&mut self) -> CancellationEvent {
        self.completion = Some(ViewCompletion::Cancelled);
        CancellationEvent::Handled
    }

    fn view_id(&self) -> Option<&'static str> {
        Some(PREVIEW_VIEW_ID)
    }

    fn handle_mouse_event(&mut self, mouse_event: MouseEvent) -> bool {
        match mouse_event.kind {
            MouseEventKind::ScrollUp => {
                self.scroll_top = self.scroll_top.saturating_sub(3);
                true
            }
            MouseEventKind::ScrollDown => {
                self.scroll_top = self.scroll_top.saturating_add(3);
                true
            }
            _ => false,
        }
    }

    fn wants_mouse_capture(&self) -> bool {
        true
    }
}

impl Renderable for PromptPilotPreviewView {
    fn desired_height(&self, width: u16) -> u16 {
        let outer = Rect::new(0, 0, width, u16::MAX);
        let inner = menu_surface_inset(outer);
        let content_height = self.lines(inner.width.max(1)).len() as u16;
        content_height
            .saturating_add(menu_surface_padding_height())
            .clamp(MIN_HEIGHT, MAX_HEIGHT)
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let content_area = render_menu_surface(area, buf);
        if content_area.width == 0 || content_area.height == 0 {
            return;
        }

        let lines = self.lines(content_area.width);
        let visible_height = usize::from(content_area.height);
        let max_scroll = lines.len().saturating_sub(visible_height);
        let scroll_top = self.scroll_top.min(max_scroll);

        for (offset, line) in lines
            .into_iter()
            .skip(scroll_top)
            .take(visible_height)
            .enumerate()
        {
            Paragraph::new(line).render(
                Rect {
                    x: content_area.x,
                    y: content_area.y.saturating_add(offset as u16),
                    width: content_area.width,
                    height: 1,
                },
                buf,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;
    use insta::assert_snapshot;
    use pretty_assertions::assert_eq;
    use ratatui::buffer::Buffer;

    fn render_snapshot(view: &dyn Renderable, area: Rect) -> String {
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf);
        let mut lines = Vec::new();
        for y in 0..buf.area().height {
            let mut row = String::new();
            for x in 0..buf.area().width {
                row.push(buf[(x, y)].symbol().chars().next().unwrap_or(' '));
            }
            lines.push(row);
        }
        lines.join("\n")
    }

    #[test]
    fn enhance_prompt_keeps_original_prompt_and_adds_execution_guidance() {
        let preview = enhance_prompt("fix login bug");

        assert_eq!(preview.original_prompt, "fix login bug");
        assert!(preview.understanding.contains("fix login bug"));
        assert!(preview.enhanced_prompt.contains("Task: fix login bug"));
        assert!(
            preview
                .enhanced_prompt
                .contains("Run the most relevant formatter and tests")
        );
    }

    #[test]
    fn summarize_recent_commits_uses_git_history_guidance() {
        let preview = enhance_prompt("Summarize recent commits");

        assert_eq!(preview.original_prompt, "Summarize recent commits");
        assert!(preview.understanding.contains("recent Git history"));
        assert!(preview.enhanced_prompt.contains("git log"));
        assert!(preview.enhanced_prompt.contains("commit range"));
        assert!(!preview.enhanced_prompt.contains("Implement the change"));
    }

    #[test]
    fn conversational_prompt_is_not_enhanced() {
        let preview = enhance_prompt("hello");

        assert_eq!(
            preview,
            PromptPilotPreview {
                original_prompt: "hello".to_string(),
                understanding: "Codex should respond directly to the user's prompt without expanding it into a coding task: \"hello\".".to_string(),
                enhanced_prompt: "hello".to_string(),
            }
        );
    }

    #[test]
    fn enter_accepts_enhanced_prompt() {
        let preview = enhance_prompt("fix login bug");
        let expected = preview.enhanced_prompt.clone();
        let mut view = PromptPilotPreviewView::new(preview);

        view.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert_eq!(view.completion(), Some(ViewCompletion::Accepted));
        assert_eq!(view.take_composer_replacement(), Some(expected));
    }

    #[test]
    fn esc_cancels_without_replacement() {
        let mut view = PromptPilotPreviewView::new(enhance_prompt("fix login bug"));

        view.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

        assert_eq!(view.completion(), Some(ViewCompletion::Cancelled));
        assert_eq!(view.take_composer_replacement(), None);
    }

    #[test]
    fn prompt_pilot_preview_snapshot() {
        let view = PromptPilotPreviewView::new(enhance_prompt("fix login bug"));

        assert_snapshot!(
            "prompt_pilot_preview",
            render_snapshot(
                &view,
                Rect::new(0, 0, 80, view.desired_height(/*width*/ 80))
            )
        );
    }

    #[test]
    fn prompt_pilot_ace_loading_snapshot() {
        let mut view = PromptPilotLoadingView::new_ace("fix login bug".to_string());
        view.update_prompt_pilot_ace_progress(AceProgress::building_prompt());

        assert_snapshot!(
            "prompt_pilot_ace_loading",
            render_snapshot(
                &view,
                Rect::new(0, 0, 80, view.desired_height(/*width*/ 80))
            )
        );
    }
}
