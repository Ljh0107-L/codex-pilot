#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AceProgressStep {
    GeneratingEnhancedPrompt,
    ReviewingGuardrails,
}

impl AceProgressStep {
    pub(crate) const ORDER: [Self; 2] = [Self::GeneratingEnhancedPrompt, Self::ReviewingGuardrails];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::GeneratingEnhancedPrompt => "generating enhanced prompt",
            Self::ReviewingGuardrails => "reviewing guardrails",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AceProgress {
    completed: Vec<AceProgressStep>,
    current: Option<AceProgressStep>,
    notes: Vec<String>,
}

impl AceProgress {
    pub(crate) fn new() -> Self {
        Self {
            completed: Vec::new(),
            current: None,
            notes: Vec::new(),
        }
    }

    pub(crate) fn running(step: AceProgressStep) -> Self {
        let mut progress = Self::new();
        for ordered_step in AceProgressStep::ORDER {
            if ordered_step == step {
                progress.start(step);
                break;
            }
            progress.complete(ordered_step);
        }
        progress
    }

    #[cfg(test)]
    pub(crate) fn building_prompt() -> Self {
        Self::running(AceProgressStep::GeneratingEnhancedPrompt)
    }

    pub(crate) fn add_note(&mut self, note: impl Into<String>) {
        let note = note.into();
        if !note.trim().is_empty() {
            self.notes.push(note);
        }
    }

    pub(crate) fn start(&mut self, step: AceProgressStep) {
        if !self.completed.contains(&step) {
            self.current = Some(step);
        }
    }

    pub(crate) fn complete(&mut self, step: AceProgressStep) {
        if !self.completed.contains(&step) {
            self.completed.push(step);
        }
        if self.current == Some(step) {
            self.current = None;
        }
    }

    pub(crate) fn is_complete(&self, step: AceProgressStep) -> bool {
        self.completed.contains(&step)
    }

    pub(crate) fn current(&self) -> Option<AceProgressStep> {
        self.current
    }

    pub(crate) fn notes(&self) -> &[String] {
        &self.notes
    }
}
