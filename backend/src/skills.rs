use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::{Component, Path, PathBuf},
    sync::Arc,
};

mod lifecycle;
mod runtime_config;

pub use runtime_config::{
    SKILL_GITHUB_API_BASE_URL_ENV, SKILL_GITHUB_RAW_BASE_URL_ENV, SKILL_REGISTRY_INDEX_URL_ENV,
    SkillRegistryRuntimeConfig, SkillRegistryRuntimeConfigError,
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Deserializer, Serialize, de};
use serde_json::Value as JsonValue;
use serde_yaml_ng::Value as YamlValue;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use thiserror::Error;
use time::OffsetDateTime;
use zeroize::Zeroizing;

use crate::{
    files::FileService,
    operations::Operation,
    profiles::{ProfileError, ProfileService, Versioned},
};

pub(crate) use lifecycle::{InstallSkill, LifecycleError, LifecycleStartError};

type HmacSha256 = Hmac<Sha256>;

const MAX_SKILL_FILE_BYTES: u64 = 1024 * 1024;
const MAX_SKILL_TOOL_FILE_BYTES: u64 = 48 * 1024;
const MAX_LOCK_FILE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_SCAN_ENTRIES: usize = 10_000;
const MAX_SCAN_DEPTH: usize = 8;
const MAX_CURSOR_BYTES: usize = 4096;
const MAX_CURSOR_AGE_SECONDS: i64 = 24 * 60 * 60;
const CURSOR_VERSION: u8 = 1;

