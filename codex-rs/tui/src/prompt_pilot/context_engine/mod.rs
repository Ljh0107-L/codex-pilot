mod context_pack;
#[cfg(test)]
mod guardrails;
mod prompt_compiler;
mod providers;
mod retriever;
mod state;

use crate::legacy_core::config::Config;
use codex_git_utils::get_git_repo_root;

pub(crate) use context_pack::ContextPack;
pub(crate) use context_pack::RelevantFileBlock;
pub(crate) use context_pack::relative_path;
pub(crate) use prompt_compiler::context_pack_json;
pub(crate) use state::AceProgress;
pub(crate) use state::AceProgressStep;

#[cfg(test)]
pub(crate) async fn collect_context_pack<F>(
    config: &Config,
    original_prompt: &str,
    on_progress: F,
) -> ContextPack
where
    F: FnMut(AceProgress) + Send,
{
    collect_context_pack_inner(
        config,
        original_prompt,
        /*include_relevant_files*/ true,
        on_progress,
    )
    .await
}

pub(crate) async fn collect_bootstrap_context_pack<F>(
    config: &Config,
    original_prompt: &str,
    on_progress: F,
) -> ContextPack
where
    F: FnMut(AceProgress) + Send,
{
    collect_context_pack_inner(
        config,
        original_prompt,
        /*include_relevant_files*/ false,
        on_progress,
    )
    .await
}

pub(crate) async fn retrieve_relevant_files_for_queries(
    repo_root: &std::path::Path,
    queries: &[String],
) -> Vec<RelevantFileBlock> {
    retriever::retrieve_relevant_files_for_queries(repo_root, queries).await
}

pub(crate) fn context_pack_repo_root(context_pack: &ContextPack) -> &std::path::Path {
    context_pack.project.repo_root.as_path()
}

async fn collect_context_pack_inner<F>(
    config: &Config,
    original_prompt: &str,
    include_relevant_files: bool,
    mut on_progress: F,
) -> ContextPack
where
    F: FnMut(AceProgress) + Send,
{
    on_progress(AceProgress::running(
        AceProgressStep::GeneratingEnhancedPrompt,
    ));

    let cwd = config.cwd.as_path();
    let repo_root = get_git_repo_root(cwd).unwrap_or_else(|| cwd.to_path_buf());
    let intent = context_pack::IntentBlock::from_prompt(original_prompt, &repo_root);
    let project_docs = providers::collect_project_docs(&repo_root).await;
    let rules = providers::collect_agent_rules(config, &repo_root).await;
    let project = providers::collect_project_block(&repo_root).await;
    let git = providers::collect_git_block(cwd).await;

    let (relevant_files, mut warnings) = if include_relevant_files {
        retriever::retrieve_relevant_files(&repo_root, &intent, git.as_ref()).await
    } else {
        (Vec::new(), Vec::new())
    };

    warnings.extend(project_docs.iter().filter(|&doc| doc.truncated).map(|doc| {
        context_pack::ContextWarning {
            message: format!("Project doc `{}` was truncated.", doc.path.display()),
        }
    }));
    warnings.extend(rules.iter().filter(|&rule| rule.truncated).map(|rule| {
        context_pack::ContextWarning {
            message: format!("Project rule `{}` was truncated.", rule.path.display()),
        }
    }));

    let budget = context_pack::BudgetUsage {
        project_doc_bytes: project_docs
            .iter()
            .map(|doc| doc.content.len())
            .sum::<usize>(),
        rule_bytes: rules
            .iter()
            .map(|rule| rule.summary_or_content.len())
            .sum::<usize>(),
        git_bytes: git
            .as_ref()
            .map(|git| git.status_short.len() + git.diff_stat.as_deref().unwrap_or("").len())
            .unwrap_or_default(),
        relevant_file_count: relevant_files.len(),
    };

    ContextPack {
        intent,
        project,
        project_docs,
        rules,
        git,
        relevant_files,
        warnings,
        budget,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::legacy_core::config::Config;
    use codex_utils_absolute_path::AbsolutePathBuf;
    use pretty_assertions::assert_eq;

    async fn test_config_for(cwd: &std::path::Path) -> Config {
        let codex_home = tempfile::tempdir().expect("codex home").keep();
        let mut config =
            Config::load_default_with_cli_overrides_for_codex_home(codex_home, Vec::new())
                .await
                .expect("config");
        config.cwd = AbsolutePathBuf::from_absolute_path(cwd).expect("absolute cwd");
        config.user_instructions = None;
        config
    }

    #[tokio::test]
    async fn project_docs_rules_and_manifest_enter_context_pack() {
        let repo = tempfile::tempdir().expect("repo");
        std::fs::write(repo.path().join("README.md"), "Project docs").expect("readme");
        std::fs::write(repo.path().join("AGENTS.md"), "Project rules").expect("agents");
        std::fs::write(
            repo.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"codex-rs/tui\"]\n",
        )
        .expect("cargo");
        let config = test_config_for(repo.path()).await;

        let pack = collect_context_pack(&config, "fix codex-rs/tui/src/app.rs", |_| {}).await;

        assert_eq!(pack.project_docs[0].content, "Project docs");
        assert_eq!(pack.rules[0].summary_or_content, "Project rules");
        assert_eq!(
            pack.project.workspace_kind,
            Some("Cargo workspace".to_string())
        );
        assert_eq!(pack.git, None);
    }
}
