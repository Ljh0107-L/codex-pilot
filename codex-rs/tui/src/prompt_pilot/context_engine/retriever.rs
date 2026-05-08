use super::context_pack::ContextWarning;
use super::context_pack::GitBlock;
use super::context_pack::IntentBlock;
use super::context_pack::RelevantFileBlock;
use super::context_pack::path_string;
use codex_file_search::FileSearchOptions;
use codex_file_search::MatchType;
use std::collections::BTreeMap;
use std::num::NonZero;
use std::path::Path;
use std::path::PathBuf;

const MAX_RELEVANT_FILES: usize = 12;
const FILE_SEARCH_LIMIT: NonZero<usize> = match NonZero::new(8) {
    Some(value) => value,
    None => panic!("file search limit must be non-zero"),
};
const FILE_SEARCH_THREADS: NonZero<usize> = match NonZero::new(2) {
    Some(value) => value,
    None => panic!("file search thread count must be non-zero"),
};

#[derive(Debug, Default)]
struct Candidate {
    score: i32,
    evidence: Vec<String>,
}

pub(super) async fn retrieve_relevant_files(
    repo_root: &Path,
    intent: &IntentBlock,
    git: Option<&GitBlock>,
) -> (Vec<RelevantFileBlock>, Vec<ContextWarning>) {
    let mut candidates = BTreeMap::<PathBuf, Candidate>::new();
    let mut warnings = Vec::new();

    for mentioned_path in &intent.mentioned_paths {
        let path = normalize_repo_path(mentioned_path, repo_root);
        if is_excluded_path(&path) {
            continue;
        }
        if repo_root.join(&path).exists() {
            add_candidate(
                &mut candidates,
                path,
                100,
                "explicitly mentioned by the user".to_string(),
            );
        } else {
            warnings.push(ContextWarning {
                message: format!(
                    "Mentioned path `{}` does not exist in the project.",
                    path.display()
                ),
            });
        }
    }

    if let Some(git) = git {
        for changed_file in &git.changed_files {
            let path = normalize_repo_path(changed_file, repo_root);
            if is_excluded_path(&path) {
                continue;
            }
            add_candidate(
                &mut candidates,
                path,
                40,
                "changed in the current git worktree".to_string(),
            );
        }
    }

    for keyword in file_search_terms(intent) {
        for file_match in search_files(repo_root, keyword.clone()).await {
            if file_match.match_type != MatchType::File {
                continue;
            }
            let path = normalize_repo_path(&file_match.path, repo_root);
            if is_excluded_path(&path) {
                continue;
            }
            add_candidate(
                &mut candidates,
                path,
                20,
                format!("file-search match for `{keyword}`"),
            );
        }
    }

    let mut ranked: Vec<(PathBuf, Candidate)> = candidates.into_iter().collect();
    ranked.sort_by(|(left_path, left), (right_path, right)| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| path_string(left_path).cmp(&path_string(right_path)))
    });

    let relevant_files = ranked
        .into_iter()
        .take(MAX_RELEVANT_FILES)
        .map(|(path, candidate)| RelevantFileBlock {
            path,
            reason: candidate
                .evidence
                .first()
                .cloned()
                .unwrap_or_else(|| "deterministic candidate".to_string()),
            confidence: (candidate.score as f32 / 100.0).clamp(0.0, 1.0),
            evidence: candidate.evidence,
        })
        .collect();

    (relevant_files, warnings)
}

pub(super) async fn retrieve_relevant_files_for_queries(
    repo_root: &Path,
    queries: &[String],
) -> Vec<RelevantFileBlock> {
    let mut candidates = BTreeMap::<PathBuf, Candidate>::new();
    for query in queries {
        let query = query.trim();
        if query.len() < 2 {
            continue;
        }
        for file_match in search_files(repo_root, query.to_string()).await {
            if file_match.match_type != MatchType::File {
                continue;
            }
            let path = normalize_repo_path(&file_match.path, repo_root);
            if is_excluded_path(&path) {
                continue;
            }
            add_candidate(
                &mut candidates,
                path,
                25,
                format!("ACE agent search match for `{query}`"),
            );
        }
    }

    let mut ranked: Vec<(PathBuf, Candidate)> = candidates.into_iter().collect();
    ranked.sort_by(|(left_path, left), (right_path, right)| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| path_string(left_path).cmp(&path_string(right_path)))
    });

    ranked
        .into_iter()
        .take(MAX_RELEVANT_FILES)
        .map(|(path, candidate)| RelevantFileBlock {
            path,
            reason: candidate
                .evidence
                .first()
                .cloned()
                .unwrap_or_else(|| "ACE agent search candidate".to_string()),
            confidence: (candidate.score as f32 / 100.0).clamp(0.0, 1.0),
            evidence: candidate.evidence,
        })
        .collect()
}

