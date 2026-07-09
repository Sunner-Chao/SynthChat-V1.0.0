use std::path::{Path, PathBuf};

use crate::{
    error::{AppError, AppResult},
    models::AgentDefinition,
};

pub(super) fn workspace_root(agent: &AgentDefinition) -> AppResult<PathBuf> {
    let root = if agent.workspace_dir.trim().is_empty() {
        std::env::current_dir()?
    } else {
        PathBuf::from(agent.workspace_dir.trim())
    };
    Ok(root.canonicalize()?)
}

fn assert_within_root(canonical: &Path, root: &Path) -> AppResult<()> {
    if !canonical.starts_with(root) {
        return Err(AppError::BadRequest(format!(
            "path '{}' resolves outside the workspace root '{}'",
            canonical.display(),
            root.display()
        )));
    }
    Ok(())
}

pub(super) fn resolve_workspace_path(root: &Path, input: &str) -> AppResult<PathBuf> {
    let candidate = {
        let path = PathBuf::from(input);
        if path.is_absolute() {
            path
        } else {
            root.join(path)
        }
    };
    let canonical = candidate.canonicalize()?;
    // Guard against path traversal via `..` segments or symlinks that point
    // outside the workspace root.  Without this check a path like
    // "../../etc/passwd" or a workspace-internal symlink to "/" would bypass
    // the workspace boundary silently.
    assert_within_root(&canonical, root)?;
    Ok(canonical)
}

pub(super) fn resolve_workspace_target_path(root: &Path, input: &str) -> AppResult<PathBuf> {
    let candidate = {
        let path = PathBuf::from(input);
        if path.is_absolute() {
            path
        } else {
            root.join(path)
        }
    };
    if candidate.exists() {
        let canonical = candidate.canonicalize()?;
        assert_within_root(&canonical, root)?;
        return Ok(canonical);
    }
    let mut existing_ancestor = candidate.as_path();
    while !existing_ancestor.exists() {
        if let Some(parent) = existing_ancestor.parent() {
            existing_ancestor = parent;
        } else {
            // Whole path is new; check the candidate itself (not yet canonical).
            // Strip any `..` components before the guard so the comparison is
            // still meaningful for paths that don't exist yet.
            let normalized = normalize_non_existent_path(&candidate);
            assert_within_root(&normalized, root)?;
            return Ok(candidate);
        }
    }
    let ancestor_canonical = existing_ancestor.canonicalize()?;
    assert_within_root(&ancestor_canonical, root)?;
    let relative_target = candidate
        .strip_prefix(existing_ancestor)
        .unwrap_or_else(|_| Path::new(""));
    Ok(ancestor_canonical.join(relative_target))
}

/// Collapse `.` and `..` components in a path that may not exist on disk,
/// so we can verify it stays within the workspace root before creating it.
fn normalize_non_existent_path(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

pub(super) fn should_skip_dir(name: &str) -> bool {
    matches!(
        name,
        ".git" | "node_modules" | "target" | "dist" | "build" | ".next" | ".venv" | "__pycache__"
    )
}

pub(super) fn likely_binary(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("")
            .to_lowercase()
            .as_str(),
        "png"
            | "jpg"
            | "jpeg"
            | "gif"
            | "bmp"
            | "webp"
            | "tiff"
            | "tif"
            | "ico"
            // Keep PDFs out of automatic text inlining; read_file handles
            // best-effort PDF text extraction through its document path.
            | "pdf"
            | "mp4"
            | "mov"
            | "avi"
            | "mkv"
            | "webm"
            | "wmv"
            | "flv"
            | "m4v"
            | "mpeg"
            | "mpg"
            | "mp3"
            | "wav"
            | "ogg"
            | "flac"
            | "aac"
            | "m4a"
            | "wma"
            | "aiff"
            | "opus"
            | "zip"
            | "tar"
            | "gz"
            | "bz2"
            | "7z"
            | "rar"
            | "xz"
            | "z"
            | "tgz"
            | "iso"
            | "exe"
            | "dll"
            | "so"
            | "dylib"
            | "pdb"
            | "rlib"
            | "rmeta"
            | "bin"
            | "o"
            | "a"
            | "obj"
            | "lib"
            | "app"
            | "msi"
            | "deb"
            | "rpm"
            | "doc"
            | "docx"
            | "xls"
            | "xlsx"
            | "ppt"
            | "pptx"
            | "odt"
            | "ods"
            | "odp"
            | "ttf"
            | "otf"
            | "woff"
            | "woff2"
            | "eot"
            | "pyc"
            | "pyo"
            | "class"
            | "jar"
            | "war"
            | "ear"
            | "node"
            | "wasm"
            | "sqlite"
            | "sqlite3"
            | "db"
            | "mdb"
            | "idx"
            | "psd"
            | "ai"
            | "eps"
            | "sketch"
            | "fig"
            | "xd"
            | "blend"
            | "3ds"
            | "max"
            | "swf"
            | "fla"
            | "lockb"
            | "dat"
            | "data"
    )
}