const EXCLUDED_DIRECTORIES: &[&str] = &[
    ".git",
    ".github",
    ".hub",
    ".archive",
    ".venv",
    "venv",
    "node_modules",
    "site-packages",
    "__pycache__",
    ".tox",
    ".nox",
    ".pytest_cache",
    ".mypy_cache",
    ".ruff_cache",
];

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SkillSource {
    Bundled,
    File,
    Local,
    Registry,
    Url,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Skill {
    pub id: String,
    pub name: String,
    pub description: String,
    pub source: SkillSource,
    pub version: Option<String>,
    pub enabled: bool,
    pub uninstallable: bool,
    pub configurable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_schema: Option<JsonValue>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SkillPage {
    pub items: Vec<Skill>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SkillPatch {
    pub enabled: Option<bool>,
    pub config: Option<JsonValue>,
}

#[derive(Clone, Debug)]
pub struct ListSkills {
    pub query: Option<String>,
    pub cursor: Option<String>,
    pub limit: usize,
}

#[derive(Debug, Error)]
pub(crate) enum SkillError {
    #[error("invalid skill request")]
    InvalidRequest,
    #[error("invalid skill cursor")]
    InvalidCursor,
    #[error("skill not found")]
    NotFound,
    #[error("skill data is invalid")]
    DataInvalid,
    #[error("skill storage is unavailable")]
    StorageUnavailable,
    #[error(transparent)]
    Lifecycle(#[from] lifecycle::LifecycleStartError),
    #[error(transparent)]
    Profile(#[from] ProfileError),
}

#[derive(Clone)]
pub(crate) struct SkillService {
    profiles: Arc<ProfileService>,
    cursor: Arc<SkillCursorCodec>,
    lifecycle: lifecycle::SkillLifecycle,
}

#[derive(Clone)]
struct ScannedSkill {
    public: Skill,
    directory: PathBuf,
    managed: Option<ManagedSkillRecord>,
}

#[derive(Clone, Debug)]
pub(crate) struct ManagedManifestEntry {
    pub(crate) source: String,
    pub(crate) identifier: String,
    pub(crate) content_hash: String,
    pub(crate) install_path: String,
}

#[derive(Clone, Debug)]
pub(crate) struct ManagedSkillLocation {
    pub(crate) skill: Skill,
    pub(crate) lock_path: PathBuf,
    pub(crate) lock_revision: String,
    pub(crate) manifest_name: String,
    pub(crate) manifest: ManagedManifestEntry,
}

#[derive(Clone, Debug)]
struct ManagedSkillRecord {
    lock_path: PathBuf,
    lock_revision: String,
    manifest_name: String,
    manifest: ManagedManifestEntry,
}

#[derive(Clone, Debug)]
struct SkillProvenance {
    source: SkillSource,
    managed: ManagedSkillRecord,
}

pub(crate) struct SkillDocument {
    pub(crate) skill: Skill,
    pub(crate) file_path: String,
    pub(crate) content: String,
}

struct SkillSnapshot {
    skills: Vec<ScannedSkill>,
    config_etag: String,
    etag: String,
}

impl SkillService {
    #[cfg(test)]
    pub(crate) fn new(profiles: Arc<ProfileService>, desktop_token: &str) -> Self {
        let files = Arc::new(FileService::new(profiles.hermes_home()));
        Self::with_file_service(
            profiles,
            files,
            desktop_token,
            SkillRegistryRuntimeConfig::default(),
        )
    }

    pub(crate) fn with_file_service(
        profiles: Arc<ProfileService>,
        files: Arc<FileService>,
        desktop_token: &str,
        runtime_config: SkillRegistryRuntimeConfig,
    ) -> Self {
        Self {
            profiles: profiles.clone(),
            cursor: Arc::new(SkillCursorCodec::new(desktop_token)),
            lifecycle: lifecycle::SkillLifecycle::with_runtime_config(
                profiles,
                files,
                runtime_config,
            ),
        }
    }

    pub(crate) fn management_available(&self) -> bool {
        self.lifecycle.is_available()
    }

    pub(crate) async fn install(
        &self,
        profile_id: String,
        request: InstallSkill,
        idempotency_key: String,
        origin_request_id: String,
    ) -> Result<Operation, SkillError> {
        self.lifecycle
            .start_install(profile_id, request, idempotency_key, origin_request_id)
            .await
            .map_err(Into::into)
    }

    pub(crate) fn operation(&self, operation_id: &str) -> Result<Operation, SkillError> {
        self.lifecycle.operation(operation_id).map_err(Into::into)
    }

    pub(crate) fn uninstall(
        &self,
        profile_id: String,
        skill_id: &str,
        idempotency_key: String,
        origin_request_id: String,
    ) -> Result<Operation, SkillError> {
        if let Some(operation) =
            self.lifecycle
                .replay_uninstall(&profile_id, skill_id, &idempotency_key)?
        {
            return Ok(operation);
        }
        let location = match self.managed_skill(&profile_id, skill_id) {
            Ok(location) => location,
            Err(SkillError::NotFound) => {
                return self
                    .lifecycle
                    .replay_uninstall(&profile_id, skill_id, &idempotency_key)?
                    .ok_or(SkillError::NotFound);
            }
            Err(error) => return Err(error),
        };
        self.lifecycle
            .start_uninstall(profile_id, location, idempotency_key, origin_request_id)
            .map_err(Into::into)
    }

    pub(crate) fn list(
        &self,
        profile_id: &str,
        request: &ListSkills,
    ) -> Result<Versioned<SkillPage>, SkillError> {
        if request.limit == 0 || request.limit > 100 {
            return Err(SkillError::InvalidRequest);
        }
        let query = normalize_query(request.query.as_deref())?;
        let snapshot = self.scan(profile_id)?;
        let mut skills = snapshot.skills;
        if let Some(query) = query.as_deref() {
            skills.retain(|skill| matches_query(&skill.public, query));
        }
        skills.sort_by(|left, right| {
            left.public
                .name
                .to_lowercase()
                .cmp(&right.public.name.to_lowercase())
                .then_with(|| left.public.id.cmp(&right.public.id))
        });

        let fingerprint = skill_fingerprint(&skills);
        let filter_hash = SkillCursorCodec::filter_hash(profile_id, query.as_deref());
        let offset = match request.cursor.as_deref() {
            Some(cursor) => {
                let payload = self.cursor.decode(cursor, &filter_hash)?;
                if payload.fingerprint != fingerprint {
                    return Err(SkillError::InvalidCursor);
                }
                payload.offset
            }
            None => 0,
        };
        if offset > skills.len() {
            return Err(SkillError::InvalidCursor);
        }
        let end = offset.saturating_add(request.limit).min(skills.len());
        let items = skills[offset..end]
            .iter()
            .map(|skill| skill.public.clone())
            .collect();
        let next_cursor = if end < skills.len() {
            Some(self.cursor.encode(SkillCursorPayload {
                version: CURSOR_VERSION,
                filter_hash,
                fingerprint,
                offset: end,
                issued_at: 0,
            })?)
        } else {
            None
        };
        Ok(Versioned {
            value: SkillPage { items, next_cursor },
            etag: snapshot.etag,
        })
    }

    pub(crate) fn update(
        &self,
        profile_id: &str,
        skill_id: &str,
        patch: &SkillPatch,
        expected_etag: &str,
    ) -> Result<Versioned<Skill>, SkillError> {
        if patch.enabled.is_none() || patch.config.is_some() {
            return Err(SkillError::InvalidRequest);
        }
        let snapshot = self.scan(profile_id)?;
        if expected_etag != snapshot.etag {
            return Err(SkillError::Profile(ProfileError::RevisionConflict {
                current_etag: snapshot.etag,
            }));
        }
        let skill = snapshot
            .skills
            .into_iter()
            .find(|skill| skill.public.id == skill_id)
            .ok_or(SkillError::NotFound)?;
        let enabled = patch.enabled.expect("validated above");
        if let Err(error) = self.profiles.update_skill_enabled(
            profile_id,
            &skill.public.name,
            enabled,
            &snapshot.config_etag,
        ) {
            return match error {
                ProfileError::RevisionConflict { .. } => {
                    let current = self.scan(profile_id)?;
                    Err(SkillError::Profile(ProfileError::RevisionConflict {
                        current_etag: current.etag,
                    }))
                }
                error => Err(SkillError::Profile(error)),
            };
        }
        let updated = self.scan(profile_id)?;
        let skill = updated
            .skills
            .iter()
            .find(|candidate| candidate.public.id == skill_id)
            .ok_or(SkillError::NotFound)?
            .public
            .clone();
        Ok(Versioned {
            value: skill,
            etag: updated.etag,
        })
    }

    pub(crate) fn managed_skill(
        &self,
        profile_id: &str,
        skill_id: &str,
    ) -> Result<ManagedSkillLocation, SkillError> {
        let scanned = self
            .scan(profile_id)?
            .skills
            .into_iter()
            .find(|skill| skill.public.id == skill_id)
            .ok_or(SkillError::NotFound)?;
        let managed = scanned.managed.ok_or(SkillError::NotFound)?;
        Ok(ManagedSkillLocation {
            skill: scanned.public,
            lock_path: managed.lock_path,
            lock_revision: managed.lock_revision,
            manifest_name: managed.manifest_name,
            manifest: managed.manifest,
        })
    }

    pub(crate) fn enabled_for_tool(
        &self,
        profile_id: &str,
        query: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Skill>, SkillError> {
        if limit == 0 || limit > 20 {
            return Err(SkillError::InvalidRequest);
        }
        let query = normalize_query(query)?;
        let mut skills: Vec<_> = self
            .scan(profile_id)?
            .skills
            .into_iter()
            .map(|skill| skill.public)
            .filter(|skill| {
                skill.enabled
                    && query
                        .as_deref()
                        .is_none_or(|query| matches_query(skill, query))
            })
            .collect();
        skills.sort_by(|left, right| {
            left.name
                .to_lowercase()
                .cmp(&right.name.to_lowercase())
                .then_with(|| left.id.cmp(&right.id))
        });
        skills.truncate(limit);
        Ok(skills)
    }

    pub(crate) fn read_for_tool(
        &self,
        profile_id: &str,
        identifier: &str,
        requested_file: Option<&str>,
    ) -> Result<SkillDocument, SkillError> {
        if identifier.trim() != identifier
            || identifier.is_empty()
            || identifier.chars().count() > 256
            || identifier.chars().any(char::is_control)
        {
            return Err(SkillError::InvalidRequest);
        }
        let skill = self
            .scan(profile_id)?
            .skills
            .into_iter()
            .find(|skill| {
                skill.public.enabled
                    && (skill.public.id == identifier || skill.public.name == identifier)
            })
            .ok_or(SkillError::NotFound)?;
        let relative = requested_file.unwrap_or("SKILL.md");
        let (path, normalized) = resolve_skill_file(&skill.directory, relative)?;
        let metadata = fs::symlink_metadata(&path).map_err(|_| SkillError::NotFound)?;
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || metadata.len() > MAX_SKILL_TOOL_FILE_BYTES
        {
            return Err(SkillError::DataInvalid);
        }
        let bytes = fs::read(&path).map_err(|_| SkillError::StorageUnavailable)?;
        let content = String::from_utf8(bytes).map_err(|_| SkillError::DataInvalid)?;
        Ok(SkillDocument {
            skill: skill.public,
            file_path: normalized,
            content,
        })
    }

    fn scan(&self, profile_id: &str) -> Result<SkillSnapshot, SkillError> {
        let (root, settings) = self.profiles.skill_root_and_settings(profile_id)?;
        match fs::symlink_metadata(&root) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let fingerprint = skill_fingerprint(&[]);
                return Ok(SkillSnapshot {
                    skills: Vec::new(),
                    etag: composite_skill_etag(&settings.etag, &fingerprint),
                    config_etag: settings.etag,
                });
            }
            Err(_) => return Err(SkillError::StorageUnavailable),
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(SkillError::StorageUnavailable);
            }
            Ok(_) => {}
        }

        let provenance = load_provenance(&root);
        let mut candidates = Vec::new();
        let mut visited = 0;
        collect_skill_files(&root, &root, 0, &mut visited, &mut candidates)?;
        let mut ids = BTreeSet::new();
        let mut names = BTreeSet::new();
        let mut skills = Vec::with_capacity(candidates.len());
        for skill_file in candidates {
            let relative_directory = skill_file
                .parent()
                .and_then(|parent| parent.strip_prefix(&root).ok())
                .ok_or(SkillError::DataInvalid)?;
            let relative = normalized_relative_path(relative_directory)?;
            let metadata = parse_skill_file(&skill_file, relative_directory)?;
            let id = skill_id(&relative);
            if !ids.insert(id.clone()) || !names.insert(metadata.name.clone()) {
                continue;
            }
            let managed = provenance
                .get(&relative)
                .filter(|entry| entry.managed.manifest_name == metadata.name)
                .cloned();
            let source = managed
                .as_ref()
                .map_or(SkillSource::Local, |entry| entry.source);
            let enabled = !settings.value.disabled.contains(&metadata.name);
            skills.push(ScannedSkill {
                public: Skill {
                    id,
                    name: metadata.name,
                    description: metadata.description,
                    source,
                    version: metadata.version,
                    enabled,
                    uninstallable: managed.is_some(),
                    configurable: metadata.config_schema.is_some(),
                    config_schema: metadata.config_schema,
                },
                directory: skill_file
                    .parent()
                    .ok_or(SkillError::DataInvalid)?
                    .to_owned(),
                managed: managed.map(|entry| entry.managed),
            });
        }
        let fingerprint = skill_fingerprint(&skills);
        Ok(SkillSnapshot {
            skills,
            etag: composite_skill_etag(&settings.etag, &fingerprint),
            config_etag: settings.etag,
        })
    }
}

fn resolve_skill_file(root: &Path, requested: &str) -> Result<(PathBuf, String), SkillError> {
    if requested.is_empty()
        || requested.len() > 1_024
        || requested.contains('\\')
        || requested.chars().any(char::is_control)
    {
        return Err(SkillError::InvalidRequest);
    }
    let relative = Path::new(requested);
    if relative.is_absolute() {
        return Err(SkillError::InvalidRequest);
    }
    let normalized = normalized_relative_path(relative).map_err(|_| SkillError::InvalidRequest)?;
    let canonical_root = fs::canonicalize(root).map_err(|_| SkillError::StorageUnavailable)?;
    let mut current = root.to_owned();
    let components: Vec<_> = relative.components().collect();
    for (index, component) in components.iter().enumerate() {
        let Component::Normal(component) = component else {
            return Err(SkillError::InvalidRequest);
        };
        current.push(component);
        let metadata = fs::symlink_metadata(&current).map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                SkillError::NotFound
            } else {
                SkillError::StorageUnavailable
            }
        })?;
        if metadata.file_type().is_symlink() || (index + 1 < components.len() && !metadata.is_dir())
        {
            return Err(SkillError::DataInvalid);
        }
    }
    let canonical_file = fs::canonicalize(&current).map_err(|_| SkillError::NotFound)?;
    if !canonical_file.starts_with(&canonical_root) || canonical_file == canonical_root {
        return Err(SkillError::DataInvalid);
    }
    Ok((current, normalized))
}

