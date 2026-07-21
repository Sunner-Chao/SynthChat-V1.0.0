use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use uuid::Uuid;

const CATALOG_DIR: &str = ".synthchat";
const PLUGINS_DIR: &str = "plugins";
const MANIFEST_FILE: &str = "plugin.json";
const REGISTRY_FILE: &str = "registry.json";
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
const MAX_REGISTRY_BYTES: u64 = 1024 * 1024;
const MAX_PLUGINS: usize = 512;
const MAX_NAME_CHARS: usize = 120;
const MAX_DESCRIPTION_CHARS: usize = 4_096;
const MAX_TOOLS: usize = 128;
const MAX_ENV_NAMES: usize = 128;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
    pub provided_tools: Vec<String>,
    pub requires_env: Vec<String>,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum PluginExecution {
    ManifestOnly,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Plugin {
    #[serde(flatten)]
    pub manifest: PluginManifest,
    pub enabled: bool,
    pub execution: PluginExecution,
    pub installed_at: String,
    pub updated_at: String,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PluginPage {
    pub items: Vec<Plugin>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InstallPlugin {
    pub source_path: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PluginPatch {
    pub enabled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VersionedPlugin<T> {
    pub value: T,
    pub revision: u64,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Registry {
    revision: u64,
    plugins: BTreeMap<String, Registration>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Registration {
    directory: String,
    enabled: bool,
    installed_at: String,
    updated_at: String,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PluginError {
    #[error("invalid plugin request")]
    InvalidRequest,
    #[error("plugin not found")]
    NotFound,
    #[error("plugin is already installed")]
    AlreadyInstalled,
    #[error("plugin catalog revision conflict")]
    RevisionConflict { current_revision: u64 },
    #[error("plugin manifest is invalid")]
    ManifestInvalid,
    #[error("plugin catalog limit reached")]
    LimitReached,
    #[error("plugin storage is unavailable")]
    StorageUnavailable,
}

#[derive(Clone)]
pub struct PluginService {
    hermes_home: Arc<PathBuf>,
    root: Arc<PathBuf>,
    process_lock: Arc<Mutex<()>>,
}

impl PluginService {
    pub fn new(hermes_home: &Path) -> Self {
        Self {
            hermes_home: Arc::new(hermes_home.to_owned()),
            root: Arc::new(hermes_home.join(CATALOG_DIR).join(PLUGINS_DIR)),
            process_lock: Arc::new(Mutex::new(())),
        }
    }

    pub fn list(&self) -> Result<VersionedPlugin<PluginPage>, PluginError> {
        let _guard = self.lock()?;
        let root = self.ensure_root()?;
        let registry = self.load_registry(&root)?;
        let mut items = registry
            .plugins
            .iter()
            .map(|(id, registration)| self.registered_plugin(&root, id, registration))
            .collect::<Result<Vec<_>, _>>()?;
        items.sort_by(|left, right| {
            left.manifest
                .name
                .to_lowercase()
                .cmp(&right.manifest.name.to_lowercase())
                .then_with(|| left.manifest.id.cmp(&right.manifest.id))
        });
        Ok(VersionedPlugin {
            value: PluginPage { items },
            revision: registry.revision,
        })
    }

    pub fn install(&self, request: &InstallPlugin) -> Result<VersionedPlugin<Plugin>, PluginError> {
        let _guard = self.lock()?;
        let root = self.ensure_root()?;
        let source = self.resolve_source(&root, &request.source_path)?;
        let manifest = read_manifest(&source)?;
        let directory = source
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or(PluginError::InvalidRequest)?;
        if directory != manifest.id {
            return Err(PluginError::ManifestInvalid);
        }

        let mut registry = self.load_registry(&root)?;
        if registry.plugins.contains_key(&manifest.id) {
            return Err(PluginError::AlreadyInstalled);
        }
        if registry.plugins.len() >= MAX_PLUGINS {
            return Err(PluginError::LimitReached);
        }
        let now = timestamp()?;
        let registration = Registration {
            directory: directory.to_owned(),
            enabled: false,
            installed_at: now.clone(),
            updated_at: now,
        };
        registry
            .plugins
            .insert(manifest.id.clone(), registration.clone());
        registry.revision = next_revision(registry.revision)?;
        self.save_registry(&root, &registry)?;
        Ok(VersionedPlugin {
            value: public_plugin(manifest, &registration),
            revision: registry.revision,
        })
    }

    pub fn update(
        &self,
        plugin_id: &str,
        patch: &PluginPatch,
        expected_revision: u64,
    ) -> Result<VersionedPlugin<Plugin>, PluginError> {
        validate_id(plugin_id)?;
        let _guard = self.lock()?;
        let root = self.ensure_root()?;
        let mut registry = self.load_registry(&root)?;
        require_revision(registry.revision, expected_revision)?;
        let current = registry
            .plugins
            .get(plugin_id)
            .cloned()
            .ok_or(PluginError::NotFound)?;
        let directory = checked_registered_directory(&root, plugin_id, &current.directory)?;
        let manifest = read_manifest(&directory)?;
        if manifest.id != plugin_id {
            return Err(PluginError::ManifestInvalid);
        }
        let mut updated = current;
        updated.enabled = patch.enabled;
        updated.updated_at = timestamp()?;
        registry
            .plugins
            .insert(plugin_id.to_owned(), updated.clone());
        registry.revision = next_revision(registry.revision)?;
        self.save_registry(&root, &registry)?;
        Ok(VersionedPlugin {
            value: public_plugin(manifest, &updated),
            revision: registry.revision,
        })
    }

    pub fn uninstall(&self, plugin_id: &str, expected_revision: u64) -> Result<u64, PluginError> {
        validate_id(plugin_id)?;
        let _guard = self.lock()?;
        let root = self.ensure_root()?;
        let mut registry = self.load_registry(&root)?;
        require_revision(registry.revision, expected_revision)?;
        if registry.plugins.remove(plugin_id).is_none() {
            return Err(PluginError::NotFound);
        }
        registry.revision = next_revision(registry.revision)?;
        self.save_registry(&root, &registry)?;
        Ok(registry.revision)
    }

    fn registered_plugin(
        &self,
        root: &Path,
        id: &str,
        registration: &Registration,
    ) -> Result<Plugin, PluginError> {
        validate_id(id).map_err(|_| PluginError::ManifestInvalid)?;
        validate_registration(registration)?;
        let directory = checked_registered_directory(root, id, &registration.directory)?;
        let manifest = read_manifest(&directory)?;
        if manifest.id != id {
            return Err(PluginError::ManifestInvalid);
        }
        Ok(public_plugin(manifest, registration))
    }

    fn ensure_root(&self) -> Result<PathBuf, PluginError> {
        fs::create_dir_all(self.hermes_home.as_path()).map_err(map_storage)?;
        reject_symlink_or_non_directory(self.hermes_home.as_path())?;
        let catalog = self.hermes_home.join(CATALOG_DIR);
        ensure_directory(&catalog)?;
        ensure_directory(self.root.as_path())?;
        let canonical_home = fs::canonicalize(self.hermes_home.as_path()).map_err(map_storage)?;
        let canonical_root = fs::canonicalize(self.root.as_path()).map_err(map_storage)?;
        if !canonical_root.starts_with(&canonical_home) || canonical_root == canonical_home {
            return Err(PluginError::StorageUnavailable);
        }
        Ok(canonical_root)
    }

    fn resolve_source(&self, root: &Path, requested: &str) -> Result<PathBuf, PluginError> {
        if requested.trim() != requested
            || requested.is_empty()
            || requested.len() > 4_096
            || requested.chars().any(char::is_control)
        {
            return Err(PluginError::InvalidRequest);
        }
        let requested = Path::new(requested);
        let source = if requested.is_absolute() {
            requested.to_owned()
        } else {
            let mut components = requested.components();
            let Some(Component::Normal(component)) = components.next() else {
                return Err(PluginError::InvalidRequest);
            };
            if components.next().is_some() {
                return Err(PluginError::InvalidRequest);
            }
            root.join(component)
        };
        let metadata = fs::symlink_metadata(&source).map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                PluginError::NotFound
            } else {
                map_storage(error)
            }
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(PluginError::InvalidRequest);
        }
        let canonical = fs::canonicalize(&source).map_err(map_storage)?;
        let relative = canonical
            .strip_prefix(root)
            .map_err(|_| PluginError::InvalidRequest)?;
        if relative.components().count() != 1
            || !matches!(relative.components().next(), Some(Component::Normal(_)))
        {
            return Err(PluginError::InvalidRequest);
        }
        Ok(canonical)
    }

    fn load_registry(&self, root: &Path) -> Result<Registry, PluginError> {
        let path = root.join(REGISTRY_FILE);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(Registry::default());
            }
            Err(error) => return Err(map_storage(error)),
        };
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || metadata.len() > MAX_REGISTRY_BYTES
        {
            return Err(PluginError::StorageUnavailable);
        }
        let bytes = fs::read(&path).map_err(map_storage)?;
        let registry: Registry =
            serde_json::from_slice(&bytes).map_err(|_| PluginError::StorageUnavailable)?;
        if registry.plugins.len() > MAX_PLUGINS
            || registry.plugins.iter().any(|(id, registration)| {
                validate_id(id).is_err()
                    || registration.directory != *id
                    || validate_registration(registration).is_err()
            })
        {
            return Err(PluginError::StorageUnavailable);
        }
        Ok(registry)
    }

    fn save_registry(&self, root: &Path, registry: &Registry) -> Result<(), PluginError> {
        let bytes =
            serde_json::to_vec_pretty(registry).map_err(|_| PluginError::StorageUnavailable)?;
        if bytes.len() as u64 > MAX_REGISTRY_BYTES {
            return Err(PluginError::LimitReached);
        }
        let destination = root.join(REGISTRY_FILE);
        match fs::symlink_metadata(&destination) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                return Err(PluginError::StorageUnavailable);
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(map_storage(error)),
        }
        let temporary = root.join(format!(".registry-{}.tmp", Uuid::new_v4().simple()));
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(map_storage)?;
        if let Err(error) = file.write_all(&bytes).and_then(|_| file.sync_all()) {
            let _ = fs::remove_file(&temporary);
            return Err(map_storage(error));
        }
        drop(file);
        if destination.exists() {
            fs::remove_file(&destination).map_err(map_storage)?;
        }
        if let Err(error) = fs::rename(&temporary, &destination) {
            let _ = fs::remove_file(&temporary);
            return Err(map_storage(error));
        }
        Ok(())
    }

    fn lock(&self) -> Result<MutexGuard<'_, ()>, PluginError> {
        self.process_lock
            .lock()
            .map_err(|_| PluginError::StorageUnavailable)
    }
}

fn read_manifest(directory: &Path) -> Result<PluginManifest, PluginError> {
    let manifest_path = directory.join(MANIFEST_FILE);
    let metadata = fs::symlink_metadata(&manifest_path).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            PluginError::ManifestInvalid
        } else {
            map_storage(error)
        }
    })?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > MAX_MANIFEST_BYTES
    {
        return Err(PluginError::ManifestInvalid);
    }
    let bytes = fs::read(&manifest_path).map_err(map_storage)?;
    if bytes.len() as u64 > MAX_MANIFEST_BYTES {
        return Err(PluginError::ManifestInvalid);
    }
    let manifest: PluginManifest =
        serde_json::from_slice(&bytes).map_err(|_| PluginError::ManifestInvalid)?;
    validate_manifest(&manifest)?;
    Ok(manifest)
}

