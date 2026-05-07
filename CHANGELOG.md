# Changelog

Codex Pilot release tags use the upstream-compatible Rust release format:

`rust-v<codex-pilot-version>`

For example, `rust-v1.0.0` publishes Codex Pilot `1.0.0`. The OpenAI Codex base for each release is recorded in that release's `Base` section.

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
