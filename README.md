# Codex Pilot

An unofficial OpenAI Codex CLI fork with PromptPilot prompt enhancement.

> This project is not affiliated with OpenAI.  
> Codex Pilot is based on OpenAI Codex CLI and intends to keep the core Codex CLI behavior intact.  
> The product change is a minimal prompt-enhancement layer in the TUI.

## What is Codex Pilot?

Codex Pilot is an experimental fork of OpenAI Codex CLI.

The goal is to provide a lightweight PromptPilot experience in the Codex terminal UI:

- show how Codex is likely to understand your prompt
- rewrite rough instructions into clearer coding-agent prompts
- preview the enhanced prompt before execution
- reduce vague or overly broad agent runs

Codex Pilot is not a replacement for OpenAI Codex CLI. It is a small fork focused on prompt enhancement UX.

## PromptPilot features

- `Ctrl+X` prompt enhancement
- interpreted intent preview
- enhanced prompt preview before execution
- apply/cancel flow that edits the composer draft without submitting
- language-aware output that keeps interpreted intent and enhanced prompts in the original prompt's primary language

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
