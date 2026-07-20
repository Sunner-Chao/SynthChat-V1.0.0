use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    ffi::OsString,
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak},
};

use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt};
use cap_std::{
    ambient_authority,
    fs::{Dir, File as CapFile, OpenOptions},
};
use globset::{GlobBuilder, GlobMatcher};
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use sha2::{Digest, Sha256};

use super::{
    ToolExecutionControl, ToolExecutionControlError,
    fuzzy::{FuzzyError, find_and_replace as fuzzy_find_and_replace},
    v4a::{V4aHunk, V4aHunkLine, V4aOperation, V4aPatch, parse_v4a_patch},
};

const MAX_ARGUMENT_BYTES: usize = 64 * 1024;
const MAX_PATH_BYTES: usize = 1_024;
const MAX_WRITE_CONTENT_BYTES: usize = 60 * 1024;
const WRITE_CHUNK_BYTES: usize = 8 * 1024;
const MAX_PATCH_TEXT_BYTES: usize = 60 * 1024;
const MAX_PATCH_TARGET_BYTES: usize = 2 * 1024 * 1024;
const MAX_PATCH_AGGREGATE_BYTES: usize = 16 * 1024 * 1024;
const MAX_PATCH_DIFF_BYTES: usize = 36 * 1024;
const UTF8_BOM: &[u8] = b"\xef\xbb\xbf";
const MAX_READ_FILE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_SEARCH_FILE_BYTES: u64 = 1024 * 1024;
const MAX_SEARCH_TOTAL_BYTES: u64 = 16 * 1024 * 1024;
const MAX_SEARCH_ENTRIES: usize = 10_000;
const MAX_SEARCH_OFFSET: usize = 10_000;
const MAX_SEARCH_RESULTS: usize = 100;
const MAX_OUTPUT_BYTES: usize = 60 * 1024;
const MAX_LINE_CHARS: usize = 2_000;
const MAX_SUMMARY_PATH_CHARS: usize = 400;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum WorkspaceToolError {
    InvalidArguments,
    ExecutionFailed,
    InvalidResult,
    Cancelled,
    DeadlineExceeded,
}

