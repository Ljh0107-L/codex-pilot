# Changelog

Codex Pilot release tags use the upstream-compatible Rust release format:

`rust-v<codex-pilot-version>`

For example, `rust-v1.0.0` publishes Codex Pilot `1.0.0`. The OpenAI Codex base for each release is recorded in that release's `Base` section.

## 1.1.0 - 2026-05-08

### Base

- OpenAI Codex base: `0.129.0`
- Codex Pilot version: `1.1.0`
- Release branch: `release/1.1.0`

### Added

- Added PromptPilot ACE, a context-aware prompt enhancement layer for `Ctrl+X`.
- Added `/pilot context on` and `/pilot context off` for session-scoped ACE control.
- Added footer status for `ACE on` / `ACE off`.
- Added read-only project context collection from project docs, `AGENTS.md`, manifests, git status, file search signals, and small source snippets.
- Added an LLM-driven ACE loop for deciding whether to search files, read snippets, or finish the enhanced prompt.
- Added conversation distillation so ACE can use relevant prior chat context before project search.
- Added semantic LLM guardrail review before showing the final preview.
- Added `[prompt_pilot]` options `ace_default_enabled` and `ace_max_iterations`.

### Changed

- Kept ACE disabled by default unless `ace_default_enabled = true` is configured.
- Kept ACE apply behavior composer-only; applying an enhanced prompt still does not submit it.
- Kept `/pilot context on/off` session-scoped and non-persistent.
- Updated PromptPilot progress and preview surfaces to support scrolling.
- Limited terminal mouse capture to PromptPilot scrollable surfaces so normal terminal text selection continues to work.
- Updated README documentation for ACE usage and configuration.

### Not Changed

- Authentication
- Sandboxing
- Tool execution
- File editing logic
- Approval logic
- Model provider logic
- Plan mode
- Core Codex agent behavior

## 1.0.1 - 2026-05-07

### Added

- Added optional `[prompt_pilot]` configuration for a separate OpenAI-compatible chat completions model used only for prompt enhancement.

### Changed

- Changed the npm global command from `codex` to `codex-pilot` so Codex Pilot does not conflict with OpenAI Codex CLI installs.

## 1.0.0 - 2026-05-07

### Base

- OpenAI Codex base: `fad956b08`
- Codex Pilot version: `1.0.0`
- Release branch: `release/1.0.0`

### Added

- Added PromptPilot prompt enhancement in the TUI behind `Ctrl+X`.
- Added a pre-execution preview that shows interpreted intent, the original prompt, and an enhanced prompt.
- Added an apply/cancel flow so enhanced prompts replace the composer draft only after explicit confirmation.

### Changed

- Kept enhanced prompts as a composer-only draft edit; applying a PromptPilot suggestion does not submit the task.
- Updated PromptPilot to use supported `medium` reasoning and keep interpreted intent/enhanced prompts in the original prompt's primary language.
- Prepared npm packaging under `@ljh0107-l/codex-pilot` with scoped native optional package aliases.
- Adjusted fork CI so pull requests do not depend on OpenAI-internal runners, paid macOS xlarge runners, or BuildBuddy credentials.
- Forked the release workflow for unsigned Codex Pilot release artifacts and gated automatic npm publishing behind `PUBLISH_NPM=true`.
- Focused the initial npm release on CLI packages for Linux, macOS, and Windows x64; Windows ARM64 packaging is deferred.

### Not Changed

- Authentication
- Sandboxing
- Tool execution
- File editing logic
- Approval logic
- Model provider logic
- Core Codex agent behavior
