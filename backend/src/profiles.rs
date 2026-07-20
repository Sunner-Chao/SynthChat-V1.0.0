use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use fs2::FileExt;
use keyring_core::{CredentialStore, Error as KeyringError};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::{Map as JsonMap, Value as JsonValue};
use serde_yaml_ng::{Mapping as YamlMapping, Value as YamlValue};
use sha2::{Digest, Sha256};
use tempfile::{Builder as TempBuilder, NamedTempFile};
use thiserror::Error;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use url::Url;
use zeroize::Zeroizing;

const DEFAULT_PROFILE_ID: &str = "default";
const SECRET_SERVICE: &str = "cc.synthchat.v1.hermes.secrets";
const SYNTHCHAT_DIR: &str = ".synthchat";
const MAX_CONFIG_BYTES: u64 = 4 * 1024 * 1024;
const MAX_METADATA_BYTES: u64 = 256 * 1024;
const MAX_SECRET_BYTES: usize = 2560;
const EPOCH_TIMESTAMP: &str = "1970-01-01T00:00:00Z";
const DEFAULT_WEB_EXTRACT_CHAR_LIMIT: usize = 15_000;
const MIN_WEB_EXTRACT_CHAR_LIMIT: usize = 2_000;
const MAX_WEB_EXTRACT_CHAR_LIMIT: usize = 500_000;
const TAVILY_API_KEY: &str = "TAVILY_API_KEY";
pub(crate) const DEFAULT_CODE_EXECUTION_TIMEOUT_SECONDS: u64 = 300;
pub(crate) const MAX_CODE_EXECUTION_TIMEOUT_SECONDS: u64 = 600;
pub(crate) const DEFAULT_CODE_EXECUTION_TOOL_CALLS: usize = 50;
pub(crate) const MAX_CODE_EXECUTION_TOOL_CALLS: usize = 100;

#[derive(Clone)]
pub struct ProfileService {
    root: Arc<PathBuf>,
    process_lock: Arc<Mutex<()>>,
    secret_store: SecretStore,
}