struct ParsedMetadata {
    name: String,
    description: String,
    version: Option<String>,
    config_schema: Option<JsonValue>,
}

fn collect_skill_files(
    root: &Path,
    directory: &Path,
    depth: usize,
    visited: &mut usize,
    output: &mut Vec<PathBuf>,
) -> Result<(), SkillError> {
    if depth > MAX_SCAN_DEPTH {
        return Ok(());
    }
    let metadata = fs::symlink_metadata(directory).map_err(|_| SkillError::StorageUnavailable)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Ok(());
    }
    let skill_file = directory.join("SKILL.md");
    match fs::symlink_metadata(&skill_file) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err(SkillError::DataInvalid);
        }
        Ok(_) => {
            if directory != root {
                output.push(skill_file);
            }
            return Ok(());
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(_) => return Err(SkillError::StorageUnavailable),
    }

    let mut entries = fs::read_dir(directory)
        .map_err(|_| SkillError::StorageUnavailable)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| SkillError::StorageUnavailable)?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        *visited = visited.saturating_add(1);
        if *visited > MAX_SCAN_ENTRIES {
            return Err(SkillError::DataInvalid);
        }
        let file_name = entry.file_name();
        let Some(file_name) = file_name.to_str() else {
            continue;
        };
        if file_name.starts_with('.') || EXCLUDED_DIRECTORIES.contains(&file_name) {
            continue;
        }
        let metadata = entry
            .metadata()
            .map_err(|_| SkillError::StorageUnavailable)?;
        let file_type = entry
            .file_type()
            .map_err(|_| SkillError::StorageUnavailable)?;
        if file_type.is_symlink() || !metadata.is_dir() {
            continue;
        }
        collect_skill_files(root, &entry.path(), depth + 1, visited, output)?;
    }
    Ok(())
}

fn parse_skill_file(path: &Path, relative_directory: &Path) -> Result<ParsedMetadata, SkillError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| SkillError::StorageUnavailable)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > MAX_SKILL_FILE_BYTES
    {
        return Err(SkillError::DataInvalid);
    }
    let bytes = fs::read(path).map_err(|_| SkillError::StorageUnavailable)?;
    parse_skill_bytes(&bytes, relative_directory)
}

fn parse_skill_bytes(
    bytes: &[u8],
    relative_directory: &Path,
) -> Result<ParsedMetadata, SkillError> {
    if bytes.len() as u64 > MAX_SKILL_FILE_BYTES {
        return Err(SkillError::DataInvalid);
    }
    let content = std::str::from_utf8(bytes).map_err(|_| SkillError::DataInvalid)?;
    let fallback_name = relative_directory
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(SkillError::DataInvalid)?;
    let (frontmatter, body) = parse_frontmatter(content)?;
    let name = frontmatter
        .as_ref()
        .and_then(|mapping| yaml_string(mapping, "name"))
        .unwrap_or_else(|| fallback_heading(body).unwrap_or_else(|| fallback_name.to_owned()));
    validate_public_text(&name, 128)?;
    let description = frontmatter
        .as_ref()
        .and_then(|mapping| yaml_string(mapping, "description"))
        .unwrap_or_else(|| fallback_paragraph(body).unwrap_or_default());
    validate_public_text(&description, 2_000)?;
    let version = frontmatter
        .as_ref()
        .and_then(|mapping| yaml_scalar_string(mapping, "version"));
    if let Some(version) = version.as_deref() {
        validate_public_text(version, 128)?;
    }
    let config_schema = frontmatter
        .as_ref()
        .and_then(|mapping| {
            mapping
                .get(YamlValue::String("config_schema".to_owned()))
                .or_else(|| mapping.get(YamlValue::String("configSchema".to_owned())))
        })
        .map(|value| serde_json::to_value(value).map_err(|_| SkillError::DataInvalid))
        .transpose()?;
    if config_schema
        .as_ref()
        .is_some_and(|schema| !schema.is_object())
    {
        return Err(SkillError::DataInvalid);
    }
    Ok(ParsedMetadata {
        name,
        description,
        version,
        config_schema,
    })
}

