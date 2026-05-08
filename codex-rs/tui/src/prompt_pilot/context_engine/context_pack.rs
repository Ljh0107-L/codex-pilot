use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeSet;
use std::path::Path;
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct ContextPack {
    pub(crate) intent: IntentBlock,
    pub(crate) project: ProjectBlock,
    pub(crate) project_docs: Vec<ProjectDocBlock>,
    pub(crate) rules: Vec<RuleBlock>,
    pub(crate) git: Option<GitBlock>,
    pub(crate) relevant_files: Vec<RelevantFileBlock>,
    pub(crate) warnings: Vec<ContextWarning>,
    pub(crate) budget: BudgetUsage,
}

impl ContextPack {
    #[cfg(test)]
    pub(crate) fn allowed_path_strings(&self) -> BTreeSet<String> {
        let mut paths = BTreeSet::new();
        paths.insert(path_string(&self.project.repo_root));
        for path in &self.intent.mentioned_paths {
            paths.insert(path_string(path));
        }
        for doc in &self.project_docs {
            paths.insert(path_string(&doc.path));
        }
        for manifest in &self.project.manifests {
            paths.insert(path_string(&manifest.path));
        }
        for rule in &self.rules {
            paths.insert(path_string(&rule.path));
        }
        if let Some(git) = &self.git {
            for path in &git.changed_files {
                paths.insert(path_string(path));
            }
        }
        for file in &self.relevant_files {
            paths.insert(path_string(&file.path));
        }
        paths
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct IntentBlock {
    pub(crate) original_prompt: String,
    pub(crate) language: String,
    pub(crate) hard_constraints: Vec<String>,
    pub(crate) mentioned_paths: Vec<PathBuf>,
    pub(crate) keywords: Vec<String>,
}

impl IntentBlock {
    pub(crate) fn from_prompt(original_prompt: &str, repo_root: &Path) -> Self {
        Self {
            original_prompt: original_prompt.to_string(),
            language: detect_language(original_prompt).to_string(),
            hard_constraints: extract_hard_constraints(original_prompt),
            mentioned_paths: extract_mentioned_paths(original_prompt, repo_root),
            keywords: extract_keywords(original_prompt),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ProjectBlock {
    pub(crate) repo_root: PathBuf,
    pub(crate) detected_languages: Vec<String>,
    pub(crate) workspace_kind: Option<String>,
    pub(crate) relevant_modules: Vec<String>,
    pub(crate) test_commands: Vec<String>,
    pub(crate) manifests: Vec<ManifestBlock>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ProjectDocBlock {
    pub(crate) path: PathBuf,
    pub(crate) content: String,
    pub(crate) truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ManifestBlock {
    pub(crate) path: PathBuf,
    pub(crate) summary: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RuleBlock {
    pub(crate) path: PathBuf,
    pub(crate) summary_or_content: String,
    pub(crate) truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct GitBlock {
    pub(crate) branch: Option<String>,
    pub(crate) has_changes: bool,
    pub(crate) changed_files: Vec<PathBuf>,
    pub(crate) status_short: String,
    pub(crate) diff_stat: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct RelevantFileBlock {
    pub(crate) path: PathBuf,
    pub(crate) reason: String,
    pub(crate) confidence: f32,
    pub(crate) evidence: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ContextWarning {
    pub(crate) message: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BudgetUsage {
    pub(crate) project_doc_bytes: usize,
    pub(crate) rule_bytes: usize,
    pub(crate) git_bytes: usize,
    pub(crate) relevant_file_count: usize,
}

pub(crate) fn relative_path(path: &Path, repo_root: &Path) -> PathBuf {
    path.strip_prefix(repo_root)
        .map(Path::to_path_buf)
        .unwrap_or_else(|_| path.to_path_buf())
}

pub(super) fn path_string(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn detect_language(prompt: &str) -> &'static str {
    let cjk = prompt
        .chars()
        .filter(|ch| matches!(*ch as u32, 0x4E00..=0x9FFF))
        .count();
    let latin = prompt.chars().filter(char::is_ascii_alphabetic).count();
    if cjk > 0 && cjk.saturating_mul(3) >= latin {
        "zh"
    } else {
        "en"
    }
}

fn extract_hard_constraints(prompt: &str) -> Vec<String> {
    prompt
        .split(['\n', '。', ';', '；'])
        .map(str::trim)
        .filter(|part| {
            let lower = part.to_ascii_lowercase();
            !part.is_empty()
                && (part.contains("必须")
                    || part.contains("不要")
                    || part.contains("不能")
                    || part.contains("只")
                    || lower.contains("must")
                    || lower.contains("do not")
                    || lower.contains("don't")
                    || lower.contains("never")
                    || lower.contains("only")
                    || lower.contains("should not"))
        })
        .take(8)
        .map(ToString::to_string)
        .collect()
}

fn extract_mentioned_paths(prompt: &str, repo_root: &Path) -> Vec<PathBuf> {
    let mut paths = BTreeSet::new();
    for token in prompt.split_whitespace() {
        let token = token.trim_matches(|ch: char| {
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
        });
        if !looks_like_path(token) {
            continue;
        }
        let path = PathBuf::from(token);
        let path = if path.is_absolute() {
            relative_path(&path, repo_root)
        } else {
            path
        };
        paths.insert(path);
    }
    paths.into_iter().collect()
}

fn looks_like_path(token: &str) -> bool {
    if token.starts_with("http://") || token.starts_with("https://") {
        return false;
    }
    let has_known_extension = Path::new(token)
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
    token.starts_with("./")
        || token.starts_with("../")
        || token.contains('/')
        || token.contains('\\')
        || has_known_extension
}

fn extract_keywords(prompt: &str) -> Vec<String> {
    let mut keywords = BTreeSet::new();
    let mut current = String::new();
    for ch in prompt.chars() {
        if ch.is_ascii_alphanumeric()
            || ch == '_'
            || ch == '-'
            || matches!(ch as u32, 0x4E00..=0x9FFF)
        {
            current.push(ch);
        } else {
            insert_keyword(&mut keywords, &current);
            current.clear();
        }
    }
    insert_keyword(&mut keywords, &current);
    keywords.into_iter().take(16).collect()
}

fn insert_keyword(keywords: &mut BTreeSet<String>, value: &str) {
    let value = value.trim().to_ascii_lowercase();
    if value.chars().count() < 2 || is_stop_word(&value) {
        return;
    }
    keywords.insert(value);
}

fn is_stop_word(value: &str) -> bool {
    matches!(
        value,
        "the"
            | "and"
            | "for"
            | "with"
            | "this"
            | "that"
            | "into"
            | "from"
            | "should"
            | "please"
            | "帮我"
            | "一下"
            | "这个"
            | "我们"
            | "现在"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn chinese_prompt_sets_chinese_language() {
        let intent = IntentBlock::from_prompt(
            "不要改变 Plan mode，只修改 tui/src/app.rs",
            Path::new("/repo"),
        );

        assert_eq!(intent.language, "zh");
        assert_eq!(
            intent.hard_constraints,
            vec!["不要改变 Plan mode，只修改 tui/src/app.rs"]
        );
        assert_eq!(
            intent.mentioned_paths,
            vec![PathBuf::from("tui/src/app.rs")]
        );
    }
}