#[derive(Clone)]
enum SecretStore {
    Available(Arc<CredentialStore>),
    Unavailable,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Provider {
    pub id: String,
    pub display_name: String,
    pub default_base_url: Option<String>,
    pub requires_secret: bool,
    pub secret_names: Vec<String>,
    pub supports_model_discovery: bool,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProfileSummary {
    pub id: String,
    pub display_name: String,
    pub is_default: bool,
    pub is_active: bool,
    pub color: Option<String>,
    pub avatar_file_id: Option<String>,
    pub engine_state: ProfileEngineState,
    pub config_revision: String,
    pub created_at: Option<String>,
    pub updated_at: String,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ProfileEngineState {
    Stopped,
    Starting,
    Running,
    Degraded,
    Failed,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProfileMetadata {
    pub id: String,
    pub display_name: String,
    pub is_default: bool,
    pub color: Option<String>,
    pub avatar_file_id: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Versioned<T> {
    pub value: T,
    pub etag: String,
}

pub(crate) struct ConfigDocumentMutation<T> {
    pub(crate) value: T,
    pub(crate) changed: bool,
}

impl<T> ConfigDocumentMutation<T> {
    pub(crate) fn unchanged(value: T) -> Self {
        Self {
            value,
            changed: false,
        }
    }

    pub(crate) fn changed(value: T) -> Self {
        Self {
            value,
            changed: true,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateProfile {
    pub id: String,
    pub display_name: String,
    #[serde(default)]
    pub clone_from_profile_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ModelConfig {
    pub provider: String,
    pub model: String,
    pub base_url: Option<String>,
    pub reasoning_effort: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProfileConfig {
    pub revision: String,
    pub model: ModelConfig,
    pub code_execution: CodeExecutionConfig,
    pub toolsets: BTreeMap<String, bool>,
    pub skills: BTreeMap<String, bool>,
    pub memory_provider: String,
    pub platforms: BTreeMap<String, bool>,
    pub extensions: JsonMap<String, JsonValue>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CodeExecutionMode {
    Project,
    Strict,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CodeExecutionConfig {
    pub mode: CodeExecutionMode,
    pub timeout_seconds: u64,
    pub max_tool_calls: usize,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WebProvider {
    pub id: String,
    pub display_name: String,
    pub supports_search: bool,
    pub supports_extract: bool,
    pub secret_names: Vec<String>,
    pub default_base_url: String,
    pub custom_endpoint_supported: bool,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum WebProviderStatus {
    Ready,
    Unconfigured,
    MissingSecret,
    Unsupported,
    CapabilityUnsupported,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct EffectiveWebProvider {
    pub provider_id: Option<String>,
    pub status: WebProviderStatus,
    pub missing_secret_names: Vec<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WebConfig {
    pub revision: String,
    pub shared_provider: Option<String>,
    pub search_provider: Option<String>,
    pub extract_provider: Option<String>,
    pub extract_char_limit: usize,
    pub effective_search: EffectiveWebProvider,
    pub effective_extract: EffectiveWebProvider,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebConfigPatch {
    #[serde(default, deserialize_with = "deserialize_nullable_patch_field")]
    pub shared_provider: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_nullable_patch_field")]
    pub search_provider: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_nullable_patch_field")]
    pub extract_provider: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_present_usize")]
    pub extract_char_limit: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProfileSkillSettings {
    pub(crate) disabled: BTreeSet<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProfileMemorySettings {
    pub(crate) memory_enabled: bool,
    pub(crate) user_profile_enabled: bool,
    pub(crate) provider: String,
    pub(crate) memory_char_limit: usize,
    pub(crate) user_char_limit: usize,
}

impl ProfileMemorySettings {
    pub(crate) fn any_enabled(&self) -> bool {
        self.memory_enabled || self.user_profile_enabled
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SecretStatus {
    pub name: String,
    pub configured: bool,
    pub storage: &'static str,
    pub updated_at: Option<String>,
}

#[derive(Debug, Error)]
pub enum ProfileError {
    #[error("invalid profile id")]
    InvalidProfileId,
    #[error("the default profile id is reserved")]
    ReservedProfileId,
    #[error("invalid profile metadata")]
    InvalidProfileMetadata,
    #[error("invalid profile configuration")]
    InvalidProfileConfig,
    #[error("invalid secret name")]
    InvalidSecretName,
    #[error("invalid secret value")]
    InvalidSecretValue,
    #[error("profile not found")]
    ProfileNotFound,
    #[error("profile already exists")]
    ProfileAlreadyExists,
    #[error("the profile cannot be deleted")]
    ProfileDeleteConflict,
    #[error("idempotency key was reused with a different request")]
    IdempotencyConflict,
    #[error("idempotency record points to a removed resource")]
    IdempotencyResourceGone,
    #[error("resource revision conflict")]
    RevisionConflict { current_etag: String },
    #[error("secret storage is unavailable")]
    SecretStorageUnavailable,
    #[error("profile path is unsafe")]
    UnsafeProfilePath,
    #[error("profile data exceeds its size limit")]
    DataTooLarge,
    #[error("profile data is malformed")]
    DataInvalid,
    #[error("profile storage operation failed")]
    Storage(#[source] io::Error),
}

impl ProfileService {
    pub fn with_system_store(root: PathBuf) -> Self {
        let secret_store = match system_credential_store() {
            Some(store) => SecretStore::Available(store),
            None => SecretStore::Unavailable,
        };
        Self::new(root, secret_store)
    }

    pub fn with_credential_store(root: PathBuf, store: Arc<CredentialStore>) -> Self {
        Self::new(root, SecretStore::Available(store))
    }

    pub fn without_credential_store(root: PathBuf) -> Self {
        Self::new(root, SecretStore::Unavailable)
    }

    fn new(root: PathBuf, secret_store: SecretStore) -> Self {
        Self {
            root: Arc::new(root),
            process_lock: Arc::new(Mutex::new(())),
            secret_store,
        }
    }

    pub(crate) fn hermes_home(&self) -> &Path {
        self.root.as_path()
    }

    pub fn providers(&self) -> Vec<Provider> {
        PROVIDERS
            .iter()
            .map(|provider| Provider {
                id: provider.id.to_owned(),
                display_name: provider.display_name.to_owned(),
                default_base_url: provider.default_base_url.map(ToOwned::to_owned),
                requires_secret: provider.requires_secret,
                secret_names: provider
                    .secret_names
                    .iter()
                    .map(|name| (*name).to_owned())
                    .collect(),
                // Discovery is enabled only when the corresponding Rust route is live.
                supports_model_discovery: false,
            })
            .collect()
    }

    pub fn list_profiles(
        &self,
        engine_state: ProfileEngineState,
    ) -> Result<Vec<ProfileSummary>, ProfileError> {
        self.with_storage_lock(|| {
            let active_id = self.read_active_profile_id()?;
            let mut ids = vec![DEFAULT_PROFILE_ID.to_owned()];
            ids.extend(self.named_profile_ids()?);
            ids.into_iter()
                .map(|id| self.profile_summary_locked(&id, &active_id, engine_state))
                .collect()
        })
    }

    pub fn get_profile(&self, id: &str) -> Result<Versioned<ProfileMetadata>, ProfileError> {
        validate_profile_id(id)?;
        self.with_storage_lock(|| self.profile_metadata_locked(id))
    }

    pub(crate) fn with_existing_profile<T, E>(
        &self,
        id: &str,
        operation: impl FnOnce() -> Result<T, E>,
    ) -> Result<Result<T, E>, ProfileError> {
        validate_profile_id(id)?;
        self.with_storage_lock(|| {
            self.require_profile_locked(id)?;
            Ok(operation())
        })
    }

    pub(crate) fn with_memory_profile<T, E>(
        &self,
        id: &str,
        operation: impl FnOnce(&Path, &ProfileMemorySettings) -> Result<T, E>,
    ) -> Result<Result<T, E>, ProfileError> {
        validate_profile_id(id)?;
        self.with_storage_lock(|| {
            let root = self.profile_root_locked(id)?;
            let settings = self.profile_memory_settings_locked(id)?;
            Ok(operation(&root, &settings))
        })
    }

    pub(crate) fn with_hermes_state_db<T, E>(
        &self,
        id: &str,
        operation: impl FnOnce(&Path) -> Result<T, E>,
    ) -> Result<Result<T, E>, ProfileError> {
        validate_profile_id(id)?;
        self.with_storage_lock(|| {
            let path = self.profile_root_locked(id)?.join("state.db");
            Ok(operation(&path))
        })
    }

    pub fn create_profile(
        &self,
        request: &CreateProfile,
        idempotency_key: &str,
    ) -> Result<Versioned<ProfileMetadata>, ProfileError> {
        validate_named_profile_id(&request.id)?;
        validate_display_name(&request.display_name)?;
        if let Some(source) = request.clone_from_profile_id.as_deref() {
            validate_profile_id(source)?;
        }
        let fingerprint = request_fingerprint(request)?;

        self.with_storage_lock(|| {
            let mut record =
                if let Some(mut record) = self.read_idempotency_record(idempotency_key)? {
                    if record.fingerprint != fingerprint {
                        return Err(ProfileError::IdempotencyConflict);
                    }
                    if record.profile_id != request.id {
                        return Err(ProfileError::IdempotencyConflict);
                    }
                    if self.profile_exists_locked(&record.profile_id)? {
                        let resource = self.profile_metadata_locked(&record.profile_id)?;
                        if resource.etag != record.resource_etag {
                            return Err(ProfileError::IdempotencyResourceGone);
                        }
                        if record.state == IdempotencyState::Pending {
                            record.state = IdempotencyState::Completed;
                            self.write_idempotency_record(idempotency_key, &record)?;
                        }
                        return Ok(resource);
                    }
                    if record.state == IdempotencyState::Completed {
                        return Err(ProfileError::IdempotencyResourceGone);
                    }
                    record
                } else {
                    if self.profile_exists_locked(&request.id)? {
                        return Err(ProfileError::ProfileAlreadyExists);
                    }
                    if let Some(source) = request.clone_from_profile_id.as_deref() {
                        let _ = self.read_raw_config_locked(source)?;
                    }
                    let created_at = now_timestamp()?;
                    let metadata = profile_metadata_document(request, &created_at);
                    let metadata_bytes = json_bytes_bounded(&metadata, MAX_METADATA_BYTES)?;
                    let record = IdempotencyRecord {
                        fingerprint,
                        profile_id: request.id.clone(),
                        created_at,
                        resource_etag: etag_for_bytes(&metadata_bytes),
                        state: IdempotencyState::Pending,
                    };
                    self.write_idempotency_record(idempotency_key, &record)?;
                    record
                };

            let clone_bytes = request
                .clone_from_profile_id
                .as_deref()
                .map(|source| self.read_raw_config_locked(source))
                .transpose()?
                .flatten();
            let profiles_dir = self.ensure_profiles_dir()?;
            let target = profiles_dir.join(&request.id);
            ensure_path_absent(&target)?;
            let staging = TempBuilder::new()
                .prefix(".profile-create-")
                .tempdir_in(&profiles_dir)
                .map_err(ProfileError::Storage)?;
            let metadata = profile_metadata_document(request, &record.created_at);
            atomic_write_json_bounded(
                &staging.path().join("profile-meta.json"),
                &metadata,
                MAX_METADATA_BYTES,
            )?;
            if let Some(bytes) = clone_bytes.as_deref() {
                atomic_write(&staging.path().join("config.yaml"), bytes)?;
            }
            sync_directory(staging.path())?;
            let staged_path = staging.keep();
            fs::rename(&staged_path, &target).map_err(ProfileError::Storage)?;
            sync_directory(&profiles_dir)?;

            let resource = self.profile_metadata_locked(&request.id)?;
            if resource.etag != record.resource_etag {
                return Err(ProfileError::DataInvalid);
            }
            record.state = IdempotencyState::Completed;
            self.write_idempotency_record(idempotency_key, &record)?;
            Ok(resource)
        })
    }

    pub fn update_profile(
        &self,
        id: &str,
        expected_etag: &str,
        patch: &JsonValue,
    ) -> Result<Versioned<ProfileMetadata>, ProfileError> {
        validate_profile_id(id)?;
        let patch = validate_metadata_patch(patch)?;
        self.with_storage_lock(|| {
            let current = self.profile_metadata_locked(id)?;
            ensure_revision(expected_etag, &current.etag)?;
            if patch.is_empty() {
                return Ok(current);
            }

            let path = self.profile_file_locked(id, "profile-meta.json")?;
            let raw = read_optional_bounded(&path, MAX_METADATA_BYTES)?.unwrap_or_default();
            let mut object = parse_metadata_object(&raw)?;
            let before = object.clone();
            apply_metadata_patch(&mut object, &patch)?;
            if object == before {
                return Ok(current);
            }
            set_metadata_updated_at(&mut object, now_timestamp()?);
            atomic_write_json_bounded(&path, &object, MAX_METADATA_BYTES)?;
            self.profile_metadata_locked(id)
        })
    }

    pub fn activate_profile(
        &self,
        id: &str,
        engine_state: ProfileEngineState,
    ) -> Result<ProfileSummary, ProfileError> {
        validate_profile_id(id)?;
        self.with_storage_lock(|| {
            if !self.profile_exists_locked(id)? {
                return Err(ProfileError::ProfileNotFound);
            }
            let summary = self.profile_summary_locked(id, id, engine_state)?;
            let current = self.read_active_profile_id()?;
            if current != id {
                let path = self.root.join("active_profile");
                reject_symlink(&path)?;
                atomic_write(&path, format!("{id}\n").as_bytes())?;
            }
            Ok(summary)
        })
    }

    pub fn delete_profile(&self, id: &str) -> Result<(), ProfileError> {
        validate_profile_id(id)?;
        if id == DEFAULT_PROFILE_ID {
            return Err(ProfileError::ProfileDeleteConflict);
        }
        self.with_storage_lock(|| {
            if !self.profile_exists_locked(id)? {
                return Ok(());
            }
            if self.read_active_profile_id()? == id {
                return Err(ProfileError::ProfileDeleteConflict);
            }

            let profiles_dir = self.ensure_profiles_dir()?;
            let target = profiles_dir.join(id);
            verify_direct_child_directory(&profiles_dir, &target)?;
            self.delete_indexed_secrets_locked(id)?;
            fs::remove_dir_all(&target).map_err(ProfileError::Storage)?;
            let index_path = self.secret_index_path(id)?;
            remove_regular_file_if_exists(&index_path)?;
            sync_directory(&profiles_dir)?;
            Ok(())
        })
    }

    pub fn get_config(&self, id: &str) -> Result<Versioned<ProfileConfig>, ProfileError> {
        validate_profile_id(id)?;
        self.with_storage_lock(|| self.profile_config_locked(id))
    }

    pub(crate) fn transact_config_document<T, E>(
        &self,
        id: &str,
        expected_etag: Option<&str>,
        operation: impl FnOnce(&mut YamlValue) -> Result<ConfigDocumentMutation<T>, E>,
    ) -> Result<Versioned<T>, E>
    where
        E: From<ProfileError>,
    {
        validate_profile_id(id).map_err(E::from)?;
        self.with_storage_lock(|| {
            let path = self
                .profile_file_locked(id, "config.yaml")
                .map_err(E::from)?;
            let raw = read_optional_bounded(&path, MAX_CONFIG_BYTES)
                .map_err(E::from)?
                .unwrap_or_default();
            let current_etag = quote_revision(&revision_for_bytes(&raw));
            if let Some(expected_etag) = expected_etag {
                ensure_revision(expected_etag, &current_etag).map_err(E::from)?;
            }
            let mut document = parse_config_document(&raw).map_err(E::from)?;
            let mutation = operation(&mut document)?;
            let etag = if mutation.changed {
                let serialized = serde_yaml_ng::to_string(&document)
                    .map_err(|_| E::from(ProfileError::InvalidProfileConfig))?;
                if serialized.len() as u64 > MAX_CONFIG_BYTES {
                    return Err(E::from(ProfileError::DataTooLarge));
                }
                if serialized.as_bytes() == raw {
                    current_etag
                } else {
                    atomic_write(&path, serialized.as_bytes()).map_err(E::from)?;
                    quote_revision(&revision_for_bytes(serialized.as_bytes()))
                }
            } else {
                current_etag
            };
            Ok(Versioned {
                value: mutation.value,
                etag,
            })
        })
    }

    pub(crate) fn missing_secret_names(
        &self,
        id: &str,
        names: &BTreeSet<String>,
    ) -> Result<BTreeSet<String>, ProfileError> {
        validate_profile_id(id)?;
        for name in names {
            validate_secret_name(name)?;
        }
        self.with_storage_lock(|| {
            self.require_profile_locked(id)?;
            if names.is_empty() {
                return Ok(BTreeSet::new());
            }
            let store = self.available_secret_store()?;
            names
                .iter()
                .filter_map(|name| match secret_is_configured(store, id, name) {
                    Ok(true) => None,
                    Ok(false) => Some(Ok(name.clone())),
                    Err(error) => Some(Err(error)),
                })
                .collect()
        })
    }

    pub(crate) fn get_web_config(&self, id: &str) -> Result<Versioned<WebConfig>, ProfileError> {
        validate_profile_id(id)?;
        self.with_storage_lock(|| self.profile_web_config_locked(id))
    }

    pub(crate) fn update_web_config(
        &self,
        id: &str,
        expected_etag: &str,
        patch: &WebConfigPatch,
    ) -> Result<Versioned<WebConfig>, ProfileError> {
        validate_profile_id(id)?;
        validate_web_config_patch(patch)?;
        self.with_storage_lock(|| {
            let current = self.profile_web_config_locked(id)?;
            ensure_revision(expected_etag, &current.etag)?;
            if web_config_patch_is_empty(patch) {
                return Ok(current);
            }

            let path = self.profile_file_locked(id, "config.yaml")?;
            let raw = read_optional_bounded(&path, MAX_CONFIG_BYTES)?.unwrap_or_default();
            let mut document = parse_config_document(&raw)?;
            apply_web_config_patch(&mut document, patch)?;
            let candidate = self.web_config_from_document_locked(
                id,
                &document,
                current.value.revision.clone(),
            )?;
            if candidate == current.value {
                return Ok(current);
            }
            let serialized = serde_yaml_ng::to_string(&document)
                .map_err(|_| ProfileError::InvalidProfileConfig)?;
            if serialized.len() as u64 > MAX_CONFIG_BYTES {
                return Err(ProfileError::DataTooLarge);
            }
            atomic_write(&path, serialized.as_bytes())?;
            self.profile_web_config_locked(id)
        })
    }

    pub(crate) fn skill_root_and_settings(
        &self,
        id: &str,
    ) -> Result<(PathBuf, Versioned<ProfileSkillSettings>), ProfileError> {
        validate_profile_id(id)?;
        self.with_storage_lock(|| {
            let root = self.profile_root_locked(id)?.join("skills");
            reject_symlink(&root)?;
            let settings = self.profile_skill_settings_locked(id)?;
            Ok((root, settings))
        })
    }

    pub(crate) fn update_skill_enabled(
        &self,
        id: &str,
        skill_name: &str,
        enabled: bool,
        expected_etag: &str,
    ) -> Result<Versioned<ProfileSkillSettings>, ProfileError> {
        validate_profile_id(id)?;
        validate_skill_name(skill_name)?;
        self.with_storage_lock(|| {
            let current = self.profile_skill_settings_locked(id)?;
            ensure_revision(expected_etag, &current.etag)?;
            let currently_enabled = !current.value.disabled.contains(skill_name);
            if currently_enabled == enabled {
                return Ok(current);
            }

            let path = self.profile_file_locked(id, "config.yaml")?;
            let raw = read_optional_bounded(&path, MAX_CONFIG_BYTES)?.unwrap_or_default();
            let mut document = parse_config_document(&raw)?;
            let mut disabled = current.value.disabled;
            if enabled {
                disabled.remove(skill_name);
            } else {
                disabled.insert(skill_name.to_owned());
            }
            set_yaml_path(
                &mut document,
                &["skills", "disabled"],
                yaml_string_set(&disabled),
            )?;
            let serialized = serde_yaml_ng::to_string(&document)
                .map_err(|_| ProfileError::InvalidProfileConfig)?;
            if serialized.len() as u64 > MAX_CONFIG_BYTES {
                return Err(ProfileError::DataTooLarge);
            }
            atomic_write(&path, serialized.as_bytes())?;
            self.profile_skill_settings_locked(id)
        })
    }

    pub fn update_config(
        &self,
        id: &str,
        expected_etag: &str,
        patch: &JsonValue,
    ) -> Result<Versioned<ProfileConfig>, ProfileError> {
        validate_profile_id(id)?;
        let patch = validate_config_patch(patch)?;
        self.with_storage_lock(|| {
            let current = self.profile_config_locked(id)?;
            ensure_revision(expected_etag, &current.etag)?;
            if patch.is_empty() {
                return Ok(current);
            }

            let path = self.profile_file_locked(id, "config.yaml")?;
            let raw = read_optional_bounded(&path, MAX_CONFIG_BYTES)?.unwrap_or_default();
            let mut document = parse_config_document(&raw)?;
            apply_config_patch(&mut document, &patch)?;
            let candidate = config_from_document(&document, current.value.revision.clone())?;
            if candidate == current.value {
                return Ok(current);
            }
            let serialized = serde_yaml_ng::to_string(&document)
                .map_err(|_| ProfileError::InvalidProfileConfig)?;
            if serialized.len() as u64 > MAX_CONFIG_BYTES {
                return Err(ProfileError::DataTooLarge);
            }
            atomic_write(&path, serialized.as_bytes())?;
            self.profile_config_locked(id)
        })
    }

    pub fn list_secret_statuses(&self, id: &str) -> Result<Vec<SecretStatus>, ProfileError> {
        validate_profile_id(id)?;
        self.with_storage_lock(|| {
            self.require_profile_locked(id)?;
            let store = self.available_secret_store()?;
            let index = self.read_secret_index(id)?;
            let mut names: BTreeSet<String> = PROVIDERS
                .iter()
                .flat_map(|provider| provider.secret_names.iter().copied())
                .map(ToOwned::to_owned)
                .collect();
            names.insert(TAVILY_API_KEY.to_owned());
            names.extend(index.keys().cloned());
            names
                .into_iter()
                .map(|name| {
                    let configured = secret_is_configured(store, id, &name)?;
                    Ok(SecretStatus {
                        updated_at: index.get(&name).cloned(),
                        name,
                        configured,
                        storage: "osKeychain",
                    })
                })
                .collect()
        })
    }

    pub fn put_secret(
        &self,
        id: &str,
        name: &str,
        value: &SecretString,
    ) -> Result<SecretStatus, ProfileError> {
        validate_profile_id(id)?;
        validate_secret_name(name)?;
        validate_secret_value(value)?;
        self.with_storage_lock(|| {
            self.require_profile_locked(id)?;
            let store = self.available_secret_store()?;
            let entry = store
                .build(SECRET_SERVICE, &secret_account(id, name), None)
                .map_err(|_| ProfileError::SecretStorageUnavailable)?;
            let updated_at = now_timestamp()?;
            let mut index = self.read_secret_index(id)?;
            let previous = index.insert(name.to_owned(), updated_at.clone());
            self.write_secret_index(id, &index)?;
            if entry.set_secret(value.expose_secret().as_bytes()).is_err() {
                match previous {
                    Some(previous) => {
                        index.insert(name.to_owned(), previous);
                    }
                    None => {
                        index.remove(name);
                    }
                }
                let _ = self.write_secret_index(id, &index);
                return Err(ProfileError::SecretStorageUnavailable);
            }
            Ok(SecretStatus {
                name: name.to_owned(),
                configured: true,
                storage: "osKeychain",
                updated_at: Some(updated_at),
            })
        })
    }

    pub fn delete_secret(&self, id: &str, name: &str) -> Result<(), ProfileError> {
        validate_profile_id(id)?;
        validate_secret_name(name)?;
        self.with_storage_lock(|| {
            self.require_profile_locked(id)?;
            let store = self.available_secret_store()?;
            delete_secret_from_store(store, id, name)?;
            let mut index = self.read_secret_index(id)?;
            if index.remove(name).is_some() {
                self.write_secret_index(id, &index)?;
            }
            Ok(())
        })
    }

    pub(crate) fn first_secret_snapshot(
        &self,
        id: &str,
        names: &[String],
        required: bool,
    ) -> Result<Option<(String, SecretString)>, ProfileError> {
        validate_profile_id(id)?;
        for name in names {
            validate_secret_name(name)?;
        }
        self.with_storage_lock(|| {
            self.require_profile_locked(id)?;
            let store = match &self.secret_store {
                SecretStore::Available(store) => store,
                SecretStore::Unavailable if !required => return Ok(None),
                SecretStore::Unavailable => return Err(ProfileError::SecretStorageUnavailable),
            };
            for name in names {
                let entry = store
                    .build(SECRET_SERVICE, &secret_account(id, name), None)
                    .map_err(|_| ProfileError::SecretStorageUnavailable)?;
                match entry.get_secret() {
                    Ok(value) => {
                        let value = Zeroizing::new(value);
                        let value = std::str::from_utf8(value.as_slice())
                            .map_err(|_| ProfileError::SecretStorageUnavailable)?;
                        if value.is_empty() {
                            return Err(ProfileError::SecretStorageUnavailable);
                        }
                        return Ok(Some((name.clone(), SecretString::from(value.to_owned()))));
                    }
                    Err(KeyringError::NoEntry) => {}
                    Err(_) => return Err(ProfileError::SecretStorageUnavailable),
                }
            }
            Ok(None)
        })
    }

    pub(crate) fn secret_redaction_snapshots(
        &self,
        id: &str,
    ) -> Result<Vec<SecretString>, ProfileError> {
        validate_profile_id(id)?;
        self.with_storage_lock(|| {
            self.require_profile_locked(id)?;
            let store = match &self.secret_store {
                SecretStore::Available(store) => store,
                SecretStore::Unavailable => return Ok(Vec::new()),
            };
            let index = self.read_secret_index(id)?;
            let mut names: BTreeSet<String> = PROVIDERS
                .iter()
                .flat_map(|provider| provider.secret_names.iter().copied())
                .map(ToOwned::to_owned)
                .collect();
            names.insert(TAVILY_API_KEY.to_owned());
            names.extend(index.keys().cloned());

            let mut secrets = Vec::new();
            for name in names {
                let entry = store
                    .build(SECRET_SERVICE, &secret_account(id, &name), None)
                    .map_err(|_| ProfileError::SecretStorageUnavailable)?;
                match entry.get_secret() {
                    Ok(value) => {
                        let value = Zeroizing::new(value);
                        let value = std::str::from_utf8(value.as_slice())
                            .map_err(|_| ProfileError::SecretStorageUnavailable)?;
                        if !value.is_empty() {
                            secrets.push(SecretString::from(value.to_owned()));
                        }
                    }
                    Err(KeyringError::NoEntry) => {}
                    Err(_) => return Err(ProfileError::SecretStorageUnavailable),
                }
            }
            Ok(secrets)
        })
    }

    fn available_secret_store(&self) -> Result<&Arc<CredentialStore>, ProfileError> {
        match &self.secret_store {
            SecretStore::Available(store) => Ok(store),
            SecretStore::Unavailable => Err(ProfileError::SecretStorageUnavailable),
        }
    }

    fn with_storage_lock<T, E>(&self, operation: impl FnOnce() -> Result<T, E>) -> Result<T, E>
    where
        E: From<ProfileError>,
    {
        let _process_guard = self
            .process_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.ensure_storage_root().map_err(E::from)?;
        let lock_path = self.synthchat_dir().map_err(E::from)?.join("profiles.lock");
        reject_symlink(&lock_path).map_err(E::from)?;
        let lock_file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .map_err(ProfileError::Storage)
            .map_err(E::from)?;
        FileExt::lock_exclusive(&lock_file)
            .map_err(ProfileError::Storage)
            .map_err(E::from)?;
        let result = operation();
        let unlock_result = FileExt::unlock(&lock_file).map_err(ProfileError::Storage);
        match (result, unlock_result) {
            (Ok(value), Ok(())) => Ok(value),
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(E::from(error)),
        }
    }

    fn ensure_storage_root(&self) -> Result<(), ProfileError> {
        fs::create_dir_all(self.root.as_ref()).map_err(ProfileError::Storage)?;
        ensure_directory(self.root.as_ref())?;
        let synthchat = self.root.join(SYNTHCHAT_DIR);
        fs::create_dir_all(&synthchat).map_err(ProfileError::Storage)?;
        ensure_directory(&synthchat)
    }

    fn synthchat_dir(&self) -> Result<PathBuf, ProfileError> {
        let path = self.root.join(SYNTHCHAT_DIR);
        ensure_directory(&path)?;
        Ok(path)
    }

    fn ensure_profiles_dir(&self) -> Result<PathBuf, ProfileError> {
        let path = self.root.join("profiles");
        fs::create_dir_all(&path).map_err(ProfileError::Storage)?;
        ensure_directory(&path)?;
        Ok(path)
    }

    fn named_profile_ids(&self) -> Result<Vec<String>, ProfileError> {
        let profiles_dir = self.root.join("profiles");
        match fs::symlink_metadata(&profiles_dir) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    return Err(ProfileError::UnsafeProfilePath);
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(ProfileError::Storage(error)),
        }
        let mut ids = Vec::new();
        for entry in fs::read_dir(&profiles_dir).map_err(ProfileError::Storage)? {
            let entry = entry.map_err(ProfileError::Storage)?;
            let Some(id) = entry.file_name().to_str().map(ToOwned::to_owned) else {
                continue;
            };
            if validate_named_profile_id(&id).is_err() {
                continue;
            }
            let metadata = fs::symlink_metadata(entry.path()).map_err(ProfileError::Storage)?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(ProfileError::UnsafeProfilePath);
            }
            ids.push(id);
        }
        ids.sort();
        Ok(ids)
    }

    fn profile_exists_locked(&self, id: &str) -> Result<bool, ProfileError> {
        if id == DEFAULT_PROFILE_ID {
            return Ok(true);
        }
        let profiles_dir = self.root.join("profiles");
        match fs::symlink_metadata(&profiles_dir) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(ProfileError::UnsafeProfilePath);
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(ProfileError::Storage(error)),
        }
        let path = profiles_dir.join(id);
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                Err(ProfileError::UnsafeProfilePath)
            }
            Ok(_) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(ProfileError::Storage(error)),
        }
    }

    fn require_profile_locked(&self, id: &str) -> Result<(), ProfileError> {
        if self.profile_exists_locked(id)? {
            Ok(())
        } else {
            Err(ProfileError::ProfileNotFound)
        }
    }

    fn profile_root_locked(&self, id: &str) -> Result<PathBuf, ProfileError> {
        self.require_profile_locked(id)?;
        if id == DEFAULT_PROFILE_ID {
            return Ok(self.root.as_ref().clone());
        }
        let profiles_dir = self.root.join("profiles");
        ensure_directory(&profiles_dir)?;
        let profile = profiles_dir.join(id);
        verify_direct_child_directory(&profiles_dir, &profile)?;
        Ok(profile)
    }

    fn profile_file_locked(&self, id: &str, name: &str) -> Result<PathBuf, ProfileError> {
        let path = self.profile_root_locked(id)?.join(name);
        reject_symlink(&path)?;
        Ok(path)
    }

    fn profile_metadata_locked(
        &self,
        id: &str,
    ) -> Result<Versioned<ProfileMetadata>, ProfileError> {
        let path = self.profile_file_locked(id, "profile-meta.json")?;
        let raw = read_optional_bounded(&path, MAX_METADATA_BYTES)?.unwrap_or_default();
        let object = parse_metadata_object(&raw)?;
        let metadata = metadata_from_object(id, &object)?;
        Ok(Versioned {
            value: metadata,
            etag: etag_for_bytes(&raw),
        })
    }

    fn profile_summary_locked(
        &self,
        id: &str,
        active_id: &str,
        engine_state: ProfileEngineState,
    ) -> Result<ProfileSummary, ProfileError> {
        let metadata = self.profile_metadata_locked(id)?.value;
        let config_revision = self.profile_config_locked(id)?.value.revision;
        Ok(ProfileSummary {
            id: metadata.id,
            display_name: metadata.display_name,
            is_default: metadata.is_default,
            is_active: id == active_id,
            color: metadata.color,
            avatar_file_id: metadata.avatar_file_id,
            engine_state,
            config_revision,
            created_at: metadata.created_at,
            updated_at: metadata.updated_at,
        })
    }

    fn profile_config_locked(&self, id: &str) -> Result<Versioned<ProfileConfig>, ProfileError> {
        let raw = self.read_raw_config_locked(id)?.unwrap_or_default();
        let document = parse_config_document(&raw)?;
        let revision = revision_for_bytes(&raw);
        let value = config_from_document(&document, revision.clone())?;
        Ok(Versioned {
            value,
            etag: quote_revision(&revision),
        })
    }

    fn profile_web_config_locked(&self, id: &str) -> Result<Versioned<WebConfig>, ProfileError> {
        let raw = self.read_raw_config_locked(id)?.unwrap_or_default();
        let document = parse_config_document(&raw)?;
        let revision = revision_for_bytes(&raw);
        let value = self.web_config_from_document_locked(id, &document, revision.clone())?;
        Ok(Versioned {
            value,
            etag: quote_revision(&revision),
        })
    }

    fn web_config_from_document_locked(
        &self,
        id: &str,
        document: &YamlValue,
        revision: String,
    ) -> Result<WebConfig, ProfileError> {
        let shared_provider =
            normalized_optional_web_provider(yaml_optional_string(document, &["web", "backend"])?);
        let search_provider = normalized_optional_web_provider(yaml_optional_string(
            document,
            &["web", "search_backend"],
        )?);
        let extract_provider = normalized_optional_web_provider(yaml_optional_string(
            document,
            &["web", "extract_backend"],
        )?);
        let extract_char_limit = web_extract_char_limit(document)?;
        let tavily_configured =
            secret_is_configured(self.available_secret_store()?, id, TAVILY_API_KEY)?;
        let effective_search = effective_web_provider(
            search_provider.as_deref().or(shared_provider.as_deref()),
            tavily_configured,
        );
        let effective_extract = effective_web_provider(
            extract_provider.as_deref().or(shared_provider.as_deref()),
            tavily_configured,
        );
        Ok(WebConfig {
            revision,
            shared_provider,
            search_provider,
            extract_provider,
            extract_char_limit,
            effective_search,
            effective_extract,
        })
    }

    fn profile_skill_settings_locked(
        &self,
        id: &str,
    ) -> Result<Versioned<ProfileSkillSettings>, ProfileError> {
        let raw = self.read_raw_config_locked(id)?.unwrap_or_default();
        let document = parse_config_document(&raw)?;
        let disabled = yaml_compatible_string_set(&document, &["skills", "disabled"])?;
        Ok(Versioned {
            value: ProfileSkillSettings { disabled },
            etag: quote_revision(&revision_for_bytes(&raw)),
        })
    }

    fn profile_memory_settings_locked(
        &self,
        id: &str,
    ) -> Result<ProfileMemorySettings, ProfileError> {
        let raw = self.read_raw_config_locked(id)?.unwrap_or_default();
        let document = parse_config_document(&raw)?;
        Ok(ProfileMemorySettings {
            memory_enabled: yaml_optional_bool(&document, &["memory", "memory_enabled"])?
                .unwrap_or(true),
            user_profile_enabled: yaml_optional_bool(
                &document,
                &["memory", "user_profile_enabled"],
            )?
            .unwrap_or(true),
            provider: normalized_memory_provider(yaml_optional_string(
                &document,
                &["memory", "provider"],
            )?),
            memory_char_limit: yaml_positive_usize(&document, &["memory", "memory_char_limit"])
                .unwrap_or(2_200),
            user_char_limit: yaml_positive_usize(&document, &["memory", "user_char_limit"])
                .unwrap_or(1_375),
        })
    }

    fn read_raw_config_locked(&self, id: &str) -> Result<Option<Vec<u8>>, ProfileError> {
        let path = self.profile_file_locked(id, "config.yaml")?;
        read_optional_bounded(&path, MAX_CONFIG_BYTES)
    }

    fn read_active_profile_id(&self) -> Result<String, ProfileError> {
        let path = self.root.join("active_profile");
        reject_symlink(&path)?;
        let Some(raw) = read_optional_bounded(&path, 256)? else {
            return Ok(DEFAULT_PROFILE_ID.to_owned());
        };
        let value = std::str::from_utf8(&raw)
            .map_err(|_| ProfileError::DataInvalid)?
            .trim();
        if value.is_empty() {
            return Ok(DEFAULT_PROFILE_ID.to_owned());
        }
        validate_profile_id(value)?;
        if self.profile_exists_locked(value)? {
            Ok(value.to_owned())
        } else {
            Ok(DEFAULT_PROFILE_ID.to_owned())
        }
    }

    fn idempotency_path(&self, key: &str) -> Result<PathBuf, ProfileError> {
        let idempotency_dir = self.synthchat_dir()?.join("idempotency");
        fs::create_dir_all(&idempotency_dir).map_err(ProfileError::Storage)?;
        ensure_directory(&idempotency_dir)?;
        let directory = idempotency_dir.join("profiles");
        fs::create_dir_all(&directory).map_err(ProfileError::Storage)?;
        ensure_directory(&directory)?;
        let digest = sha256_hex(format!("POST\n/api/v1/profiles\n{key}").as_bytes());
        Ok(directory.join(format!("{digest}.json")))
    }

    fn read_idempotency_record(
        &self,
        key: &str,
    ) -> Result<Option<IdempotencyRecord>, ProfileError> {
        let path = self.idempotency_path(key)?;
        let Some(raw) = read_optional_bounded(&path, 16 * 1024)? else {
            return Ok(None);
        };
        serde_json::from_slice(&raw)
            .map(Some)
            .map_err(|_| ProfileError::DataInvalid)
    }

    fn write_idempotency_record(
        &self,
        key: &str,
        record: &IdempotencyRecord,
    ) -> Result<(), ProfileError> {
        atomic_write_json_bounded(&self.idempotency_path(key)?, record, 16 * 1024)
    }

    fn secret_index_path(&self, id: &str) -> Result<PathBuf, ProfileError> {
        let directory = self.synthchat_dir()?.join("secret-index");
        fs::create_dir_all(&directory).map_err(ProfileError::Storage)?;
        ensure_directory(&directory)?;
        Ok(directory.join(format!("{id}.json")))
    }

    fn read_secret_index(&self, id: &str) -> Result<BTreeMap<String, String>, ProfileError> {
        let path = self.secret_index_path(id)?;
        let Some(raw) = read_optional_bounded(&path, 256 * 1024)? else {
            return Ok(BTreeMap::new());
        };
        let index: BTreeMap<String, String> =
            serde_json::from_slice(&raw).map_err(|_| ProfileError::DataInvalid)?;
        for name in index.keys() {
            validate_secret_name(name).map_err(|_| ProfileError::DataInvalid)?;
        }
        Ok(index)
    }

    fn write_secret_index(
        &self,
        id: &str,
        index: &BTreeMap<String, String>,
    ) -> Result<(), ProfileError> {
        atomic_write_json_bounded(&self.secret_index_path(id)?, index, 256 * 1024)
    }

    fn delete_indexed_secrets_locked(&self, id: &str) -> Result<(), ProfileError> {
        let index = self.read_secret_index(id)?;
        if index.is_empty() {
            return Ok(());
        }
        let store = self.available_secret_store()?;
        for name in index.keys() {
            delete_secret_from_store(store, id, name)?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
struct ProviderDefinition {
    id: &'static str,
    display_name: &'static str,
    default_base_url: Option<&'static str>,
    requires_secret: bool,
    secret_names: &'static [&'static str],
}

const PROVIDERS: &[ProviderDefinition] = &[
    ProviderDefinition {
        id: "auto",
        display_name: "Automatic",
        default_base_url: None,
        requires_secret: true,
        secret_names: &["OPENROUTER_API_KEY", "OPENAI_API_KEY"],
    },
    ProviderDefinition {
        id: "openrouter",
        display_name: "OpenRouter",
        default_base_url: Some("https://openrouter.ai/api/v1"),
        requires_secret: true,
        secret_names: &["OPENROUTER_API_KEY"],
    },
    ProviderDefinition {
        id: "custom",
        display_name: "Custom OpenAI-compatible",
        default_base_url: None,
        requires_secret: true,
        secret_names: &["OPENAI_API_KEY"],
    },
    ProviderDefinition {
        id: "openai-api",
        display_name: "OpenAI API",
        default_base_url: Some("https://api.openai.com/v1"),
        requires_secret: true,
        secret_names: &["OPENAI_API_KEY"],
    },
    ProviderDefinition {
        id: "lmstudio",
        display_name: "LM Studio",
        default_base_url: Some("http://127.0.0.1:1234/v1"),
        requires_secret: false,
        secret_names: &["LM_API_KEY"],
    },
    ProviderDefinition {
        id: "copilot",
        display_name: "GitHub Copilot",
        default_base_url: Some("https://api.githubcopilot.com"),
        requires_secret: true,
        secret_names: &["COPILOT_GITHUB_TOKEN", "GH_TOKEN", "GITHUB_TOKEN"],
    },
    ProviderDefinition {
        id: "gemini",
        display_name: "Google AI Studio",
        default_base_url: Some("https://generativelanguage.googleapis.com/v1beta"),
        requires_secret: true,
        secret_names: &["GOOGLE_API_KEY", "GEMINI_API_KEY"],
    },
    ProviderDefinition {
        id: "zai",
        display_name: "Z.AI / GLM",
        default_base_url: Some("https://api.z.ai/api/paas/v4"),
        requires_secret: true,
        secret_names: &["GLM_API_KEY", "ZAI_API_KEY", "Z_AI_API_KEY"],
    },
    ProviderDefinition {
        id: "kimi-coding",
        display_name: "Kimi / Moonshot",
        default_base_url: Some("https://api.moonshot.ai/v1"),
        requires_secret: true,
        secret_names: &["KIMI_API_KEY", "KIMI_CODING_API_KEY"],
    },
    ProviderDefinition {
        id: "kimi-coding-cn",
        display_name: "Kimi / Moonshot (China)",
        default_base_url: Some("https://api.moonshot.cn/v1"),
        requires_secret: true,
        secret_names: &["KIMI_CN_API_KEY"],
    },
    ProviderDefinition {
        id: "stepfun",
        display_name: "StepFun Step Plan",
        default_base_url: Some("https://api.stepfun.ai/step_plan/v1"),
        requires_secret: true,
        secret_names: &["STEPFUN_API_KEY"],
    },
    ProviderDefinition {
        id: "arcee",
        display_name: "Arcee AI",
        default_base_url: Some("https://api.arcee.ai/api/v1"),
        requires_secret: true,
        secret_names: &["ARCEEAI_API_KEY"],
    },
    ProviderDefinition {
        id: "gmi",
        display_name: "GMI Cloud",
        default_base_url: Some("https://api.gmi-serving.com/v1"),
        requires_secret: true,
        secret_names: &["GMI_API_KEY"],
    },
    ProviderDefinition {
        id: "minimax",
        display_name: "MiniMax",
        default_base_url: Some("https://api.minimax.io/anthropic"),
        requires_secret: true,
        secret_names: &["MINIMAX_API_KEY"],
    },
    ProviderDefinition {
        id: "anthropic",
        display_name: "Anthropic",
        default_base_url: Some("https://api.anthropic.com"),
        requires_secret: true,
        secret_names: &[
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_TOKEN",
            "CLAUDE_CODE_OAUTH_TOKEN",
        ],
    },
    ProviderDefinition {
        id: "alibaba",
        display_name: "Qwen Cloud",
        default_base_url: Some("https://dashscope-intl.aliyuncs.com/compatible-mode/v1"),
        requires_secret: true,
        secret_names: &["DASHSCOPE_API_KEY"],
    },
    ProviderDefinition {
        id: "alibaba-coding-plan",
        display_name: "Alibaba Cloud (Coding Plan)",
        default_base_url: Some("https://coding-intl.dashscope.aliyuncs.com/v1"),
        requires_secret: true,
        secret_names: &["ALIBABA_CODING_PLAN_API_KEY", "DASHSCOPE_API_KEY"],
    },
    ProviderDefinition {
        id: "minimax-cn",
        display_name: "MiniMax (China)",
        default_base_url: Some("https://api.minimaxi.com/anthropic"),
        requires_secret: true,
        secret_names: &["MINIMAX_CN_API_KEY"],
    },
    ProviderDefinition {
        id: "deepseek",
        display_name: "DeepSeek",
        default_base_url: Some("https://api.deepseek.com/v1"),
        requires_secret: true,
        secret_names: &["DEEPSEEK_API_KEY"],
    },
    ProviderDefinition {
        id: "xai",
        display_name: "xAI",
        default_base_url: Some("https://api.x.ai/v1"),
        requires_secret: true,
        secret_names: &["XAI_API_KEY"],
    },
    ProviderDefinition {
        id: "nvidia",
        display_name: "NVIDIA NIM",
        default_base_url: Some("https://integrate.api.nvidia.com/v1"),
        requires_secret: true,
        secret_names: &["NVIDIA_API_KEY"],
    },
    ProviderDefinition {
        id: "opencode-zen",
        display_name: "OpenCode Zen",
        default_base_url: Some("https://opencode.ai/zen/v1"),
        requires_secret: true,
        secret_names: &["OPENCODE_ZEN_API_KEY"],
    },
    ProviderDefinition {
        id: "opencode-go",
        display_name: "OpenCode Go",
        default_base_url: Some("https://opencode.ai/zen/go/v1"),
        requires_secret: true,
        secret_names: &["OPENCODE_GO_API_KEY"],
    },
    ProviderDefinition {
        id: "kilocode",
        display_name: "Kilo Code",
        default_base_url: Some("https://api.kilo.ai/api/gateway"),
        requires_secret: true,
        secret_names: &["KILOCODE_API_KEY"],
    },
    ProviderDefinition {
        id: "huggingface",
        display_name: "Hugging Face",
        default_base_url: Some("https://router.huggingface.co/v1"),
        requires_secret: true,
        secret_names: &["HF_TOKEN"],
    },
    ProviderDefinition {
        id: "xiaomi",
        display_name: "Xiaomi MiMo",
        default_base_url: Some("https://api.xiaomimimo.com/v1"),
        requires_secret: true,
        secret_names: &["XIAOMI_API_KEY"],
    },
    ProviderDefinition {
        id: "tencent-tokenhub",
        display_name: "Tencent TokenHub",
        default_base_url: Some("https://tokenhub.tencentmaas.com/v1"),
        requires_secret: true,
        secret_names: &["TOKENHUB_API_KEY"],
    },
    ProviderDefinition {
        id: "ollama-cloud",
        display_name: "Ollama Cloud",
        default_base_url: Some("https://ollama.com/v1"),
        requires_secret: true,
        secret_names: &["OLLAMA_API_KEY"],
    },
    ProviderDefinition {
        id: "azure-foundry",
        display_name: "Azure Foundry",
        default_base_url: None,
        requires_secret: true,
        secret_names: &["AZURE_FOUNDRY_API_KEY"],
    },
    ProviderDefinition {
        id: "deepinfra",
        display_name: "DeepInfra",
        default_base_url: Some("https://api.deepinfra.com/v1/openai"),
        requires_secret: true,
        secret_names: &["DEEPINFRA_API_KEY"],
    },
    ProviderDefinition {
        id: "fireworks",
        display_name: "Fireworks AI",
        default_base_url: Some("https://api.fireworks.ai/inference/v1"),
        requires_secret: true,
        secret_names: &["FIREWORKS_API_KEY"],
    },
    ProviderDefinition {
        id: "novita",
        display_name: "NovitaAI",
        default_base_url: Some("https://api.novita.ai/openai/v1"),
        requires_secret: true,
        secret_names: &["NOVITA_API_KEY"],
    },
    ProviderDefinition {
        id: "upstage",
        display_name: "Upstage Solar",
        default_base_url: Some("https://api.upstage.ai/v1"),
        requires_secret: true,
        secret_names: &["UPSTAGE_API_KEY"],
    },
];

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct IdempotencyRecord {
    fingerprint: String,
    profile_id: String,
    created_at: String,
    resource_etag: String,
    state: IdempotencyState,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
enum IdempotencyState {
    Pending,
    Completed,
}

fn validate_profile_id(id: &str) -> Result<(), ProfileError> {
    if id == DEFAULT_PROFILE_ID || is_valid_named_profile_id(id) {
        Ok(())
    } else {
        Err(ProfileError::InvalidProfileId)
    }
}

fn validate_named_profile_id(id: &str) -> Result<(), ProfileError> {
    if id == DEFAULT_PROFILE_ID {
        return Err(ProfileError::ReservedProfileId);
    }
    if is_valid_named_profile_id(id) {
        Ok(())
    } else {
        Err(ProfileError::InvalidProfileId)
    }
}

fn is_valid_named_profile_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    if bytes.is_empty() || bytes.len() > 64 {
        return false;
    }
    let first = bytes[0];
    if !(first == b'_' || first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return false;
    }
    bytes.iter().all(|byte| {
        *byte == b'_' || *byte == b'-' || byte.is_ascii_lowercase() || byte.is_ascii_digit()
    })
}

fn validate_display_name(value: &str) -> Result<(), ProfileError> {
    let length = value.chars().count();
    if value.trim().is_empty() || length > 80 {
        Err(ProfileError::InvalidProfileMetadata)
    } else {
        Ok(())
    }
}

fn validate_color(value: &str) -> Result<(), ProfileError> {
    let bytes = value.as_bytes();
    if bytes.len() == 7 && bytes[0] == b'#' && bytes[1..].iter().all(u8::is_ascii_hexdigit) {
        Ok(())
    } else {
        Err(ProfileError::InvalidProfileMetadata)
    }
}

fn validate_secret_name(value: &str) -> Result<(), ProfileError> {
    let bytes = value.as_bytes();
    if bytes.is_empty() || bytes.len() > 128 || !bytes[0].is_ascii_uppercase() {
        return Err(ProfileError::InvalidSecretName);
    }
    if bytes
        .iter()
        .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || *byte == b'_')
    {
        Ok(())
    } else {
        Err(ProfileError::InvalidSecretName)
    }
}

fn validate_skill_name(value: &str) -> Result<(), ProfileError> {
    let length = value.chars().count();
    if value.trim() != value
        || value.is_empty()
        || length > 128
        || value.chars().any(char::is_control)
    {
        Err(ProfileError::InvalidProfileConfig)
    } else {
        Ok(())
    }
}

fn validate_secret_value(value: &SecretString) -> Result<(), ProfileError> {
    let value = value.expose_secret();
    if value.is_empty() || value.len() > MAX_SECRET_BYTES || value.chars().count() > 2560 {
        Err(ProfileError::InvalidSecretValue)
    } else {
        Ok(())
    }
}

fn request_fingerprint(request: &CreateProfile) -> Result<String, ProfileError> {
    let bytes = serde_json::to_vec(request).map_err(|_| ProfileError::DataInvalid)?;
    Ok(sha256_hex(&bytes))
}

fn profile_metadata_document(
    request: &CreateProfile,
    created_at: &str,
) -> JsonMap<String, JsonValue> {
    let mut metadata = JsonMap::new();
    metadata.insert("name".to_owned(), request.display_name.clone().into());
    metadata.insert(
        "_synthchat".to_owned(),
        serde_json::json!({ "createdAt": created_at, "updatedAt": created_at }),
    );
    metadata
}

fn ensure_revision(expected: &str, current: &str) -> Result<(), ProfileError> {
    if expected == current {
        Ok(())
    } else {
        Err(ProfileError::RevisionConflict {
            current_etag: current.to_owned(),
        })
    }
}

fn revision_for_bytes(bytes: &[u8]) -> String {
    format!("rev_{}", sha256_hex(bytes))
}

fn etag_for_bytes(bytes: &[u8]) -> String {
    quote_revision(&revision_for_bytes(bytes))
}

fn quote_revision(revision: &str) -> String {
    format!("\"{revision}\"")
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

fn now_timestamp() -> Result<String, ProfileError> {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|_| ProfileError::DataInvalid)
}

fn ensure_directory(path: &Path) -> Result<(), ProfileError> {
    let metadata = fs::symlink_metadata(path).map_err(ProfileError::Storage)?;
    if unsafe_profile_metadata(&metadata) || !metadata.is_dir() {
        Err(ProfileError::UnsafeProfilePath)
    } else {
        Ok(())
    }
}

fn ensure_path_absent(path: &Path) -> Result<(), ProfileError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Err(ProfileError::ProfileAlreadyExists),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(ProfileError::Storage(error)),
    }
}

fn reject_symlink(path: &Path) -> Result<(), ProfileError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if unsafe_profile_metadata(&metadata) => Err(ProfileError::UnsafeProfilePath),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(ProfileError::Storage(error)),
    }
}

fn unsafe_profile_metadata(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return true;
        }
    }
    false
}

fn verify_direct_child_directory(parent: &Path, child: &Path) -> Result<(), ProfileError> {
    ensure_directory(parent)?;
    ensure_directory(child)?;
    let canonical_parent = fs::canonicalize(parent).map_err(ProfileError::Storage)?;
    let canonical_child = fs::canonicalize(child).map_err(ProfileError::Storage)?;
    if canonical_child.parent() == Some(canonical_parent.as_path()) {
        Ok(())
    } else {
        Err(ProfileError::UnsafeProfilePath)
    }
}

fn read_optional_bounded(path: &Path, maximum: u64) -> Result<Option<Vec<u8>>, ProfileError> {
    reject_symlink(path)?;
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(ProfileError::Storage(error)),
    };
    if !metadata.is_file() {
        return Err(ProfileError::UnsafeProfilePath);
    }
    if metadata.len() > maximum {
        return Err(ProfileError::DataTooLarge);
    }
    let file = File::open(path).map_err(ProfileError::Storage)?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(maximum + 1)
        .read_to_end(&mut bytes)
        .map_err(ProfileError::Storage)?;
    if bytes.len() as u64 > maximum {
        return Err(ProfileError::DataTooLarge);
    }
    Ok(Some(bytes))
}

fn atomic_write_json_bounded<T: Serialize + ?Sized>(
    path: &Path,
    value: &T,
    maximum: u64,
) -> Result<(), ProfileError> {
    let bytes = json_bytes_bounded(value, maximum)?;
    atomic_write(path, &bytes)
}

fn json_bytes_bounded<T: Serialize + ?Sized>(
    value: &T,
    maximum: u64,
) -> Result<Vec<u8>, ProfileError> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|_| ProfileError::DataInvalid)?;
    if bytes.len() as u64 > maximum {
        return Err(ProfileError::DataTooLarge);
    }
    Ok(bytes)
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), ProfileError> {
    let parent = path.parent().ok_or(ProfileError::UnsafeProfilePath)?;
    ensure_directory(parent)?;
    reject_symlink(path)?;
    let mut temporary = NamedTempFile::new_in(parent).map_err(ProfileError::Storage)?;
    temporary.write_all(bytes).map_err(ProfileError::Storage)?;
    temporary.flush().map_err(ProfileError::Storage)?;
    temporary
        .as_file()
        .sync_all()
        .map_err(ProfileError::Storage)?;
    temporary
        .persist(path)
        .map_err(|error| ProfileError::Storage(error.error))?;
    sync_directory(parent)
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<(), ProfileError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(ProfileError::Storage)
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<(), ProfileError> {
    Ok(())
}

fn remove_regular_file_if_exists(path: &Path) -> Result<(), ProfileError> {
    reject_symlink(path)?;
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(ProfileError::Storage(error)),
    }
}

fn parse_metadata_object(raw: &[u8]) -> Result<JsonMap<String, JsonValue>, ProfileError> {
    if raw.is_empty() {
        return Ok(JsonMap::new());
    }
    let value: JsonValue = serde_json::from_slice(raw).map_err(|_| ProfileError::DataInvalid)?;
    value.as_object().cloned().ok_or(ProfileError::DataInvalid)
}

fn metadata_from_object(
    id: &str,
    object: &JsonMap<String, JsonValue>,
) -> Result<ProfileMetadata, ProfileError> {
    let display_name = match object.get("name") {
        Some(JsonValue::String(value)) => {
            validate_display_name(value).map_err(|_| ProfileError::DataInvalid)?;
            value.clone()
        }
        Some(_) => return Err(ProfileError::DataInvalid),
        None if id == DEFAULT_PROFILE_ID => "Default".to_owned(),
        None => id.to_owned(),
    };
    let color = optional_metadata_string(object, "color")?;
    if let Some(value) = color.as_deref() {
        validate_color(value).map_err(|_| ProfileError::DataInvalid)?;
    }
    let avatar_file_id = optional_metadata_string(object, "avatar")?;
    let (created_at, updated_at) = metadata_timestamps(object)?;
    Ok(ProfileMetadata {
        id: id.to_owned(),
        display_name,
        is_default: id == DEFAULT_PROFILE_ID,
        color,
        avatar_file_id,
        created_at,
        updated_at,
    })
}

fn optional_metadata_string(
    object: &JsonMap<String, JsonValue>,
    key: &str,
) -> Result<Option<String>, ProfileError> {
    match object.get(key) {
        None | Some(JsonValue::Null) => Ok(None),
        Some(JsonValue::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(ProfileError::DataInvalid),
    }
}

fn metadata_timestamps(
    object: &JsonMap<String, JsonValue>,
) -> Result<(Option<String>, String), ProfileError> {
    let Some(value) = object.get("_synthchat") else {
        return Ok((None, EPOCH_TIMESTAMP.to_owned()));
    };
    let internal = value.as_object().ok_or(ProfileError::DataInvalid)?;
    let created_at = optional_json_string(internal, "createdAt")?;
    let updated_at =
        optional_json_string(internal, "updatedAt")?.unwrap_or_else(|| EPOCH_TIMESTAMP.to_owned());
    if let Some(created_at) = created_at.as_deref() {
        validate_timestamp(created_at)?;
    }
    validate_timestamp(&updated_at)?;
    Ok((created_at, updated_at))
}

fn validate_timestamp(value: &str) -> Result<(), ProfileError> {
    OffsetDateTime::parse(value, &Rfc3339)
        .map(|_| ())
        .map_err(|_| ProfileError::DataInvalid)
}

fn optional_json_string(
    object: &JsonMap<String, JsonValue>,
    key: &str,
) -> Result<Option<String>, ProfileError> {
    match object.get(key) {
        None | Some(JsonValue::Null) => Ok(None),
        Some(JsonValue::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(ProfileError::DataInvalid),
    }
}

fn validate_metadata_patch(value: &JsonValue) -> Result<JsonMap<String, JsonValue>, ProfileError> {
    let object = value
        .as_object()
        .cloned()
        .ok_or(ProfileError::InvalidProfileMetadata)?;
    for (key, value) in &object {
        match (key.as_str(), value) {
            ("displayName", JsonValue::String(name)) => validate_display_name(name)?,
            ("color", JsonValue::String(color)) => validate_color(color)?,
            ("color", JsonValue::Null) => {}
            ("avatarFileId", JsonValue::String(avatar))
                if !avatar.is_empty() && avatar.len() <= 256 => {}
            ("avatarFileId", JsonValue::Null) => {}
            _ => return Err(ProfileError::InvalidProfileMetadata),
        }
    }
    Ok(object)
}

fn apply_metadata_patch(
    object: &mut JsonMap<String, JsonValue>,
    patch: &JsonMap<String, JsonValue>,
) -> Result<(), ProfileError> {
    for (key, value) in patch {
        match key.as_str() {
            "displayName" => {
                object.insert("name".to_owned(), value.clone());
            }
            "color" => {
                if value.is_null() {
                    object.remove("color");
                } else {
                    object.insert("color".to_owned(), value.clone());
                }
            }
            "avatarFileId" => {
                if value.is_null() {
                    object.remove("avatar");
                } else {
                    object.insert("avatar".to_owned(), value.clone());
                }
            }
            _ => return Err(ProfileError::InvalidProfileMetadata),
        }
    }
    Ok(())
}

fn set_metadata_updated_at(object: &mut JsonMap<String, JsonValue>, timestamp: String) {
    let internal = object
        .entry("_synthchat".to_owned())
        .or_insert_with(|| JsonValue::Object(JsonMap::new()));
    if !internal.is_object() {
        *internal = JsonValue::Object(JsonMap::new());
    }
    internal
        .as_object_mut()
        .expect("the internal metadata value was initialized as an object")
        .insert("updatedAt".to_owned(), timestamp.into());
}

fn parse_config_document(raw: &[u8]) -> Result<YamlValue, ProfileError> {
    if raw.is_empty() {
        return Ok(YamlValue::Mapping(YamlMapping::new()));
    }
    let document: YamlValue =
        serde_yaml_ng::from_slice(raw).map_err(|_| ProfileError::DataInvalid)?;
    if document.is_mapping() {
        Ok(document)
    } else if document.is_null() {
        Ok(YamlValue::Mapping(YamlMapping::new()))
    } else {
        Err(ProfileError::DataInvalid)
    }
}

fn config_from_document(
    document: &YamlValue,
    revision: String,
) -> Result<ProfileConfig, ProfileError> {
    let provider = yaml_optional_string(document, &["model", "provider"])?
        .unwrap_or_else(|| "auto".to_owned());
    let model = yaml_optional_string(document, &["model", "default"])?.unwrap_or_default();
    let base_url = yaml_optional_string(document, &["model", "base_url"])?;
    if let Some(value) = base_url.as_deref() {
        validate_base_url(value).map_err(|_| ProfileError::DataInvalid)?;
    }
    let reasoning_effort = yaml_optional_string(document, &["model", "reasoning_effort"])?;
    if let Some(value) = reasoning_effort.as_deref() {
        validate_reasoning_effort(value).map_err(|_| ProfileError::DataInvalid)?;
    }

    let mut toolsets = yaml_bool_catalog(document, &["platform_toolsets", "cli"])?;
    for disabled in yaml_string_sequence(document, &["agent", "disabled_toolsets"])? {
        toolsets.insert(disabled, false);
    }
    // Skills and messaging platforms are derived by their dedicated stores. Their
    // real Hermes YAML shapes are not boolean catalogs and must remain untouched.
    let skills = BTreeMap::new();
    let platforms = BTreeMap::new();
    let memory_provider =
        normalized_memory_provider(yaml_optional_string(document, &["memory", "provider"])?);
    let code_execution = code_execution_from_document(document)?;
    let extensions = yaml_extensions(document)?;

    Ok(ProfileConfig {
        revision,
        model: ModelConfig {
            provider,
            model,
            base_url,
            reasoning_effort,
        },
        code_execution,
        toolsets,
        skills,
        memory_provider,
        platforms,
        extensions,
    })
}

fn code_execution_from_document(document: &YamlValue) -> Result<CodeExecutionConfig, ProfileError> {
    match yaml_get(document, &["code_execution"]) {
        None | Some(YamlValue::Mapping(_)) => {}
        Some(_) => return Err(ProfileError::DataInvalid),
    }
    let mode = match yaml_get(document, &["code_execution", "mode"]) {
        None => CodeExecutionMode::Project,
        Some(YamlValue::String(value)) if value == "project" => CodeExecutionMode::Project,
        Some(YamlValue::String(value)) if value == "strict" => CodeExecutionMode::Strict,
        Some(_) => return Err(ProfileError::DataInvalid),
    };
    let timeout_seconds = yaml_optional_u64(document, &["code_execution", "timeout"])?
        .unwrap_or(DEFAULT_CODE_EXECUTION_TIMEOUT_SECONDS);
    let max_tool_calls = yaml_optional_u64(document, &["code_execution", "max_tool_calls"])?
        .unwrap_or(DEFAULT_CODE_EXECUTION_TOOL_CALLS as u64);
    if timeout_seconds == 0
        || timeout_seconds > MAX_CODE_EXECUTION_TIMEOUT_SECONDS
        || max_tool_calls == 0
        || max_tool_calls > MAX_CODE_EXECUTION_TOOL_CALLS as u64
    {
        return Err(ProfileError::DataInvalid);
    }
    Ok(CodeExecutionConfig {
        mode,
        timeout_seconds,
        max_tool_calls: usize::try_from(max_tool_calls).map_err(|_| ProfileError::DataInvalid)?,
    })
}

fn yaml_get<'a>(document: &'a YamlValue, path: &[&str]) -> Option<&'a YamlValue> {
    let mut current = document;
    for segment in path {
        let mapping = current.as_mapping()?;
        current = mapping.get(YamlValue::String((*segment).to_owned()))?;
    }
    Some(current)
}

fn yaml_optional_string(
    document: &YamlValue,
    path: &[&str],
) -> Result<Option<String>, ProfileError> {
    match yaml_get(document, path) {
        None | Some(YamlValue::Null) => Ok(None),
        Some(YamlValue::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(ProfileError::DataInvalid),
    }
}

fn yaml_optional_u64(document: &YamlValue, path: &[&str]) -> Result<Option<u64>, ProfileError> {
    match yaml_get(document, path) {
        None => Ok(None),
        Some(YamlValue::Number(value)) => value.as_u64().map(Some).ok_or(ProfileError::DataInvalid),
        Some(_) => Err(ProfileError::DataInvalid),
    }
}

fn normalized_memory_provider(value: Option<String>) -> String {
    match value {
        Some(value) if !value.trim().is_empty() => value,
        _ => "builtin".to_owned(),
    }
}

fn normalized_optional_web_provider(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
}

fn effective_web_provider(selected: Option<&str>, tavily_configured: bool) -> EffectiveWebProvider {
    match selected {
        Some("tavily") if tavily_configured => EffectiveWebProvider {
            provider_id: Some("tavily".to_owned()),
            status: WebProviderStatus::Ready,
            missing_secret_names: Vec::new(),
        },
        Some("tavily") => EffectiveWebProvider {
            provider_id: Some("tavily".to_owned()),
            status: WebProviderStatus::MissingSecret,
            missing_secret_names: vec![TAVILY_API_KEY.to_owned()],
        },
        Some(provider) => EffectiveWebProvider {
            provider_id: Some(provider.to_owned()),
            status: WebProviderStatus::Unsupported,
            missing_secret_names: Vec::new(),
        },
        None if tavily_configured => EffectiveWebProvider {
            provider_id: Some("tavily".to_owned()),
            status: WebProviderStatus::Ready,
            missing_secret_names: Vec::new(),
        },
        None => EffectiveWebProvider {
            provider_id: None,
            status: WebProviderStatus::Unconfigured,
            missing_secret_names: Vec::new(),
        },
    }
}

fn deserialize_nullable_patch_field<'de, D, T>(
    deserializer: D,
) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

fn deserialize_present_usize<'de, D>(deserializer: D) -> Result<Option<usize>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    usize::deserialize(deserializer).map(Some)
}

fn yaml_optional_bool(document: &YamlValue, path: &[&str]) -> Result<Option<bool>, ProfileError> {
    match yaml_get(document, path) {
        None | Some(YamlValue::Null) => Ok(None),
        Some(YamlValue::Bool(value)) => Ok(Some(*value)),
        Some(_) => Err(ProfileError::DataInvalid),
    }
}

fn yaml_positive_usize(document: &YamlValue, path: &[&str]) -> Option<usize> {
    let value = yaml_get(document, path)?;
    let parsed = match value {
        YamlValue::Number(value) => value.as_u64().and_then(|value| usize::try_from(value).ok()),
        YamlValue::String(value) => value.parse::<usize>().ok(),
        _ => None,
    }?;
    (parsed > 0).then_some(parsed)
}

fn web_extract_char_limit(document: &YamlValue) -> Result<usize, ProfileError> {
    let Some(value) = yaml_get(document, &["web", "extract_char_limit"]) else {
        return Ok(DEFAULT_WEB_EXTRACT_CHAR_LIMIT);
    };
    let limit = match value {
        YamlValue::Number(value) => value
            .as_u64()
            .and_then(|value| usize::try_from(value).ok())
            .ok_or(ProfileError::DataInvalid)?,
        _ => return Err(ProfileError::DataInvalid),
    };
    if !(MIN_WEB_EXTRACT_CHAR_LIMIT..=MAX_WEB_EXTRACT_CHAR_LIMIT).contains(&limit) {
        return Err(ProfileError::DataInvalid);
    }
    Ok(limit)
}

fn yaml_string_sequence(document: &YamlValue, path: &[&str]) -> Result<Vec<String>, ProfileError> {
    match yaml_get(document, path) {
        None | Some(YamlValue::Null) => Ok(Vec::new()),
        Some(YamlValue::Sequence(values)) => values
            .iter()
            .map(|value| match value {
                YamlValue::String(value) if !value.is_empty() => Ok(value.clone()),
                _ => Err(ProfileError::DataInvalid),
            })
            .collect(),
        Some(_) => Err(ProfileError::DataInvalid),
    }
}

fn yaml_compatible_string_set(
    document: &YamlValue,
    path: &[&str],
) -> Result<BTreeSet<String>, ProfileError> {
    match yaml_get(document, path) {
        None | Some(YamlValue::Null) => Ok(BTreeSet::new()),
        Some(YamlValue::String(value)) => {
            validate_skill_name(value).map_err(|_| ProfileError::DataInvalid)?;
            Ok(BTreeSet::from([value.clone()]))
        }
        Some(YamlValue::Sequence(values)) => values
            .iter()
            .map(|value| match value {
                YamlValue::String(value) => {
                    validate_skill_name(value).map_err(|_| ProfileError::DataInvalid)?;
                    Ok(value.clone())
                }
                _ => Err(ProfileError::DataInvalid),
            })
            .collect(),
        Some(_) => Err(ProfileError::DataInvalid),
    }
}

fn yaml_bool_catalog(
    document: &YamlValue,
    path: &[&str],
) -> Result<BTreeMap<String, bool>, ProfileError> {
    match yaml_get(document, path) {
        None | Some(YamlValue::Null) => Ok(BTreeMap::new()),
        Some(YamlValue::Sequence(values)) => values
            .iter()
            .map(|value| match value {
                YamlValue::String(value) if !value.is_empty() => Ok((value.clone(), true)),
                _ => Err(ProfileError::DataInvalid),
            })
            .collect(),
        Some(YamlValue::Mapping(mapping)) => mapping
            .iter()
            .map(|(key, value)| match (key, value) {
                (YamlValue::String(key), YamlValue::Bool(value)) if !key.is_empty() => {
                    Ok((key.clone(), *value))
                }
                _ => Err(ProfileError::DataInvalid),
            })
            .collect(),
        Some(_) => Err(ProfileError::DataInvalid),
    }
}

fn yaml_extensions(document: &YamlValue) -> Result<JsonMap<String, JsonValue>, ProfileError> {
    let Some(value) = yaml_get(document, &["extensions"]) else {
        return Ok(JsonMap::new());
    };
    if value.is_null() {
        return Ok(JsonMap::new());
    }
    let json = serde_json::to_value(value).map_err(|_| ProfileError::DataInvalid)?;
    let object = json.as_object().ok_or(ProfileError::DataInvalid)?;
    Ok(filter_safe_extensions(object))
}

fn filter_safe_extensions(object: &JsonMap<String, JsonValue>) -> JsonMap<String, JsonValue> {
    object
        .iter()
        .filter(|(key, _)| !contains_sensitive_term(key))
        .map(|(key, value)| (key.clone(), filter_safe_extension_value(value)))
        .collect()
}

fn filter_safe_extension_value(value: &JsonValue) -> JsonValue {
    match value {
        JsonValue::Object(object) => JsonValue::Object(filter_safe_extensions(object)),
        JsonValue::Array(values) => {
            JsonValue::Array(values.iter().map(filter_safe_extension_value).collect())
        }
        _ => value.clone(),
    }
}

fn web_config_patch_is_empty(patch: &WebConfigPatch) -> bool {
    patch.shared_provider.is_none()
        && patch.search_provider.is_none()
        && patch.extract_provider.is_none()
        && patch.extract_char_limit.is_none()
}

fn validate_web_config_patch(patch: &WebConfigPatch) -> Result<(), ProfileError> {
    for provider in [
        patch.shared_provider.as_ref(),
        patch.search_provider.as_ref(),
        patch.extract_provider.as_ref(),
    ]
    .into_iter()
    .flatten()
    .flatten()
    {
        if provider != "tavily" {
            return Err(ProfileError::InvalidProfileConfig);
        }
    }
    if patch.extract_char_limit.is_some_and(|limit| {
        !(MIN_WEB_EXTRACT_CHAR_LIMIT..=MAX_WEB_EXTRACT_CHAR_LIMIT).contains(&limit)
    }) {
        return Err(ProfileError::InvalidProfileConfig);
    }
    Ok(())
}

fn apply_web_config_patch(
    document: &mut YamlValue,
    patch: &WebConfigPatch,
) -> Result<(), ProfileError> {
    for (path, provider) in [
        (&["web", "backend"][..], &patch.shared_provider),
        (&["web", "search_backend"][..], &patch.search_provider),
        (&["web", "extract_backend"][..], &patch.extract_provider),
    ] {
        if let Some(provider) = provider {
            let value = provider.as_ref().map_or(YamlValue::Null, |provider| {
                YamlValue::String(provider.clone())
            });
            set_yaml_path(document, path, value)?;
        }
    }
    if let Some(limit) = patch.extract_char_limit {
        set_yaml_path(
            document,
            &["web", "extract_char_limit"],
            serde_yaml_ng::to_value(limit).map_err(|_| ProfileError::InvalidProfileConfig)?,
        )?;
    }
    Ok(())
}

fn validate_config_patch(value: &JsonValue) -> Result<JsonMap<String, JsonValue>, ProfileError> {
    let object = value
        .as_object()
        .cloned()
        .ok_or(ProfileError::InvalidProfileConfig)?;
    for (key, value) in &object {
        match key.as_str() {
            "model" => validate_model_patch(value)?,
            "codeExecution" => validate_code_execution_patch(value)?,
            "toolsets" => validate_bool_patch_map(value)?,
            "memoryProvider" => match value {
                JsonValue::String(value) if !value.trim().is_empty() && value.len() <= 128 => {}
                _ => return Err(ProfileError::InvalidProfileConfig),
            },
            "extensions" => {
                let extensions = value
                    .as_object()
                    .ok_or(ProfileError::InvalidProfileConfig)?;
                validate_extension_patch(extensions)?;
            }
            _ => return Err(ProfileError::InvalidProfileConfig),
        }
    }
    Ok(object)
}

fn validate_code_execution_patch(value: &JsonValue) -> Result<(), ProfileError> {
    let object = value
        .as_object()
        .ok_or(ProfileError::InvalidProfileConfig)?;
    for (key, value) in object {
        match key.as_str() {
            "mode" if matches!(value.as_str(), Some("project" | "strict")) => {}
            "timeoutSeconds"
                if value.as_u64().is_some_and(|value| {
                    value > 0 && value <= MAX_CODE_EXECUTION_TIMEOUT_SECONDS
                }) => {}
            "maxToolCalls"
                if value.as_u64().is_some_and(|value| {
                    value > 0 && value <= MAX_CODE_EXECUTION_TOOL_CALLS as u64
                }) => {}
            _ => return Err(ProfileError::InvalidProfileConfig),
        }
    }
    Ok(())
}

fn validate_model_patch(value: &JsonValue) -> Result<(), ProfileError> {
    let object = value
        .as_object()
        .ok_or(ProfileError::InvalidProfileConfig)?;
    for (key, value) in object {
        match key.as_str() {
            "provider" | "model" => match value {
                JsonValue::String(value) if value.len() <= 512 => {}
                _ => return Err(ProfileError::InvalidProfileConfig),
            },
            "baseUrl" => match value {
                JsonValue::Null => {}
                JsonValue::String(value) => validate_base_url(value)?,
                _ => return Err(ProfileError::InvalidProfileConfig),
            },
            "reasoningEffort" => match value {
                JsonValue::Null => {}
                JsonValue::String(value) => validate_reasoning_effort(value)?,
                _ => return Err(ProfileError::InvalidProfileConfig),
            },
            _ => return Err(ProfileError::InvalidProfileConfig),
        }
    }
    Ok(())
}

fn validate_bool_patch_map(value: &JsonValue) -> Result<(), ProfileError> {
    let object = value
        .as_object()
        .ok_or(ProfileError::InvalidProfileConfig)?;
    for (key, value) in object {
        if key.is_empty() || key.len() > 128 || !value.is_boolean() {
            return Err(ProfileError::InvalidProfileConfig);
        }
    }
    Ok(())
}

fn validate_base_url(value: &str) -> Result<(), ProfileError> {
    let url = Url::parse(value).map_err(|_| ProfileError::InvalidProfileConfig)?;
    if matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
    {
        Ok(())
    } else {
        Err(ProfileError::InvalidProfileConfig)
    }
}

fn validate_reasoning_effort(value: &str) -> Result<(), ProfileError> {
    if matches!(value, "minimal" | "low" | "medium" | "high" | "xhigh") {
        Ok(())
    } else {
        Err(ProfileError::InvalidProfileConfig)
    }
}

fn validate_extension_patch(object: &JsonMap<String, JsonValue>) -> Result<(), ProfileError> {
    for (key, value) in object {
        if contains_sensitive_term(key) || value.is_null() {
            return Err(ProfileError::InvalidProfileConfig);
        }
        match value {
            JsonValue::Object(nested) => validate_extension_patch(nested)?,
            JsonValue::Array(values) => validate_extension_array(values)?,
            _ => {}
        }
    }
    Ok(())
}

fn validate_extension_array(values: &[JsonValue]) -> Result<(), ProfileError> {
    for value in values {
        if value.is_null() {
            return Err(ProfileError::InvalidProfileConfig);
        }
        match value {
            JsonValue::Object(nested) => validate_extension_patch(nested)?,
            JsonValue::Array(nested) => validate_extension_array(nested)?,
            _ => {}
        }
    }
    Ok(())
}

fn contains_sensitive_term(key: &str) -> bool {
    let normalized = key.to_ascii_lowercase();
    ["token", "key", "password", "secret", "credential"]
        .iter()
        .any(|term| normalized.contains(term))
}

fn apply_config_patch(
    document: &mut YamlValue,
    patch: &JsonMap<String, JsonValue>,
) -> Result<(), ProfileError> {
    for (key, value) in patch {
        match key.as_str() {
            "model" => apply_model_patch(document, value)?,
            "codeExecution" => apply_code_execution_patch(document, value)?,
            "toolsets" => apply_toolsets_patch(document, value)?,
            "memoryProvider" => {
                let provider = value.as_str().ok_or(ProfileError::InvalidProfileConfig)?;
                set_yaml_path(
                    document,
                    &["memory", "provider"],
                    YamlValue::String(if provider == "builtin" {
                        String::new()
                    } else {
                        provider.to_owned()
                    }),
                )?;
            }
            "extensions" => apply_extensions_patch(document, value)?,
            _ => return Err(ProfileError::InvalidProfileConfig),
        }
    }
    Ok(())
}

fn apply_code_execution_patch(
    document: &mut YamlValue,
    value: &JsonValue,
) -> Result<(), ProfileError> {
    let object = value
        .as_object()
        .ok_or(ProfileError::InvalidProfileConfig)?;
    for (key, value) in object {
        let path: &[&str] = match key.as_str() {
            "mode" => &["code_execution", "mode"],
            "timeoutSeconds" => &["code_execution", "timeout"],
            "maxToolCalls" => &["code_execution", "max_tool_calls"],
            _ => return Err(ProfileError::InvalidProfileConfig),
        };
        set_yaml_path(
            document,
            path,
            serde_yaml_ng::to_value(value).map_err(|_| ProfileError::InvalidProfileConfig)?,
        )?;
    }
    Ok(())
}

fn apply_model_patch(document: &mut YamlValue, value: &JsonValue) -> Result<(), ProfileError> {
    let object = value
        .as_object()
        .ok_or(ProfileError::InvalidProfileConfig)?;
    for (key, value) in object {
        let path: &[&str] = match key.as_str() {
            "provider" => &["model", "provider"],
            "model" => &["model", "default"],
            "baseUrl" => &["model", "base_url"],
            "reasoningEffort" => &["model", "reasoning_effort"],
            _ => return Err(ProfileError::InvalidProfileConfig),
        };
        let yaml = if value.is_null() {
            YamlValue::Null
        } else {
            YamlValue::String(
                value
                    .as_str()
                    .ok_or(ProfileError::InvalidProfileConfig)?
                    .to_owned(),
            )
        };
        set_yaml_path(document, path, yaml)?;
    }
    Ok(())
}

fn apply_toolsets_patch(document: &mut YamlValue, value: &JsonValue) -> Result<(), ProfileError> {
    let patch = json_bool_map(value)?;
    let current = yaml_bool_catalog(document, &["platform_toolsets", "cli"])?;
    let mut enabled: BTreeSet<String> = current
        .iter()
        .filter(|(_, enabled)| **enabled)
        .map(|(name, _)| name.clone())
        .collect();
    let mut disabled: BTreeSet<String> =
        yaml_string_sequence(document, &["agent", "disabled_toolsets"])?
            .into_iter()
            .collect();
    disabled.extend(
        current
            .into_iter()
            .filter(|(_, enabled)| !enabled)
            .map(|(name, _)| name),
    );
    for (name, is_enabled) in patch {
        if is_enabled {
            enabled.insert(name.clone());
            disabled.remove(&name);
        } else {
            enabled.remove(&name);
            disabled.insert(name);
        }
    }
    set_yaml_path(
        document,
        &["platform_toolsets", "cli"],
        yaml_string_set(&enabled),
    )?;
    set_yaml_path(
        document,
        &["agent", "disabled_toolsets"],
        yaml_string_set(&disabled),
    )
}

fn json_bool_map(value: &JsonValue) -> Result<BTreeMap<String, bool>, ProfileError> {
    value
        .as_object()
        .ok_or(ProfileError::InvalidProfileConfig)?
        .iter()
        .map(|(key, value)| {
            value
                .as_bool()
                .map(|value| (key.clone(), value))
                .ok_or(ProfileError::InvalidProfileConfig)
        })
        .collect()
}

fn yaml_string_set(values: &BTreeSet<String>) -> YamlValue {
    YamlValue::Sequence(values.iter().cloned().map(YamlValue::String).collect())
}

fn apply_extensions_patch(document: &mut YamlValue, value: &JsonValue) -> Result<(), ProfileError> {
    let patch = value
        .as_object()
        .ok_or(ProfileError::InvalidProfileConfig)?;
    let current = yaml_get(document, &["extensions"])
        .cloned()
        .unwrap_or_else(|| YamlValue::Mapping(YamlMapping::new()));
    let mut json = if current.is_null() {
        JsonValue::Object(JsonMap::new())
    } else {
        serde_json::to_value(current).map_err(|_| ProfileError::DataInvalid)?
    };
    let target = json.as_object_mut().ok_or(ProfileError::DataInvalid)?;
    merge_json_object(target, patch);
    let yaml = serde_yaml_ng::to_value(json).map_err(|_| ProfileError::InvalidProfileConfig)?;
    set_yaml_path(document, &["extensions"], yaml)
}

fn merge_json_object(target: &mut JsonMap<String, JsonValue>, patch: &JsonMap<String, JsonValue>) {
    for (key, value) in patch {
        match (target.get_mut(key), value) {
            (Some(JsonValue::Object(existing)), JsonValue::Object(incoming)) => {
                merge_json_object(existing, incoming);
            }
            _ => {
                target.insert(key.clone(), value.clone());
            }
        }
    }
}

fn set_yaml_path(
    document: &mut YamlValue,
    path: &[&str],
    value: YamlValue,
) -> Result<(), ProfileError> {
    let (last, parents) = path
        .split_last()
        .ok_or(ProfileError::InvalidProfileConfig)?;
    let mut current = document;
    for segment in parents {
        let mapping = current.as_mapping_mut().ok_or(ProfileError::DataInvalid)?;
        current = mapping
            .entry(YamlValue::String((*segment).to_owned()))
            .or_insert_with(|| YamlValue::Mapping(YamlMapping::new()));
        if !current.is_mapping() {
            return Err(ProfileError::DataInvalid);
        }
    }
    current
        .as_mapping_mut()
        .ok_or(ProfileError::DataInvalid)?
        .insert(YamlValue::String((*last).to_owned()), value);
    Ok(())
}

fn secret_account(profile_id: &str, secret_name: &str) -> String {
    format!("{profile_id}:{secret_name}")
}

fn secret_is_configured(
    store: &Arc<CredentialStore>,
    profile_id: &str,
    name: &str,
) -> Result<bool, ProfileError> {
    let entry = store
        .build(SECRET_SERVICE, &secret_account(profile_id, name), None)
        .map_err(|_| ProfileError::SecretStorageUnavailable)?;
    match entry.get_secret() {
        Ok(value) => {
            let _secret = Zeroizing::new(value);
            Ok(true)
        }
        Err(KeyringError::NoEntry) => Ok(false),
        Err(_) => Err(ProfileError::SecretStorageUnavailable),
    }
}

fn delete_secret_from_store(
    store: &Arc<CredentialStore>,
    profile_id: &str,
    name: &str,
) -> Result<(), ProfileError> {
    let entry = store
        .build(SECRET_SERVICE, &secret_account(profile_id, name), None)
        .map_err(|_| ProfileError::SecretStorageUnavailable)?;
    match entry.delete_credential() {
        Ok(()) | Err(KeyringError::NoEntry) => Ok(()),
        Err(_) => Err(ProfileError::SecretStorageUnavailable),
    }
}

#[cfg(target_os = "windows")]
fn system_credential_store() -> Option<Arc<CredentialStore>> {
    windows_native_keyring_store::Store::new()
        .ok()
        .map(|store| store as Arc<CredentialStore>)
}

#[cfg(target_os = "macos")]
fn system_credential_store() -> Option<Arc<CredentialStore>> {
    apple_native_keyring_store::keychain::Store::new()
        .ok()
        .map(|store| store as Arc<CredentialStore>)
}

#[cfg(target_os = "linux")]
fn system_credential_store() -> Option<Arc<CredentialStore>> {
    zbus_secret_service_keyring_store::Store::new()
        .ok()
        .map(|store| store as Arc<CredentialStore>)
}

#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
fn system_credential_store() -> Option<Arc<CredentialStore>> {
    None
}

#[cfg(test)]
mod tests {
    use std::{fs, sync::Arc, thread};

    use keyring_core::{CredentialStore, mock};
    use secrecy::SecretString;
    use serde_json::json;
    use tempfile::TempDir;

    use super::*;

    struct Fixture {
        _home: TempDir,
        service: ProfileService,
    }

    impl Fixture {
        fn new() -> Self {
            let home = tempfile::tempdir().unwrap();
            let store: Arc<CredentialStore> = mock::Store::new().unwrap();
            let service =
                ProfileService::with_credential_store(home.path().to_owned(), store.clone());
            Self {
                _home: home,
                service,
            }
        }

        fn create(&self, id: &str) -> Versioned<ProfileMetadata> {
            self.service
                .create_profile(
                    &CreateProfile {
                        id: id.to_owned(),
                        display_name: format!("Profile {id}"),
                        clone_from_profile_id: None,
                    },
                    &format!("create-{id}"),
                )
                .unwrap()
        }
    }

    #[test]
    fn default_profile_is_a_stable_logical_resource() {
        let fixture = Fixture::new();
        let first = fixture.service.get_profile(DEFAULT_PROFILE_ID).unwrap();
        let second = fixture.service.get_profile(DEFAULT_PROFILE_ID).unwrap();

        assert_eq!(first, second);
        assert_eq!(first.value.display_name, "Default");
        assert!(first.value.is_default);
        assert_eq!(first.value.updated_at, EPOCH_TIMESTAMP);
        assert_eq!(
            fixture
                .service
                .list_profiles(ProfileEngineState::Stopped)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn create_is_idempotent_across_service_reconstruction() {
        let home = tempfile::tempdir().unwrap();
        let store: Arc<CredentialStore> = mock::Store::new().unwrap();
        let request = CreateProfile {
            id: "work".to_owned(),
            display_name: "Work".to_owned(),
            clone_from_profile_id: None,
        };
        let first_service =
            ProfileService::with_credential_store(home.path().to_owned(), store.clone());
        let first = first_service
            .create_profile(&request, "stable-create-key")
            .unwrap();
        drop(first_service);

        let second_service = ProfileService::with_credential_store(home.path().to_owned(), store);
        let replay = second_service
            .create_profile(&request, "stable-create-key")
            .unwrap();
        assert_eq!(first, replay);

        let mut changed = request;
        changed.display_name = "Different".to_owned();
        assert!(matches!(
            second_service.create_profile(&changed, "stable-create-key"),
            Err(ProfileError::IdempotencyConflict)
        ));
    }

    #[test]
    fn clone_copies_only_config_and_preserves_unknown_yaml() {
        let fixture = Fixture::new();
        fs::create_dir_all(fixture._home.path()).unwrap();
        fs::write(
            fixture._home.path().join("config.yaml"),
            "model:\n  provider: openrouter\nunknown:\n  keep: true\n",
        )
        .unwrap();
        fixture
            .service
            .put_secret(
                DEFAULT_PROFILE_ID,
                "OPENROUTER_API_KEY",
                &SecretString::from("source-secret".to_owned()),
            )
            .unwrap();

        fixture
            .service
            .create_profile(
                &CreateProfile {
                    id: "clone".to_owned(),
                    display_name: "Clone".to_owned(),
                    clone_from_profile_id: Some(DEFAULT_PROFILE_ID.to_owned()),
                },
                "clone-profile-key",
            )
            .unwrap();
        let clone_config = fs::read_to_string(
            fixture
                ._home
                .path()
                .join("profiles")
                .join("clone")
                .join("config.yaml"),
        )
        .unwrap();
        assert!(clone_config.contains("unknown:"));
        let statuses = fixture.service.list_secret_statuses("clone").unwrap();
        assert!(statuses.iter().all(|status| !status.configured));
    }

    #[test]
    fn metadata_and_config_revisions_are_independent() {
        let fixture = Fixture::new();
        fixture.create("work");
        let metadata_before = fixture.service.get_profile("work").unwrap();
        let config_before = fixture.service.get_config("work").unwrap();
        let config_after = fixture
            .service
            .update_config(
                "work",
                &config_before.etag,
                &json!({"model": {"provider": "openrouter"}}),
            )
            .unwrap();
        let metadata_after = fixture.service.get_profile("work").unwrap();

        assert_ne!(config_before.etag, config_after.etag);
        assert_eq!(metadata_before.etag, metadata_after.etag);
        assert!(matches!(
            fixture.service.update_config(
                "work",
                &config_before.etag,
                &json!({"model": {"model": "new-model"}})
            ),
            Err(ProfileError::RevisionConflict { .. })
        ));
    }

    #[test]
    fn config_patch_preserves_unknown_yaml_and_noop_revision() {
        let fixture = Fixture::new();
        fs::create_dir_all(fixture._home.path()).unwrap();
        fs::write(
            fixture._home.path().join("config.yaml"),
            "unknown:\n  nested: 42\nplatform_toolsets:\n  cli:\n    - terminal\nagent:\n  disabled_toolsets:\n    - browser\nskills:\n  config:\n    planning:\n      mode: strict\ntelegram:\n  enabled: false\n",
        )
        .unwrap();
        let before = fixture.service.get_config(DEFAULT_PROFILE_ID).unwrap();
        let no_op = fixture
            .service
            .update_config(DEFAULT_PROFILE_ID, &before.etag, &json!({}))
            .unwrap();
        assert_eq!(before, no_op);

        let after = fixture
            .service
            .update_config(
                DEFAULT_PROFILE_ID,
                &before.etag,
                &json!({
                    "model": {"baseUrl": "https://example.test/v1", "reasoningEffort": null},
                    "toolsets": {"terminal": false, "browser": true},
                    "extensions": {"ui": {"density": "compact"}}
                }),
            )
            .unwrap();
        assert_eq!(
            after.value.model.base_url.as_deref(),
            Some("https://example.test/v1")
        );
        assert_eq!(after.value.toolsets.get("terminal"), Some(&false));
        assert_eq!(after.value.toolsets.get("browser"), Some(&true));
        assert!(after.value.skills.is_empty());
        assert!(after.value.platforms.is_empty());
        let persisted = fs::read_to_string(fixture._home.path().join("config.yaml")).unwrap();
        assert!(persisted.contains("unknown:"));
        assert!(persisted.contains("nested: 42"));
        assert!(persisted.contains("platform_toolsets:"));
        assert!(persisted.contains("mode: strict"));
        assert!(persisted.contains("telegram:"));
    }

    #[test]
    fn code_execution_config_round_trips_hermes_yaml_and_patch() {
        let fixture = Fixture::new();
        fs::write(
            fixture._home.path().join("config.yaml"),
            "code_execution:\n  mode: strict\n  timeout: 120\n  max_tool_calls: 7\nunknown:\n  keep: true\n",
        )
        .unwrap();

        let before = fixture.service.get_config(DEFAULT_PROFILE_ID).unwrap();
        assert_eq!(before.value.code_execution.mode, CodeExecutionMode::Strict);
        assert_eq!(before.value.code_execution.timeout_seconds, 120);
        assert_eq!(before.value.code_execution.max_tool_calls, 7);

        let after = fixture
            .service
            .update_config(
                DEFAULT_PROFILE_ID,
                &before.etag,
                &json!({
                    "codeExecution": {
                        "mode": "project",
                        "timeoutSeconds": 600,
                        "maxToolCalls": 100
                    }
                }),
            )
            .unwrap();
        assert_ne!(after.etag, before.etag);
        assert_eq!(after.value.code_execution.mode, CodeExecutionMode::Project);
        assert_eq!(after.value.code_execution.timeout_seconds, 600);
        assert_eq!(after.value.code_execution.max_tool_calls, 100);

        let persisted = fs::read_to_string(fixture._home.path().join("config.yaml")).unwrap();
        assert!(persisted.contains("mode: project"));
        assert!(persisted.contains("timeout: 600"));
        assert!(persisted.contains("max_tool_calls: 100"));
        assert!(persisted.contains("keep: true"));
    }

    #[test]
    fn code_execution_config_rejects_present_invalid_yaml() {
        let fixture = Fixture::new();
        for yaml in [
            "code_execution: null\n",
            "code_execution: []\n",
            "code_execution:\n  mode: null\n",
            "code_execution:\n  mode: sandbox\n",
            "code_execution:\n  timeout: null\n",
            "code_execution:\n  timeout: 0\n",
            "code_execution:\n  timeout: 601\n",
            "code_execution:\n  max_tool_calls: null\n",
            "code_execution:\n  max_tool_calls: 0\n",
            "code_execution:\n  max_tool_calls: 101\n",
        ] {
            fs::write(fixture._home.path().join("config.yaml"), yaml).unwrap();
            assert!(matches!(
                fixture.service.get_config(DEFAULT_PROFILE_ID),
                Err(ProfileError::DataInvalid)
            ));
        }
    }

    #[test]
    fn web_config_exposes_the_shared_config_revision_and_default_limit() {
        let fixture = Fixture::new();
        let config = fixture.service.get_web_config(DEFAULT_PROFILE_ID).unwrap();

        assert_eq!(config.etag, quote_revision(&config.value.revision));
        assert_eq!(
            config.value.extract_char_limit,
            DEFAULT_WEB_EXTRACT_CHAR_LIMIT
        );
    }

    #[test]
    fn web_config_rejects_present_invalid_extract_limits() {
        let fixture = Fixture::new();
        for yaml in [
            "web:\n  extract_char_limit: null\n",
            "web:\n  extract_char_limit: 1.5\n",
            "web:\n  extract_char_limit: \"15000\"\n",
            "web:\n  extract_char_limit: 1999\n",
            "web:\n  extract_char_limit: 500001\n",
        ] {
            fs::write(fixture._home.path().join("config.yaml"), yaml).unwrap();
            assert!(matches!(
                fixture.service.get_web_config(DEFAULT_PROFILE_ID),
                Err(ProfileError::DataInvalid)
            ));
        }
    }

    #[test]
    fn config_patch_rejects_null_deletion_sensitive_extensions_and_non_http_urls() {
        let fixture = Fixture::new();
        let etag = fixture.service.get_config(DEFAULT_PROFILE_ID).unwrap().etag;
        for patch in [
            json!({"memoryProvider": null}),
            json!({"codeExecution": null}),
            json!({"codeExecution": {"mode": "sandbox"}}),
            json!({"codeExecution": {"timeoutSeconds": 0}}),
            json!({"codeExecution": {"timeoutSeconds": 601}}),
            json!({"codeExecution": {"timeoutSeconds": 1.5}}),
            json!({"codeExecution": {"maxToolCalls": 0}}),
            json!({"codeExecution": {"maxToolCalls": 101}}),
            json!({"codeExecution": {"unknown": true}}),
            json!({"extensions": {"apiToken": "not-allowed"}}),
            json!({"extensions": {"ui": null}}),
            json!({"model": {"baseUrl": "file:///tmp/config"}}),
            json!({"model": {"baseUrl": "https://user:password@example.test/v1"}}),
            json!({"model": {"baseUrl": "https://example.test/v1?api_key=value"}}),
            json!({"model": {"baseUrl": "https://example.test/v1#fragment"}}),
        ] {
            assert!(matches!(
                fixture
                    .service
                    .update_config(DEFAULT_PROFILE_ID, &etag, &patch),
                Err(ProfileError::InvalidProfileConfig)
            ));
        }
    }

    #[test]
    fn empty_hermes_memory_provider_projects_as_builtin() {
        let fixture = Fixture::new();
        fs::write(
            fixture._home.path().join("config.yaml"),
            "memory:\n  provider: \"\"\n",
        )
        .unwrap();
        assert_eq!(
            fixture
                .service
                .get_config(DEFAULT_PROFILE_ID)
                .unwrap()
                .value
                .memory_provider,
            "builtin"
        );
    }

    #[test]
    fn hermes_defaults_and_semantic_noops_do_not_create_config_files() {
        let fixture = Fixture::new();
        let before = fixture.service.get_config(DEFAULT_PROFILE_ID).unwrap();
        assert_eq!(before.value.model.provider, "auto");
        assert_eq!(before.value.model.base_url, None);
        assert_eq!(before.value.memory_provider, "builtin");
        assert_eq!(before.value.code_execution.mode, CodeExecutionMode::Project);
        assert_eq!(
            before.value.code_execution.timeout_seconds,
            DEFAULT_CODE_EXECUTION_TIMEOUT_SECONDS
        );
        assert_eq!(
            before.value.code_execution.max_tool_calls,
            DEFAULT_CODE_EXECUTION_TOOL_CALLS
        );

        let after = fixture
            .service
            .update_config(
                DEFAULT_PROFILE_ID,
                &before.etag,
                &json!({
                    "model": {"provider": "auto", "baseUrl": null},
                    "codeExecution": {
                        "mode": "project",
                        "timeoutSeconds": DEFAULT_CODE_EXECUTION_TIMEOUT_SECONDS,
                        "maxToolCalls": DEFAULT_CODE_EXECUTION_TOOL_CALLS
                    },
                    "memoryProvider": "builtin"
                }),
            )
            .unwrap();
        assert_eq!(before, after);
        assert!(!fixture._home.path().join("config.yaml").exists());
    }

    #[test]
    fn real_hermes_nested_skills_and_platform_overrides_are_preserved_but_not_faked() {
        let fixture = Fixture::new();
        fs::write(
            fixture._home.path().join("config.yaml"),
            "skills:\n  config:\n    research:\n      max_steps: 5\n  guard_agent_created: true\ntelegram:\n  enabled: false\nextensions:\n  apiToken: hidden\n  ui:\n    density: compact\n  nested:\n    password: hidden-too\n",
        )
        .unwrap();

        let config = fixture.service.get_config(DEFAULT_PROFILE_ID).unwrap();
        assert!(config.value.skills.is_empty());
        assert!(config.value.platforms.is_empty());
        assert_eq!(config.value.extensions["ui"]["density"], "compact");
        assert!(!config.value.extensions.contains_key("apiToken"));
        assert_eq!(config.value.extensions["nested"], json!({}));
        for patch in [
            json!({"skills": {"research": false}}),
            json!({"platforms": {"telegram": true}}),
        ] {
            assert!(matches!(
                fixture
                    .service
                    .update_config(DEFAULT_PROFILE_ID, &config.etag, &patch),
                Err(ProfileError::InvalidProfileConfig)
            ));
        }
        let persisted = fs::read_to_string(fixture._home.path().join("config.yaml")).unwrap();
        assert!(persisted.contains("max_steps: 5"));
        assert!(persisted.contains("enabled: false"));
    }

    #[test]
    fn provider_catalog_uses_pinned_hermes_ids_and_ordered_secret_aliases() {
        let fixture = Fixture::new();
        let providers = fixture.service.providers();
        assert!(
            providers
                .iter()
                .all(|provider| !provider.supports_model_discovery)
        );
        assert!(providers.iter().any(|provider| {
            provider.id == "auto"
                && provider.secret_names
                    == vec!["OPENROUTER_API_KEY".to_owned(), "OPENAI_API_KEY".to_owned()]
        }));
        assert!(providers.iter().any(|provider| {
            provider.id == "openai-api"
                && provider.secret_names == vec!["OPENAI_API_KEY".to_owned()]
        }));
        assert!(!providers.iter().any(|provider| provider.id == "openai"));
        let anthropic = providers
            .iter()
            .find(|provider| provider.id == "anthropic")
            .unwrap();
        assert_eq!(
            anthropic.secret_names,
            vec![
                "ANTHROPIC_API_KEY".to_owned(),
                "ANTHROPIC_TOKEN".to_owned(),
                "CLAUDE_CODE_OAUTH_TOKEN".to_owned()
            ]
        );
        let gemini = providers
            .iter()
            .find(|provider| provider.id == "gemini")
            .unwrap();
        assert_eq!(
            gemini.secret_names,
            vec!["GOOGLE_API_KEY".to_owned(), "GEMINI_API_KEY".to_owned()]
        );
    }

    #[test]
    fn pending_idempotency_record_recovers_and_completed_record_never_aliases_recreation() {
        let fixture = Fixture::new();
        fixture
            .service
            .list_profiles(ProfileEngineState::Stopped)
            .unwrap();
        let request = CreateProfile {
            id: "recover".to_owned(),
            display_name: "Recover".to_owned(),
            clone_from_profile_id: None,
        };
        let created_at = now_timestamp().unwrap();
        let metadata = profile_metadata_document(&request, &created_at);
        let metadata_bytes = json_bytes_bounded(&metadata, MAX_METADATA_BYTES).unwrap();
        let record = IdempotencyRecord {
            fingerprint: request_fingerprint(&request).unwrap(),
            profile_id: request.id.clone(),
            created_at,
            resource_etag: etag_for_bytes(&metadata_bytes),
            state: IdempotencyState::Pending,
        };
        fixture
            .service
            .write_idempotency_record("recover-create-key", &record)
            .unwrap();
        fixture
            .service
            .create_profile(&request, "recover-create-key")
            .unwrap();
        assert_eq!(
            fixture
                .service
                .read_idempotency_record("recover-create-key")
                .unwrap()
                .unwrap()
                .state,
            IdempotencyState::Completed
        );

        fixture.service.delete_profile("recover").unwrap();
        fixture
            .service
            .create_profile(&request, "replacement-create-key")
            .unwrap();
        assert!(matches!(
            fixture
                .service
                .create_profile(&request, "recover-create-key"),
            Err(ProfileError::IdempotencyResourceGone)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn profiles_parent_symlink_is_rejected_before_active_state_changes() {
        use std::os::unix::fs::symlink;

        let fixture = Fixture::new();
        let outside = tempfile::tempdir().unwrap();
        fs::create_dir(outside.path().join("work")).unwrap();
        symlink(outside.path(), fixture._home.path().join("profiles")).unwrap();

        assert!(matches!(
            fixture
                .service
                .activate_profile("work", ProfileEngineState::Stopped),
            Err(ProfileError::UnsafeProfilePath)
        ));
        assert!(!fixture._home.path().join("active_profile").exists());
    }

    #[test]
    fn active_and_default_profiles_cannot_be_deleted() {
        let fixture = Fixture::new();
        fixture.create("work");
        assert!(matches!(
            fixture.service.delete_profile(DEFAULT_PROFILE_ID),
            Err(ProfileError::ProfileDeleteConflict)
        ));
        fixture
            .service
            .activate_profile("work", ProfileEngineState::Stopped)
            .unwrap();
        assert!(matches!(
            fixture.service.delete_profile("work"),
            Err(ProfileError::ProfileDeleteConflict)
        ));
        fixture
            .service
            .activate_profile(DEFAULT_PROFILE_ID, ProfileEngineState::Stopped)
            .unwrap();
        fixture.service.delete_profile("work").unwrap();
        fixture.service.delete_profile("work").unwrap();
    }

    #[test]
    fn secrets_use_the_injected_store_and_the_index_never_contains_values() {
        let fixture = Fixture::new();
        fixture.create("work");
        let secret = "highly-sensitive-test-value";
        let status = fixture
            .service
            .put_secret(
                "work",
                "CUSTOM_API_KEY",
                &SecretString::from(secret.to_owned()),
            )
            .unwrap();
        assert!(status.configured);
        let statuses = fixture.service.list_secret_statuses("work").unwrap();
        assert!(
            statuses
                .iter()
                .any(|status| status.name == "CUSTOM_API_KEY" && status.configured)
        );
        let redaction_snapshots = fixture.service.secret_redaction_snapshots("work").unwrap();
        assert!(
            redaction_snapshots
                .iter()
                .any(|snapshot| snapshot.expose_secret() == secret)
        );

        let index = fs::read_to_string(
            fixture
                ._home
                .path()
                .join(SYNTHCHAT_DIR)
                .join("secret-index")
                .join("work.json"),
        )
        .unwrap();
        assert!(!index.contains(secret));
        fixture
            .service
            .delete_secret("work", "CUSTOM_API_KEY")
            .unwrap();
        assert!(
            !fixture
                .service
                .list_secret_statuses("work")
                .unwrap()
                .iter()
                .any(|status| status.name == "CUSTOM_API_KEY")
        );
    }

    #[test]
    fn secret_limit_is_enforced_in_utf8_bytes() {
        let fixture = Fixture::new();
        let exactly = SecretString::from("é".repeat(1280));
        fixture
            .service
            .put_secret(DEFAULT_PROFILE_ID, "CUSTOM_SECRET", &exactly)
            .unwrap();
        let too_large = SecretString::from("界".repeat(854));
        assert!(matches!(
            fixture
                .service
                .put_secret(DEFAULT_PROFILE_ID, "CUSTOM_SECRET", &too_large),
            Err(ProfileError::InvalidSecretValue)
        ));
    }

    #[test]
    fn secret_values_are_written_as_exact_utf8_bytes() {
        let home = tempfile::tempdir().unwrap();
        let store: Arc<CredentialStore> = mock::Store::new().unwrap();
        let service = ProfileService::with_credential_store(home.path().to_owned(), store.clone());
        let value = "a".repeat(MAX_SECRET_BYTES);
        service
            .put_secret(
                DEFAULT_PROFILE_ID,
                "ASCII_LIMIT_SECRET",
                &SecretString::from(value.clone()),
            )
            .unwrap();
        let entry = store
            .build(
                SECRET_SERVICE,
                &secret_account(DEFAULT_PROFILE_ID, "ASCII_LIMIT_SECRET"),
                None,
            )
            .unwrap();
        let stored = Zeroizing::new(entry.get_secret().unwrap());
        assert_eq!(stored.as_slice(), value.as_bytes());
    }

    #[test]
    fn unavailable_keychain_does_not_disable_profiles_or_config() {
        let home = tempfile::tempdir().unwrap();
        let service = ProfileService::without_credential_store(home.path().to_owned());
        assert_eq!(
            service
                .list_profiles(ProfileEngineState::Stopped)
                .unwrap()
                .len(),
            1
        );
        assert!(service.get_config(DEFAULT_PROFILE_ID).is_ok());
        assert!(matches!(
            service.list_secret_statuses(DEFAULT_PROFILE_ID),
            Err(ProfileError::SecretStorageUnavailable)
        ));
        assert!(matches!(
            service.get_web_config(DEFAULT_PROFILE_ID),
            Err(ProfileError::SecretStorageUnavailable)
        ));
        assert!(matches!(
            service.update_web_config(
                DEFAULT_PROFILE_ID,
                "\"stale-revision\"",
                &WebConfigPatch::default(),
            ),
            Err(ProfileError::SecretStorageUnavailable)
        ));
    }

    #[test]
    fn concurrent_updates_with_one_etag_cannot_both_commit() {
        let fixture = Fixture::new();
        let etag = fixture.service.get_config(DEFAULT_PROFILE_ID).unwrap().etag;
        let first = fixture.service.clone();
        let second = fixture.service.clone();
        let first_etag = etag.clone();
        let first_thread = thread::spawn(move || {
            first.update_config(
                DEFAULT_PROFILE_ID,
                &first_etag,
                &json!({"model": {"model": "first"}}),
            )
        });
        let second_thread = thread::spawn(move || {
            second.update_config(
                DEFAULT_PROFILE_ID,
                &etag,
                &json!({"model": {"model": "second"}}),
            )
        });
        let results = [first_thread.join().unwrap(), second_thread.join().unwrap()];
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result, Err(ProfileError::RevisionConflict { .. })))
                .count(),
            1
        );
    }

    #[test]
    fn independent_service_instances_share_the_cross_process_file_lock() {
        let home = tempfile::tempdir().unwrap();
        let store: Arc<CredentialStore> = mock::Store::new().unwrap();
        let first = ProfileService::with_credential_store(home.path().to_owned(), store.clone());
        let second = ProfileService::with_credential_store(home.path().to_owned(), store);
        let etag = first.get_config(DEFAULT_PROFILE_ID).unwrap().etag;
        let second_etag = etag.clone();
        let first_thread = thread::spawn(move || {
            first.update_config(
                DEFAULT_PROFILE_ID,
                &etag,
                &json!({"model": {"model": "first-instance"}}),
            )
        });
        let second_thread = thread::spawn(move || {
            second.update_config(
                DEFAULT_PROFILE_ID,
                &second_etag,
                &json!({"model": {"model": "second-instance"}}),
            )
        });
        let results = [first_thread.join().unwrap(), second_thread.join().unwrap()];
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result, Err(ProfileError::RevisionConflict { .. })))
                .count(),
            1
        );
    }
}