fn parse_frontmatter(content: &str) -> Result<(Option<serde_yaml_ng::Mapping>, &str), SkillError> {
    if !content.starts_with("---") {
        return Ok((None, content));
    }
    let mut offset = 0;
    for line in content.split_inclusive('\n') {
        let line_start = offset;
        offset += line.len();
        if line_start == 0 {
            continue;
        }
        if line.trim_end_matches(['\r', '\n']).trim() == "---" {
            let yaml = &content[3..line_start];
            let value: YamlValue =
                serde_yaml_ng::from_str(yaml).map_err(|_| SkillError::DataInvalid)?;
            let mapping = value.as_mapping().cloned().ok_or(SkillError::DataInvalid)?;
            return Ok((Some(mapping), &content[offset..]));
        }
    }
    Err(SkillError::DataInvalid)
}

fn yaml_string(mapping: &serde_yaml_ng::Mapping, key: &str) -> Option<String> {
    mapping
        .get(YamlValue::String(key.to_owned()))
        .and_then(YamlValue::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn yaml_scalar_string(mapping: &serde_yaml_ng::Mapping, key: &str) -> Option<String> {
    match mapping.get(YamlValue::String(key.to_owned()))? {
        YamlValue::String(value) => Some(value.trim().to_owned()),
        YamlValue::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn fallback_heading(body: &str) -> Option<String> {
    body.lines()
        .find_map(|line| line.strip_prefix("# ").map(str::trim))
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn fallback_paragraph(body: &str) -> Option<String> {
    body.lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('#'))
        .map(ToOwned::to_owned)
}

fn validate_public_text(value: &str, maximum_chars: usize) -> Result<(), SkillError> {
    if value.chars().count() > maximum_chars || value.chars().any(char::is_control) {
        Err(SkillError::DataInvalid)
    } else {
        Ok(())
    }
}

fn normalized_relative_path(path: &Path) -> Result<String, SkillError> {
    let mut parts = Vec::new();
    for component in path.components() {
        let Component::Normal(value) = component else {
            return Err(SkillError::DataInvalid);
        };
        let value = value.to_str().ok_or(SkillError::DataInvalid)?;
        if value.is_empty() || matches!(value, "." | "..") {
            return Err(SkillError::DataInvalid);
        }
        parts.push(value);
    }
    if parts.is_empty() {
        return Err(SkillError::DataInvalid);
    }
    Ok(parts.join("/"))
}

fn skill_id(relative: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"synthchat-skill-id-v1\0");
    digest.update(relative.as_bytes());
    format!("skill_{}", &hex(&digest.finalize())[..32])
}

fn normalize_query(query: Option<&str>) -> Result<Option<String>, SkillError> {
    let Some(query) = query else {
        return Ok(None);
    };
    let query = query.trim();
    if query.chars().count() > 500 || query.chars().any(char::is_control) {
        return Err(SkillError::InvalidRequest);
    }
    if query.is_empty() {
        Ok(None)
    } else {
        Ok(Some(query.to_lowercase()))
    }
}

fn matches_query(skill: &Skill, query: &str) -> bool {
    skill.name.to_lowercase().contains(query)
        || skill.description.to_lowercase().contains(query)
        || skill.id.to_lowercase().contains(query)
}

fn skill_fingerprint(skills: &[ScannedSkill]) -> String {
    let mut digest = Sha256::new();
    digest.update(b"synthchat-skill-page-v1\0");
    for skill in skills {
        let serialized =
            serde_json::to_vec(&skill.public).expect("the public Skill resource always serializes");
        digest.update((serialized.len() as u64).to_be_bytes());
        digest.update(serialized);
    }
    hex(&digest.finalize())
}

fn composite_skill_etag(config_etag: &str, catalog_fingerprint: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"synthchat-skill-catalog-revision-v1\0");
    for part in [config_etag, catalog_fingerprint] {
        digest.update((part.len() as u64).to_be_bytes());
        digest.update(part.as_bytes());
    }
    format!("\"skills_{}\"", hex(&digest.finalize()))
}

#[derive(Deserialize)]
struct SkillLockDocument {
    #[serde(default)]
    version: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_unique_installed")]
    installed: BTreeMap<String, RawManifestEntry>,
}

struct RawManifestEntry(serde_json::Map<String, JsonValue>);

impl<'de> Deserialize<'de> for RawManifestEntry {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct UniqueManifestEntry;

        impl<'de> de::Visitor<'de> for UniqueManifestEntry {
            type Value = RawManifestEntry;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a manifest object with unique field names")
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: de::MapAccess<'de>,
            {
                let mut fields = serde_json::Map::new();
                while let Some((name, value)) = map.next_entry::<String, JsonValue>()? {
                    if fields.insert(name.clone(), value).is_some() {
                        return Err(de::Error::custom(format!(
                            "duplicate manifest field: {name}"
                        )));
                    }
                }
                Ok(RawManifestEntry(fields))
            }
        }

        deserializer.deserialize_map(UniqueManifestEntry)
    }
}

fn deserialize_unique_installed<'de, D>(
    deserializer: D,
) -> Result<BTreeMap<String, RawManifestEntry>, D::Error>
where
    D: Deserializer<'de>,
{
    struct UniqueInstalled;

    impl<'de> de::Visitor<'de> for UniqueInstalled {
        type Value = BTreeMap<String, RawManifestEntry>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("an object with unique installed skill names")
        }

        fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
        where
            A: de::MapAccess<'de>,
        {
            let mut entries = BTreeMap::new();
            while let Some((name, entry)) = map.next_entry::<String, RawManifestEntry>()? {
                if entries.insert(name.clone(), entry).is_some() {
                    return Err(de::Error::custom(format!(
                        "duplicate installed skill name: {name}"
                    )));
                }
            }
            Ok(entries)
        }
    }

    deserializer.deserialize_map(UniqueInstalled)
}

fn load_provenance(root: &Path) -> BTreeMap<String, SkillProvenance> {
    let hub_path = root.join(".hub");
    let Ok(hub_metadata) = fs::symlink_metadata(&hub_path) else {
        return BTreeMap::new();
    };
    if path_is_redirect(&hub_metadata) || !hub_metadata.is_dir() {
        return BTreeMap::new();
    }

    let lock_path = hub_path.join("lock.json");
    let Ok(metadata) = fs::symlink_metadata(&lock_path) else {
        return BTreeMap::new();
    };
    if path_is_redirect(&metadata) || !metadata.is_file() || metadata.len() > MAX_LOCK_FILE_BYTES {
        return BTreeMap::new();
    }
    let Ok(bytes) = fs::read(&lock_path) else {
        return BTreeMap::new();
    };
    let Ok(document) = serde_json::from_slice::<SkillLockDocument>(&bytes) else {
        return BTreeMap::new();
    };
    if document.version.is_some_and(|version| version != 1) {
        return BTreeMap::new();
    }
    let lock_revision = hex(&Sha256::digest(&bytes));
    let mut duplicate_paths = BTreeSet::new();
    let mut provenance = BTreeMap::new();
    for (manifest_name, raw_entry) in document.installed {
        let Some((install_path, source, managed)) = parse_managed_manifest_entry(
            root,
            &lock_path,
            &lock_revision,
            &manifest_name,
            &raw_entry,
        ) else {
            continue;
        };
        if provenance.contains_key(&install_path) {
            duplicate_paths.insert(install_path);
            continue;
        }
        provenance.insert(install_path, SkillProvenance { source, managed });
    }
    for duplicate in duplicate_paths {
        provenance.remove(&duplicate);
    }
    provenance
}