fn validate_manifest(manifest: &PluginManifest) -> Result<(), PluginError> {
    validate_id(&manifest.id).map_err(|_| PluginError::ManifestInvalid)?;
    if invalid_text(&manifest.name, MAX_NAME_CHARS, false)
        || invalid_text(&manifest.version, 64, false)
        || !valid_version(&manifest.version)
        || invalid_text(&manifest.description, MAX_DESCRIPTION_CHARS, true)
        || invalid_text(&manifest.author, MAX_NAME_CHARS, false)
        || manifest.provided_tools.len() > MAX_TOOLS
        || manifest.requires_env.len() > MAX_ENV_NAMES
        || manifest
            .provided_tools
            .iter()
            .any(|tool| !valid_tool_name(tool))
        || manifest
            .requires_env
            .iter()
            .any(|name| !valid_env_name(name))
        || has_duplicates(&manifest.provided_tools)
        || has_duplicates(&manifest.requires_env)
    {
        return Err(PluginError::ManifestInvalid);
    }
    Ok(())
}

fn validate_registration(registration: &Registration) -> Result<(), PluginError> {
    if registration.directory.is_empty()
        || registration.installed_at.is_empty()
        || registration.updated_at.is_empty()
        || OffsetDateTime::parse(&registration.installed_at, &Rfc3339).is_err()
        || OffsetDateTime::parse(&registration.updated_at, &Rfc3339).is_err()
    {
        return Err(PluginError::StorageUnavailable);
    }
    Ok(())
}

