# Changelog

Codex Pilot release tags use this format:

`<openai-codex-version>-pilot.<codex-pilot-version>`

For example, `0.128.0-pilot.1.0.0` means the release is based on OpenAI Codex `0.128.0` and includes Codex Pilot `1.0.0` changes.

## Unreleased

### Base

- OpenAI Codex base: TBD before release
- Codex Pilot target version: `1.0.0`
- Release tag: `<openai-codex-version>-pilot.1.0.0`

### Added

- Added PromptPilot prompt enhancement in the TUI behind `Ctrl+X`.
- Added a pre-execution preview that shows likely model understanding, the original prompt, and an enhanced prompt.
- Added an apply/cancel flow so enhanced prompts replace the composer draft only after explicit confirmation.

### Changed

- Kept enhanced prompts as a composer-only draft edit; applying a PromptPilot suggestion does not submit the task.

### Not Changed

- Authentication
- Sandboxing
- Tool execution
- File editing logic
- Approval logic
- Model provider logic
- Core Codex agent behavior