fn parse_managed_manifest_entry(
    root: &Path,
    lock_path: &Path,
    lock_revision: &str,
    manifest_name: &str,
    raw_entry: &RawManifestEntry,
) -> Option<(String, SkillSource, ManagedSkillRecord)> {
    validate_manifest_name(manifest_name)?;
    let entry = &raw_entry.0;
    match entry.get("state") {
        None => {}
        Some(JsonValue::String(state)) if state == "installed" => {}
        Some(_) => return None,
    }
    if entry
        .get("name")
        .is_some_and(|name| name.as_str() != Some(manifest_name))
    {
        return None;
    }

    let source = manifest_string(entry, "source", 128)?;
    let identifier = manifest_string(entry, "identifier", 2_048)?;
    let content_hash = manifest_string(entry, "content_hash", 256)?;
    let install_path = manifest_string(entry, "install_path", 1_024)?;
    let install_path = normalized_lock_path_for_name(&install_path, manifest_name)?;
    resolve_managed_directory(root, &install_path)?;
    match entry.get("metadata") {
        None => {}
        Some(metadata) if metadata.is_object() => {}
        Some(_) => return None,
    }
    optional_manifest_string(entry, "installed_at", 128)?;
    manifest_operation_id(entry)?;
    let classified = classify_source(&source, &identifier);
    let manifest = ManagedManifestEntry {
        source,
        identifier,
        content_hash,
        install_path: install_path.clone(),
    };
    Some((
        install_path,
        classified,
        ManagedSkillRecord {
            lock_path: lock_path.to_owned(),
            lock_revision: lock_revision.to_owned(),
            manifest_name: manifest_name.to_owned(),
            manifest,
        },
    ))
}

fn validate_manifest_name(value: &str) -> Option<()> {
    if value.is_empty()
        || value.trim() != value
        || value.chars().count() > 128
        || value.chars().any(char::is_control)
        || value.contains(['/', '\\'])
        || matches!(value, "." | "..")
    {
        None
    } else {
        Some(())
    }
}

fn manifest_string(
    entry: &serde_json::Map<String, JsonValue>,
    key: &str,
    maximum_chars: usize,
) -> Option<String> {
    let value = entry.get(key)?.as_str()?;
    if value.is_empty()
        || value.trim() != value
        || value.chars().count() > maximum_chars
        || value.chars().any(char::is_control)
    {
        None
    } else {
        Some(value.to_owned())
    }
}

fn optional_manifest_string(
    entry: &serde_json::Map<String, JsonValue>,
    key: &str,
    maximum_chars: usize,
) -> Option<Option<String>> {
    match entry.get(key) {
        None => Some(None),
        Some(_) => manifest_string(entry, key, maximum_chars).map(Some),
    }
}

fn manifest_operation_id(entry: &serde_json::Map<String, JsonValue>) -> Option<Option<String>> {
    const KEYS: &[&str] = &[
        "install_operation_id",
        "installOperationId",
        "operation_id",
        "operationId",
    ];
    let present: Vec<_> = KEYS
        .iter()
        .filter(|key| entry.contains_key(**key))
        .collect();
    match present.as_slice() {
        [] => Some(None),
        [key] => manifest_string(entry, key, 256).map(Some),
        _ => None,
    }
}

fn resolve_managed_directory(root: &Path, install_path: &str) -> Option<PathBuf> {
    let canonical_root = fs::canonicalize(root).ok()?;
    let mut current = root.to_owned();
    for component in install_path.split('/') {
        current.push(component);
        let metadata = fs::symlink_metadata(&current).ok()?;
        if path_is_redirect(&metadata) || !metadata.is_dir() {
            return None;
        }
    }
    let canonical_directory = fs::canonicalize(current).ok()?;
    if canonical_directory == canonical_root || !canonical_directory.starts_with(&canonical_root) {
        None
    } else {
        Some(canonical_directory)
    }
}

fn normalized_lock_path(value: &str) -> Option<String> {
    if value.is_empty()
        || value.trim() != value
        || value.contains('\0')
        || value.contains('\\')
        || value.chars().any(char::is_control)
        || has_windows_drive_prefix(value)
    {
        return None;
    }
    let path = Path::new(value);
    if path.is_absolute() {
        return None;
    }
    let normalized = normalized_relative_path(path).ok()?;
    (normalized == value).then_some(normalized)
}

fn has_windows_drive_prefix(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

fn normalized_lock_path_for_name(value: &str, manifest_name: &str) -> Option<String> {
    let normalized = normalized_lock_path(value)?;
    (normalized.rsplit('/').next() == Some(manifest_name)).then_some(normalized)
}

fn classify_source(source: &str, identifier: &str) -> SkillSource {
    match source {
        "official" => SkillSource::Bundled,
        "file" => SkillSource::File,
        "url" => SkillSource::Url,
        _ if identifier.starts_with("http://") || identifier.starts_with("https://") => {
            SkillSource::Url
        }
        _ => SkillSource::Registry,
    }
}

fn path_is_redirect(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;

        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return true;
        }
    }
    false
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SkillCursorPayload {
    version: u8,
    filter_hash: String,
    fingerprint: String,
    offset: usize,
    issued_at: i64,
}

struct SkillCursorCodec {
    key: Zeroizing<[u8; 32]>,
}

impl SkillCursorCodec {
    fn new(desktop_token: &str) -> Self {
        let mut digest = Sha256::new();
        digest.update(b"synthchat-skill-cursor-v1\0");
        digest.update(desktop_token.as_bytes());
        Self {
            key: Zeroizing::new(digest.finalize().into()),
        }
    }

    fn filter_hash(profile_id: &str, query: Option<&str>) -> String {
        let mut digest = Sha256::new();
        digest.update(b"synthchat-skill-filter-v1\0");
        for part in [profile_id, query.unwrap_or_default()] {
            digest.update((part.len() as u64).to_be_bytes());
            digest.update(part.as_bytes());
        }
        hex(&digest.finalize())
    }