fn checked_registered_directory(
    root: &Path,
    id: &str,
    directory: &str,
) -> Result<PathBuf, PluginError> {
    if directory != id {
        return Err(PluginError::StorageUnavailable);
    }
    let path = root.join(directory);
    let metadata = fs::symlink_metadata(&path).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            PluginError::ManifestInvalid
        } else {
            map_storage(error)
        }
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(PluginError::ManifestInvalid);
    }
    let canonical = fs::canonicalize(&path).map_err(map_storage)?;
    if canonical.parent() != Some(root) {
        return Err(PluginError::ManifestInvalid);
    }
    Ok(canonical)
}

fn public_plugin(manifest: PluginManifest, registration: &Registration) -> Plugin {
    Plugin {
        manifest,
        enabled: registration.enabled,
        execution: PluginExecution::ManifestOnly,
        installed_at: registration.installed_at.clone(),
        updated_at: registration.updated_at.clone(),
    }
}

fn validate_id(value: &str) -> Result<(), PluginError> {
    if value.is_empty()
        || value.len() > 64
        || !value.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || (index > 0 && matches!(byte, b'-' | b'_' | b'.'))
        })
    {
        return Err(PluginError::InvalidRequest);
    }
    Ok(())
}

fn invalid_text(value: &str, max: usize, allow_empty: bool) -> bool {
    (!allow_empty && value.trim().is_empty())
        || value.trim() != value
        || value.chars().count() > max
        || value.chars().any(char::is_control)
}

