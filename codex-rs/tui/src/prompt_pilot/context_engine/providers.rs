use super::context_pack::GitBlock;
use super::context_pack::ManifestBlock;
use super::context_pack::ProjectBlock;
use super::context_pack::ProjectDocBlock;
use super::context_pack::RuleBlock;
use super::context_pack::relative_path;
use crate::legacy_core::AgentsMdManager;
use crate::legacy_core::config::Config;
use codex_exec_server::LOCAL_FS;
use codex_git_utils::collect_git_worktree_summary;
use codex_utils_absolute_path::AbsolutePathBuf;
use std::collections::BTreeSet;
use std::path::Path;
use std::path::PathBuf;

const MAX_PROJECT_DOC_BYTES: usize = 6_000;
const MAX_RULE_BYTES: usize = 8_000;
const MAX_MANIFEST_BYTES: usize = 6_000;
const MAX_GIT_TEXT_BYTES: usize = 4_000;

struct LimitedText {
    content: String,
    truncated: bool,
}

pub(super) async fn collect_project_docs(repo_root: &Path) -> Vec<ProjectDocBlock> {
    let mut docs = Vec::new();
    for relative in ["README.md", "README", "docs/README.md"] {
        let path = repo_root.join(relative);
        let Some(text) = read_limited_text(&path, MAX_PROJECT_DOC_BYTES).await else {
            continue;
        };
        if text.content.trim().is_empty() {
            continue;
        }
        docs.push(ProjectDocBlock {
            path: PathBuf::from(relative),
            content: text.content,
            truncated: text.truncated,
        });
    }
    docs
}

pub(super) async fn collect_agent_rules(config: &Config, repo_root: &Path) -> Vec<RuleBlock> {
    let sources = AgentsMdManager::new(config)
        .instruction_sources(LOCAL_FS.as_ref())
        .await;
    let mut rules = Vec::new();
    for source in sources {
        let Some(text) = read_limited_text(source.as_path(), MAX_RULE_BYTES).await else {
            continue;
        };
        if text.content.trim().is_empty() {
            continue;
        }
        rules.push(RuleBlock {
            path: relative_path(source.as_path(), repo_root),
            summary_or_content: text.content,
            truncated: text.truncated,
        });
    }
    rules
}

pub(super) async fn collect_project_block(repo_root: &Path) -> ProjectBlock {
    let mut detected_languages = BTreeSet::new();
    let mut relevant_modules = BTreeSet::new();
    let mut test_commands = BTreeSet::new();
    let mut manifests = Vec::new();
    let mut workspace_kind = None;

    for manifest_name in ["Cargo.toml", "package.json", "pyproject.toml", "go.mod"] {
        let path = repo_root.join(manifest_name);
        let Some(text) = read_limited_text(&path, MAX_MANIFEST_BYTES).await else {
            continue;
        };
        let summary = summarize_manifest(manifest_name, &text.content);
        if summary.is_empty() {
            continue;
        }
        match manifest_name {
            "Cargo.toml" => {
                detected_languages.insert("Rust".to_string());
                test_commands.insert("cargo test".to_string());
                if text.content.contains("[workspace]") {
                    workspace_kind = Some("Cargo workspace".to_string());
                    for member in cargo_workspace_members(&text.content) {
                        relevant_modules.insert(member);
                    }
                } else {
                    workspace_kind.get_or_insert_with(|| "Cargo package".to_string());
                }
            }
            "package.json" => {
                detected_languages.insert("JavaScript/TypeScript".to_string());
                test_commands.insert("npm test".to_string());
                workspace_kind.get_or_insert_with(|| "Node package".to_string());
            }
            "pyproject.toml" => {
                detected_languages.insert("Python".to_string());
                test_commands.insert("pytest".to_string());
                workspace_kind.get_or_insert_with(|| "Python project".to_string());
            }
            "go.mod" => {
                detected_languages.insert("Go".to_string());
                test_commands.insert("go test ./...".to_string());
                workspace_kind.get_or_insert_with(|| "Go module".to_string());
            }
            _ => {}
        }
        manifests.push(ManifestBlock {
            path: PathBuf::from(manifest_name),
            summary,
        });
    }

    ProjectBlock {
        repo_root: repo_root.to_path_buf(),
        detected_languages: detected_languages.into_iter().collect(),
        workspace_kind,
        relevant_modules: relevant_modules.into_iter().take(24).collect(),
        test_commands: test_commands.into_iter().collect(),
        manifests,
    }
}