    fn encode(&self, mut payload: SkillCursorPayload) -> Result<String, SkillError> {
        payload.version = CURSOR_VERSION;
        payload.issued_at = OffsetDateTime::now_utc().unix_timestamp();
        let bytes = serde_json::to_vec(&payload).map_err(|_| SkillError::DataInvalid)?;
        let token = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(&bytes),
            URL_SAFE_NO_PAD.encode(self.sign(&bytes))
        );
        if token.len() > MAX_CURSOR_BYTES {
            return Err(SkillError::DataInvalid);
        }
        Ok(token)
    }

    fn decode(
        &self,
        token: &str,
        expected_filter_hash: &str,
    ) -> Result<SkillCursorPayload, SkillError> {
        if token.is_empty() || token.len() > MAX_CURSOR_BYTES {
            return Err(SkillError::InvalidCursor);
        }
        let mut parts = token.split('.');
        let (Some(payload), Some(signature), None) = (parts.next(), parts.next(), parts.next())
        else {
            return Err(SkillError::InvalidCursor);
        };
        let payload = URL_SAFE_NO_PAD
            .decode(payload)
            .map_err(|_| SkillError::InvalidCursor)?;
        let signature = URL_SAFE_NO_PAD
            .decode(signature)
            .map_err(|_| SkillError::InvalidCursor)?;
        if signature.len() != 32
            || !bool::from(self.sign(&payload).as_slice().ct_eq(signature.as_slice()))
        {
            return Err(SkillError::InvalidCursor);
        }
        let decoded: SkillCursorPayload =
            serde_json::from_slice(&payload).map_err(|_| SkillError::InvalidCursor)?;
        let now = OffsetDateTime::now_utc().unix_timestamp();
        if decoded.version != CURSOR_VERSION
            || decoded.filter_hash != expected_filter_hash
            || decoded.issued_at > now + 300
            || now.saturating_sub(decoded.issued_at) > MAX_CURSOR_AGE_SECONDS
        {
            return Err(SkillError::InvalidCursor);
        }
        Ok(decoded)
    }

    fn sign(&self, bytes: &[u8]) -> [u8; 32] {
        let mut mac = HmacSha256::new_from_slice(self.key.as_ref())
            .expect("a SHA-256 digest is a valid HMAC key");
        mac.update(bytes);
        mac.finalize().into_bytes().into()
    }
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(DIGITS[(byte >> 4) as usize] as char);
        output.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    output
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::json;
    use tempfile::TempDir;

    use super::*;

    const TOKEN: &str = "0123456789abcdef0123456789abcdef";

    struct Fixture {
        home: TempDir,
        service: SkillService,
    }

    impl Fixture {
        fn new() -> Self {
            let home = tempfile::tempdir().unwrap();
            let profiles = Arc::new(ProfileService::without_credential_store(
                home.path().to_owned(),
            ));
            let service = SkillService::new(profiles, TOKEN);
            Self { home, service }
        }

        fn skill(&self, category: &str, folder: &str, content: &str) {
            let root = self.home.path().join("skills").join(category).join(folder);
            fs::create_dir_all(&root).unwrap();
            fs::write(root.join("SKILL.md"), content).unwrap();
        }

        fn list(&self, limit: usize, cursor: Option<String>) -> SkillPage {
            self.versioned_list(None, limit, cursor).value
        }

        fn versioned_list(
            &self,
            query: Option<&str>,
            limit: usize,
            cursor: Option<String>,
        ) -> Versioned<SkillPage> {
            self.service
                .list(
                    "default",
                    &ListSkills {
                        query: query.map(ToOwned::to_owned),
                        cursor,
                        limit,
                    },
                )
                .unwrap()
        }

        fn write_lock(&self, document: &JsonValue) {
            let hub = self.home.path().join("skills/.hub");
            fs::create_dir_all(&hub).unwrap();
            fs::write(
                hub.join("lock.json"),
                serde_json::to_vec_pretty(document).unwrap(),
            )
            .unwrap();
        }
    }

    #[test]
    fn scans_hermes_frontmatter_filters_support_trees_and_paginates() {
        let fixture = Fixture::new();
        fixture.skill(
            "research",
            "papers",
            "---\nname: paper-search\ndescription: Find papers\nversion: 1.2.3\n---\n# Body\n",
        );
        fixture.skill(
            "writing",
            "editor",
            "# Editorial workflow\n\nImprove a draft without frontmatter.\n",
        );
        let nested = fixture
            .home
            .path()
            .join("skills/research/papers/references/archived");
        fs::create_dir_all(&nested).unwrap();
        fs::write(
            nested.join("SKILL.md"),
            "---\nname: hidden\ndescription: hidden\n---\n",
        )
        .unwrap();

        let first = fixture.list(1, None);
        assert_eq!(first.items.len(), 1);
        assert!(first.next_cursor.is_some());
        let second = fixture.list(1, first.next_cursor);
        assert_eq!(second.items.len(), 1);
        assert!(second.next_cursor.is_none());
        let names: BTreeSet<_> = [first.items[0].name.clone(), second.items[0].name.clone()]
            .into_iter()
            .collect();
        assert_eq!(
            names,
            BTreeSet::from(["Editorial workflow".to_owned(), "paper-search".to_owned()])
        );
        let paper = second
            .items
            .iter()
            .chain(first.items.iter())
            .find(|item| item.name == "paper-search")
            .unwrap();
        assert_eq!(paper.version.as_deref(), Some("1.2.3"));
    }

    #[test]
    fn toggle_uses_composite_etag_and_preserves_nested_skill_configuration() {
        let fixture = Fixture::new();
        fixture.skill(
            "research",
            "papers",
            "---\nname: paper-search\ndescription: Find papers\n---\n",
        );
        fs::write(
            fixture.home.path().join("config.yaml"),
            "unknown:\n  keep: true\nskills:\n  config:\n    paper-search:\n      mode: strict\n",
        )
        .unwrap();
        let config = fixture.service.profiles.get_config("default").unwrap();
        let listed = fixture.versioned_list(None, 10, None);
        assert_ne!(listed.etag, config.etag);
        let skill = listed.value.items[0].clone();
        let disabled = fixture
            .service
            .update(
                "default",
                &skill.id,
                &SkillPatch {
                    enabled: Some(false),
                    config: None,
                },
                &listed.etag,
            )
            .unwrap();
        assert!(!disabled.value.enabled);
        assert_ne!(disabled.etag, listed.etag);
        let persisted = fs::read_to_string(fixture.home.path().join("config.yaml")).unwrap();
        assert!(persisted.contains("mode: strict"));
        assert!(persisted.contains("unknown:"));
        assert!(persisted.contains("paper-search"));

        assert!(matches!(
            fixture.service.update(
                "default",
                &skill.id,
                &SkillPatch {
                    enabled: Some(true),
                    config: None,
                },
                &listed.etag,
            ),
            Err(SkillError::Profile(ProfileError::RevisionConflict {
                current_etag
            })) if current_etag == disabled.etag
        ));
        let relisted = fixture.versioned_list(None, 10, None);
        assert_eq!(relisted.etag, disabled.etag);
        assert!(!relisted.value.items[0].enabled);
    }

    #[test]
    fn etag_tracks_the_unfiltered_catalog_while_cursor_tracks_the_filter() {
        let fixture = Fixture::new();
        fixture.skill(
            "research",
            "alpha-one",
            "---\nname: alpha-one\ndescription: First alpha\n---\n",
        );
        fixture.skill(
            "research",
            "alpha-two",
            "---\nname: alpha-two\ndescription: Second alpha\n---\n",
        );
        let first = fixture.versioned_list(Some("alpha"), 1, None);
        let cursor = first.value.next_cursor.unwrap();

        fixture.skill(
            "writing",
            "unrelated",
            "---\nname: unrelated\ndescription: Does not match\n---\n",
        );
        let continued = fixture.versioned_list(Some("alpha"), 1, Some(cursor));
        assert_eq!(continued.value.items.len(), 1);
        assert_ne!(continued.etag, first.etag);

        fs::remove_dir_all(fixture.home.path().join("skills/writing/unrelated")).unwrap();
        assert_eq!(
            fixture.versioned_list(Some("alpha"), 10, None).etag,
            first.etag
        );
    }

    #[test]
    fn catalog_drift_conflicts_before_profile_mutation() {
        let fixture = Fixture::new();
        fixture.skill(
            "research",
            "papers",
            "---\nname: paper-search\ndescription: Find papers\n---\n",
        );
        let initial = fixture.versioned_list(None, 10, None);
        let skill = initial.value.items[0].clone();
        fixture.skill(
            "writing",
            "editor",
            "---\nname: editor\ndescription: Edit text\n---\n",
        );
        let current = fixture.versioned_list(None, 10, None);
        assert_ne!(current.etag, initial.etag);

        assert!(matches!(
            fixture.service.update(
                "default",
                &skill.id,
                &SkillPatch {
                    enabled: Some(false),
                    config: None,
                },
                &initial.etag,
            ),
            Err(SkillError::Profile(ProfileError::RevisionConflict {
                current_etag
            })) if current_etag == current.etag
        ));
        assert!(!fixture.home.path().join("config.yaml").exists());

        let updated = fixture
            .service
            .update(
                "default",
                &skill.id,
                &SkillPatch {
                    enabled: Some(false),
                    config: None,
                },
                &current.etag,
            )
            .unwrap();
        assert!(!updated.value.enabled);
        assert_eq!(updated.etag, fixture.versioned_list(None, 10, None).etag);
        assert_ne!(
            updated.etag,
            fixture.service.profiles.get_config("default").unwrap().etag
        );
    }

    #[test]
    fn safe_v1_provenance_marks_only_installed_managed_skills() {
        let fixture = Fixture::new();
        for (category, folder, name) in [
            ("research", "paper-search", "paper-search"),
            ("files", "disk-skill", "disk-skill"),
            ("pending", "pending-skill", "pending-skill"),
            ("bad", "locked-name", "different-name"),
            ("local", "handmade", "handmade"),
        ] {
            fixture.skill(
                category,
                folder,
                &format!("---\nname: {name}\ndescription: test\n---\n"),
            );
        }
        fixture.write_lock(&json!({
            "version": 1,
            "installed": {
                "paper-search": {
                    "source": "official",
                    "identifier": "official/research/paper-search",
                    "content_hash": "sha256:paper",
                    "install_path": "research/paper-search",
                    "metadata": {"channel": "stable"},
                    "installed_at": "2026-07-17T00:00:00Z",
                    "install_operation_id": "op_install_paper"
                },
                "disk-skill": {
                    "state": "installed",
                    "source": "file",
                    "identifier": "disk-skill.zip",
                    "content_hash": "sha256:file",
                    "install_path": "files/disk-skill"
                },
                "pending-skill": {
                    "state": "pending",
                    "source": "github",
                    "identifier": "owner/repo/pending-skill",
                    "content_hash": "sha256:pending",
                    "install_path": "pending/pending-skill"
                },
                "locked-name": {
                    "source": "github",
                    "identifier": "owner/repo/locked-name",
                    "content_hash": "sha256:mismatch",
                    "install_path": "bad/locked-name"
                }
            }
        }));

        let page = fixture.list(20, None);
        let paper = page
            .items
            .iter()
            .find(|skill| skill.name == "paper-search")
            .unwrap();
        assert_eq!(paper.source, SkillSource::Bundled);
        assert!(paper.uninstallable);
        let file = page
            .items
            .iter()
            .find(|skill| skill.name == "disk-skill")
            .unwrap();
        assert_eq!(file.source, SkillSource::File);
        assert!(file.uninstallable);
        for name in ["pending-skill", "different-name", "handmade"] {
            let skill = page.items.iter().find(|skill| skill.name == name).unwrap();
            assert_eq!(skill.source, SkillSource::Local);
            assert!(!skill.uninstallable);
        }

        let location = fixture.service.managed_skill("default", &paper.id).unwrap();
        assert_eq!(location.skill, *paper);
        assert_eq!(location.manifest_name, "paper-search");
        assert_eq!(location.manifest.source, "official");
        assert_eq!(
            location.manifest.identifier,
            "official/research/paper-search"
        );
        assert_eq!(location.manifest.content_hash, "sha256:paper");
        assert_eq!(location.manifest.install_path, "research/paper-search");
        assert_eq!(
            location.lock_path,
            fixture.home.path().join("skills/.hub/lock.json")
        );
        assert_eq!(location.lock_revision.len(), 64);
    }

    #[test]
    fn poisoned_or_ambiguous_manifest_entries_are_never_managed() {
        let fixture = Fixture::new();
        fixture.skill(
            "safe",
            "paper-search",
            "---\nname: paper-search\ndescription: Find papers\n---\n",
        );
        let hub = fixture.home.path().join("skills/.hub");
        fs::create_dir_all(&hub).unwrap();
        fs::write(
            hub.join("lock.json"),
            r#"{
                "version": 1,
                "installed": {
                    "paper-search": {
                        "source": "official",
                        "identifier": "first",
                        "content_hash": "sha256:first",
                        "install_path": "safe/paper-search"
                    },
                    "paper-search": {
                        "source": "official",
                        "identifier": "second",
                        "content_hash": "sha256:second",
                        "install_path": "safe/paper-search"
                    }
                }
            }"#,
        )
        .unwrap();
        let duplicate = fixture.list(10, None).items.remove(0);
        assert_eq!(duplicate.source, SkillSource::Local);
        assert!(!duplicate.uninstallable);

        fs::write(
            hub.join("lock.json"),
            r#"{
                "version": 1,
                "installed": {
                    "paper-search": {
                        "name": "paper-search",
                        "name": "other-name",
                        "source": "official",
                        "identifier": "official/safe/paper-search",
                        "content_hash": "sha256:paper",
                        "install_path": "safe/paper-search"
                    }
                }
            }"#,
        )
        .unwrap();
        let duplicate_field = fixture.list(10, None).items.remove(0);
        assert_eq!(duplicate_field.source, SkillSource::Local);
        assert!(!duplicate_field.uninstallable);

        fixture.write_lock(&json!({
            "version": 1,
            "installed": {
                "paper-search": {
                    "name": "other-name",
                    "source": "official",
                    "identifier": "official/safe/paper-search",
                    "content_hash": "sha256:paper",
                    "install_path": "../paper-search"
                }
            }
        }));
        let poisoned = fixture.list(10, None).items.remove(0);
        assert_eq!(poisoned.source, SkillSource::Local);
        assert!(!poisoned.uninstallable);
        assert!(matches!(
            fixture.service.managed_skill("default", &poisoned.id),
            Err(SkillError::NotFound)
        ));
    }

    #[test]
    fn lock_install_paths_are_portably_strict() {
        assert_eq!(
            normalized_lock_path_for_name("nested/paper-search", "paper-search").as_deref(),
            Some("nested/paper-search")
        );
        for malicious in [
            "",
            ".",
            "..",
            "../paper-search",
            "/tmp/paper-search",
            "C:/Windows/paper-search",
            "nested/../paper-search",
            "nested\\paper-search",
            "nested//paper-search",
            "nested/other-name",
            " nested/paper-search",
        ] {
            assert_eq!(
                normalized_lock_path_for_name(malicious, "paper-search"),
                None,
                "accepted unsafe install_path {malicious:?}"
            );
        }
    }

    #[test]
    #[cfg(any(unix, windows))]
    fn symlinked_lock_file_is_never_trusted() {
        let fixture = Fixture::new();
        fixture.skill(
            "research",
            "paper-search",
            "---\nname: paper-search\ndescription: Find papers\n---\n",
        );
        let external_lock = fixture.home.path().join("external-lock.json");
        fs::write(
            &external_lock,
            serde_json::to_vec(&json!({
                "version": 1,
                "installed": {
                    "paper-search": {
                        "source": "official",
                        "identifier": "official/research/paper-search",
                        "content_hash": "sha256:paper",
                        "install_path": "research/paper-search"
                    }
                }
            }))
            .unwrap(),
        )
        .unwrap();
        let hub = fixture.home.path().join("skills/.hub");
        fs::create_dir_all(&hub).unwrap();
        let lock_path = hub.join("lock.json");
        #[cfg(unix)]
        let linked = std::os::unix::fs::symlink(&external_lock, &lock_path).is_ok();
        #[cfg(windows)]
        let linked = std::os::windows::fs::symlink_file(&external_lock, &lock_path).is_ok();
        if !linked {
            return;
        }

        let skill = fixture.list(10, None).items.remove(0);
        assert_eq!(skill.source, SkillSource::Local);
        assert!(!skill.uninstallable);
    }

    #[test]
    fn query_cursor_and_config_patch_fail_closed() {
        let fixture = Fixture::new();
        fixture.skill(
            "research",
            "papers",
            "---\nname: paper-search\ndescription: Find papers\n---\n",
        );
        let page = fixture
            .service
            .list(
                "default",
                &ListSkills {
                    query: Some("PAPERS".to_owned()),
                    cursor: None,
                    limit: 10,
                },
            )
            .unwrap()
            .value;
        assert_eq!(page.items.len(), 1);
        let skill = &page.items[0];
        let config = fixture.service.profiles.get_config("default").unwrap();
        assert!(matches!(
            fixture.service.update(
                "default",
                &skill.id,
                &SkillPatch {
                    enabled: None,
                    config: Some(json!({"secret": "plaintext"})),
                },
                &config.etag,
            ),
            Err(SkillError::InvalidRequest)
        ));
        assert!(matches!(
            fixture.service.list(
                "default",
                &ListSkills {
                    query: None,
                    cursor: Some("tampered".to_owned()),
                    limit: 10,
                },
            ),
            Err(SkillError::InvalidCursor)
        ));
    }

    #[test]
    fn skill_registry_runtime_config_preserves_official_defaults() {
        let config = SkillRegistryRuntimeConfig::default();

        assert_eq!(
            config.registry_index_url().as_str(),
            "https://hermes-agent.nousresearch.com/docs/api/skills-index.json"
        );
        assert_eq!(
            config.github_api_base_url().as_str(),
            "https://api.github.com/"
        );
        assert_eq!(
            config.github_raw_base_url().as_str(),
            "https://raw.githubusercontent.com/"
        );
    }

    #[test]
    fn skill_registry_runtime_config_accepts_public_https_prefixes() {
        let config = SkillRegistryRuntimeConfig::from_urls_for_tests(
            "https://registry.example.test/catalog/skills-index.json",
            "https://github-api.example.test:8443/github/api/v3",
            "https://raw.example.test:8443/github/raw",
        )
        .unwrap();

        assert_eq!(
            config.github_api_base_url().as_str(),
            "https://github-api.example.test:8443/github/api/v3/"
        );
        assert_eq!(
            config.github_raw_base_url().as_str(),
            "https://raw.example.test:8443/github/raw/"
        );
        assert_eq!(
            config
                .github_api_url(&["repos", "NousResearch", "hermes-agent"])
                .unwrap()
                .as_str(),
            "https://github-api.example.test:8443/github/api/v3/repos/NousResearch/hermes-agent"
        );
    }

    #[test]
    fn skill_registry_runtime_config_rejects_unsafe_endpoints() {
        for value in [
            "http://registry.example.test/index.json",
            "https://user@registry.example.test/index.json",
            "https://registry.example.test/index.json?channel=stable",
            "https://registry.example.test/index.json#stable",
            "https://127.0.0.1/index.json",
            "https://metadata.google.internal/index.json",
            "https://registry.example.test/a//index.json",
            "https://registry.example.test/a/../index.json",
            "https://registry.example.test/a/./index.json",
            "https://registry.example.test/a%2Findex.json",
        ] {
            assert!(
                SkillRegistryRuntimeConfig::from_urls_for_tests(
                    value,
                    "https://api.github.com/",
                    "https://raw.githubusercontent.com/",
                )
                .is_err(),
                "accepted unsafe registry endpoint {value:?}"
            );
            assert!(
                SkillRegistryRuntimeConfig::from_urls_for_tests(
                    "https://registry.example.test/index.json",
                    value,
                    "https://raw.githubusercontent.com/",
                )
                .is_err(),
                "accepted unsafe GitHub API endpoint {value:?}"
            );
            assert!(
                SkillRegistryRuntimeConfig::from_urls_for_tests(
                    "https://registry.example.test/index.json",
                    "https://api.github.com/",
                    value,
                )
                .is_err(),
                "accepted unsafe GitHub raw endpoint {value:?}"
            );
        }
    }

    #[test]
    fn skill_registry_runtime_config_joins_only_path_segments() {
        let config = SkillRegistryRuntimeConfig::default();

        for segment in ["", ".", "..", "/escape", "a/b", "a\\b", "a?b", "a#b"] {
            assert!(
                config.github_api_url(&["repos", segment]).is_err(),
                "accepted unsafe URL path segment {segment:?}"
            );
        }
    }
}