pub(super) struct WorkspaceToolResult {
    pub(super) raw_result_json: String,
    pub(super) input_summary: String,
    pub(super) result_summary: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct WorkspaceFilePrecondition {
    pub(super) path: String,
    pub(super) state: WorkspaceFileState,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum WorkspaceFileState {
    Missing,
    Existing { sha256: String },
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct WorkspacePatchPlan {
    root: PathBuf,
    arguments_sha256: [u8; 32],
    preconditions: Vec<WorkspaceFilePrecondition>,
    lock_paths: Vec<PathBuf>,
    operations: Vec<PlannedPatchOperation>,
}

impl std::fmt::Debug for WorkspacePatchPlan {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WorkspacePatchPlan")
            .field("target_count", &self.preconditions.len())
            .field("operation_count", &self.operations.len())
            .finish_non_exhaustive()
    }
}

#[derive(Clone, PartialEq, Eq)]
enum PlannedPatchOperation {
    Add {
        path: String,
        candidate: Vec<u8>,
        diff: String,
    },
    Update {
        path: String,
        candidate: Vec<u8>,
        diff: String,
        replacements: usize,
    },
    Delete {
        path: String,
        diff: String,
    },
    Move {
        source: String,
        destination: String,
        diff: String,
    },
}

#[derive(Default)]
struct PatchApplyState {
    diff: String,
    files_modified: Vec<String>,
    files_created: Vec<String>,
    files_deleted: Vec<String>,
    replacements: usize,
    bytes_written: usize,
    applied: usize,
}

struct AppliedPatchOperation {
    diff: String,
    files_modified: Vec<String>,
    files_created: Vec<String>,
    files_deleted: Vec<String>,
    replacements: usize,
    bytes_written: usize,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
struct WorkspaceMutationKey {
    root: PathBuf,
    relative: PathBuf,
}

static WORKSPACE_MUTATION_LOCKS: OnceLock<Mutex<HashMap<WorkspaceMutationKey, Weak<Mutex<()>>>>> =
    OnceLock::new();

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadFileInput {
    path: String,
    #[serde(default = "default_read_offset")]
    offset: usize,
    #[serde(default = "default_read_limit")]
    limit: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteFileInput {
    path: String,
    content: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
struct PatchInput {
    mode: PatchMode,
    path: Option<String>,
    old_string: Option<String>,
    new_string: Option<String>,
    #[serde(default)]
    replace_all: bool,
    patch: Option<String>,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum PatchMode {
    Replace,
    Patch,
}

struct ReplacePatchInput {
    path: String,
    old_string: String,
    new_string: String,
    replace_all: bool,
}

enum ParsedPatchInput {
    Replace(ReplacePatchInput),
    V4a(super::v4a::V4aPatch),
}

#[derive(Clone, Copy)]
enum LineEnding {
    Lf,
    CrLf,
    Cr,
}

#[derive(Clone, Copy)]
enum AtomicWriteCommit<'a> {
    #[cfg(test)]
    Replace,
    ReplaceIfUnchanged(&'a [u8]),
    CreateIfMissing,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
struct SearchFilesInput {
    pattern: String,
    #[serde(default)]
    target: SearchTarget,
    #[serde(default = "default_search_path")]
    path: String,
    file_glob: Option<String>,
    #[serde(default = "default_search_limit")]
    limit: usize,
    #[serde(default)]
    offset: usize,
    #[serde(default)]
    output_mode: SearchOutputMode,
    #[serde(default)]
    context: usize,
}

#[derive(Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum SearchTarget {
    #[default]
    Content,
    Files,
}

#[derive(Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum SearchOutputMode {
    #[default]
    Content,
    FilesOnly,
    Count,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ReadFileResult {
    path: String,
    content: String,
    offset: usize,
    returned_lines: usize,
    total_lines: usize,
    next_offset: Option<usize>,
    truncated: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WriteFileResult {
    path: String,
    bytes_written: usize,
    created: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PatchResult {
    success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(skip_serializing_if = "String::is_empty")]
    diff: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    files_modified: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    files_created: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    files_deleted: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    replacements: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bytes_written: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

struct TempFileCleanup<'a> {
    directory: &'a Dir,
    name: String,
    armed: bool,
}

impl TempFileCleanup<'_> {
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for TempFileCleanup<'_> {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.directory.remove_file(&self.name);
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SearchFilesResult {
    target: &'static str,
    items: Vec<JsonValue>,
    offset: usize,
    returned: usize,
    next_offset: Option<usize>,
    truncated: bool,
    omitted_sensitive_files: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ContentMatch {
    path: String,
    line: usize,
    text: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    before: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    after: Vec<String>,
}

struct SearchPage {
    offset: usize,
    limit: usize,
    seen: usize,
    items: Vec<JsonValue>,
}

struct WorkspacePath {
    components: Vec<String>,
    relative: PathBuf,
    display: String,
}

enum SearchCapability {
    Directory(Dir),
    File(CapFile),
}

enum WorkspaceFileSnapshot {
    Missing(WorkspaceFilePrecondition),
    Existing {
        precondition: WorkspaceFilePrecondition,
        parent: Dir,
        content: Vec<u8>,
        permissions: cap_std::fs::Permissions,
    },
}

struct PreparedPatchTarget {
    requested: WorkspacePath,
    precondition: WorkspaceFilePrecondition,
    content: Option<Vec<u8>>,
}

impl WorkspaceFileSnapshot {
    fn precondition(&self) -> &WorkspaceFilePrecondition {
        match self {
            Self::Missing(precondition) | Self::Existing { precondition, .. } => precondition,
        }
    }
}

impl SearchPage {
    fn new(offset: usize, limit: usize) -> Self {
        Self {
            offset,
            limit,
            seen: 0,
            items: Vec::with_capacity(limit.saturating_add(1)),
        }
    }

    fn record(
        &mut self,
        build: impl FnOnce() -> Result<JsonValue, WorkspaceToolError>,
    ) -> Result<bool, WorkspaceToolError> {
        let index = self.seen;
        self.seen = self
            .seen
            .checked_add(1)
            .ok_or(WorkspaceToolError::InvalidResult)?;
        if index >= self.offset && self.items.len() <= self.limit {
            self.items.push(build()?);
        }
        Ok(self.items.len() > self.limit)
    }

    fn into_items(mut self) -> (Vec<JsonValue>, bool) {
        let truncated = self.items.len() > self.limit;
        if truncated {
            self.items.truncate(self.limit);
        }
        (self.items, truncated)
    }
}

struct SearchTraversal<'a> {
    file_glob: Option<&'a GlobMatcher>,
    name_glob: Option<&'a GlobMatcher>,
    regex: Option<&'a Regex>,
    output_mode: SearchOutputMode,
    context: usize,
    control: &'a ToolExecutionControl,
    entries_seen: usize,
    bytes_seen: u64,
    omitted_sensitive_files: usize,
    page: SearchPage,
}

struct SearchTraversalOptions<'a> {
    file_glob: Option<&'a GlobMatcher>,
    name_glob: Option<&'a GlobMatcher>,
    regex: Option<&'a Regex>,
    output_mode: SearchOutputMode,
    context: usize,
}

impl<'a> SearchTraversal<'a> {
    fn new(
        options: SearchTraversalOptions<'a>,
        offset: usize,
        limit: usize,
        control: &'a ToolExecutionControl,
    ) -> Self {
        Self {
            file_glob: options.file_glob,
            name_glob: options.name_glob,
            regex: options.regex,
            output_mode: options.output_mode,
            context: options.context,
            control,
            entries_seen: 0,
            bytes_seen: 0,
            omitted_sensitive_files: 0,
            page: SearchPage::new(offset, limit),
        }
    }

    fn record_entry(&mut self) -> Result<(), WorkspaceToolError> {
        self.entries_seen = self
            .entries_seen
            .checked_add(1)
            .filter(|seen| *seen <= MAX_SEARCH_ENTRIES)
            .ok_or(WorkspaceToolError::ExecutionFailed)?;
        Ok(())
    }

    fn walk_directory(
        &mut self,
        directory: &Dir,
        relative: &Path,
        depth: usize,
    ) -> Result<bool, WorkspaceToolError> {
        check_active(self.control)?;
        let mut names = Vec::<OsString>::new();
        for entry in directory.entries().map_err(map_io)? {
            check_active(self.control)?;
            let entry = entry.map_err(map_io)?;
            self.record_entry()?;
            names.push(entry.file_name());
        }
        names.sort();

        for name in names {
            check_active(self.control)?;
            let metadata = directory.symlink_metadata(&name).map_err(map_io)?;
            let file_type = metadata.file_type();
            if file_type.is_symlink() {
                continue;
            }

            let mut child_relative = relative.to_path_buf();
            child_relative.push(&name);
            if file_type.is_dir() {
                let lower_name = name.to_string_lossy().to_ascii_lowercase();
                if is_sensitive_name(&lower_name) || is_ignored_directory(&lower_name) {
                    continue;
                }
                let child_depth = depth
                    .checked_add(1)
                    .ok_or(WorkspaceToolError::ExecutionFailed)?;
                if child_depth >= 32 {
                    continue;
                }
                let child = directory.open_dir_nofollow(&name).map_err(map_io)?;
                if self.walk_directory(&child, &child_relative, child_depth)? {
                    return Ok(true);
                }
                continue;
            }
            if !file_type.is_file() {
                continue;
            }
            if is_sensitive_path(&child_relative) {
                self.omitted_sensitive_files = self.omitted_sensitive_files.saturating_add(1);
                continue;
            }
            let file = open_file_nofollow(directory, &name)?;
            if self.visit_file(file, &child_relative)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn visit_file(&mut self, file: CapFile, relative: &Path) -> Result<bool, WorkspaceToolError> {
        check_active(self.control)?;
        let metadata = file.metadata().map_err(map_io)?;
        if !metadata.is_file() {
            return Err(WorkspaceToolError::ExecutionFailed);
        }
        let relative_text = display_relative(relative)?;
        let file_name = relative
            .file_name()
            .ok_or(WorkspaceToolError::ExecutionFailed)?;
        if let Some(glob) = self.file_glob
            && !glob.is_match(relative)
            && !glob.is_match(file_name)
        {
            return Ok(false);
        }
        if let Some(glob) = self.name_glob {
            if (glob.is_match(relative) || glob.is_match(file_name))
                && self.page.record(|| Ok(JsonValue::String(relative_text)))?
            {
                return Ok(true);
            }
            return Ok(false);
        }
        if metadata.len() > MAX_SEARCH_FILE_BYTES {
            return Ok(false);
        }
        let bytes = read_bounded(file, MAX_SEARCH_FILE_BYTES)?;
        check_active(self.control)?;
        let actual_bytes =
            u64::try_from(bytes.len()).map_err(|_| WorkspaceToolError::InvalidResult)?;
        self.bytes_seen = self
            .bytes_seen
            .checked_add(actual_bytes)
            .filter(|total| *total <= MAX_SEARCH_TOTAL_BYTES)
            .ok_or(WorkspaceToolError::ExecutionFailed)?;
        if bytes.contains(&0) {
            return Ok(false);
        }
        let Ok(content) = std::str::from_utf8(&bytes) else {
            return Ok(false);
        };
        collect_content_matches(
            &mut self.page,
            &relative_text,
            content,
            self.regex.expect("content searches compile a regex"),
            self.output_mode,
            self.context,
            self.control,
        )
    }
}

#[cfg(test)]
pub(super) fn execute_read_file(
    workspace_root: Option<&Path>,
    raw_arguments_json: &str,
) -> Result<WorkspaceToolResult, WorkspaceToolError> {
    execute_read_file_controlled(
        workspace_root,
        raw_arguments_json,
        &ToolExecutionControl::new(std::time::Instant::now() + std::time::Duration::from_secs(60)),
    )
}

pub(super) fn execute_read_file_controlled(
    workspace_root: Option<&Path>,
    raw_arguments_json: &str,
    control: &ToolExecutionControl,
) -> Result<WorkspaceToolResult, WorkspaceToolError> {
    check_active(control)?;
    let root = workspace_root.ok_or(WorkspaceToolError::ExecutionFailed)?;
    let input: ReadFileInput = strict_json_object(raw_arguments_json)?;
    if input.offset == 0 || !(1..=2_000).contains(&input.limit) {
        return Err(WorkspaceToolError::InvalidArguments);
    }
    let requested = parse_workspace_path(&input.path)?;
    check_active(control)?;
    if is_sensitive_path(&requested.relative) {
        return Err(WorkspaceToolError::ExecutionFailed);
    }
    let (file_name, parent_components) = requested
        .components
        .split_last()
        .ok_or(WorkspaceToolError::ExecutionFailed)?;
    let workspace = open_workspace(root)?;
    let parent = open_dir_components(&workspace, parent_components)?;
    let file = open_file_nofollow(&parent, file_name)?;
    let metadata = file.metadata().map_err(map_io)?;
    if !metadata.is_file() || metadata.len() > MAX_READ_FILE_BYTES {
        return Err(WorkspaceToolError::ExecutionFailed);
    }
    let bytes = read_bounded(file, MAX_READ_FILE_BYTES)?;
    check_active(control)?;
    if bytes.contains(&0) {
        return Err(WorkspaceToolError::ExecutionFailed);
    }
    let content = std::str::from_utf8(&bytes).map_err(|_| WorkspaceToolError::ExecutionFailed)?;
    let lines: Vec<_> = content.lines().collect();
    let start = input.offset.saturating_sub(1).min(lines.len());
    let requested_end = start.saturating_add(input.limit).min(lines.len());
    let mut rendered = String::new();
    let mut returned = 0;
    let mut truncated_by_output = false;
    for (index, line) in lines[start..requested_end].iter().enumerate() {
        check_active(control)?;
        let line = bounded_line(line);
        let rendered_line = format!("{}|{}\n", start + index + 1, line);
        if rendered.len().saturating_add(rendered_line.len()) > MAX_OUTPUT_BYTES / 2 {
            truncated_by_output = true;
            break;
        }
        rendered.push_str(&rendered_line);
        returned += 1;
    }
    let consumed_end = start.saturating_add(returned);
    let truncated = truncated_by_output || consumed_end < lines.len();
    let next_offset = truncated.then_some(consumed_end.saturating_add(1));
    let result = ReadFileResult {
        path: requested.display.clone(),
        content: rendered,
        offset: input.offset,
        returned_lines: returned,
        total_lines: lines.len(),
        next_offset,
        truncated,
    };
    let raw_result_json = bounded_json(&result)?;
    Ok(WorkspaceToolResult {
        raw_result_json,
        input_summary: format!("Read {}", bounded_summary_path(&requested.display)),
        result_summary: format!(
            "{returned} lines from {}",
            bounded_summary_path(&requested.display)
        ),
    })
}

pub(super) fn summarize_write_file(raw_arguments_json: &str) -> Result<String, WorkspaceToolError> {
    let (input, requested) = parse_write_file_input(raw_arguments_json)?;
    Ok(format!(
        "Write {} ({} bytes)",
        bounded_summary_path(&requested.display),
        input.content.len()
    ))
}

pub(super) fn prepare_write_file_precondition(
    workspace_root: Option<&Path>,
    raw_arguments_json: &str,
    control: &ToolExecutionControl,
) -> Result<WorkspaceFilePrecondition, WorkspaceToolError> {
    check_active(control)?;
    let root = workspace_root.ok_or(WorkspaceToolError::ExecutionFailed)?;
    let (_, requested) = parse_write_file_input(raw_arguments_json)?;
    let snapshot = capture_workspace_file(root, &requested, control)?;
    Ok(snapshot.precondition().clone())
}

#[cfg(test)]
pub(super) fn execute_write_file(
    workspace_root: Option<&Path>,
    raw_arguments_json: &str,
) -> Result<WorkspaceToolResult, WorkspaceToolError> {
    execute_write_file_controlled(
        workspace_root,
        raw_arguments_json,
        &ToolExecutionControl::new(std::time::Instant::now() + std::time::Duration::from_secs(60)),
    )
}

#[cfg(test)]
pub(super) fn execute_write_file_controlled(
    workspace_root: Option<&Path>,
    raw_arguments_json: &str,
    control: &ToolExecutionControl,
) -> Result<WorkspaceToolResult, WorkspaceToolError> {
    let precondition =
        prepare_write_file_precondition(workspace_root, raw_arguments_json, control)?;
    execute_write_file_with_precondition(workspace_root, raw_arguments_json, &precondition, control)
}

pub(super) fn execute_write_file_with_precondition(
    workspace_root: Option<&Path>,
    raw_arguments_json: &str,
    expected: &WorkspaceFilePrecondition,
    control: &ToolExecutionControl,
) -> Result<WorkspaceToolResult, WorkspaceToolError> {
    check_active(control)?;
    let root = workspace_root.ok_or(WorkspaceToolError::ExecutionFailed)?;
    let (input, requested) = parse_write_file_input(raw_arguments_json)?;
    if expected.path != requested.display {
        return Err(WorkspaceToolError::ExecutionFailed);
    }
    with_workspace_mutation_locks(root, std::slice::from_ref(&requested.relative), || {
        check_active(control)?;
        let snapshot = capture_workspace_file(root, &requested, control)?;
        if snapshot.precondition() != expected {
            return Err(WorkspaceToolError::ExecutionFailed);
        }
        let (file_name, parent_components) = requested
            .components
            .split_last()
            .ok_or(WorkspaceToolError::InvalidArguments)?;
        let created = match snapshot {
            WorkspaceFileSnapshot::Missing(_) => {
                let workspace = open_workspace(root)?;
                let parent = open_or_create_dir_components(&workspace, parent_components, control)?;
                atomic_create_file(&parent, file_name, input.content.as_bytes(), control)?;
                true
            }
            WorkspaceFileSnapshot::Existing {
                parent,
                content,
                permissions,
                ..
            } => {
                atomic_replace_file(
                    &parent,
                    file_name,
                    input.content.as_bytes(),
                    permissions,
                    &content,
                    control,
                )?;
                false
            }
        };
        let bytes_written = input.content.len();
        let result = WriteFileResult {
            path: requested.display.clone(),
            bytes_written,
            created,
        };
        Ok(WorkspaceToolResult {
            raw_result_json: bounded_json(&result)?,
            input_summary: format!(
                "Write {} ({bytes_written} bytes)",
                bounded_summary_path(&requested.display)
            ),
            result_summary: format!(
                "Wrote {bytes_written} bytes to {}",
                bounded_summary_path(&requested.display)
            ),
        })
    })
}

pub(crate) fn summarize_patch(raw_arguments_json: &str) -> Result<String, WorkspaceToolError> {
    match parse_patch_input(raw_arguments_json)? {
        ParsedPatchInput::Replace(input) => {
            let requested = parse_mutating_workspace_path(&input.path)?;
            let scope = if input.replace_all { "all" } else { "one" };
            Ok(format!(
                "Patch {} (+{}/-{} lines, {scope})",
                bounded_summary_path(&requested.display),
                patch_line_count(&input.new_string),
                patch_line_count(&input.old_string),
            ))
        }
        ParsedPatchInput::V4a(patch) => {
            validate_v4a_paths(&patch)?;
            Ok(format!(
                "Apply V4A patch ({} operation{})",
                patch.operations.len(),
                if patch.operations.len() == 1 { "" } else { "s" }
            ))
        }
    }
}

#[cfg(test)]
pub(super) fn prepare_workspace_file_precondition(
    workspace_root: Option<&Path>,
    path: &str,
    control: &ToolExecutionControl,
) -> Result<WorkspaceFilePrecondition, WorkspaceToolError> {
    check_active(control)?;
    let root = workspace_root.ok_or(WorkspaceToolError::ExecutionFailed)?;
    let requested = parse_mutating_workspace_path(path)?;
    let snapshot = capture_workspace_file(root, &requested, control)?;
    Ok(snapshot.precondition().clone())
}

#[cfg(test)]
pub(super) fn verify_workspace_file_precondition(
    workspace_root: Option<&Path>,
    expected: &WorkspaceFilePrecondition,
    control: &ToolExecutionControl,
) -> Result<(), WorkspaceToolError> {
    check_active(control)?;
    let root = workspace_root.ok_or(WorkspaceToolError::ExecutionFailed)?;
    let requested = parse_mutating_workspace_path(&expected.path)?;
    let current = capture_workspace_file(root, &requested, control)?;
    if current.precondition() == expected {
        Ok(())
    } else {
        Err(WorkspaceToolError::ExecutionFailed)
    }
}

pub(crate) fn prepare_patch_plan(
    workspace_root: Option<&Path>,
    raw_arguments_json: &str,
    control: &ToolExecutionControl,
) -> Result<WorkspacePatchPlan, WorkspaceToolError> {
    check_active(control)?;
    let root = workspace_root.ok_or(WorkspaceToolError::ExecutionFailed)?;
    let canonical_root = std::fs::canonicalize(root).map_err(map_io)?;
    let input = parse_patch_input(raw_arguments_json)?;
    let mut plan = match input {
        ParsedPatchInput::Replace(input) => prepare_replace_patch_plan(root, input, control)?,
        ParsedPatchInput::V4a(patch) => prepare_v4a_patch_plan(root, patch, control)?,
    };
    plan.root = canonical_root;
    plan.arguments_sha256 = Sha256::digest(raw_arguments_json.as_bytes()).into();
    Ok(plan)
}

#[cfg(test)]
pub(super) fn execute_patch(
    workspace_root: Option<&Path>,
    raw_arguments_json: &str,
) -> Result<WorkspaceToolResult, WorkspaceToolError> {
    execute_patch_controlled(
        workspace_root,
        raw_arguments_json,
        &ToolExecutionControl::new(std::time::Instant::now() + std::time::Duration::from_secs(60)),
    )
}

#[cfg(test)]
pub(crate) fn execute_patch_controlled(
    workspace_root: Option<&Path>,
    raw_arguments_json: &str,
    control: &ToolExecutionControl,
) -> Result<WorkspaceToolResult, WorkspaceToolError> {
    let plan = prepare_patch_plan(workspace_root, raw_arguments_json, control)?;
    execute_patch_with_plan(workspace_root, raw_arguments_json, &plan, control)
}

pub(crate) fn execute_patch_with_plan(
    workspace_root: Option<&Path>,
    raw_arguments_json: &str,
    plan: &WorkspacePatchPlan,
    control: &ToolExecutionControl,
) -> Result<WorkspaceToolResult, WorkspaceToolError> {
    check_active(control)?;
    let root = workspace_root.ok_or(WorkspaceToolError::ExecutionFailed)?;
    let canonical_root = std::fs::canonicalize(root).map_err(map_io)?;
    let arguments_sha256: [u8; 32] = Sha256::digest(raw_arguments_json.as_bytes()).into();
    if plan.root != canonical_root || plan.arguments_sha256 != arguments_sha256 {
        return Err(WorkspaceToolError::ExecutionFailed);
    }
    with_workspace_mutation_locks(root, &plan.lock_paths, || {
        check_active(control)?;
        let mut snapshots = capture_and_verify_patch_targets(root, &plan.preconditions, control)?;
        validate_move_destination_parents(root, &plan.operations, control)?;
        execute_planned_patch_operations(root, plan, &mut snapshots, control)
    })
}

#[cfg(test)]
pub(super) fn execute_search_files(
    workspace_root: Option<&Path>,
    raw_arguments_json: &str,
) -> Result<WorkspaceToolResult, WorkspaceToolError> {
    execute_search_files_controlled(
        workspace_root,
        raw_arguments_json,
        &ToolExecutionControl::new(std::time::Instant::now() + std::time::Duration::from_secs(60)),
    )
}

pub(super) fn execute_search_files_controlled(
    workspace_root: Option<&Path>,
    raw_arguments_json: &str,
    control: &ToolExecutionControl,
) -> Result<WorkspaceToolResult, WorkspaceToolError> {
    check_active(control)?;
    let root = workspace_root.ok_or(WorkspaceToolError::ExecutionFailed)?;
    let input: SearchFilesInput = strict_json_object(raw_arguments_json)?;
    if input.pattern.is_empty()
        || input.pattern.len() > 2_048
        || input.pattern.chars().any(char::is_control)
        || input.path.len() > MAX_PATH_BYTES
        || !(1..=MAX_SEARCH_RESULTS).contains(&input.limit)
        || input.offset > MAX_SEARCH_OFFSET
        || input.context > 10
        || input.target == SearchTarget::Files
            && (input.file_glob.is_some()
                || input.context != 0
                || input.output_mode != SearchOutputMode::Content)
    {
        return Err(WorkspaceToolError::InvalidArguments);
    }
    let requested = parse_workspace_path(&input.path)?;
    check_active(control)?;
    if is_sensitive_path(&requested.relative) {
        return Err(WorkspaceToolError::ExecutionFailed);
    }
    let file_glob = input.file_glob.as_deref().map(build_glob).transpose()?;
    let regex = if input.target == SearchTarget::Content {
        Some(build_regex(&input.pattern)?)
    } else {
        None
    };
    let name_glob = if input.target == SearchTarget::Files {
        Some(build_glob(&input.pattern)?)
    } else {
        None
    };

    let workspace = open_workspace(root)?;
    let target = open_search_capability(&workspace, &requested.components)?;
    let mut traversal = SearchTraversal::new(
        SearchTraversalOptions {
            file_glob: file_glob.as_ref(),
            name_glob: name_glob.as_ref(),
            regex: regex.as_ref(),
            output_mode: input.output_mode,
            context: input.context,
        },
        input.offset,
        input.limit,
        control,
    );
    traversal.record_entry()?;
    match target {
        SearchCapability::Directory(directory) => {
            traversal.walk_directory(&directory, &requested.relative, 0)?;
        }
        SearchCapability::File(file) => {
            traversal.visit_file(file, &requested.relative)?;
        }
    }
    let omitted_sensitive_files = traversal.omitted_sensitive_files;
    let (mut items, mut truncated) = traversal.page.into_items();
    loop {
        check_active(control)?;
        let next_offset = if truncated {
            let next = input
                .offset
                .checked_add(items.len())
                .filter(|next| *next > input.offset)
                .ok_or(WorkspaceToolError::InvalidResult)?;
            Some(next)
        } else {
            None
        };
        let result = SearchFilesResult {
            target: match input.target {
                SearchTarget::Content => "content",
                SearchTarget::Files => "files",
            },
            returned: items.len(),
            next_offset,
            truncated,
            items: items.clone(),
            offset: input.offset,
            omitted_sensitive_files,
        };
        let serialized =
            serde_json::to_string(&result).map_err(|_| WorkspaceToolError::InvalidResult)?;
        if serialized.len() <= MAX_OUTPUT_BYTES {
            let summary_target = if input.target == SearchTarget::Files {
                "files"
            } else {
                "matches"
            };
            return Ok(WorkspaceToolResult {
                raw_result_json: serialized,
                input_summary: format!(
                    "Search {summary_target} in {}",
                    bounded_summary_path(&requested.display)
                ),
                result_summary: format!("{} {summary_target}", items.len()),
            });
        }
        if items.pop().is_none() {
            return Err(WorkspaceToolError::InvalidResult);
        }
        truncated = true;
    }
}

fn collect_content_matches(
    output: &mut SearchPage,
    path: &str,
    content: &str,
    regex: &Regex,
    mode: SearchOutputMode,
    context: usize,
    control: &ToolExecutionControl,
) -> Result<bool, WorkspaceToolError> {
    let lines: Vec<_> = content.lines().collect();
    match mode {
        SearchOutputMode::FilesOnly => {
            for line in &lines {
                check_active(control)?;
                if regex.is_match(line) {
                    return output.record(|| Ok(serde_json::json!({"path": path})));
                }
            }
        }
        SearchOutputMode::Count => {
            let mut count = 0usize;
            for line in &lines {
                check_active(control)?;
                if regex.is_match(line) {
                    count = count
                        .checked_add(1)
                        .ok_or(WorkspaceToolError::InvalidResult)?;
                }
            }
            if count > 0 {
                return output.record(|| Ok(serde_json::json!({"path": path, "count": count})));
            }
        }
        SearchOutputMode::Content => {
            for (index, line) in lines.iter().enumerate() {
                check_active(control)?;
                if !regex.is_match(line) {
                    continue;
                }
                let before_start = index.saturating_sub(context);
                let after_end = index.saturating_add(context + 1).min(lines.len());
                if output.record(|| {
                    serde_json::to_value(ContentMatch {
                        path: path.to_owned(),
                        line: index + 1,
                        text: bounded_line(line),
                        before: lines[before_start..index]
                            .iter()
                            .map(|line| bounded_line(line))
                            .collect(),
                        after: lines[index + 1..after_end]
                            .iter()
                            .map(|line| bounded_line(line))
                            .collect(),
                    })
                    .map_err(|_| WorkspaceToolError::InvalidResult)
                })? {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}

fn check_active(control: &ToolExecutionControl) -> Result<(), WorkspaceToolError> {
    control.check().map_err(|error| match error {
        ToolExecutionControlError::Cancelled => WorkspaceToolError::Cancelled,
        ToolExecutionControlError::DeadlineExceeded => WorkspaceToolError::DeadlineExceeded,
    })
}

fn parse_workspace_path(requested: &str) -> Result<WorkspacePath, WorkspaceToolError> {
    if requested.is_empty()
        || requested.len() > MAX_PATH_BYTES
        || requested.contains('\\')
        || requested.chars().any(char::is_control)
    {
        return Err(WorkspaceToolError::InvalidArguments);
    }
    let relative = Path::new(requested);
    if relative.is_absolute() {
        return Err(WorkspaceToolError::InvalidArguments);
    }
    let mut components = Vec::new();
    for component in relative.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(component) => {
                let component = component
                    .to_str()
                    .ok_or(WorkspaceToolError::InvalidArguments)?;
                if component.is_empty() || !is_portable_workspace_component(component) {
                    return Err(WorkspaceToolError::InvalidArguments);
                }
                components.push(component.to_owned());
            }
            _ => return Err(WorkspaceToolError::InvalidArguments),
        }
    }
    let display = if components.is_empty() {
        ".".to_owned()
    } else {
        components.join("/")
    };
    Ok(WorkspacePath {
        relative: components.iter().collect(),
        components,
        display,
    })
}

fn parse_write_file_input(
    raw_arguments_json: &str,
) -> Result<(WriteFileInput, WorkspacePath), WorkspaceToolError> {
    let input: WriteFileInput = strict_json_object(raw_arguments_json)?;
    if input.content.len() > MAX_WRITE_CONTENT_BYTES || input.content.contains('\0') {
        return Err(WorkspaceToolError::InvalidArguments);
    }
    let requested = parse_mutating_workspace_path(&input.path)?;
    validate_structured_write(&requested.relative, &input.content)?;
    Ok((input, requested))
}

fn parse_patch_input(raw_arguments_json: &str) -> Result<ParsedPatchInput, WorkspaceToolError> {
    let input: PatchInput = strict_json_object(raw_arguments_json)?;
    match input.mode {
        PatchMode::Replace => {
            if input.patch.is_some() {
                return Err(WorkspaceToolError::InvalidArguments);
            }
            let path = input.path.ok_or(WorkspaceToolError::InvalidArguments)?;
            let old_string = input
                .old_string
                .ok_or(WorkspaceToolError::InvalidArguments)?;
            let new_string = input
                .new_string
                .ok_or(WorkspaceToolError::InvalidArguments)?;
            if old_string.is_empty()
                || old_string == new_string
                || old_string.len() > MAX_PATCH_TEXT_BYTES
                || new_string.len() > MAX_PATCH_TEXT_BYTES
                || old_string.contains('\0')
                || new_string.contains('\0')
            {
                return Err(WorkspaceToolError::InvalidArguments);
            }
            Ok(ParsedPatchInput::Replace(ReplacePatchInput {
                path,
                old_string,
                new_string,
                replace_all: input.replace_all,
            }))
        }
        PatchMode::Patch => {
            if input.path.is_some()
                || input.old_string.is_some()
                || input.new_string.is_some()
                || input.replace_all
            {
                return Err(WorkspaceToolError::InvalidArguments);
            }
            let patch = input.patch.ok_or(WorkspaceToolError::InvalidArguments)?;
            if patch.is_empty() || patch.contains('\0') {
                return Err(WorkspaceToolError::InvalidArguments);
            }
            parse_v4a_patch(&patch)
                .map(ParsedPatchInput::V4a)
                .map_err(|_| WorkspaceToolError::InvalidArguments)
        }
    }
}

fn parse_mutating_workspace_path(path: &str) -> Result<WorkspacePath, WorkspaceToolError> {
    let requested = parse_workspace_path(path)?;
    if requested.components.is_empty() || is_sensitive_path(&requested.relative) {
        return Err(WorkspaceToolError::ExecutionFailed);
    }
    Ok(requested)
}

fn validate_v4a_paths(patch: &V4aPatch) -> Result<(), WorkspaceToolError> {
    for operation in &patch.operations {
        for path in v4a_operation_paths(operation) {
            parse_mutating_workspace_path(path)?;
        }
    }
    Ok(())
}

fn v4a_operation_paths(operation: &V4aOperation) -> Vec<&str> {
    match operation {
        V4aOperation::Add { path, .. }
        | V4aOperation::Update { path, .. }
        | V4aOperation::Delete { path } => vec![path.as_str()],
        V4aOperation::Move {
            source,
            destination,
        } => vec![source.as_str(), destination.as_str()],
    }
}

fn prepare_replace_patch_plan(
    root: &Path,
    input: ReplacePatchInput,
    control: &ToolExecutionControl,
) -> Result<WorkspacePatchPlan, WorkspaceToolError> {
    let requested = parse_mutating_workspace_path(&input.path)?;
    let snapshot = capture_workspace_file(root, &requested, control)?;
    let WorkspaceFileSnapshot::Existing {
        precondition,
        content,
        ..
    } = snapshot
    else {
        return Err(WorkspaceToolError::ExecutionFailed);
    };
    let (candidate, replacements) = prepare_patch_candidate(&input, &requested, &content, control)?;
    let diff = bounded_unified_diff(
        &requested.display,
        Some(&content),
        Some(candidate.as_bytes()),
    )?;
    enforce_patch_budget(&[content.len(), candidate.len(), diff.len()])?;
    Ok(WorkspacePatchPlan {
        root: PathBuf::new(),
        arguments_sha256: [0; 32],
        preconditions: vec![precondition],
        lock_paths: vec![requested.relative],
        operations: vec![PlannedPatchOperation::Update {
            path: requested.display,
            candidate: candidate.into_bytes(),
            diff,
            replacements,
        }],
    })
}

fn prepare_v4a_patch_plan(
    root: &Path,
    patch: V4aPatch,
    control: &ToolExecutionControl,
) -> Result<WorkspacePatchPlan, WorkspaceToolError> {
    validate_v4a_paths(&patch)?;
    let mut requested_paths = BTreeMap::new();
    let mut seen = BTreeSet::new();
    let mut result_path_bytes = 0usize;
    for operation in &patch.operations {
        for path in v4a_operation_paths(operation) {
            if !seen.insert(path.to_owned()) {
                return Err(WorkspaceToolError::InvalidArguments);
            }
            result_path_bytes = result_path_bytes
                .checked_add(path.len())
                .ok_or(WorkspaceToolError::InvalidArguments)?;
            if result_path_bytes > MAX_OUTPUT_BYTES / 4 {
                return Err(WorkspaceToolError::InvalidArguments);
            }
            requested_paths.insert(path.to_owned(), parse_mutating_workspace_path(path)?);
        }
    }

    let mut targets = BTreeMap::new();
    let mut aggregate_bytes = 0usize;
    for (path, requested) in requested_paths {
        check_active(control)?;
        let snapshot = capture_workspace_file(root, &requested, control)?;
        let (precondition, content) = match snapshot {
            WorkspaceFileSnapshot::Missing(precondition) => (precondition, None),
            WorkspaceFileSnapshot::Existing {
                precondition,
                content,
                ..
            } => {
                aggregate_bytes = checked_patch_budget_add(aggregate_bytes, content.len())?;
                (precondition, Some(content))
            }
        };
        targets.insert(
            path,
            PreparedPatchTarget {
                requested,
                precondition,
                content,
            },
        );
    }

    let mut operations = Vec::with_capacity(patch.operations.len());
    for operation in patch.operations {
        check_active(control)?;
        let planned = match operation {
            V4aOperation::Add { path, content } => {
                let target = targets
                    .get(path.as_str())
                    .ok_or(WorkspaceToolError::InvalidResult)?;
                if target.content.is_some() || content.contains('\0') {
                    return Err(WorkspaceToolError::ExecutionFailed);
                }
                validate_structured_write(&target.requested.relative, &content)?;
                if content.len() > MAX_PATCH_TARGET_BYTES {
                    return Err(WorkspaceToolError::InvalidArguments);
                }
                let diff = bounded_unified_diff(path.as_str(), None, Some(content.as_bytes()))?;
                aggregate_bytes = checked_patch_budget_add(aggregate_bytes, content.len())?;
                aggregate_bytes = checked_patch_budget_add(aggregate_bytes, diff.len())?;
                PlannedPatchOperation::Add {
                    path: path.as_str().to_owned(),
                    candidate: content.into_bytes(),
                    diff,
                }
            }
            V4aOperation::Update { path, hunks } => {
                let target = targets
                    .get(path.as_str())
                    .ok_or(WorkspaceToolError::InvalidResult)?;
                let content = target
                    .content
                    .as_deref()
                    .ok_or(WorkspaceToolError::ExecutionFailed)?;
                let (candidate, replacements) =
                    prepare_v4a_update_candidate(&hunks, &target.requested, content, control)?;
                let diff =
                    bounded_unified_diff(path.as_str(), Some(content), Some(candidate.as_bytes()))?;
                aggregate_bytes = checked_patch_budget_add(aggregate_bytes, candidate.len())?;
                aggregate_bytes = checked_patch_budget_add(aggregate_bytes, diff.len())?;
                PlannedPatchOperation::Update {
                    path: path.as_str().to_owned(),
                    candidate: candidate.into_bytes(),
                    diff,
                    replacements,
                }
            }
            V4aOperation::Delete { path } => {
                let target = targets
                    .get(path.as_str())
                    .ok_or(WorkspaceToolError::InvalidResult)?;
                let content = target
                    .content
                    .as_deref()
                    .ok_or(WorkspaceToolError::ExecutionFailed)?;
                validate_patch_text(content)?;
                let diff = bounded_unified_diff(path.as_str(), Some(content), None)?;
                aggregate_bytes = checked_patch_budget_add(aggregate_bytes, diff.len())?;
                PlannedPatchOperation::Delete {
                    path: path.as_str().to_owned(),
                    diff,
                }
            }
            V4aOperation::Move {
                source,
                destination,
            } => {
                let source_target = targets
                    .get(source.as_str())
                    .ok_or(WorkspaceToolError::InvalidResult)?;
                let source_content = source_target
                    .content
                    .as_deref()
                    .ok_or(WorkspaceToolError::ExecutionFailed)?;
                validate_patch_text(source_content)?;
                let destination_target = targets
                    .get(destination.as_str())
                    .ok_or(WorkspaceToolError::InvalidResult)?;
                if destination_target.content.is_some()
                    || !workspace_parent_exists(root, &destination_target.requested, control)?
                {
                    return Err(WorkspaceToolError::ExecutionFailed);
                }
                let diff = move_diff(source.as_str(), destination.as_str());
                aggregate_bytes = checked_patch_budget_add(aggregate_bytes, diff.len())?;
                PlannedPatchOperation::Move {
                    source: source.as_str().to_owned(),
                    destination: destination.as_str().to_owned(),
                    diff,
                }
            }
        };
        operations.push(planned);
    }

    let preconditions = targets
        .values()
        .map(|target| target.precondition.clone())
        .collect();
    let lock_paths = targets
        .values()
        .map(|target| target.requested.relative.clone())
        .collect();
    Ok(WorkspacePatchPlan {
        root: PathBuf::new(),
        arguments_sha256: [0; 32],
        preconditions,
        lock_paths,
        operations,
    })
}

fn checked_patch_budget_add(
    current: usize,
    additional: usize,
) -> Result<usize, WorkspaceToolError> {
    let total = current
        .checked_add(additional)
        .ok_or(WorkspaceToolError::InvalidArguments)?;
    if total > MAX_PATCH_AGGREGATE_BYTES {
        Err(WorkspaceToolError::InvalidArguments)
    } else {
        Ok(total)
    }
}

fn enforce_patch_budget(parts: &[usize]) -> Result<(), WorkspaceToolError> {
    let mut total = 0usize;
    for part in parts {
        total = checked_patch_budget_add(total, *part)?;
    }
    Ok(())
}

fn validate_patch_text(content: &[u8]) -> Result<(), WorkspaceToolError> {
    if content.contains(&0) {
        return Err(WorkspaceToolError::ExecutionFailed);
    }
    let content = content.strip_prefix(UTF8_BOM).unwrap_or(content);
    std::str::from_utf8(content)
        .map(|_| ())
        .map_err(|_| WorkspaceToolError::ExecutionFailed)
}

fn workspace_parent_exists(
    root: &Path,
    requested: &WorkspacePath,
    control: &ToolExecutionControl,
) -> Result<bool, WorkspaceToolError> {
    let (_, parent_components) = requested
        .components
        .split_last()
        .ok_or(WorkspaceToolError::InvalidArguments)?;
    let workspace = open_workspace(root)?;
    open_dir_components_if_exists(&workspace, parent_components, control)
        .map(|parent| parent.is_some())
}

fn prepare_patch_candidate(
    input: &ReplacePatchInput,
    requested: &WorkspacePath,
    original_bytes: &[u8],
    control: &ToolExecutionControl,
) -> Result<(String, usize), WorkspaceToolError> {
    check_active(control)?;
    if original_bytes.contains(&0) {
        return Err(WorkspaceToolError::ExecutionFailed);
    }
    let (had_bom, logical_bytes) = original_bytes
        .strip_prefix(UTF8_BOM)
        .map_or((false, original_bytes), |without_bom| (true, without_bom));
    let original =
        std::str::from_utf8(logical_bytes).map_err(|_| WorkspaceToolError::ExecutionFailed)?;
    let line_ending = detect_line_ending(original);
    let normalized_original = normalize_line_endings(original);
    let normalized_old = normalize_line_endings(&input.old_string);
    let normalized_new = normalize_line_endings(&input.new_string);
    if normalized_old == normalized_new {
        return Err(WorkspaceToolError::InvalidArguments);
    }
    let replacement = fuzzy_find_and_replace(
        &normalized_original,
        &normalized_old,
        &normalized_new,
        input.replace_all,
        control,
    )
    .map_err(map_fuzzy_error)?;
    let candidate = replacement.content;
    let replacements = replacement.match_count;
    validate_structured_write(&requested.relative, &candidate)?;
    let candidate = restore_line_endings(&candidate, line_ending);
    let mut on_disk = String::with_capacity(candidate.len().saturating_add(UTF8_BOM.len()));
    if had_bom {
        on_disk.push('\u{feff}');
    }
    on_disk.push_str(&candidate);
    if on_disk.len() > MAX_PATCH_TARGET_BYTES {
        return Err(WorkspaceToolError::InvalidArguments);
    }
    check_active(control)?;
    Ok((on_disk, replacements))
}

fn prepare_v4a_update_candidate(
    hunks: &[V4aHunk],
    requested: &WorkspacePath,
    original_bytes: &[u8],
    control: &ToolExecutionControl,
) -> Result<(String, usize), WorkspaceToolError> {
    check_active(control)?;
    validate_patch_text(original_bytes)?;
    let (had_bom, logical_bytes) = original_bytes
        .strip_prefix(UTF8_BOM)
        .map_or((false, original_bytes), |without_bom| (true, without_bom));
    let original =
        std::str::from_utf8(logical_bytes).map_err(|_| WorkspaceToolError::ExecutionFailed)?;
    let line_ending = detect_line_ending(original);
    let mut candidate = normalize_line_endings(original);
    let mut replacements = 0usize;

    for hunk in hunks {
        check_active(control)?;
        if !hunk.changes_content() {
            continue;
        }
        let mut search_lines = Vec::new();
        let mut replacement_lines = Vec::new();
        for line in &hunk.lines {
            match line {
                V4aHunkLine::Context(content) => {
                    search_lines.push(content.as_str());
                    replacement_lines.push(content.as_str());
                }
                V4aHunkLine::Remove(content) => search_lines.push(content.as_str()),
                V4aHunkLine::Add(content) => replacement_lines.push(content.as_str()),
            }
        }
        if search_lines.is_empty() {
            candidate = apply_addition_only_hunk(
                &candidate,
                hunk.context_hint.as_deref(),
                &replacement_lines.join("\n"),
                control,
            )?;
            replacements = replacements
                .checked_add(1)
                .ok_or(WorkspaceToolError::InvalidResult)?;
        } else {
            let replacement = fuzzy_find_and_replace(
                &candidate,
                &search_lines.join("\n"),
                &replacement_lines.join("\n"),
                false,
                control,
            )
            .map_err(map_fuzzy_error)?;
            candidate = replacement.content;
            replacements = replacements
                .checked_add(replacement.match_count)
                .ok_or(WorkspaceToolError::InvalidResult)?;
        }
        if candidate.len() > MAX_PATCH_TARGET_BYTES {
            return Err(WorkspaceToolError::InvalidArguments);
        }
    }
    if replacements == 0 {
        return Err(WorkspaceToolError::ExecutionFailed);
    }
    validate_structured_write(&requested.relative, &candidate)?;
    let candidate = restore_line_endings(&candidate, line_ending);
    let mut on_disk = String::with_capacity(candidate.len().saturating_add(UTF8_BOM.len()));
    if had_bom {
        on_disk.push('\u{feff}');
    }
    on_disk.push_str(&candidate);
    if on_disk.len() > MAX_PATCH_TARGET_BYTES {
        return Err(WorkspaceToolError::InvalidArguments);
    }
    Ok((on_disk, replacements))
}

fn apply_addition_only_hunk(
    content: &str,
    context_hint: Option<&str>,
    insertion: &str,
    control: &ToolExecutionControl,
) -> Result<String, WorkspaceToolError> {
    check_active(control)?;
    let output = if let Some(hint) = context_hint {
        if hint.is_empty() {
            return Err(WorkspaceToolError::InvalidArguments);
        }
        let matches = overlapping_match_positions(content, hint, control)?;
        if matches.len() != 1 {
            return Err(WorkspaceToolError::ExecutionFailed);
        }
        let hint_start = matches[0];
        if let Some(relative_eol) = content[hint_start..].find('\n') {
            let eol = hint_start + relative_eol + 1;
            format!("{}{}\n{}", &content[..eol], insertion, &content[eol..])
        } else {
            format!("{content}\n{insertion}")
        }
    } else {
        format!("{}\n{}\n", content.trim_end_matches('\n'), insertion)
    };
    if output.len() > MAX_PATCH_TARGET_BYTES {
        Err(WorkspaceToolError::InvalidArguments)
    } else {
        Ok(output)
    }
}

fn overlapping_match_positions(
    content: &str,
    pattern: &str,
    control: &ToolExecutionControl,
) -> Result<Vec<usize>, WorkspaceToolError> {
    let mut positions = Vec::new();
    let mut search_from = 0usize;
    while search_from <= content.len() {
        check_active(control)?;
        let Some(relative) = content[search_from..].find(pattern) else {
            break;
        };
        let position = search_from + relative;
        positions.push(position);
        if positions.len() > 1 {
            break;
        }
        let advance = content[position..]
            .chars()
            .next()
            .map(char::len_utf8)
            .ok_or(WorkspaceToolError::InvalidResult)?;
        search_from = position + advance;
    }
    Ok(positions)
}

fn detect_line_ending(content: &str) -> Option<LineEnding> {
    if content.contains("\r\n") {
        Some(LineEnding::CrLf)
    } else if content.contains('\n') {
        Some(LineEnding::Lf)
    } else if content.contains('\r') {
        Some(LineEnding::Cr)
    } else {
        None
    }
}

fn normalize_line_endings(content: &str) -> String {
    content.replace("\r\n", "\n").replace('\r', "\n")
}

fn restore_line_endings(content: &str, line_ending: Option<LineEnding>) -> String {
    match line_ending {
        Some(LineEnding::CrLf) => content.replace('\n', "\r\n"),
        Some(LineEnding::Cr) => content.replace('\n', "\r"),
        Some(LineEnding::Lf) | None => content.to_owned(),
    }
}

fn capture_workspace_file(
    root: &Path,
    requested: &WorkspacePath,
    control: &ToolExecutionControl,
) -> Result<WorkspaceFileSnapshot, WorkspaceToolError> {
    check_active(control)?;
    let (file_name, parent_components) = requested
        .components
        .split_last()
        .ok_or(WorkspaceToolError::InvalidArguments)?;
    let workspace = open_workspace(root)?;
    let Some(parent) = open_dir_components_if_exists(&workspace, parent_components, control)?
    else {
        return Ok(WorkspaceFileSnapshot::Missing(missing_file_precondition(
            requested,
        )));
    };
    let file = match open_file_nofollow_io(&parent, file_name) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(WorkspaceFileSnapshot::Missing(missing_file_precondition(
                requested,
            )));
        }
        Err(error) => return Err(map_io(error)),
    };
    let metadata = file.metadata().map_err(map_io)?;
    if !metadata.is_file() || metadata.len() > MAX_PATCH_TARGET_BYTES as u64 {
        return Err(WorkspaceToolError::ExecutionFailed);
    }
    let permissions = metadata.permissions();
    let content = read_bounded(file, MAX_PATCH_TARGET_BYTES as u64)?;
    check_active(control)?;
    let precondition = WorkspaceFilePrecondition {
        path: requested.display.clone(),
        state: WorkspaceFileState::Existing {
            sha256: format!("{:x}", Sha256::digest(&content)),
        },
    };
    Ok(WorkspaceFileSnapshot::Existing {
        precondition,
        parent,
        content,
        permissions,
    })
}

fn missing_file_precondition(requested: &WorkspacePath) -> WorkspaceFilePrecondition {
    WorkspaceFilePrecondition {
        path: requested.display.clone(),
        state: WorkspaceFileState::Missing,
    }
}

fn open_dir_components_if_exists(
    root: &Dir,
    components: &[String],
    control: &ToolExecutionControl,
) -> Result<Option<Dir>, WorkspaceToolError> {
    let mut current = root.try_clone().map_err(map_io)?;
    for component in components {
        check_active(control)?;
        current = match current.open_dir_nofollow(component) {
            Ok(directory) => directory,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(map_io(error)),
        };
    }
    Ok(Some(current))
}

fn capture_and_verify_patch_targets(
    root: &Path,
    preconditions: &[WorkspaceFilePrecondition],
    control: &ToolExecutionControl,
) -> Result<BTreeMap<String, WorkspaceFileSnapshot>, WorkspaceToolError> {
    let mut snapshots = BTreeMap::new();
    for expected in preconditions {
        check_active(control)?;
        let requested = parse_mutating_workspace_path(&expected.path)?;
        let snapshot = capture_workspace_file(root, &requested, control)?;
        if snapshot.precondition() != expected
            || snapshots.insert(expected.path.clone(), snapshot).is_some()
        {
            return Err(WorkspaceToolError::ExecutionFailed);
        }
    }
    Ok(snapshots)
}

fn validate_move_destination_parents(
    root: &Path,
    operations: &[PlannedPatchOperation],
    control: &ToolExecutionControl,
) -> Result<(), WorkspaceToolError> {
    for operation in operations {
        if let PlannedPatchOperation::Move { destination, .. } = operation {
            check_active(control)?;
            let requested = parse_mutating_workspace_path(destination)?;
            if !workspace_parent_exists(root, &requested, control)? {
                return Err(WorkspaceToolError::ExecutionFailed);
            }
        }
    }
    Ok(())
}

fn execute_planned_patch_operations(
    root: &Path,
    plan: &WorkspacePatchPlan,
    snapshots: &mut BTreeMap<String, WorkspaceFileSnapshot>,
    control: &ToolExecutionControl,
) -> Result<WorkspaceToolResult, WorkspaceToolError> {
    let mut state = PatchApplyState::default();
    for (index, operation) in plan.operations.iter().enumerate() {
        let applied = check_active(control)
            .and_then(|()| apply_planned_patch_operation(root, operation, snapshots, control));
        match applied {
            Ok(applied) => {
                append_bounded_diff(&mut state.diff, &applied.diff);
                state.files_modified.extend(applied.files_modified);
                state.files_created.extend(applied.files_created);
                state.files_deleted.extend(applied.files_deleted);
                state.replacements = state
                    .replacements
                    .checked_add(applied.replacements)
                    .ok_or(WorkspaceToolError::InvalidResult)?;
                state.bytes_written = state
                    .bytes_written
                    .checked_add(applied.bytes_written)
                    .ok_or(WorkspaceToolError::InvalidResult)?;
                state.applied += 1;
            }
            Err(error) if state.applied == 0 && is_control_error(error) => return Err(error),
            Err(error) => {
                let result = patch_result_from_state(
                    state,
                    false,
                    Some(format!(
                        "Apply phase failed at operation {} after {} committed; workspace state may be partial ({})",
                        index + 1,
                        index,
                        workspace_error_label(error)
                    )),
                );
                return patch_tool_output(
                    result,
                    format!(
                        "Patch apply failed at operation {}; workspace state may be partial",
                        index + 1
                    ),
                );
            }
        }
    }
    let applied = state.applied;
    let result = patch_result_from_state(state, true, None);
    patch_tool_output(result, format!("Applied {applied} patch operation(s)"))
}

fn is_control_error(error: WorkspaceToolError) -> bool {
    matches!(
        error,
        WorkspaceToolError::Cancelled | WorkspaceToolError::DeadlineExceeded
    )
}

fn workspace_error_label(error: WorkspaceToolError) -> &'static str {
    match error {
        WorkspaceToolError::InvalidArguments => "invalid operation",
        WorkspaceToolError::ExecutionFailed => "filesystem conflict",
        WorkspaceToolError::InvalidResult => "verification failure",
        WorkspaceToolError::Cancelled => "cancelled",
        WorkspaceToolError::DeadlineExceeded => "deadline exceeded",
    }
}

fn patch_result_from_state(
    state: PatchApplyState,
    success: bool,
    error: Option<String>,
) -> PatchResult {
    let path = if state.files_modified.len() == 1
        && state.files_created.is_empty()
        && state.files_deleted.is_empty()
    {
        state.files_modified.first().cloned()
    } else {
        None
    };
    PatchResult {
        success,
        path,
        diff: state.diff,
        files_modified: state.files_modified,
        files_created: state.files_created,
        files_deleted: state.files_deleted,
        replacements: (state.replacements > 0).then_some(state.replacements),
        bytes_written: (state.bytes_written > 0).then_some(state.bytes_written),
        error,
    }
}

fn patch_tool_output(
    result: PatchResult,
    result_summary: String,
) -> Result<WorkspaceToolResult, WorkspaceToolError> {
    let operation_count = result
        .files_modified
        .len()
        .saturating_add(result.files_created.len())
        .saturating_add(result.files_deleted.len());
    Ok(WorkspaceToolResult {
        raw_result_json: bounded_json(&result)?,
        input_summary: format!("Apply patch ({operation_count} affected path(s))"),
        result_summary,
    })
}

fn apply_planned_patch_operation(
    root: &Path,
    operation: &PlannedPatchOperation,
    snapshots: &mut BTreeMap<String, WorkspaceFileSnapshot>,
    control: &ToolExecutionControl,
) -> Result<AppliedPatchOperation, WorkspaceToolError> {
    match operation {
        PlannedPatchOperation::Add {
            path,
            candidate,
            diff,
        } => {
            let snapshot = snapshots
                .remove(path)
                .ok_or(WorkspaceToolError::InvalidResult)?;
            if !matches!(snapshot, WorkspaceFileSnapshot::Missing(_)) {
                return Err(WorkspaceToolError::ExecutionFailed);
            }
            let requested = parse_mutating_workspace_path(path)?;
            let (file_name, parent_components) = requested
                .components
                .split_last()
                .ok_or(WorkspaceToolError::InvalidArguments)?;
            let workspace = open_workspace(root)?;
            let parent = open_or_create_dir_components(&workspace, parent_components, control)?;
            atomic_create_file(&parent, file_name, candidate, control)?;
            Ok(AppliedPatchOperation {
                diff: diff.clone(),
                files_modified: Vec::new(),
                files_created: vec![path.clone()],
                files_deleted: Vec::new(),
                replacements: 0,
                bytes_written: candidate.len(),
            })
        }
        PlannedPatchOperation::Update {
            path,
            candidate,
            diff,
            replacements,
        } => {
            let snapshot = snapshots
                .remove(path)
                .ok_or(WorkspaceToolError::InvalidResult)?;
            let WorkspaceFileSnapshot::Existing {
                parent,
                content,
                permissions,
                ..
            } = snapshot
            else {
                return Err(WorkspaceToolError::ExecutionFailed);
            };
            let requested = parse_mutating_workspace_path(path)?;
            let (file_name, _) = requested
                .components
                .split_last()
                .ok_or(WorkspaceToolError::InvalidArguments)?;
            atomic_replace_file(
                &parent,
                file_name,
                candidate,
                permissions,
                &content,
                control,
            )?;
            Ok(AppliedPatchOperation {
                diff: diff.clone(),
                files_modified: vec![path.clone()],
                files_created: Vec::new(),
                files_deleted: Vec::new(),
                replacements: *replacements,
                bytes_written: candidate.len(),
            })
        }
        PlannedPatchOperation::Delete { path, diff } => {
            let snapshot = snapshots
                .remove(path)
                .ok_or(WorkspaceToolError::InvalidResult)?;
            let WorkspaceFileSnapshot::Existing {
                parent, content, ..
            } = snapshot
            else {
                return Err(WorkspaceToolError::ExecutionFailed);
            };
            let requested = parse_mutating_workspace_path(path)?;
            let (file_name, _) = requested
                .components
                .split_last()
                .ok_or(WorkspaceToolError::InvalidArguments)?;
            delete_file_checked(&parent, file_name, &content)?;
            Ok(AppliedPatchOperation {
                diff: diff.clone(),
                files_modified: Vec::new(),
                files_created: Vec::new(),
                files_deleted: vec![path.clone()],
                replacements: 0,
                bytes_written: 0,
            })
        }
        PlannedPatchOperation::Move {
            source,
            destination,
            diff,
        } => {
            let source_snapshot = snapshots
                .remove(source)
                .ok_or(WorkspaceToolError::InvalidResult)?;
            let destination_snapshot = snapshots
                .remove(destination)
                .ok_or(WorkspaceToolError::InvalidResult)?;
            let WorkspaceFileSnapshot::Existing {
                parent: source_parent,
                content,
                ..
            } = source_snapshot
            else {
                return Err(WorkspaceToolError::ExecutionFailed);
            };
            if !matches!(destination_snapshot, WorkspaceFileSnapshot::Missing(_)) {
                return Err(WorkspaceToolError::ExecutionFailed);
            }
            let source_requested = parse_mutating_workspace_path(source)?;
            let destination_requested = parse_mutating_workspace_path(destination)?;
            let (source_name, _) = source_requested
                .components
                .split_last()
                .ok_or(WorkspaceToolError::InvalidArguments)?;
            let (destination_name, destination_parents) = destination_requested
                .components
                .split_last()
                .ok_or(WorkspaceToolError::InvalidArguments)?;
            let workspace = open_workspace(root)?;
            let destination_parent = open_dir_components(&workspace, destination_parents)?;
            move_file_checked(
                &source_parent,
                source_name,
                &destination_parent,
                destination_name,
                &content,
            )?;
            Ok(AppliedPatchOperation {
                diff: diff.clone(),
                files_modified: vec![source.clone(), destination.clone()],
                files_created: Vec::new(),
                files_deleted: Vec::new(),
                replacements: 0,
                bytes_written: 0,
            })
        }
    }
}

fn verify_file_content(
    parent: &Dir,
    file_name: &str,
    expected: &[u8],
) -> Result<(), WorkspaceToolError> {
    let file = open_file_nofollow(parent, file_name)?;
    let metadata = file.metadata().map_err(map_io)?;
    let expected_len =
        u64::try_from(expected.len()).map_err(|_| WorkspaceToolError::InvalidResult)?;
    if !metadata.is_file() || metadata.len() != expected_len {
        return Err(WorkspaceToolError::ExecutionFailed);
    }
    if read_bounded(file, MAX_PATCH_TARGET_BYTES as u64)? != expected {
        return Err(WorkspaceToolError::ExecutionFailed);
    }
    Ok(())
}

fn delete_file_checked(
    parent: &Dir,
    file_name: &str,
    expected: &[u8],
) -> Result<(), WorkspaceToolError> {
    verify_file_content(parent, file_name, expected)?;
    parent.remove_file(file_name).map_err(map_io)?;
    match parent.symlink_metadata(file_name) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Ok(_) => Err(WorkspaceToolError::ExecutionFailed),
        Err(error) => Err(map_io(error)),
    }
}

fn move_file_checked(
    source_parent: &Dir,
    source_name: &str,
    destination_parent: &Dir,
    destination_name: &str,
    expected: &[u8],
) -> Result<(), WorkspaceToolError> {
    verify_file_content(source_parent, source_name, expected)?;
    match destination_parent.symlink_metadata(destination_name) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Ok(_) => return Err(WorkspaceToolError::ExecutionFailed),
        Err(error) => return Err(map_io(error)),
    }
    source_parent
        .hard_link(source_name, destination_parent, destination_name)
        .map_err(map_io)?;
    if let Err(error) = verify_file_content(destination_parent, destination_name, expected) {
        let _ = destination_parent.remove_file(destination_name);
        return Err(error);
    }
    source_parent.remove_file(source_name).map_err(map_io)?;
    match source_parent.symlink_metadata(source_name) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Ok(_) => return Err(WorkspaceToolError::ExecutionFailed),
        Err(error) => return Err(map_io(error)),
    }
    verify_file_content(destination_parent, destination_name, expected)
}

pub(super) fn with_workspace_mutation_locks<T>(
    root: &Path,
    relative_paths: &[PathBuf],
    operation: impl FnOnce() -> Result<T, WorkspaceToolError>,
) -> Result<T, WorkspaceToolError> {
    if relative_paths.is_empty() {
        return Err(WorkspaceToolError::InvalidArguments);
    }
    let canonical_root = std::fs::canonicalize(root).map_err(map_io)?;
    let mut keys = relative_paths
        .iter()
        .map(|relative| {
            normalize_lock_relative(relative).map(|relative| WorkspaceMutationKey {
                root: canonical_root.clone(),
                relative,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    keys.sort_unstable();
    keys.dedup();

    let registry = WORKSPACE_MUTATION_LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let locks = {
        let mut registered = registry
            .lock()
            .map_err(|_| WorkspaceToolError::ExecutionFailed)?;
        registered.retain(|_, weak| weak.strong_count() > 0);
        keys.iter()
            .map(|key| {
                if let Some(lock) = registered.get(key).and_then(Weak::upgrade) {
                    lock
                } else {
                    let lock = Arc::new(Mutex::new(()));
                    registered.insert(key.clone(), Arc::downgrade(&lock));
                    lock
                }
            })
            .collect::<Vec<_>>()
    };
    let mut guards = Vec::with_capacity(locks.len());
    for lock in &locks {
        guards.push(
            lock.lock()
                .map_err(|_| WorkspaceToolError::ExecutionFailed)?,
        );
    }
    let result = operation();
    drop(guards);
    drop(locks);
    if let Ok(mut registered) = registry.lock() {
        registered.retain(|_, weak| weak.strong_count() > 0);
    }
    result
}

fn normalize_lock_relative(relative: &Path) -> Result<PathBuf, WorkspaceToolError> {
    if relative.is_absolute() {
        return Err(WorkspaceToolError::InvalidArguments);
    }
    let mut normalized = PathBuf::new();
    for component in relative.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(component) => normalized.push(component),
            _ => return Err(WorkspaceToolError::InvalidArguments),
        }
    }
    if normalized.as_os_str().is_empty() {
        return Err(WorkspaceToolError::InvalidArguments);
    }
    Ok(normalized)
}

fn map_fuzzy_error(error: FuzzyError) -> WorkspaceToolError {
    match error {
        FuzzyError::Cancelled => WorkspaceToolError::Cancelled,
        FuzzyError::DeadlineExceeded => WorkspaceToolError::DeadlineExceeded,
        FuzzyError::EmptyOldString
        | FuzzyError::IdenticalStrings
        | FuzzyError::InputTooLarge
        | FuzzyError::ResultTooLarge => WorkspaceToolError::InvalidArguments,
        FuzzyError::Ambiguous { .. }
        | FuzzyError::NoMatch
        | FuzzyError::EscapeDrift
        | FuzzyError::OverlappingMatches
        | FuzzyError::ComplexityLimit => WorkspaceToolError::ExecutionFailed,
    }
}

fn patch_line_count(value: &str) -> usize {
    if value.is_empty() {
        0
    } else {
        value.matches('\n').count().saturating_add(1)
    }
}

fn bounded_unified_diff(
    path: &str,
    old: Option<&[u8]>,
    new: Option<&[u8]>,
) -> Result<String, WorkspaceToolError> {
    let old_text = diff_text(old.unwrap_or_default())?;
    let new_text = diff_text(new.unwrap_or_default())?;
    if old_text == new_text && old.is_some() == new.is_some() {
        return Err(WorkspaceToolError::InvalidResult);
    }
    let old_lines = old_text.split_inclusive('\n').collect::<Vec<_>>();
    let new_lines = new_text.split_inclusive('\n').collect::<Vec<_>>();
    let mut prefix = 0usize;
    while prefix < old_lines.len()
        && prefix < new_lines.len()
        && old_lines[prefix] == new_lines[prefix]
    {
        prefix += 1;
    }
    let mut suffix = 0usize;
    while suffix < old_lines.len().saturating_sub(prefix)
        && suffix < new_lines.len().saturating_sub(prefix)
        && old_lines[old_lines.len() - suffix - 1] == new_lines[new_lines.len() - suffix - 1]
    {
        suffix += 1;
    }
    let context_before = prefix.min(3);
    let context_after = suffix.min(3);
    let context_start = prefix - context_before;
    let old_change_end = old_lines.len() - suffix;
    let new_change_end = new_lines.len() - suffix;
    let old_count = old_change_end - context_start + context_after;
    let new_count = new_change_end - context_start + context_after;
    let old_start = if old_count == 0 {
        context_start
    } else {
        context_start + 1
    };
    let new_start = if new_count == 0 {
        context_start
    } else {
        context_start + 1
    };

    let from = if old.is_some() {
        format!("a/{path}")
    } else {
        "/dev/null".to_owned()
    };
    let to = if new.is_some() {
        format!("b/{path}")
    } else {
        "/dev/null".to_owned()
    };
    let mut diff =
        format!("--- {from}\n+++ {to}\n@@ -{old_start},{old_count} +{new_start},{new_count} @@\n");
    for line in &old_lines[context_start..prefix] {
        if !push_unified_diff_line(&mut diff, ' ', line) {
            return Ok(diff);
        }
    }
    for line in &old_lines[prefix..old_change_end] {
        if !push_unified_diff_line(&mut diff, '-', line) {
            return Ok(diff);
        }
    }
    for line in &new_lines[prefix..new_change_end] {
        if !push_unified_diff_line(&mut diff, '+', line) {
            return Ok(diff);
        }
    }
    for line in &old_lines[old_change_end..old_change_end + context_after] {
        if !push_unified_diff_line(&mut diff, ' ', line) {
            return Ok(diff);
        }
    }
    Ok(diff)
}

fn diff_text(bytes: &[u8]) -> Result<&str, WorkspaceToolError> {
    let bytes = bytes.strip_prefix(UTF8_BOM).unwrap_or(bytes);
    std::str::from_utf8(bytes).map_err(|_| WorkspaceToolError::ExecutionFailed)
}

fn push_unified_diff_line(target: &mut String, prefix: char, line: &str) -> bool {
    let needs_newline = !line.ends_with('\n');
    let required = 1usize
        .saturating_add(line.len())
        .saturating_add(usize::from(needs_newline));
    if target.len().saturating_add(required) <= MAX_PATCH_DIFF_BYTES {
        target.push(prefix);
        target.push_str(line);
        if needs_newline {
            target.push('\n');
        }
        true
    } else {
        let marker = "# diff truncated\n";
        if target.len().saturating_add(marker.len()) <= MAX_PATCH_DIFF_BYTES {
            target.push_str(marker);
        }
        false
    }
}

fn move_diff(source: &str, destination: &str) -> String {
    format!("rename from {source}\nrename to {destination}\n")
}

fn append_bounded_diff(target: &mut String, fragment: &str) {
    if target.len().saturating_add(fragment.len()) <= MAX_PATCH_DIFF_BYTES {
        target.push_str(fragment);
    } else if !target.ends_with("# diff truncated\n") {
        let marker = "# diff truncated\n";
        if target.len().saturating_add(marker.len()) <= MAX_PATCH_DIFF_BYTES {
            target.push_str(marker);
        }
    }
}

fn validate_structured_write(path: &Path, content: &str) -> Result<(), WorkspaceToolError> {
    let Some(extension) = path.extension().and_then(|extension| extension.to_str()) else {
        return Ok(());
    };
    match extension.to_ascii_lowercase().as_str() {
        "json" => serde_json::from_str::<JsonValue>(content)
            .map(|_| ())
            .map_err(|_| WorkspaceToolError::InvalidArguments),
        "yaml" | "yml" => {
            for document in serde_yaml_ng::Deserializer::from_str(content) {
                serde::de::IgnoredAny::deserialize(document)
                    .map_err(|_| WorkspaceToolError::InvalidArguments)?;
            }
            Ok(())
        }
        "toml" => content
            .parse::<toml_edit::DocumentMut>()
            .map(|_| ())
            .map_err(|_| WorkspaceToolError::InvalidArguments),
        _ => Ok(()),
    }
}

fn is_portable_workspace_component(component: &str) -> bool {
    if component.contains(':') || component.ends_with(' ') || component.ends_with('.') {
        return false;
    }
    let stem = component
        .split_once('.')
        .map_or(component, |(stem, _)| stem)
        .to_ascii_lowercase();
    !matches!(
        stem.as_str(),
        "con"
            | "prn"
            | "aux"
            | "nul"
            | "com1"
            | "com2"
            | "com3"
            | "com4"
            | "com5"
            | "com6"
            | "com7"
            | "com8"
            | "com9"
            | "lpt1"
            | "lpt2"
            | "lpt3"
            | "lpt4"
            | "lpt5"
            | "lpt6"
            | "lpt7"
            | "lpt8"
            | "lpt9"
            | "conin$"
            | "conout$"
    )
}

fn open_workspace(root: &Path) -> Result<Dir, WorkspaceToolError> {
    Dir::open_ambient_dir(root, ambient_authority()).map_err(map_io)
}

fn open_dir_components(root: &Dir, components: &[String]) -> Result<Dir, WorkspaceToolError> {
    let mut current = root.try_clone().map_err(map_io)?;
    for component in components {
        current = current.open_dir_nofollow(component).map_err(map_io)?;
    }
    Ok(current)
}

fn open_or_create_dir_components(
    root: &Dir,
    components: &[String],
    control: &ToolExecutionControl,
) -> Result<Dir, WorkspaceToolError> {
    let mut current = root.try_clone().map_err(map_io)?;
    for component in components {
        check_active(control)?;
        current = match current.open_dir_nofollow(component) {
            Ok(directory) => directory,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                match current.create_dir(component) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(map_io(error)),
                }
                current.open_dir_nofollow(component).map_err(map_io)?
            }
            Err(error) => return Err(map_io(error)),
        };
    }
    Ok(current)
}

#[cfg(test)]
fn atomic_write_file(
    parent: &Dir,
    file_name: &str,
    content: &[u8],
    permissions: Option<cap_std::fs::Permissions>,
    control: &ToolExecutionControl,
) -> Result<(), WorkspaceToolError> {
    atomic_write_file_inner(
        parent,
        file_name,
        content,
        permissions,
        AtomicWriteCommit::Replace,
        control,
    )
}

fn atomic_create_file(
    parent: &Dir,
    file_name: &str,
    content: &[u8],
    control: &ToolExecutionControl,
) -> Result<(), WorkspaceToolError> {
    atomic_write_file_inner(
        parent,
        file_name,
        content,
        None,
        AtomicWriteCommit::CreateIfMissing,
        control,
    )
}

fn atomic_replace_file(
    parent: &Dir,
    file_name: &str,
    content: &[u8],
    permissions: cap_std::fs::Permissions,
    expected_content: &[u8],
    control: &ToolExecutionControl,
) -> Result<(), WorkspaceToolError> {
    atomic_write_file_inner(
        parent,
        file_name,
        content,
        Some(permissions),
        AtomicWriteCommit::ReplaceIfUnchanged(expected_content),
        control,
    )
}

fn atomic_write_file_inner(
    parent: &Dir,
    file_name: &str,
    content: &[u8],
    permissions: Option<cap_std::fs::Permissions>,
    commit: AtomicWriteCommit<'_>,
    control: &ToolExecutionControl,
) -> Result<(), WorkspaceToolError> {
    check_active(control)?;
    let temp_name = format!(".synthchat-write-{}.tmp", uuid::Uuid::new_v4().simple());
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    options.follow(FollowSymlinks::No);
    let mut temp = parent.open_with(&temp_name, &options).map_err(map_io)?;
    let mut cleanup = TempFileCleanup {
        directory: parent,
        name: temp_name.clone(),
        armed: true,
    };
    let write_result = (|| {
        for chunk in content.chunks(WRITE_CHUNK_BYTES) {
            check_active(control)?;
            temp.write_all(chunk).map_err(map_io)?;
        }
        check_active(control)?;
        temp.flush().map_err(map_io)?;
        if let Some(permissions) = permissions {
            temp.set_permissions(permissions).map_err(map_io)?;
        }
        check_active(control)?;
        temp.sync_all().map_err(map_io)
    })();
    drop(temp);
    write_result?;
    match commit {
        #[cfg(test)]
        AtomicWriteCommit::Replace => {}
        AtomicWriteCommit::ReplaceIfUnchanged(expected_content) => {
            check_active(control)?;
            let current = open_file_nofollow(parent, file_name)?;
            let metadata = current.metadata().map_err(map_io)?;
            let expected_len = u64::try_from(expected_content.len())
                .map_err(|_| WorkspaceToolError::InvalidArguments)?;
            if !metadata.is_file() || metadata.len() != expected_len {
                return Err(WorkspaceToolError::ExecutionFailed);
            }
            let current = read_bounded(current, MAX_PATCH_TARGET_BYTES as u64)?;
            if current != expected_content {
                return Err(WorkspaceToolError::ExecutionFailed);
            }
        }
        AtomicWriteCommit::CreateIfMissing => match parent.symlink_metadata(file_name) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Ok(_) => return Err(WorkspaceToolError::ExecutionFailed),
            Err(error) => return Err(map_io(error)),
        },
    }
    check_active(control)?;
    match commit {
        AtomicWriteCommit::CreateIfMissing => {
            parent
                .hard_link(&temp_name, parent, file_name)
                .map_err(map_io)?;
            parent.remove_file(&temp_name).map_err(map_io)?;
        }
        #[cfg(test)]
        AtomicWriteCommit::Replace => {
            parent
                .rename(&temp_name, parent, file_name)
                .map_err(map_io)?;
        }
        AtomicWriteCommit::ReplaceIfUnchanged(_) => {
            parent
                .rename(&temp_name, parent, file_name)
                .map_err(map_io)?;
        }
    }
    cleanup.disarm();
    let verified = open_file_nofollow(parent, file_name)?;
    let metadata = verified.metadata().map_err(map_io)?;
    let content_len =
        u64::try_from(content.len()).map_err(|_| WorkspaceToolError::InvalidResult)?;
    if !metadata.is_file() || metadata.len() != content_len {
        return Err(WorkspaceToolError::ExecutionFailed);
    }
    if read_bounded(verified, content_len)? != content {
        return Err(WorkspaceToolError::ExecutionFailed);
    }
    Ok(())
}

fn open_file_nofollow<P: AsRef<Path>>(
    directory: &Dir,
    path: P,
) -> Result<CapFile, WorkspaceToolError> {
    open_file_nofollow_io(directory, path).map_err(map_io)
}

fn open_file_nofollow_io<P: AsRef<Path>>(directory: &Dir, path: P) -> io::Result<CapFile> {
    let mut options = OpenOptions::new();
    options.read(true);
    options.follow(FollowSymlinks::No);
    directory.open_with(path, &options)
}

fn open_search_capability(
    root: &Dir,
    components: &[String],
) -> Result<SearchCapability, WorkspaceToolError> {
    let Some((name, parents)) = components.split_last() else {
        return root
            .try_clone()
            .map(SearchCapability::Directory)
            .map_err(map_io);
    };
    let parent = open_dir_components(root, parents)?;
    if let Ok(directory) = parent.open_dir_nofollow(name) {
        return Ok(SearchCapability::Directory(directory));
    }
    let file = open_file_nofollow(&parent, name)?;
    if !file.metadata().map_err(map_io)?.is_file() {
        return Err(WorkspaceToolError::ExecutionFailed);
    }
    Ok(SearchCapability::File(file))
}

fn is_ignored_directory(name: &str) -> bool {
    matches!(name, "node_modules" | "target" | "__pycache__")
}

fn is_sensitive_path(path: &Path) -> bool {
    path.components().any(|component| match component {
        Component::CurDir => false,
        Component::Normal(component) => {
            is_sensitive_name(&component.to_string_lossy().to_ascii_lowercase())
        }
        _ => true,
    })
}

fn is_sensitive_name(name: &str) -> bool {
    name == ".env"
        || name.starts_with(".env.")
        || name.starts_with(".synthchat-write-")
        || matches!(
            name,
            ".git"
                | ".hg"
                | ".svn"
                | ".ssh"
                | ".aws"
                | ".gnupg"
                | ".synthchat"
                | ".hermes"
                | ".npmrc"
                | ".pypirc"
                | ".netrc"
                | "_netrc"
                | ".git-credentials"
                | "auth.json"
                | "config.yaml"
                | "config.yml"
                | "credentials"
                | "credentials.json"
                | "secrets.json"
                | "secrets.yaml"
                | "secrets.yml"
                | "id_rsa"
                | "id_ed25519"
                | "id_ecdsa"
                | "id_dsa"
        )
        || [
            ".pem",
            ".key",
            ".p12",
            ".pfx",
            ".kdbx",
            ".db",
            ".db-wal",
            ".db-shm",
            ".sqlite",
            ".sqlite-wal",
            ".sqlite-shm",
            ".sqlite3",
            ".sqlite3-wal",
            ".sqlite3-shm",
        ]
        .iter()
        .any(|suffix| name.ends_with(suffix))
}

fn display_relative(path: &Path) -> Result<String, WorkspaceToolError> {
    let mut parts = Vec::new();
    for component in path.components() {
        let Component::Normal(component) = component else {
            return Err(WorkspaceToolError::ExecutionFailed);
        };
        parts.push(
            component
                .to_str()
                .ok_or(WorkspaceToolError::ExecutionFailed)?,
        );
    }
    Ok(parts.join("/"))
}

fn build_regex(pattern: &str) -> Result<Regex, WorkspaceToolError> {
    RegexBuilder::new(pattern)
        .size_limit(2 * 1024 * 1024)
        .dfa_size_limit(2 * 1024 * 1024)
        .build()
        .map_err(|_| WorkspaceToolError::InvalidArguments)
}

fn build_glob(pattern: &str) -> Result<GlobMatcher, WorkspaceToolError> {
    if pattern.is_empty() || pattern.len() > 2_048 || pattern.chars().any(char::is_control) {
        return Err(WorkspaceToolError::InvalidArguments);
    }
    GlobBuilder::new(pattern)
        .literal_separator(false)
        .backslash_escape(false)
        .build()
        .map(|glob| glob.compile_matcher())
        .map_err(|_| WorkspaceToolError::InvalidArguments)
}

fn strict_json_object<T: for<'de> Deserialize<'de>>(raw: &str) -> Result<T, WorkspaceToolError> {
    if raw.is_empty() || raw.len() > MAX_ARGUMENT_BYTES {
        return Err(WorkspaceToolError::InvalidArguments);
    }
    let first: JsonValue =
        serde_json::from_str(raw).map_err(|_| WorkspaceToolError::InvalidArguments)?;
    if !first.is_object() {
        return Err(WorkspaceToolError::InvalidArguments);
    }
    serde_json::from_str(raw).map_err(|_| WorkspaceToolError::InvalidArguments)
}

fn read_bounded(file: CapFile, maximum: u64) -> Result<Vec<u8>, WorkspaceToolError> {
    let mut bytes = Vec::new();
    file.take(maximum.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(map_io)?;
    let length = u64::try_from(bytes.len()).map_err(|_| WorkspaceToolError::InvalidResult)?;
    if length > maximum {
        return Err(WorkspaceToolError::ExecutionFailed);
    }
    Ok(bytes)
}

fn bounded_json(value: &impl Serialize) -> Result<String, WorkspaceToolError> {
    let value = serde_json::to_string(value).map_err(|_| WorkspaceToolError::InvalidResult)?;
    if value.len() > MAX_OUTPUT_BYTES {
        Err(WorkspaceToolError::InvalidResult)
    } else {
        Ok(value)
    }
}

fn bounded_line(line: &str) -> String {
    let mut output: String = line.chars().take(MAX_LINE_CHARS).collect();
    if line.chars().count() > MAX_LINE_CHARS {
        output.push_str("...[truncated]");
    }
    output
}

fn bounded_summary_path(path: &str) -> String {
    path.chars().take(MAX_SUMMARY_PATH_CHARS).collect()
}

fn map_io(error: io::Error) -> WorkspaceToolError {
    match error.kind() {
        io::ErrorKind::InvalidInput => WorkspaceToolError::InvalidArguments,
        _ => WorkspaceToolError::ExecutionFailed,
    }
}

fn default_read_offset() -> usize {
    1
}

fn default_read_limit() -> usize {
    500
}

fn default_search_path() -> String {
    ".".to_owned()
}

fn default_search_limit() -> usize {
    50
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn read_file_is_line_numbered_paginated_and_workspace_scoped() {
        let home = tempfile::tempdir().unwrap();
        fs::create_dir_all(home.path().join("src")).unwrap();
        fs::write(home.path().join("src/main.rs"), "one\ntwo\nthree\n").unwrap();
        fs::write(home.path().join(".env"), "SECRET=value\n").unwrap();

        let output = execute_read_file(
            Some(home.path()),
            r#"{"path":"src/main.rs","offset":2,"limit":1}"#,
        )
        .unwrap();
        let result: JsonValue = serde_json::from_str(&output.raw_result_json).unwrap();
        assert_eq!(result["path"], "src/main.rs");
        assert_eq!(result["content"], "2|two\n");
        assert_eq!(result["nextOffset"], 3);
        assert_eq!(result["totalLines"], 3);
        assert_eq!(output.input_summary, "Read src/main.rs");
        for invalid in [
            r#"{"path":"../outside"}"#,
            r#"{"path":"/absolute"}"#,
            r#"{"path":".env"}"#,
            r#"{"path":"src/main.rs","offset":0}"#,
        ] {
            assert!(execute_read_file(Some(home.path()), invalid).is_err());
        }
        assert!(execute_read_file(None, r#"{"path":"src/main.rs"}"#).is_err());

        let bounded = home.path().join("bounded.txt");
        fs::write(&bounded, b"12345").unwrap();
        let workspace = open_workspace(home.path()).unwrap();
        let file = open_file_nofollow(&workspace, "bounded.txt").unwrap();
        assert_eq!(
            read_bounded(file, 4),
            Err(WorkspaceToolError::ExecutionFailed)
        );
    }

    #[test]
    fn write_file_creates_parents_and_atomically_replaces_workspace_text() {
        let home = TempDir::new().unwrap();
        let first = execute_write_file(
            Some(home.path()),
            r#"{"path":"src/new.txt","content":"first\n"}"#,
        )
        .unwrap();
        let first_result: JsonValue = serde_json::from_str(&first.raw_result_json).unwrap();
        assert_eq!(first_result["path"], "src/new.txt");
        assert_eq!(first_result["bytesWritten"], 6);
        assert_eq!(first_result["created"], true);
        assert_eq!(
            fs::read_to_string(home.path().join("src/new.txt")).unwrap(),
            "first\n"
        );
        assert_eq!(first.input_summary, "Write src/new.txt (6 bytes)");
        assert!(!first.raw_result_json.contains("first"));

        let second = execute_write_file(
            Some(home.path()),
            r#"{"path":"src/new.txt","content":"replacement"}"#,
        )
        .unwrap();
        let second_result: JsonValue = serde_json::from_str(&second.raw_result_json).unwrap();
        assert_eq!(second_result["created"], false);
        assert_eq!(
            fs::read_to_string(home.path().join("src/new.txt")).unwrap(),
            "replacement"
        );
        assert!(fs::read_dir(home.path().join("src")).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".synthchat-write-")
        }));

        for invalid in [
            r#"{"path":"../outside","content":"x"}"#,
            r#"{"path":"/absolute","content":"x"}"#,
            r#"{"path":".env","content":"x"}"#,
            r#"{"path":"src/new.txt","content":"x","extra":true}"#,
            r#"{"path":".","content":"x"}"#,
            r#"{"path":"safe.txt:stream","content":"x"}"#,
            r#"{"path":"trailing.","content":"x"}"#,
            r#"{"path":"trailing ","content":"x"}"#,
            r#"{"path":"CON.txt","content":"x"}"#,
            r#"{"path":"nested/.git/config","content":"x"}"#,
            r#"{"path":"nested/secrets.json","content":"{}"}"#,
            r#"{"path":"nested/state.sqlite3","content":"x"}"#,
        ] {
            assert!(execute_write_file(Some(home.path()), invalid).is_err());
        }
        assert!(execute_write_file(None, r#"{"path":"new.txt","content":"x"}"#).is_err());
        assert_eq!(
            summarize_write_file(r#"{"path":"safe.txt","content":"secret body"}"#).unwrap(),
            "Write safe.txt (11 bytes)"
        );
    }

    #[test]
    fn write_file_validates_structured_content_before_touching_disk() {
        let home = TempDir::new().unwrap();
        fs::create_dir(home.path().join("data")).unwrap();
        fs::write(home.path().join("data/existing.json"), r#"{"valid":true}"#).unwrap();

        for (path, content) in [
            ("data/existing.json", r#"{"broken":"#),
            ("new/nested.json", r#"[1, 2"#),
            ("new/nested.yaml", "key: [unterminated\n"),
            ("new/nested.yml", "key:\n\tinvalid: tab\n"),
            ("new/nested.toml", "key = \"unterminated\n"),
        ] {
            let arguments = serde_json::json!({"path": path, "content": content}).to_string();
            assert!(matches!(
                execute_write_file(Some(home.path()), &arguments),
                Err(WorkspaceToolError::InvalidArguments)
            ));
        }
        assert_eq!(
            fs::read_to_string(home.path().join("data/existing.json")).unwrap(),
            r#"{"valid":true}"#
        );
        assert!(!home.path().join("new").exists());

        for (path, content) in [
            ("data/valid.JSON", r#"{"items":[1,2,3]}"#),
            (
                "data/multi.yaml",
                "---\nkind: First\n---\nvalue: !Ref external-value\n",
            ),
            ("data/empty.yml", ""),
            (
                "data/valid.toml",
                "title = \"Hermes\"\n[tool]\nenabled = true\n",
            ),
            ("data/opaque.txt", r#"{"not":"complete"#),
        ] {
            let arguments = serde_json::json!({"path": path, "content": content}).to_string();
            execute_write_file(Some(home.path()), &arguments).unwrap();
            assert_eq!(fs::read_to_string(home.path().join(path)).unwrap(), content);
        }
    }

    #[test]
    fn write_file_enforces_content_bounds_preserves_permissions_and_cleans_failures() {
        let home = TempDir::new().unwrap();
        let bounded_path = home.path().join("bounded.txt");
        let maximum_content = "x".repeat(MAX_WRITE_CONTENT_BYTES);
        let maximum_arguments =
            serde_json::json!({"path": "bounded.txt", "content": maximum_content}).to_string();
        assert!(maximum_arguments.len() <= MAX_ARGUMENT_BYTES);
        execute_write_file(Some(home.path()), &maximum_arguments).unwrap();
        assert_eq!(fs::metadata(&bounded_path).unwrap().len(), 60 * 1024);

        let oversized_arguments = serde_json::json!({
            "path": "bounded.txt",
            "content": "y".repeat(MAX_WRITE_CONTENT_BYTES + 1),
        })
        .to_string();
        assert!(matches!(
            execute_write_file(Some(home.path()), &oversized_arguments),
            Err(WorkspaceToolError::InvalidArguments)
        ));
        assert_eq!(fs::metadata(&bounded_path).unwrap().len(), 60 * 1024);

        let nul_arguments =
            serde_json::json!({"path": "nul-content.txt", "content": "before\0after"}).to_string();
        assert!(matches!(
            execute_write_file(Some(home.path()), &nul_arguments),
            Err(WorkspaceToolError::InvalidArguments)
        ));
        assert!(!home.path().join("nul-content.txt").exists());

        #[cfg(windows)]
        #[allow(clippy::permissions_set_readonly_false)]
        {
            let permission_path = home.path().join("permissions.txt");
            fs::write(&permission_path, "old").unwrap();
            let mut permissions = fs::metadata(&permission_path).unwrap().permissions();
            permissions.set_readonly(true);
            fs::set_permissions(&permission_path, permissions).unwrap();
            assert!(matches!(
                execute_write_file(
                    Some(home.path()),
                    r#"{"path":"permissions.txt","content":"new"}"#,
                ),
                Err(WorkspaceToolError::ExecutionFailed)
            ));
            assert_eq!(fs::read_to_string(&permission_path).unwrap(), "old");
            assert!(
                fs::metadata(&permission_path)
                    .unwrap()
                    .permissions()
                    .readonly()
            );
            // Windows exposes the readonly file attribute through Permissions.
            let mut writable = fs::metadata(&permission_path).unwrap().permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            writable.set_readonly(false);
            fs::set_permissions(&permission_path, writable).unwrap();
        }

        fs::create_dir(home.path().join("occupied")).unwrap();
        let workspace = open_workspace(home.path()).unwrap();
        let control = ToolExecutionControl::new(
            std::time::Instant::now() + std::time::Duration::from_secs(60),
        );
        assert_eq!(
            atomic_write_file(&workspace, "occupied", b"replacement", None, &control),
            Err(WorkspaceToolError::ExecutionFailed)
        );
        assert!(home.path().join("occupied").is_dir());
        assert!(fs::read_dir(home.path()).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".synthchat-write-")
        }));
    }

    #[cfg(unix)]
    #[test]
    fn write_file_preserves_unix_mode_bits() {
        use std::os::unix::fs::PermissionsExt;

        let home = TempDir::new().unwrap();
        let path = home.path().join("executable.sh");
        fs::write(&path, "old\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o751)).unwrap();

        execute_write_file(
            Some(home.path()),
            r#"{"path":"executable.sh","content":"new\n"}"#,
        )
        .unwrap();
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o751
        );
    }

    #[test]
    fn workspace_preconditions_fail_closed_when_targets_change() {
        let home = TempDir::new().unwrap();
        let control = ToolExecutionControl::new(
            std::time::Instant::now() + std::time::Duration::from_secs(60),
        );

        let missing =
            prepare_workspace_file_precondition(Some(home.path()), "nested/new.txt", &control)
                .unwrap();
        assert_eq!(missing.path, "nested/new.txt");
        assert_eq!(missing.state, WorkspaceFileState::Missing);
        fs::create_dir(home.path().join("nested")).unwrap();
        fs::write(home.path().join("nested/new.txt"), "appeared").unwrap();
        assert!(verify_workspace_file_precondition(Some(home.path()), &missing, &control).is_err());

        let existing =
            prepare_workspace_file_precondition(Some(home.path()), "nested/new.txt", &control)
                .unwrap();
        let WorkspaceFileState::Existing { ref sha256 } = existing.state else {
            panic!("existing file must produce an existing precondition");
        };
        assert_eq!(sha256.len(), 64);
        assert!(!sha256.contains("appeared"));
        assert!(verify_workspace_file_precondition(Some(home.path()), &existing, &control).is_ok());
        fs::write(home.path().join("nested/new.txt"), "changed").unwrap();
        assert!(
            verify_workspace_file_precondition(Some(home.path()), &existing, &control).is_err()
        );

        let write_arguments = r#"{"path":"claim.txt","content":"approved"}"#;
        let write_precondition =
            prepare_write_file_precondition(Some(home.path()), write_arguments, &control).unwrap();
        fs::write(home.path().join("claim.txt"), "external").unwrap();
        assert!(matches!(
            execute_write_file_with_precondition(
                Some(home.path()),
                write_arguments,
                &write_precondition,
                &control,
            ),
            Err(WorkspaceToolError::ExecutionFailed)
        ));
        assert_eq!(
            fs::read_to_string(home.path().join("claim.txt")).unwrap(),
            "external"
        );

        let disappearing_arguments = r#"{"path":"gone.txt","content":"replacement"}"#;
        fs::write(home.path().join("gone.txt"), "original").unwrap();
        let disappearing =
            prepare_write_file_precondition(Some(home.path()), disappearing_arguments, &control)
                .unwrap();
        fs::remove_file(home.path().join("gone.txt")).unwrap();
        assert!(matches!(
            execute_write_file_with_precondition(
                Some(home.path()),
                disappearing_arguments,
                &disappearing,
                &control,
            ),
            Err(WorkspaceToolError::ExecutionFailed)
        ));
        assert!(!home.path().join("gone.txt").exists());

        fs::write(home.path().join("patch.txt"), "old").unwrap();
        let patch_arguments =
            r#"{"mode":"replace","path":"patch.txt","old_string":"old","new_string":"approved"}"#;
        let patch_plan = prepare_patch_plan(Some(home.path()), patch_arguments, &control).unwrap();
        fs::write(home.path().join("patch.txt"), "external").unwrap();
        assert!(matches!(
            execute_patch_with_plan(Some(home.path()), patch_arguments, &patch_plan, &control,),
            Err(WorkspaceToolError::ExecutionFailed)
        ));
        assert_eq!(
            fs::read_to_string(home.path().join("patch.txt")).unwrap(),
            "external"
        );
    }

    #[test]
    fn concurrent_missing_precondition_allows_exactly_one_create() {
        use std::sync::{Arc, Barrier};

        let home = TempDir::new().unwrap();
        let root = Arc::new(home.path().to_path_buf());
        let control = ToolExecutionControl::new(
            std::time::Instant::now() + std::time::Duration::from_secs(60),
        );
        let precondition = prepare_write_file_precondition(
            Some(root.as_path()),
            r#"{"path":"race.txt","content":"first"}"#,
            &control,
        )
        .unwrap();
        assert_eq!(precondition.state, WorkspaceFileState::Missing);
        let barrier = Arc::new(Barrier::new(2));
        let handles = ["first", "second"].map(|content| {
            let root = Arc::clone(&root);
            let barrier = Arc::clone(&barrier);
            let precondition = precondition.clone();
            std::thread::spawn(move || {
                let control = ToolExecutionControl::new(
                    std::time::Instant::now() + std::time::Duration::from_secs(60),
                );
                let arguments =
                    serde_json::json!({"path": "race.txt", "content": content}).to_string();
                barrier.wait();
                execute_write_file_with_precondition(
                    Some(root.as_path()),
                    &arguments,
                    &precondition,
                    &control,
                )
            })
        });
        let results = handles.map(|handle| handle.join().unwrap());
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(results.iter().filter(|result| result.is_err()).count(), 1);
        assert!(matches!(
            fs::read_to_string(home.path().join("race.txt"))
                .unwrap()
                .as_str(),
            "first" | "second"
        ));
    }

    #[test]
    fn patch_replace_requires_uniqueness_and_keeps_public_summaries_redacted() {
        let home = TempDir::new().unwrap();
        fs::write(
            home.path().join("unique.txt"),
            "before\nsecret-old\nafter\n",
        )
        .unwrap();
        let arguments = serde_json::json!({
            "mode": "replace",
            "path": "unique.txt",
            "old_string": "secret-old",
            "new_string": "secret-new",
        })
        .to_string();
        let summary = summarize_patch(&arguments).unwrap();
        assert!(!summary.contains("secret-old"));
        assert!(!summary.contains("secret-new"));
        let output = execute_patch(Some(home.path()), &arguments).unwrap();
        assert!(output.raw_result_json.contains("secret-old"));
        assert!(output.raw_result_json.contains("secret-new"));
        assert!(!output.input_summary.contains("secret-old"));
        assert!(!output.input_summary.contains("secret-new"));
        assert!(!output.result_summary.contains("secret-old"));
        assert!(!output.result_summary.contains("secret-new"));
        let result: JsonValue = serde_json::from_str(&output.raw_result_json).unwrap();
        assert_eq!(result["success"], true);
        assert_eq!(result["path"], "unique.txt");
        assert_eq!(result["filesModified"][0], "unique.txt");
        assert_eq!(result["replacements"], 1);
        assert_eq!(
            fs::read_to_string(home.path().join("unique.txt")).unwrap(),
            "before\nsecret-new\nafter\n"
        );

        fs::write(home.path().join("duplicate.txt"), "word word word").unwrap();
        let duplicate = serde_json::json!({
            "mode": "replace",
            "path": "duplicate.txt",
            "old_string": "word",
            "new_string": "changed",
        })
        .to_string();
        assert!(execute_patch(Some(home.path()), &duplicate).is_err());
        assert_eq!(
            fs::read_to_string(home.path().join("duplicate.txt")).unwrap(),
            "word word word"
        );
        let replace_all = serde_json::json!({
            "mode": "replace",
            "path": "duplicate.txt",
            "old_string": "word",
            "new_string": "changed",
            "replace_all": true,
        })
        .to_string();
        let output = execute_patch(Some(home.path()), &replace_all).unwrap();
        let result: JsonValue = serde_json::from_str(&output.raw_result_json).unwrap();
        assert_eq!(result["replacements"], 3);
        assert_eq!(
            fs::read_to_string(home.path().join("duplicate.txt")).unwrap(),
            "changed changed changed"
        );

        fs::write(home.path().join("overlap.txt"), "aaaa").unwrap();
        let overlap = serde_json::json!({
            "mode": "replace",
            "path": "overlap.txt",
            "old_string": "aa",
            "new_string": "b",
            "replace_all": true,
        })
        .to_string();
        let output = execute_patch(Some(home.path()), &overlap).unwrap();
        let result: JsonValue = serde_json::from_str(&output.raw_result_json).unwrap();
        assert_eq!(result["replacements"], 2);
        assert_eq!(
            fs::read_to_string(home.path().join("overlap.txt")).unwrap(),
            "bb"
        );
    }

    #[test]
    fn patch_schema_rejects_missing_cross_mode_null_and_unknown_fields() {
        let invalid = [
            serde_json::json!({}),
            serde_json::json!({"mode": "unknown"}),
            serde_json::json!({"mode": "replace"}),
            serde_json::json!({
                "mode": "replace",
                "path": null,
                "old_string": "old",
                "new_string": "new"
            }),
            serde_json::json!({
                "mode": "replace",
                "path": "file.txt",
                "old_string": null,
                "new_string": "new"
            }),
            serde_json::json!({
                "mode": "replace",
                "path": "file.txt",
                "old_string": "old",
                "new_string": "new",
                "patch": "*** Add File: other.txt\n+content"
            }),
            serde_json::json!({"mode": "patch"}),
            serde_json::json!({"mode": "patch", "patch": null}),
            serde_json::json!({
                "mode": "patch",
                "patch": "*** Add File: new.txt\n+content",
                "path": "other.txt"
            }),
            serde_json::json!({
                "mode": "patch",
                "patch": "*** Add File: new.txt\n+content",
                "replace_all": true
            }),
            serde_json::json!({
                "mode": "replace",
                "path": "file.txt",
                "old_string": "old",
                "new_string": "new",
                "unknown": true
            }),
        ];
        for arguments in invalid {
            assert!(matches!(
                summarize_patch(&arguments.to_string()),
                Err(WorkspaceToolError::InvalidArguments)
            ));
        }
    }

    #[test]
    fn patch_replace_supports_deletion_bom_crlf_and_rejects_creation_or_corruption() {
        let home = TempDir::new().unwrap();
        fs::write(
            home.path().join("windows.txt"),
            b"\xef\xbb\xbfbefore\r\nremove me\r\nafter\r\n",
        )
        .unwrap();
        let deletion = serde_json::json!({
            "mode": "replace",
            "path": "windows.txt",
            "old_string": "remove me\n",
            "new_string": "",
        })
        .to_string();
        execute_patch(Some(home.path()), &deletion).unwrap();
        assert_eq!(
            fs::read(home.path().join("windows.txt")).unwrap(),
            b"\xef\xbb\xbfbefore\r\nafter\r\n"
        );

        let missing = serde_json::json!({
            "mode": "replace",
            "path": "missing.txt",
            "old_string": "old",
            "new_string": "new",
        })
        .to_string();
        assert!(execute_patch(Some(home.path()), &missing).is_err());
        assert!(!home.path().join("missing.txt").exists());

        fs::write(home.path().join("data.json"), r#"{"key":"value"}"#).unwrap();
        let invalid_json = serde_json::json!({
            "mode": "replace",
            "path": "data.json",
            "old_string": "\"value\"",
            "new_string": "",
        })
        .to_string();
        assert!(execute_patch(Some(home.path()), &invalid_json).is_err());
        assert_eq!(
            fs::read_to_string(home.path().join("data.json")).unwrap(),
            r#"{"key":"value"}"#
        );

        for invalid in [
            serde_json::json!({
                "mode": "replace",
                "path": "data.json",
                "old_string": "",
                "new_string": "x",
            }),
            serde_json::json!({
                "mode": "replace",
                "path": "data.json",
                "old_string": "same",
                "new_string": "same",
            }),
            serde_json::json!({
                "mode": "patch",
                "patch": "*** Begin Patch\n*** End Patch",
            }),
        ] {
            assert!(execute_patch(Some(home.path()), &invalid.to_string()).is_err());
        }
    }

    #[test]
    fn v4a_plan_preflights_and_applies_all_operation_types() {
        let home = TempDir::new().unwrap();
        fs::write(home.path().join("update.txt"), "one\ntwo\nthree\n").unwrap();
        fs::write(home.path().join("delete.txt"), "old-delete-body\n").unwrap();
        fs::write(home.path().join("move-source.txt"), "move-body\n").unwrap();
        fs::create_dir(home.path().join("moved")).unwrap();
        let patch = "*** Begin Patch\n\
*** Update File: update.txt\n\
@@ update @@\n\
 one\n\
-two\n\
+changed\n\
 three\n\
*** Add File: created/new.txt\n\
+created-body\n\
*** Delete File: delete.txt\n\
*** Move File: move-source.txt -> moved/move-target.txt\n\
*** End Patch";
        let arguments = serde_json::json!({"mode": "patch", "patch": patch}).to_string();
        let control = ToolExecutionControl::new(
            std::time::Instant::now() + std::time::Duration::from_secs(60),
        );

        let summary = summarize_patch(&arguments).unwrap();
        assert_eq!(summary, "Apply V4A patch (4 operations)");
        assert!(!summary.contains("created-body"));
        let plan = prepare_patch_plan(Some(home.path()), &arguments, &control).unwrap();
        let debug = format!("{plan:?}");
        assert!(debug.contains("operation_count"));
        assert!(!debug.contains("created-body"));
        assert!(!home.path().join("created/new.txt").exists());
        assert!(home.path().join("delete.txt").exists());
        assert!(home.path().join("move-source.txt").exists());

        let output =
            execute_patch_with_plan(Some(home.path()), &arguments, &plan, &control).unwrap();
        let result: JsonValue = serde_json::from_str(&output.raw_result_json).unwrap();
        assert_eq!(result["success"], true);
        assert!(result["diff"].as_str().unwrap().contains("-two"));
        assert!(result["diff"].as_str().unwrap().contains("+changed"));
        assert!(result["diff"].as_str().unwrap().contains("created-body"));
        assert!(
            !output
                .raw_result_json
                .contains(&home.path().display().to_string())
        );
        assert_eq!(result["filesCreated"][0], "created/new.txt");
        assert_eq!(result["filesDeleted"][0], "delete.txt");
        assert!(
            result["filesModified"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!("update.txt"))
        );
        assert!(
            result["filesModified"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!("move-source.txt"))
        );
        assert_eq!(
            fs::read_to_string(home.path().join("update.txt")).unwrap(),
            "one\nchanged\nthree\n"
        );
        assert_eq!(
            fs::read_to_string(home.path().join("created/new.txt")).unwrap(),
            "created-body"
        );
        assert!(!home.path().join("delete.txt").exists());
        assert!(!home.path().join("move-source.txt").exists());
        assert_eq!(
            fs::read_to_string(home.path().join("moved/move-target.txt")).unwrap(),
            "move-body\n"
        );
    }

    #[test]
    fn v4a_update_simulates_hunks_and_handles_addition_only_rules() {
        let home = TempDir::new().unwrap();
        fs::write(home.path().join("sequence.txt"), "alpha\nanchor\ntail\n").unwrap();
        let patch = "*** Update File: sequence.txt\n\
@@\n\
-alpha\n\
+beta\n\
@@\n\
-beta\n\
+gamma\n\
@@ anchor @@\n\
+inserted\n\
@@\n\
+appended";
        let arguments = serde_json::json!({"mode": "patch", "patch": patch}).to_string();
        let output = execute_patch(Some(home.path()), &arguments).unwrap();
        let result: JsonValue = serde_json::from_str(&output.raw_result_json).unwrap();
        assert_eq!(result["success"], true);
        assert_eq!(
            fs::read_to_string(home.path().join("sequence.txt")).unwrap(),
            "gamma\nanchor\ninserted\ntail\nappended\n"
        );

        fs::write(home.path().join("missing-hint.txt"), "only\n").unwrap();
        let missing_hint = serde_json::json!({
            "mode": "patch",
            "patch": "*** Update File: missing-hint.txt\n@@ absent @@\n+never",
        })
        .to_string();
        assert!(
            prepare_patch_plan(
                Some(home.path()),
                &missing_hint,
                &ToolExecutionControl::new(
                    std::time::Instant::now() + std::time::Duration::from_secs(60),
                ),
            )
            .is_err()
        );
        assert_eq!(
            fs::read_to_string(home.path().join("missing-hint.txt")).unwrap(),
            "only\n"
        );

        fs::write(home.path().join("ambiguous-hint.txt"), "same\nsame\n").unwrap();
        let ambiguous_hint = serde_json::json!({
            "mode": "patch",
            "patch": "*** Update File: ambiguous-hint.txt\n@@ same @@\n+never",
        })
        .to_string();
        assert!(execute_patch(Some(home.path()), &ambiguous_hint).is_err());
        assert_eq!(
            fs::read_to_string(home.path().join("ambiguous-hint.txt")).unwrap(),
            "same\nsame\n"
        );
    }

    #[test]
    fn v4a_full_preflight_rejects_conflicts_without_writes() {
        let home = TempDir::new().unwrap();
        fs::write(home.path().join("first.txt"), "old\n").unwrap();
        fs::write(home.path().join("occupied.txt"), "occupied\n").unwrap();
        let invalid_add = serde_json::json!({
            "mode": "patch",
            "patch": "*** Update File: first.txt\n@@\n-old\n+new\n*** Add File: occupied.txt\n+overwrite",
        })
        .to_string();
        assert!(execute_patch(Some(home.path()), &invalid_add).is_err());
        assert_eq!(
            fs::read_to_string(home.path().join("first.txt")).unwrap(),
            "old\n"
        );
        assert_eq!(
            fs::read_to_string(home.path().join("occupied.txt")).unwrap(),
            "occupied\n"
        );

        let duplicate = serde_json::json!({
            "mode": "patch",
            "patch": "*** Update File: first.txt\n@@\n-old\n+one\n*** Update File: first.txt\n@@\n-old\n+two",
        })
        .to_string();
        assert!(execute_patch(Some(home.path()), &duplicate).is_err());
        assert_eq!(
            fs::read_to_string(home.path().join("first.txt")).unwrap(),
            "old\n"
        );

        fs::write(home.path().join("a.txt"), "a").unwrap();
        fs::write(home.path().join("b.txt"), "b").unwrap();
        let cycle = serde_json::json!({
            "mode": "patch",
            "patch": "*** Move File: a.txt -> b.txt\n*** Move File: b.txt -> a.txt",
        })
        .to_string();
        assert!(execute_patch(Some(home.path()), &cycle).is_err());
        assert_eq!(fs::read_to_string(home.path().join("a.txt")).unwrap(), "a");
        assert_eq!(fs::read_to_string(home.path().join("b.txt")).unwrap(), "b");

        let missing_parent = serde_json::json!({
            "mode": "patch",
            "patch": "*** Move File: a.txt -> absent/destination.txt",
        })
        .to_string();
        assert!(execute_patch(Some(home.path()), &missing_parent).is_err());
        assert!(!home.path().join("absent").exists());
    }

    #[test]
    fn v4a_plan_rechecks_all_targets_and_reports_partial_apply() {
        let home = TempDir::new().unwrap();
        fs::write(home.path().join("one.txt"), "old-one\n").unwrap();
        fs::write(home.path().join("two.txt"), "old-two\n").unwrap();
        let patch = "*** Update File: one.txt\n@@\n-old-one\n+new-one\n\
*** Update File: two.txt\n@@\n-old-two\n+new-two";
        let arguments = serde_json::json!({"mode": "patch", "patch": patch}).to_string();
        let control = ToolExecutionControl::new(
            std::time::Instant::now() + std::time::Duration::from_secs(60),
        );
        let plan = prepare_patch_plan(Some(home.path()), &arguments, &control).unwrap();
        fs::write(home.path().join("two.txt"), "external\n").unwrap();
        assert!(matches!(
            execute_patch_with_plan(Some(home.path()), &arguments, &plan, &control),
            Err(WorkspaceToolError::ExecutionFailed)
        ));
        assert_eq!(
            fs::read_to_string(home.path().join("one.txt")).unwrap(),
            "old-one\n"
        );
        assert_eq!(
            fs::read_to_string(home.path().join("two.txt")).unwrap(),
            "external\n"
        );

        fs::write(home.path().join("two.txt"), "old-two\n").unwrap();
        let mut partial_plan = prepare_patch_plan(Some(home.path()), &arguments, &control).unwrap();
        let PlannedPatchOperation::Update { path, .. } = &mut partial_plan.operations[1] else {
            panic!("second operation must be an update");
        };
        *path = "not-planned.txt".to_owned();
        let output =
            execute_patch_with_plan(Some(home.path()), &arguments, &partial_plan, &control)
                .unwrap();
        let result: JsonValue = serde_json::from_str(&output.raw_result_json).unwrap();
        assert_eq!(result["success"], false);
        assert!(result["error"].as_str().unwrap().contains("partial"));
        assert!(output.result_summary.contains("partial"));
        assert_eq!(
            fs::read_to_string(home.path().join("one.txt")).unwrap(),
            "new-one\n"
        );
        assert_eq!(
            fs::read_to_string(home.path().join("two.txt")).unwrap(),
            "old-two\n"
        );
    }

    #[test]
    fn v4a_preserves_bom_crlf_validates_structured_content_and_honors_control() {
        let home = TempDir::new().unwrap();
        fs::write(
            home.path().join("windows.txt"),
            b"\xef\xbb\xbfalpha\r\nbeta\r\n",
        )
        .unwrap();
        let patch = serde_json::json!({
            "mode": "patch",
            "patch": "*** Update File: windows.txt\n@@\n-beta\n+changed",
        })
        .to_string();
        execute_patch(Some(home.path()), &patch).unwrap();
        assert_eq!(
            fs::read(home.path().join("windows.txt")).unwrap(),
            b"\xef\xbb\xbfalpha\r\nchanged\r\n"
        );

        fs::write(home.path().join("data.json"), r#"{"key":"value"}"#).unwrap();
        let invalid_json = serde_json::json!({
            "mode": "patch",
            "patch": "*** Update File: data.json\n@@\n-\"value\"\n+",
        })
        .to_string();
        assert!(execute_patch(Some(home.path()), &invalid_json).is_err());
        assert_eq!(
            fs::read_to_string(home.path().join("data.json")).unwrap(),
            r#"{"key":"value"}"#
        );

        fs::write(home.path().join("settings.toml"), "key = \"value\"\n").unwrap();
        let invalid_toml = serde_json::json!({
            "mode": "patch",
            "patch": "*** Update File: settings.toml\n@@\n-key = \"value\"\n+key = \"unterminated",
        })
        .to_string();
        assert!(execute_patch(Some(home.path()), &invalid_toml).is_err());
        assert_eq!(
            fs::read_to_string(home.path().join("settings.toml")).unwrap(),
            "key = \"value\"\n"
        );

        fs::write(home.path().join("cancel.txt"), "old\n").unwrap();
        let controlled = serde_json::json!({
            "mode": "patch",
            "patch": "*** Update File: cancel.txt\n@@\n-old\n+new",
        })
        .to_string();
        let cancelled = ToolExecutionControl::new(
            std::time::Instant::now() + std::time::Duration::from_secs(60),
        );
        cancelled.cancel();
        assert!(matches!(
            prepare_patch_plan(Some(home.path()), &controlled, &cancelled),
            Err(WorkspaceToolError::Cancelled)
        ));
        let active = ToolExecutionControl::new(
            std::time::Instant::now() + std::time::Duration::from_secs(60),
        );
        let plan = prepare_patch_plan(Some(home.path()), &controlled, &active).unwrap();
        let expired = ToolExecutionControl::new(std::time::Instant::now());
        assert!(matches!(
            execute_patch_with_plan(Some(home.path()), &controlled, &plan, &expired),
            Err(WorkspaceToolError::DeadlineExceeded)
        ));
        assert_eq!(
            fs::read_to_string(home.path().join("cancel.txt")).unwrap(),
            "old\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn v4a_update_and_move_preserve_unix_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let home = TempDir::new().unwrap();
        fs::write(home.path().join("mode.txt"), "old\n").unwrap();
        fs::set_permissions(
            home.path().join("mode.txt"),
            fs::Permissions::from_mode(0o751),
        )
        .unwrap();
        fs::create_dir(home.path().join("dest")).unwrap();
        let patch = serde_json::json!({
            "mode": "patch",
            "patch": "*** Update File: mode.txt\n@@\n-old\n+new\n*** Move File: mode.txt -> dest/mode.txt",
        })
        .to_string();
        assert!(execute_patch(Some(home.path()), &patch).is_err());

        let update = serde_json::json!({
            "mode": "patch",
            "patch": "*** Update File: mode.txt\n@@\n-old\n+new",
        })
        .to_string();
        execute_patch(Some(home.path()), &update).unwrap();
        assert_eq!(
            fs::metadata(home.path().join("mode.txt"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o751
        );
        let move_patch = serde_json::json!({
            "mode": "patch",
            "patch": "*** Move File: mode.txt -> dest/mode.txt",
        })
        .to_string();
        execute_patch(Some(home.path()), &move_patch).unwrap();
        assert_eq!(
            fs::metadata(home.path().join("dest/mode.txt"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o751
        );
    }

    #[test]
    fn internal_temp_files_and_nonportable_paths_are_hidden_from_all_workspace_tools() {
        let home = TempDir::new().unwrap();
        fs::write(home.path().join("visible.txt"), "needle public\n").unwrap();
        fs::write(
            home.path().join(".synthchat-write-abandoned.tmp"),
            "needle staged-secret\n",
        )
        .unwrap();

        for path in [
            ".synthchat-write-abandoned.tmp",
            "visible.txt:stream",
            "visible.txt.",
            "CON.txt",
        ] {
            let read = serde_json::json!({"path": path}).to_string();
            assert!(execute_read_file(Some(home.path()), &read).is_err());
            let search = serde_json::json!({"pattern": "needle", "path": path}).to_string();
            assert!(execute_search_files(Some(home.path()), &search).is_err());
            let write = serde_json::json!({"path": path, "content": "replacement"}).to_string();
            assert!(execute_write_file(Some(home.path()), &write).is_err());
        }

        let output =
            execute_search_files(Some(home.path()), r#"{"pattern":"needle","path":"."}"#).unwrap();
        assert!(output.raw_result_json.contains("visible.txt"));
        assert!(!output.raw_result_json.contains("abandoned"));
        assert!(!output.raw_result_json.contains("staged-secret"));
    }

    #[cfg(unix)]
    #[test]
    fn symlink_files_and_directories_cannot_escape_the_workspace() {
        use std::os::unix::fs::symlink;

        let home = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        fs::write(home.path().join("visible.txt"), "needle public\n").unwrap();
        fs::write(outside.path().join("secret.txt"), "needle outside-secret\n").unwrap();
        symlink(
            outside.path().join("secret.txt"),
            home.path().join("file-link"),
        )
        .unwrap();
        symlink(outside.path(), home.path().join("directory-link")).unwrap();

        for path in ["file-link", "directory-link/secret.txt"] {
            assert!(
                execute_read_file(Some(home.path()), &format!(r#"{{"path":"{path}"}}"#)).is_err()
            );
        }
        for path in ["file-link", "directory-link"] {
            assert!(
                execute_search_files(
                    Some(home.path()),
                    &format!(r#"{{"pattern":"needle","path":"{path}"}}"#),
                )
                .is_err()
            );
        }

        let output =
            execute_search_files(Some(home.path()), r#"{"pattern":"needle","path":"."}"#).unwrap();
        assert!(output.raw_result_json.contains("visible.txt"));
        assert!(!output.raw_result_json.contains("outside-secret"));

        assert!(
            execute_write_file(
                Some(home.path()),
                r#"{"path":"file-link","content":"overwritten"}"#,
            )
            .is_err()
        );
        assert!(
            execute_patch(
                Some(home.path()),
                r#"{"mode":"replace","path":"file-link","old_string":"needle","new_string":"changed"}"#,
            )
            .is_err()
        );
        assert!(
            execute_patch(
                Some(home.path()),
                r#"{"mode":"replace","path":"directory-link/secret.txt","old_string":"needle","new_string":"changed"}"#,
            )
            .is_err()
        );
        assert!(
            execute_write_file(
                Some(home.path()),
                r#"{"path":"directory-link/escaped.txt","content":"escaped"}"#,
            )
            .is_err()
        );
        assert_eq!(
            fs::read_to_string(outside.path().join("secret.txt")).unwrap(),
            "needle outside-secret\n"
        );
        assert!(!outside.path().join("escaped.txt").exists());
        assert!(
            fs::symlink_metadata(home.path().join("file-link"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(
            fs::symlink_metadata(home.path().join("directory-link"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn search_files_supports_regex_glob_modes_and_omits_credentials() {
        let home = TempDir::new().unwrap();
        fs::create_dir_all(home.path().join("src")).unwrap();
        fs::write(
            home.path().join("src/main.rs"),
            "fn main() {\n    println!(\"needle\");\n}\n",
        )
        .unwrap();
        fs::write(home.path().join("src/lib.rs"), "// needle helper\n").unwrap();
        fs::write(home.path().join(".env"), "needle=secret\n").unwrap();

        let output = execute_search_files(
            Some(home.path()),
            r#"{"pattern":"needle","target":"content","path":".","file_glob":"*.rs","limit":10,"context":1}"#,
        )
        .unwrap();
        let result: JsonValue = serde_json::from_str(&output.raw_result_json).unwrap();
        assert_eq!(result["returned"], 2);
        assert_eq!(result["omittedSensitiveFiles"], 1);
        assert!(
            result["items"][0]["path"]
                .as_str()
                .unwrap()
                .starts_with("src/")
        );
        assert!(!output.raw_result_json.contains("secret"));
        assert!(!output.input_summary.contains("needle"));
        assert!(output.input_summary.chars().count() < 2_000);

        let files = execute_search_files(
            Some(home.path()),
            r#"{"pattern":"*.rs","target":"files","path":".","limit":10}"#,
        )
        .unwrap();
        let files: JsonValue = serde_json::from_str(&files.raw_result_json).unwrap();
        assert_eq!(files["returned"], 2);
        assert!(
            execute_search_files(Some(home.path()), r#"{"pattern":"(","target":"content"}"#,)
                .is_err()
        );
    }

    #[test]
    fn sensitive_roots_credentials_and_databases_are_never_read_or_searched() {
        let home = TempDir::new().unwrap();
        fs::write(home.path().join("visible.txt"), "needle public\n").unwrap();

        for directory in [".git", ".ssh", ".aws", ".gnupg", ".synthchat", ".hermes"] {
            fs::create_dir(home.path().join(directory)).unwrap();
            fs::write(
                home.path().join(directory).join("private.txt"),
                format!("needle {directory} private\n"),
            )
            .unwrap();
            assert!(
                execute_read_file(
                    Some(home.path()),
                    &format!(r#"{{"path":"{directory}/private.txt"}}"#),
                )
                .is_err()
            );
            assert!(
                execute_search_files(
                    Some(home.path()),
                    &format!(r#"{{"pattern":"needle","path":"{directory}"}}"#),
                )
                .is_err()
            );
        }

        let sensitive_files = [
            "auth.json",
            "config.yaml",
            ".git-credentials",
            "state.sqlite3",
            "state.db-wal",
        ];
        for file in sensitive_files {
            fs::write(home.path().join(file), format!("needle {file} private\n")).unwrap();
            assert!(
                execute_read_file(Some(home.path()), &format!(r#"{{"path":"{file}"}}"#),).is_err()
            );
            assert!(
                execute_search_files(
                    Some(home.path()),
                    &format!(r#"{{"pattern":"needle","path":"{file}"}}"#),
                )
                .is_err()
            );
        }

        let output = execute_search_files(
            Some(home.path()),
            r#"{"pattern":"needle","path":".","limit":20}"#,
        )
        .unwrap();
        let result: JsonValue = serde_json::from_str(&output.raw_result_json).unwrap();
        assert_eq!(result["returned"], 1);
        assert_eq!(result["items"][0]["path"], "visible.txt");
        assert_eq!(
            result["omittedSensitiveFiles"],
            u64::try_from(sensitive_files.len()).unwrap()
        );
        assert!(!output.raw_result_json.contains("private"));
    }

    #[test]
    fn content_search_keeps_only_one_page_and_advances_after_output_trimming() {
        let home = TempDir::new().unwrap();
        let many_matches = (0..10_020)
            .map(|index| format!("needle {index}\n"))
            .collect::<String>();
        fs::write(home.path().join("many.txt"), many_matches).unwrap();

        let page = execute_search_files(
            Some(home.path()),
            r#"{"pattern":"needle","offset":10000,"limit":2}"#,
        )
        .unwrap();
        let page: JsonValue = serde_json::from_str(&page.raw_result_json).unwrap();
        assert_eq!(page["returned"], 2);
        assert_eq!(page["nextOffset"], 10_002);
        assert_eq!(page["truncated"], true);
        assert_eq!(page["items"][0]["line"], 10_001);

        let wide_line = "x".repeat(MAX_LINE_CHARS);
        let wide_match = format!("needle{}", "x".repeat(MAX_LINE_CHARS - 6));
        let wide_content = (0..30)
            .map(|index| {
                if matches!(index, 5 | 20) {
                    wide_match.as_str()
                } else {
                    wide_line.as_str()
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(home.path().join("wide.txt"), wide_content).unwrap();

        let trimmed = execute_search_files(
            Some(home.path()),
            r#"{"pattern":"needle","path":"wide.txt","limit":2,"context":10}"#,
        )
        .unwrap();
        let trimmed: JsonValue = serde_json::from_str(&trimmed.raw_result_json).unwrap();
        assert_eq!(trimmed["returned"], 1);
        assert_eq!(trimmed["nextOffset"], 1);
        assert_eq!(trimmed["truncated"], true);
    }

    #[test]
    fn execution_control_rejects_cancelled_and_expired_operations() {
        let home = TempDir::new().unwrap();
        fs::write(home.path().join("visible.txt"), "needle public\n").unwrap();

        let cancelled = ToolExecutionControl::new(
            std::time::Instant::now() + std::time::Duration::from_secs(60),
        );
        cancelled.cancel();
        assert!(matches!(
            execute_read_file_controlled(
                Some(home.path()),
                r#"{"path":"visible.txt"}"#,
                &cancelled,
            ),
            Err(WorkspaceToolError::Cancelled)
        ));

        let expired = ToolExecutionControl::new(std::time::Instant::now());
        assert!(matches!(
            execute_search_files_controlled(Some(home.path()), r#"{"pattern":"needle"}"#, &expired,),
            Err(WorkspaceToolError::DeadlineExceeded)
        ));

        let write_cancelled = ToolExecutionControl::new(
            std::time::Instant::now() + std::time::Duration::from_secs(60),
        );
        fs::write(home.path().join("cancelled.txt"), "original").unwrap();
        write_cancelled.cancel();
        assert!(matches!(
            execute_write_file_controlled(
                Some(home.path()),
                r#"{"path":"cancelled.txt","content":"must not replace"}"#,
                &write_cancelled,
            ),
            Err(WorkspaceToolError::Cancelled)
        ));
        assert_eq!(
            fs::read_to_string(home.path().join("cancelled.txt")).unwrap(),
            "original"
        );

        let write_expired = ToolExecutionControl::new(std::time::Instant::now());
        assert!(matches!(
            execute_write_file_controlled(
                Some(home.path()),
                r#"{"path":"expired.txt","content":"must not exist"}"#,
                &write_expired,
            ),
            Err(WorkspaceToolError::DeadlineExceeded)
        ));
        assert!(!home.path().join("expired.txt").exists());
    }
}