pub(super) async fn collect_git_block(cwd: &Path) -> Option<GitBlock> {
    let summary = collect_git_worktree_summary(cwd).await?;
    Some(GitBlock {
        branch: summary.branch,
        has_changes: summary.has_changes,
        changed_files: summary.changed_files,
        status_short: truncate_string(summary.status_short, MAX_GIT_TEXT_BYTES),
        diff_stat: non_empty_truncated(summary.diff_stat, MAX_GIT_TEXT_BYTES),
    })
}

async fn read_limited_text(path: &Path, max_bytes: usize) -> Option<LimitedText> {
    let absolute = AbsolutePathBuf::from_absolute_path(path).ok()?;
    let mut bytes = LOCAL_FS.read_file(&absolute, /*sandbox*/ None).await.ok()?;
    if bytes.contains(&0) {
        return None;
    }
    let truncated = bytes.len() > max_bytes;
    if truncated {
        bytes.truncate(max_bytes);
    }
    Some(LimitedText {
        content: String::from_utf8_lossy(&bytes).trim().to_string(),
        truncated,
    })
}

fn summarize_manifest(name: &str, content: &str) -> String {
    match name {
        "Cargo.toml" => summarize_cargo_manifest(content),
        "package.json" => summarize_package_json(content),
        "pyproject.toml" => summarize_pyproject(content),
        "go.mod" => summarize_go_mod(content),
        _ => String::new(),
    }
}

fn summarize_cargo_manifest(content: &str) -> String {
    let mut parts = Vec::new();
    if content.contains("[workspace]") {
        parts.push("Cargo workspace".to_string());
    }
    if let Some(name) = toml_string_value(content, "name") {
        parts.push(format!("package `{name}`"));
    }
    let members = cargo_workspace_members(content);
    if !members.is_empty() {
        parts.push(format!("members: {}", members.join(", ")));
    }
    parts.join("; ")
}

fn summarize_package_json(content: &str) -> String {
    let value = serde_json::from_str::<serde_json::Value>(content).ok();
    let name = value
        .as_ref()
        .and_then(|value| value.get("name"))
        .and_then(serde_json::Value::as_str);
    let has_workspaces = value
        .as_ref()
        .and_then(|value| value.get("workspaces"))
        .is_some();
    match (name, has_workspaces) {
        (Some(name), true) => format!("Node package `{name}` with workspaces"),
        (Some(name), false) => format!("Node package `{name}`"),
        (None, true) => "Node package with workspaces".to_string(),
        (None, false) => "Node package".to_string(),
    }
}

fn summarize_pyproject(content: &str) -> String {
    if let Some(name) = toml_string_value(content, "name") {
        format!("Python project `{name}`")
    } else {
        "Python project".to_string()
    }
}

fn summarize_go_mod(content: &str) -> String {
    content
        .lines()
        .find_map(|line| line.trim().strip_prefix("module "))
        .map(|module| format!("Go module `{}`", module.trim()))
        .unwrap_or_else(|| "Go module".to_string())
}

fn toml_string_value(content: &str, key: &str) -> Option<String> {
    let prefix = format!("{key} = ");
    content.lines().find_map(|line| {
        let value = line.trim().strip_prefix(&prefix)?.trim();
        value
            .strip_prefix('"')
            .and_then(|value| value.strip_suffix('"'))
            .map(ToString::to_string)
    })
}

fn cargo_workspace_members(content: &str) -> Vec<String> {
    let parsed = toml::from_str::<toml::Value>(content).ok();
    parsed
        .as_ref()
        .and_then(|value| value.get("workspace"))
        .and_then(|workspace| workspace.get("members"))
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(toml::Value::as_str)
        .map(ToString::to_string)
        .take(24)
        .collect()
}

fn non_empty_truncated(value: String, max_bytes: usize) -> Option<String> {
    let value = truncate_string(value, max_bytes);
    (!value.trim().is_empty()).then_some(value)
}

fn truncate_string(value: String, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value;
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    value[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn cargo_manifest_summary_detects_workspace_members() {
        let summary = summarize_cargo_manifest(
            r#"
            [workspace]
            members = ["core", "tui"]
            "#,
        );

        assert_eq!(summary, "Cargo workspace; members: core, tui");
    }
}
