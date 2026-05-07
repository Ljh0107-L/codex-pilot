# Codex Pilot

An unofficial OpenAI Codex CLI fork with Augment-style prompt enhancement.

> This project is not affiliated with OpenAI.  
> Codex Pilot is based on OpenAI Codex CLI and intends to keep the core Codex CLI behavior intact.  
> The planned product change is a minimal prompt-enhancement layer in the TUI.

## What is Codex Pilot?

Codex Pilot is an experimental fork of OpenAI Codex CLI.

The goal is to add a lightweight PromptPilot experience to the Codex terminal UI:

- show how Codex is likely to understand your prompt
- rewrite rough instructions into clearer coding-agent prompts
- preview the enhanced prompt before execution
- reduce vague or overly broad agent runs

Codex Pilot is not a replacement for OpenAI Codex CLI. It is a small fork focused on prompt enhancement UX.

## Planned PromptPilot features

- `Ctrl+P` prompt enhancement
- model understanding preview
- enhanced prompt preview before execution

## What this fork does not change

Codex Pilot intends not to modify:

- authentication
- sandboxing
- tool execution
- file editing logic
- approval logic
- model provider logic
- core Codex agent behavior

PromptPilot changes should stay limited to the TUI prompt input and preview experience.