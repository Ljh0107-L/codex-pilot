use super::context_pack::ContextPack;
use super::context_pack::path_string;
use std::path::Path;

pub(crate) fn validate_enhanced_prompt(
    context_pack: &ContextPack,
    enhanced_prompt: &str,
) -> Result<(), String> {
    if enhanced_prompt.trim().is_empty() {
        return Err("enhanced prompt is empty".to_string());
    }
    validate_paths(context_pack, enhanced_prompt)?;
    Ok(())
}

fn validate_paths(context_pack: &ContextPack, enhanced_prompt: &str) -> Result<(), String> {
    let allowed_paths = context_pack.allowed_path_strings();
    for path in extract_path_like_tokens(enhanced_prompt) {
        if allowed_paths.contains(&path) {
            continue;
        }
        return Err(format!("enhanced prompt invented path `{path}`"));
    }
    Ok(())
}

fn extract_path_like_tokens(text: &str) -> Vec<String> {
    text.split_whitespace()
        .flat_map(|token| {
            let token = token.trim_matches(is_path_edge_punctuation);
            if token.starts_with("http://") || token.starts_with("https://") {
                return Vec::new();
            }
            token
                .split(|ch| matches!(ch, '：' | '；' | '，' | '、' | ';' | ','))
                .filter_map(|part| {
                    let part = part.trim_matches(is_path_edge_punctuation);
                    let path = Path::new(part);
                    let has_known_extension = path
                        .extension()
                        .and_then(|ext| ext.to_str())
                        .is_some_and(|ext| {
                            matches!(
                                ext,
                                "rs" | "md"
                                    | "toml"
                                    | "json"
                                    | "py"
                                    | "go"
                                    | "ts"
                                    | "tsx"
                                    | "js"
                                    | "jsx"
                                    | "yaml"
                                    | "yml"
                            )
                        });
                    (has_known_extension || looks_like_filesystem_path(part))
                        .then(|| path_string(path))
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

fn looks_like_filesystem_path(part: &str) -> bool {
    if !(part.contains('/') || part.contains('\\')) {
        return false;
    }
    if part.starts_with("./")
        || part.starts_with("../")
        || part.starts_with("~/")
        || part.starts_with('/')
        || part.contains('\\')
    {
        return true;
    }

    let components = part
        .split(['/', '\\'])
        .filter(|component| !component.is_empty())
        .collect::<Vec<_>>();
    components.len() >= 2
        && components
            .iter()
            .all(|component| is_pathish_component(component))
}

fn is_pathish_component(component: &str) -> bool {
    component
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | '@' | '+'))
}

fn is_path_edge_punctuation(ch: char) -> bool {
    matches!(
        ch,
        '`' | '"'
            | '\''
            | ','
            | '.'
            | ':'
            | ';'
            | '('
            | ')'
            | '['
            | ']'
            | '{'
            | '}'
            | '<'
            | '>'
            | '，'
            | '。'
            | '；'
            | '：'
    )
}

#[cfg(test)]
mod tests {
    use super::super::context_pack::BudgetUsage;
    use super::super::context_pack::ContextWarning;
    use super::super::context_pack::GitBlock;
    use super::super::context_pack::IntentBlock;
    use super::super::context_pack::ManifestBlock;
    use super::super::context_pack::ProjectBlock;
    use super::super::context_pack::ProjectDocBlock;
    use super::super::context_pack::RelevantFileBlock;
    use super::super::context_pack::RuleBlock;
    use super::*;
    use pretty_assertions::assert_eq;
    use std::path::PathBuf;

    fn pack() -> ContextPack {
        ContextPack {
            intent: IntentBlock {
                original_prompt: "不要改变 Plan mode，只通过 slash command 开启".to_string(),
                language: "zh".to_string(),
                hard_constraints: vec![
                    "不要改变 Plan mode".to_string(),
                    "只通过 slash command 开启".to_string(),
                ],
                mentioned_paths: vec![PathBuf::from("codex-rs/tui/src/chatwidget.rs")],
                keywords: Vec::new(),
            },
            project: ProjectBlock {
                repo_root: PathBuf::from("/repo"),
                detected_languages: vec!["Rust".to_string()],
                workspace_kind: Some("Cargo workspace".to_string()),
                relevant_modules: Vec::new(),
                test_commands: vec!["cargo test -p codex-tui".to_string()],
                manifests: vec![ManifestBlock {
                    path: PathBuf::from("Cargo.toml"),
                    summary: "Cargo workspace".to_string(),
                }],
            },
            project_docs: vec![ProjectDocBlock {
                path: PathBuf::from("README.md"),
                content: "PromptPilot docs".to_string(),
                truncated: false,
            }],
            rules: vec![RuleBlock {
                path: PathBuf::from("AGENTS.md"),
                summary_or_content: "Keep changes scoped.".to_string(),
                truncated: false,
            }],
            git: Some(GitBlock {
                branch: Some("release/1.1.0".to_string()),
                has_changes: true,
                changed_files: vec![PathBuf::from("codex-rs/tui/src/chatwidget.rs")],
                status_short: String::new(),
                diff_stat: None,
            }),
            relevant_files: vec![RelevantFileBlock {
                path: PathBuf::from("codex-rs/tui/src/chatwidget.rs"),
                reason: "changed".to_string(),
                confidence: 0.8,
                evidence: vec!["changed".to_string()],
            }],
            warnings: vec![ContextWarning {
                message: "none".to_string(),
            }],
            budget: BudgetUsage {
                project_doc_bytes: 0,
                rule_bytes: 0,
                git_bytes: 0,
                relevant_file_count: 1,
            },
        }
    }

    #[test]
    fn empty_prompt_fails() {
        let err = validate_enhanced_prompt(&pack(), "   ")
            .expect_err("empty prompt should fail deterministic guardrails");

        assert_eq!(err, "enhanced prompt is empty");
    }

    #[test]
    fn invented_path_fails() {
        let err = validate_enhanced_prompt(
            &pack(),
            "请在不改变 Plan mode 的前提下，只通过 slash command 开启，并修改 codex-rs/tui/src/unknown.rs。",
        )
        .expect_err("invented path should fail");

        assert_eq!(
            err,
            "enhanced prompt invented path `codex-rs/tui/src/unknown.rs`"
        );
    }

    #[test]
    fn path_with_cjk_label_uses_context_pack_path() {
        validate_enhanced_prompt(
            &pack(),
            "请在不改变 Plan mode 的前提下，只通过 slash command 开启。注意：README.md 已进入上下文，可以作为项目说明参考。",
        )
        .expect("labeled README path should be accepted");
    }

    #[test]
    fn cjk_slash_phrase_is_not_treated_as_path() {
        validate_enhanced_prompt(
            &pack(),
            "请在不改变 Plan mode 的前提下，只通过 slash command 开启。请修复当前项目中的登录/认证相关 bug。",
        )
        .expect("Chinese slash phrase should not be treated as an invented path");
    }

    #[test]
    fn valid_enhanced_prompt_passes() {
        validate_enhanced_prompt(
            &pack(),
            "请实现 ACE 的 session 级开关，只通过 slash command 开启，并保持 Plan mode 不变。优先检查 codex-rs/tui/src/chatwidget.rs 和 Cargo.toml，按现有 TUI 约定保持改动聚焦。",
        )
        .expect("valid prompt should pass");
    }
}