fn add_candidate(
    candidates: &mut BTreeMap<PathBuf, Candidate>,
    path: PathBuf,
    score: i32,
    evidence: String,
) {
    let candidate = candidates.entry(path).or_default();
    candidate.score += score;
    if !candidate.evidence.contains(&evidence) {
        candidate.evidence.push(evidence);
    }
}

fn file_search_terms(intent: &IntentBlock) -> Vec<String> {
    let mut terms = Vec::new();
    for path in &intent.mentioned_paths {
        if let Some(name) = path.file_stem().and_then(|name| name.to_str())
            && name.len() >= 3
        {
            terms.push(name.to_string());
        }
    }
    for keyword in &intent.keywords {
        if keyword.len() >= 3 {
            terms.push(keyword.clone());
        }
    }
    for fallback in [
        "promptpilot",
        "prompt_pilot",
        "ctrl-x",
        "composer",
        "slash_command",
        "context",
    ] {
        terms.push(fallback.to_string());
    }
    terms.sort();
    terms.dedup();
    terms.truncate(10);
    terms
}

async fn search_files(repo_root: &Path, keyword: String) -> Vec<codex_file_search::FileMatch> {
    let repo_root = repo_root.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let options = FileSearchOptions {
            limit: FILE_SEARCH_LIMIT,
            exclude: exclude_patterns(),
            threads: FILE_SEARCH_THREADS,
            compute_indices: false,
            respect_gitignore: true,
        };
        codex_file_search::run(
            &keyword,
            vec![repo_root],
            options,
            /*cancel_flag*/ None,
        )
        .map(|results| results.matches)
        .unwrap_or_default()
    })
    .await
    .unwrap_or_default()
}

fn exclude_patterns() -> Vec<String> {
    [
        ".git/**",
        "**/.git/**",
        "target/**",
        "**/target/**",
        "node_modules/**",
        "**/node_modules/**",
        "dist/**",
        "**/dist/**",
        "build/**",
        "**/build/**",
        "coverage/**",
        "**/coverage/**",
        "vendor/**",
        "**/vendor/**",
        ".env*",
        "**/.env*",
        "*.pem",
        "*.key",
    ]
    .into_iter()
    .map(ToString::to_string)
    .collect()
}

fn normalize_repo_path(path: &Path, repo_root: &Path) -> PathBuf {
    if path.is_absolute() {
        path.strip_prefix(repo_root)
            .map(Path::to_path_buf)
            .unwrap_or_else(|_| path.to_path_buf())
    } else {
        path.to_path_buf()
    }
}

fn is_excluded_path(path: &Path) -> bool {
    let mut components = path
        .components()
        .filter_map(|component| component.as_os_str().to_str());
    if components.any(|component| {
        matches!(
            component,
            ".git" | "target" | "node_modules" | "dist" | "build" | "coverage" | "vendor"
        )
    }) {
        return true;
    }
    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    file_name.starts_with(".env")
        || path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| matches!(ext, "pem" | "key"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[tokio::test]
    async fn explicit_path_ranks_above_git_changed_file() {
        let repo = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(repo.path().join("src")).expect("mkdir");
        std::fs::write(repo.path().join("src/lib.rs"), "").expect("write");
        std::fs::write(repo.path().join("src/git.rs"), "").expect("write");
        let intent = IntentBlock {
            original_prompt: "fix src/lib.rs".to_string(),
            language: "en".to_string(),
            hard_constraints: Vec::new(),
            mentioned_paths: vec![PathBuf::from("src/lib.rs")],
            keywords: Vec::new(),
        };
        let git = GitBlock {
            branch: None,
            has_changes: true,
            changed_files: vec![PathBuf::from("src/git.rs")],
            status_short: " M src/git.rs".to_string(),
            diff_stat: None,
        };

        let (files, warnings) = retrieve_relevant_files(repo.path(), &intent, Some(&git)).await;

        assert_eq!(warnings, Vec::new());
        assert_eq!(files[0].path, PathBuf::from("src/lib.rs"));
        assert_eq!(files[1].path, PathBuf::from("src/git.rs"));
    }

    #[tokio::test]
    async fn nonexistent_explicit_path_warns_and_is_not_invented() {
        let repo = tempfile::tempdir().expect("tempdir");
        let intent = IntentBlock {
            original_prompt: "fix src/missing.rs".to_string(),
            language: "en".to_string(),
            hard_constraints: Vec::new(),
            mentioned_paths: vec![PathBuf::from("src/missing.rs")],
            keywords: Vec::new(),
        };

        let (files, warnings) = retrieve_relevant_files(repo.path(), &intent, /*git*/ None).await;

        assert_eq!(files, Vec::new());
        assert_eq!(
            warnings,
            vec![ContextWarning {
                message: "Mentioned path `src/missing.rs` does not exist in the project."
                    .to_string(),
            }]
        );
    }
}