fn valid_version(value: &str) -> bool {
    value.bytes().enumerate().all(|(index, byte)| {
        byte.is_ascii_alphanumeric() || (index > 0 && matches!(byte, b'.' | b'-' | b'_' | b'+'))
    })
}

fn valid_tool_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
}

fn valid_env_name(value: &str) -> bool {
    let mut bytes = value.bytes();
    matches!(bytes.next(), Some(byte) if byte.is_ascii_uppercase() || byte == b'_')
        && value.len() <= 128
        && bytes.all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
}

fn has_duplicates(values: &[String]) -> bool {
    let mut seen = BTreeSet::new();
    values.iter().any(|value| !seen.insert(value))
}

fn ensure_directory(path: &Path) -> Result<(), PluginError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            Err(PluginError::StorageUnavailable)
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir(path).map_err(map_storage)
        }
        Err(error) => Err(map_storage(error)),
    }
}

fn reject_symlink_or_non_directory(path: &Path) -> Result<(), PluginError> {
    let metadata = fs::symlink_metadata(path).map_err(map_storage)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        Err(PluginError::StorageUnavailable)
    } else {
        Ok(())
    }
}

fn require_revision(found: u64, expected: u64) -> Result<(), PluginError> {
    if found == expected {
        Ok(())
    } else {
        Err(PluginError::RevisionConflict {
            current_revision: found,
        })
    }
}

fn next_revision(revision: u64) -> Result<u64, PluginError> {
    revision
        .checked_add(1)
        .ok_or(PluginError::StorageUnavailable)
}

fn timestamp() -> Result<String, PluginError> {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|_| PluginError::StorageUnavailable)
}

