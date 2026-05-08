use std::ffi::OsStr;
use std::io;

use codex_file_system::ExecutorFileSystem;
use codex_utils_absolute_path::AbsolutePathBuf;
use toml::Value as TomlValue;

const DEFAULT_PROJECT_ROOT_MARKERS: &[&str] = &[".git"];

/// Reads `project_root_markers` from a merged `config.toml` [toml::Value].
///
/// Invariants:
/// - If `project_root_markers` is not specified, returns `Ok(None)`.
/// - If `project_root_markers` is specified, returns `Ok(Some(markers))` where
///   `markers` is a `Vec<String>` (including `Ok(Some(Vec::new()))` for an
///   empty array, which indicates that root detection should be disabled).
/// - Returns an error if `project_root_markers` is specified but is not an
///   array of strings.
pub fn project_root_markers_from_config(config: &TomlValue) -> io::Result<Option<Vec<String>>> {
    let Some(table) = config.as_table() else {
        return Ok(None);
    };
    let Some(markers_value) = table.get("project_root_markers") else {
        return Ok(None);
    };
    let TomlValue::Array(entries) = markers_value else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "project_root_markers must be an array of strings",
        ));
    };
    if entries.is_empty() {
        return Ok(Some(Vec::new()));
    }
    let mut markers = Vec::new();
    for entry in entries {
        let Some(marker) = entry.as_str() else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "project_root_markers must be an array of strings",
            ));
        };
        markers.push(marker.to_string());
    }
    Ok(Some(markers))
}

pub fn default_project_root_markers() -> Vec<String> {
    DEFAULT_PROJECT_ROOT_MARKERS
        .iter()
        .map(ToString::to_string)
        .collect()
}

/// Returns whether a configured project root marker should count as present.
///
/// The default marker is `.git`, but some shared development hosts can contain
/// an accidental empty `/tmp/.git` directory. Treat that one marker as absent
/// unless it looks like a real Git directory with a `HEAD` file.
pub async fn project_root_marker_exists(
    fs: &dyn ExecutorFileSystem,
    marker_path: &AbsolutePathBuf,
    marker: &str,
) -> io::Result<bool> {
    let metadata = match fs.get_metadata(marker_path, /*sandbox*/ None).await {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(err),
    };

    if marker == ".git" && metadata.is_directory && is_temp_dir_git_entry(marker_path) {
        return match fs
            .get_metadata(&marker_path.join("HEAD"), /*sandbox*/ None)
            .await
        {
            Ok(metadata) => Ok(metadata.is_file),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(err) => Err(err),
        };
    }

    Ok(true)
}

fn is_temp_dir_git_entry(marker_path: &AbsolutePathBuf) -> bool {
    let temp_dir = std::env::temp_dir();
    marker_path.as_path().file_name() == Some(OsStr::new(".git"))
        && marker_path.as_path().parent() == Some(temp_dir.as_path())
}
