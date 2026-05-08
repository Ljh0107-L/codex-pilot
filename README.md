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

## Installation

```bash
npm install -g @ljh0107-l/codex-pilot
```

Run the CLI with:

```bash
codex-pilot
```

## PromptPilot features

- `Ctrl+X` prompt enhancement
- interpreted intent preview
- enhanced prompt preview before execution
- apply/cancel flow that edits the composer draft without submitting
- language-aware output that keeps interpreted intent and enhanced prompts in the original prompt's primary language
- optional ACE context-aware enhancement that can use lightweight project context

### Custom PromptPilot model

By default, PromptPilot uses the active Codex session model. To use a separate OpenAI-compatible chat completions model for prompt enhancement, add:

```toml
[prompt_pilot]
model = "your-optimizer-model"
base_url = "https://your-openai-compatible-endpoint/v1"
api_key_env = "YOUR_OPTIMIZER_API_KEY"
ace_default_enabled = false
ace_max_iterations = 5
```

`base_url` defaults to `https://api.openai.com/v1` when omitted. 

`api_key_env` defaults to`OPENAI_API_KEY`, set it to an empty string for local proxies that do not require Authorization.

`ace_default_enabled` controls whether new TUI sessions start with ACE on. It defaults to false.

`/pilot context on` and `/pilot context off` still only affect the current session.

`ace_max_iterations` controls how many read-only context refinement passes ACE can use before it must produce a prompt draft. It defaults to 5 and is clamped to 1-20.

### ACE context

PromptPilot ACE is a context-aware prompt enhancement layer. It is off by default unless `ace_default_enabled = true` is set, and command changes only affect the current TUI session.

```text
/pilot context on
/pilot context off
```

When ACE is on, `Ctrl+X` enhances the current composer draft with lightweight project context such as project docs, `AGENTS.md`, manifests, git status, and relevant file signals.

Apply only replaces the composer draft, it does not submit the prompt. 

ACE does not modify files or run commands whileenhancing the draft.

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

## Friendly Links

- [LINUX DO](https://linux.do)