fn map_storage(_: io::Error) -> PluginError {
    PluginError::StorageUnavailable
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, PluginService, PathBuf) {
        let home = tempfile::tempdir().unwrap();
        let service = PluginService::new(home.path());
        let root = home.path().join(CATALOG_DIR).join(PLUGINS_DIR);
        fs::create_dir_all(&root).unwrap();
        (home, service, root)
    }

    fn write_plugin(root: &Path, id: &str, extra: &str) -> PathBuf {
        let directory = root.join(id);
        fs::create_dir_all(&directory).unwrap();
        fs::write(
            directory.join(MANIFEST_FILE),
            format!(
                r#"{{
  "id": "{id}",
  "name": "Local tools",
  "version": "1.2.0",
  "description": "Manifest-only local tools.",
  "author": "SynthChat",
  "providedTools": ["local.search"],
  "requiresEnv": ["LOCAL_PLUGIN_TOKEN"]{extra}
}}"#,
            ),
        )
        .unwrap();
        directory
    }

    #[test]
    fn registers_toggles_and_uninstalls_metadata_without_deleting_source() {
        let (_home, service, root) = fixture();
        let source = write_plugin(&root, "local-tools", "");

        let installed = service
            .install(&InstallPlugin {
                source_path: source.to_string_lossy().into_owned(),
            })
            .unwrap();
        assert_eq!(installed.revision, 1);
        assert!(!installed.value.enabled);
        assert_eq!(installed.value.execution, PluginExecution::ManifestOnly);

        let listed = service.list().unwrap();
        assert_eq!(listed.revision, 1);
        assert_eq!(listed.value.items.len(), 1);

        let enabled = service
            .update("local-tools", &PluginPatch { enabled: true }, 1)
            .unwrap();
        assert_eq!(enabled.revision, 2);
        assert!(enabled.value.enabled);

        assert_eq!(service.uninstall("local-tools", 2).unwrap(), 3);
        assert!(source.exists());
        assert!(service.list().unwrap().value.items.is_empty());
    }

    #[test]
    fn rejects_outside_nested_unknown_and_oversized_manifests() {
        let (home, service, root) = fixture();
        let outside = write_plugin(home.path(), "outside", "");
        assert_eq!(
            service.install(&InstallPlugin {
                source_path: outside.to_string_lossy().into_owned(),
            }),
            Err(PluginError::InvalidRequest)
        );

        let nested = root.join("group");
        let nested_plugin = write_plugin(&nested, "nested", "");
        assert_eq!(
            service.install(&InstallPlugin {
                source_path: nested_plugin.to_string_lossy().into_owned(),
            }),
            Err(PluginError::InvalidRequest)
        );

        let unknown = write_plugin(&root, "unknown", ",\n  \"entryPoint\": \"plugin.py\"");
        assert_eq!(
            service.install(&InstallPlugin {
                source_path: unknown.to_string_lossy().into_owned(),
            }),
            Err(PluginError::ManifestInvalid)
        );

        let oversized = root.join("oversized");
        fs::create_dir_all(&oversized).unwrap();
        fs::write(
            oversized.join(MANIFEST_FILE),
            vec![b' '; MAX_MANIFEST_BYTES as usize + 1],
        )
        .unwrap();
        assert_eq!(
            service.install(&InstallPlugin {
                source_path: oversized.to_string_lossy().into_owned(),
            }),
            Err(PluginError::ManifestInvalid)
        );
    }

    #[test]
    fn rejects_duplicate_tools_invalid_env_and_stale_revisions() {
        let (_home, service, root) = fixture();
        let duplicate = root.join("duplicate");
        fs::create_dir_all(&duplicate).unwrap();
        fs::write(
            duplicate.join(MANIFEST_FILE),
            r#"{
  "id": "duplicate",
  "name": "Duplicate",
  "version": "1.0.0",
  "description": "",
  "author": "SynthChat",
  "providedTools": ["same", "same"],
  "requiresEnv": ["lowercase"]
}"#,
        )
        .unwrap();
        assert_eq!(
            service.install(&InstallPlugin {
                source_path: duplicate.to_string_lossy().into_owned(),
            }),
            Err(PluginError::ManifestInvalid)
        );

        let source = write_plugin(&root, "valid", "");
        service
            .install(&InstallPlugin {
                source_path: source.to_string_lossy().into_owned(),
            })
            .unwrap();
        assert_eq!(
            service.update("valid", &PluginPatch { enabled: true }, 0),
            Err(PluginError::RevisionConflict {
                current_revision: 1
            })
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinked_plugin_directories_and_manifests() {
        use std::os::unix::fs::symlink;

        let (home, service, root) = fixture();
        let outside = write_plugin(home.path(), "outside-link", "");
        symlink(&outside, root.join("outside-link")).unwrap();
        assert_eq!(
            service.install(&InstallPlugin {
                source_path: root.join("outside-link").to_string_lossy().into_owned(),
            }),
            Err(PluginError::InvalidRequest)
        );

        let target = write_plugin(&root, "manifest-link", "");
        fs::remove_file(target.join(MANIFEST_FILE)).unwrap();
        let external_manifest = home.path().join("plugin.json");
        fs::write(&external_manifest, "{}").unwrap();
        symlink(external_manifest, target.join(MANIFEST_FILE)).unwrap();
        assert_eq!(
            service.install(&InstallPlugin {
                source_path: target.to_string_lossy().into_owned(),
            }),
            Err(PluginError::ManifestInvalid)
        );
    }
}
