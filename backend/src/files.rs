use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt};
use cap_std::{
    ambient_authority,
    fs::{Dir, File as CapFile, OpenOptions},
};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use uuid::Uuid;

pub const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;
pub const MAX_MULTIPART_BYTES: usize = MAX_FILE_BYTES as usize + 64 * 1024;
pub const MAX_RETAINED_FILE_OBJECTS: usize = 512;
pub const MAX_RETAINED_FILE_BYTES: u64 = 256 * 1024 * 1024;
pub const ALLOWED_MIME_TYPES: &[&str] = &[
    "application/json",
    "application/octet-stream",
    "application/pdf",
    "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    "application/x-zip-compressed",
    "application/yaml",
    "application/zip",
    "image/gif",
    "image/jpeg",
    "image/png",
    "image/webp",
    "text/csv",
    "text/markdown",
    "text/plain",
    "text/tab-separated-values",
    "text/yaml",
];

const OBJECTS_DIRECTORY: &str = "objects";
const IDEMPOTENCY_DIRECTORY: &str = "idempotency";
const STORE_LOCK_FILE: &str = "store.lock";
const CONTENT_FILE: &str = "content";
const METADATA_FILE: &str = "metadata.json";
const MAX_METADATA_BYTES: u64 = 16 * 1024;
const MAX_IDEMPOTENCY_RECORDS: usize = 4_096;
const IDEMPOTENCY_RETENTION_SECONDS: i64 = 7 * 24 * 60 * 60;
const RECORD_VERSION: u32 = 1;

#[derive(Clone, Copy)]
struct StoreQuota {
    max_objects: usize,
    max_bytes: u64,
    max_idempotency_records: usize,
}

impl Default for StoreQuota {
    fn default() -> Self {
        Self {
            max_objects: MAX_RETAINED_FILE_OBJECTS,
            max_bytes: MAX_RETAINED_FILE_BYTES,
            max_idempotency_records: MAX_IDEMPOTENCY_RECORDS,
        }
    }
}

#[derive(Clone)]
pub struct FileService {
    hermes_home: Arc<PathBuf>,
    process_lock: Arc<Mutex<()>>,
    quota: StoreQuota,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FileRef {
    pub id: String,
    pub name: String,
    pub mime_type: String,
    pub size_bytes: u64,
    pub created_at: String,
}

#[derive(Clone, Debug)]
pub(crate) struct FileUpload {
    pub(crate) name: String,
    pub(crate) mime_type: String,
    pub(crate) bytes: Vec<u8>,
}

#[derive(Clone, Debug)]
pub(crate) struct FileSnapshot {
    pub(crate) reference: FileRef,
    pub(crate) bytes: Vec<u8>,
    pub(crate) sha256: String,
}

#[derive(Debug, thiserror::Error)]
pub enum FileError {
    #[error("the file request is invalid")]
    InvalidRequest,
    #[error("the file ID is invalid")]
    InvalidFileId,
    #[error("the file MIME type is unsupported")]
    UnsupportedMimeType,
    #[error("the file exceeds the upload limit")]
    PayloadTooLarge,
    #[error("the retained file store quota is exhausted")]
    QuotaExceeded,
    #[error("the file was not found")]
    NotFound,
    #[error("the idempotency key was reused with different file data")]
    IdempotencyConflict,
    #[error("the idempotent file was deleted")]
    IdempotencyResourceGone,
    #[error("the file store contains an unsafe path")]
    UnsafePath,
    #[error("the file store contains malformed data")]
    DataInvalid,
    #[error("the file store is unavailable")]
    Storage(#[source] io::Error),
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FileObjectMetadata {
    version: u32,
    file: FileRef,
    sha256: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct IdempotencyRecord {
    version: u32,
    fingerprint: String,
    file: FileRef,
    sha256: String,
}

struct Store {
    root: Dir,
    objects: Dir,
    idempotency: Dir,
}

#[derive(Default)]
struct StoreUsage {
    objects: usize,
    bytes: u64,
    idempotency_records: usize,
}

impl FileService {
    pub fn new(hermes_home: impl Into<PathBuf>) -> Self {
        Self {
            hermes_home: Arc::new(hermes_home.into()),
            process_lock: Arc::new(Mutex::new(())),
            quota: StoreQuota::default(),
        }
    }

    #[cfg(test)]
    fn with_quota(
        hermes_home: impl Into<PathBuf>,
        max_objects: usize,
        max_bytes: u64,
        max_idempotency_records: usize,
    ) -> Self {
        Self {
            hermes_home: Arc::new(hermes_home.into()),
            process_lock: Arc::new(Mutex::new(())),
            quota: StoreQuota {
                max_objects,
                max_bytes,
                max_idempotency_records,
            },
        }
    }

    pub fn max_bytes(&self) -> u64 {
        MAX_FILE_BYTES
    }

    pub fn allowed_mime_types(&self) -> &'static [&'static str] {
        ALLOWED_MIME_TYPES
    }

    pub(crate) fn upload(
        &self,
        upload: &FileUpload,
        idempotency_key: &str,
    ) -> Result<FileRef, FileError> {
        validate_upload(upload)?;
        let fingerprint = upload_fingerprint(upload);
        let sha256 = sha256_hex(&upload.bytes);

        self.with_store_lock(|store| {
            let usage = measure_store_usage(store)?;
            let record_name = idempotency_record_name(idempotency_key);
            let done_name = idempotency_done_name(idempotency_key);
            if let Some(record) = read_json_optional::<IdempotencyRecord>(
                &store.idempotency,
                &record_name,
                MAX_METADATA_BYTES,
            )? {
                validate_record(&record)?;
                if record.fingerprint != fingerprint {
                    return Err(FileError::IdempotencyConflict);
                }
                match read_object(store, &record.file.id) {
                    Ok(snapshot) => {
                        if snapshot.reference != record.file || snapshot.sha256 != record.sha256 {
                            return Err(FileError::DataInvalid);
                        }
                        ensure_done_marker(&store.idempotency, &done_name)?;
                        return Ok(record.file);
                    }
                    Err(FileError::NotFound) if path_exists(&store.idempotency, &done_name)? => {
                        return Err(FileError::IdempotencyResourceGone);
                    }
                    Err(FileError::NotFound) => {
                        ensure_existing_reservation_fits(&usage, self.quota)?;
                        let metadata = FileObjectMetadata {
                            version: RECORD_VERSION,
                            file: record.file.clone(),
                            sha256: record.sha256.clone(),
                        };
                        create_object(store, &metadata, &upload.bytes)?;
                        ensure_done_marker(&store.idempotency, &done_name)?;
                        return Ok(record.file);
                    }
                    Err(error) => return Err(error),
                }
            }

            ensure_new_snapshot_fits(&usage, upload.bytes.len() as u64, self.quota)?;

            let file = FileRef {
                id: new_file_id(),
                name: upload.name.clone(),
                mime_type: upload.mime_type.clone(),
                size_bytes: upload.bytes.len() as u64,
                created_at: now_timestamp()?,
            };
            let record = IdempotencyRecord {
                version: RECORD_VERSION,
                fingerprint,
                file: file.clone(),
                sha256: sha256.clone(),
            };
            atomic_create_json(&store.idempotency, &record_name, &record)?;
            let metadata = FileObjectMetadata {
                version: RECORD_VERSION,
                file: file.clone(),
                sha256,
            };
            create_object(store, &metadata, &upload.bytes)?;
            ensure_done_marker(&store.idempotency, &done_name)?;
            Ok(file)
        })
    }

    pub(crate) fn read(&self, file_id: &str) -> Result<FileSnapshot, FileError> {
        self.acquire_for_skill(file_id)
    }

    pub(crate) fn acquire_for_skill(&self, file_id: &str) -> Result<FileSnapshot, FileError> {
        validate_file_id(file_id)?;
        self.with_store_lock(|store| read_object(store, file_id))
    }

    pub(crate) fn delete(&self, file_id: &str) -> Result<(), FileError> {
        validate_file_id(file_id)?;
        self.with_store_lock(|store| delete_object(store, file_id))
    }

    fn with_store_lock<T>(
        &self,
        operation: impl FnOnce(&Store) -> Result<T, FileError>,
    ) -> Result<T, FileError> {
        let _process_guard = self
            .process_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let store = self.open_store()?;
        let lock_file = open_lock_file(&store.root, STORE_LOCK_FILE)?.into_std();
        FileExt::lock_exclusive(&lock_file).map_err(FileError::Storage)?;
        let result = operation(&store);
        let unlock_result = FileExt::unlock(&lock_file).map_err(FileError::Storage);
        match (result, unlock_result) {
            (Ok(value), Ok(())) => Ok(value),
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
        }
    }

    fn open_store(&self) -> Result<Store, FileError> {
        fs::create_dir_all(self.hermes_home.as_ref()).map_err(FileError::Storage)?;
        ensure_safe_directory(self.hermes_home.as_ref())?;
        let home = open_ambient_directory_nofollow(self.hermes_home.as_ref())?;
        let synthchat = open_or_create_directory(&home, ".synthchat")?;
        let root = open_or_create_directory(&synthchat, "files")?;
        let objects = open_or_create_directory(&root, OBJECTS_DIRECTORY)?;
        let idempotency = open_or_create_directory(&root, IDEMPOTENCY_DIRECTORY)?;
        Ok(Store {
            root,
            objects,
            idempotency,
        })
    }
}

pub(crate) fn normalize_mime_type(value: &str) -> Result<String, FileError> {
    let mut parts = value.split(';');
    let base = parts.next().unwrap_or_default().trim().to_ascii_lowercase();
    if base.is_empty()
        || base
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte == b' ')
        || !ALLOWED_MIME_TYPES.contains(&base.as_str())
    {
        return Err(FileError::UnsupportedMimeType);
    }
    Ok(base)
}

fn validate_upload(upload: &FileUpload) -> Result<(), FileError> {
    validate_file_name(&upload.name)?;
    if upload.bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(FileError::PayloadTooLarge);
    }
    if normalize_mime_type(&upload.mime_type)? != upload.mime_type {
        return Err(FileError::UnsupportedMimeType);
    }
    Ok(())
}

fn validate_file_name(name: &str) -> Result<(), FileError> {
    if name.is_empty()
        || name.len() > 255
        || name.trim() != name
        || matches!(name, "." | "..")
        || name
            .chars()
            .any(|character| character.is_control() || matches!(character, '/' | '\\' | ':'))
    {
        return Err(FileError::InvalidRequest);
    }
    Ok(())
}

fn validate_file_id(file_id: &str) -> Result<(), FileError> {
    let Some(suffix) = file_id.strip_prefix("file_") else {
        return Err(FileError::InvalidFileId);
    };
    if suffix.len() != 32
        || !suffix
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(FileError::InvalidFileId);
    }
    Ok(())
}

fn validate_file_ref(file: &FileRef) -> Result<(), FileError> {
    validate_file_id(&file.id).map_err(|_| FileError::DataInvalid)?;
    validate_file_name(&file.name).map_err(|_| FileError::DataInvalid)?;
    if normalize_mime_type(&file.mime_type).map_err(|_| FileError::DataInvalid)? != file.mime_type
        || file.size_bytes > MAX_FILE_BYTES
        || OffsetDateTime::parse(&file.created_at, &Rfc3339).is_err()
    {
        return Err(FileError::DataInvalid);
    }
    Ok(())
}

fn validate_record(record: &IdempotencyRecord) -> Result<(), FileError> {
    if record.version != RECORD_VERSION
        || !is_sha256_hex(&record.fingerprint)
        || !is_sha256_hex(&record.sha256)
    {
        return Err(FileError::DataInvalid);
    }
    validate_file_ref(&record.file)
}

fn new_file_id() -> String {
    format!("file_{}", Uuid::new_v4().simple())
}

fn now_timestamp() -> Result<String, FileError> {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|_| FileError::DataInvalid)
}

fn upload_fingerprint(upload: &FileUpload) -> String {
    let mut digest = Sha256::new();
    digest.update(b"synthchat-file-upload-v1\0");
    hash_length_prefixed(&mut digest, upload.name.as_bytes());
    hash_length_prefixed(&mut digest, upload.mime_type.as_bytes());
    hash_length_prefixed(&mut digest, &upload.bytes);
    hex_digest(digest.finalize().as_slice())
}

fn hash_length_prefixed(digest: &mut Sha256, value: &[u8]) {
    digest.update((value.len() as u64).to_be_bytes());
    digest.update(value);
}

fn idempotency_record_name(key: &str) -> String {
    format!(
        "{}.json",
        sha256_hex(format!("POST\n/api/v1/files\n{key}").as_bytes())
    )
}

fn idempotency_done_name(key: &str) -> String {
    format!(
        "{}.done",
        sha256_hex(format!("POST\n/api/v1/files\n{key}").as_bytes())
    )
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex_digest(Sha256::digest(bytes).as_slice())
}

fn hex_digest(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn ensure_existing_reservation_fits(
    usage: &StoreUsage,
    quota: StoreQuota,
) -> Result<(), FileError> {
    if usage.objects > quota.max_objects
        || usage.bytes > quota.max_bytes
        || usage.idempotency_records > quota.max_idempotency_records
    {
        Err(FileError::QuotaExceeded)
    } else {
        Ok(())
    }
}

fn ensure_new_snapshot_fits(
    usage: &StoreUsage,
    size_bytes: u64,
    quota: StoreQuota,
) -> Result<(), FileError> {
    let Some(objects) = usage.objects.checked_add(1) else {
        return Err(FileError::QuotaExceeded);
    };
    let Some(bytes) = usage.bytes.checked_add(size_bytes) else {
        return Err(FileError::QuotaExceeded);
    };
    let Some(idempotency_records) = usage.idempotency_records.checked_add(1) else {
        return Err(FileError::QuotaExceeded);
    };
    if objects > quota.max_objects
        || bytes > quota.max_bytes
        || idempotency_records > quota.max_idempotency_records
    {
        Err(FileError::QuotaExceeded)
    } else {
        Ok(())
    }
}

fn measure_store_usage(store: &Store) -> Result<StoreUsage, FileError> {
    cleanup_object_transients(&store.objects)?;
    cleanup_idempotency_transients(&store.idempotency)?;
    cleanup_expired_idempotency_records(store, OffsetDateTime::now_utc())?;

    let mut usage = StoreUsage::default();
    let mut objects = BTreeMap::new();
    for name in entry_names(&store.objects)? {
        validate_file_id(&name).map_err(|_| FileError::UnsafePath)?;
        let metadata = read_object_metadata_for_usage(store, &name)?;
        add_retained_snapshot(&mut usage, metadata.file.size_bytes)?;
        if objects.insert(name, metadata).is_some() {
            return Err(FileError::DataInvalid);
        }
    }

    let mut record_names = BTreeMap::new();
    let mut done_names = BTreeSet::new();
    for name in entry_names(&store.idempotency)? {
        if let Some(stem) = idempotency_entry_stem(&name, ".json") {
            if record_names.insert(stem.to_owned(), name).is_some() {
                return Err(FileError::DataInvalid);
            }
        } else if let Some(stem) = idempotency_entry_stem(&name, ".done") {
            if !done_names.insert(stem.to_owned()) {
                return Err(FileError::DataInvalid);
            }
            ensure_done_marker(&store.idempotency, &name)?;
        } else {
            return Err(FileError::UnsafePath);
        }
    }

    let record_stems = record_names.keys().cloned().collect::<BTreeSet<_>>();
    let mut record_file_ids = BTreeSet::new();
    for (stem, record_name) in record_names {
        usage.idempotency_records = usage
            .idempotency_records
            .checked_add(1)
            .ok_or(FileError::DataInvalid)?;
        let record: IdempotencyRecord =
            read_json(&store.idempotency, &record_name, MAX_METADATA_BYTES)?;
        validate_record(&record)?;
        if !record_file_ids.insert(record.file.id.clone()) {
            return Err(FileError::DataInvalid);
        }

        if let Some(metadata) = objects.get(&record.file.id) {
            if metadata.file != record.file || metadata.sha256 != record.sha256 {
                return Err(FileError::DataInvalid);
            }
        } else if !done_names.contains(&stem) {
            add_retained_snapshot(&mut usage, record.file.size_bytes)?;
        }
    }

    if !done_names.is_subset(&record_stems) {
        return Err(FileError::DataInvalid);
    }
    Ok(usage)
}

fn add_retained_snapshot(usage: &mut StoreUsage, size_bytes: u64) -> Result<(), FileError> {
    usage.objects = usage.objects.checked_add(1).ok_or(FileError::DataInvalid)?;
    usage.bytes = usage
        .bytes
        .checked_add(size_bytes)
        .ok_or(FileError::DataInvalid)?;
    Ok(())
}

fn read_object_metadata_for_usage(
    store: &Store,
    file_id: &str,
) -> Result<FileObjectMetadata, FileError> {
    let object = open_object_directory(&store.objects, file_id)?;
    validate_object_entries(&object)?;
    let metadata: FileObjectMetadata = read_json(&object, METADATA_FILE, MAX_METADATA_BYTES)?;
    if metadata.version != RECORD_VERSION
        || metadata.file.id != file_id
        || !is_sha256_hex(&metadata.sha256)
    {
        return Err(FileError::DataInvalid);
    }
    validate_file_ref(&metadata.file)?;
    let content = open_file_nofollow(&object, CONTENT_FILE)?;
    let content_metadata = content.metadata().map_err(FileError::Storage)?;
    if !content_metadata.is_file() || content_metadata.len() != metadata.file.size_bytes {
        return Err(FileError::DataInvalid);
    }
    Ok(metadata)
}

fn cleanup_object_transients(objects: &Dir) -> Result<(), FileError> {
    for name in entry_names(objects)? {
        if validate_file_id(&name).is_ok() {
            continue;
        }
        if internal_uuid_name(&name, ".upload-") || internal_uuid_name(&name, ".deleting-") {
            cleanup_transient_object_directory(objects, &name)?;
        } else {
            return Err(FileError::UnsafePath);
        }
    }
    Ok(())
}

fn cleanup_transient_object_directory(objects: &Dir, name: &str) -> Result<(), FileError> {
    let metadata = objects.symlink_metadata(name).map_err(FileError::Storage)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(FileError::UnsafePath);
    }
    let directory = objects
        .open_dir_nofollow(name)
        .map_err(|error| map_nofollow_error(objects, name, error))?;
    let entries = entry_names(&directory)?;
    for entry_name in &entries {
        if !matches!(entry_name.as_str(), CONTENT_FILE | METADATA_FILE)
            && !internal_uuid_name(entry_name, ".tmp-")
        {
            return Err(FileError::UnsafePath);
        }
        let metadata = directory
            .symlink_metadata(entry_name)
            .map_err(FileError::Storage)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(FileError::UnsafePath);
        }
    }
    for entry_name in entries {
        directory
            .remove_file(entry_name)
            .map_err(FileError::Storage)?;
    }
    sync_directory(&directory)?;
    drop(directory);
    objects.remove_dir(name).map_err(FileError::Storage)?;
    sync_directory(objects)
}

fn cleanup_idempotency_transients(idempotency: &Dir) -> Result<(), FileError> {
    let mut removed = false;
    for name in entry_names(idempotency)? {
        if idempotency_entry_stem(&name, ".json").is_some()
            || idempotency_entry_stem(&name, ".done").is_some()
        {
            continue;
        }
        if !internal_uuid_name(&name, ".tmp-") {
            return Err(FileError::UnsafePath);
        }
        let metadata = idempotency
            .symlink_metadata(&name)
            .map_err(FileError::Storage)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(FileError::UnsafePath);
        }
        idempotency.remove_file(&name).map_err(FileError::Storage)?;
        removed = true;
    }
    if removed {
        sync_directory(idempotency)?;
    }
    Ok(())
}

fn cleanup_expired_idempotency_records(
    store: &Store,
    now: OffsetDateTime,
) -> Result<(), FileError> {
    let idempotency = &store.idempotency;
    let mut records = BTreeMap::new();
    let mut done_markers = BTreeMap::new();
    for name in entry_names(idempotency)? {
        if let Some(stem) = idempotency_entry_stem(&name, ".json") {
            records.insert(stem.to_owned(), name);
        } else if let Some(stem) = idempotency_entry_stem(&name, ".done") {
            ensure_done_marker(idempotency, &name)?;
            done_markers.insert(stem.to_owned(), name);
        } else {
            return Err(FileError::UnsafePath);
        }
    }

    let mut removed = false;
    for (stem, done_name) in &done_markers {
        if !records.contains_key(stem) {
            idempotency
                .remove_file(done_name)
                .map_err(FileError::Storage)?;
            removed = true;
        }
    }
    for (stem, record_name) in records {
        let record: IdempotencyRecord = read_json(idempotency, &record_name, MAX_METADATA_BYTES)?;
        validate_record(&record)?;
        let created_at = OffsetDateTime::parse(&record.file.created_at, &Rfc3339)
            .map_err(|_| FileError::DataInvalid)?;
        let age_seconds = now
            .unix_timestamp()
            .saturating_sub(created_at.unix_timestamp());
        if age_seconds < IDEMPOTENCY_RETENTION_SECONDS {
            continue;
        }
        match open_object_directory(&store.objects, &record.file.id) {
            Ok(object) => {
                drop(object);
                continue;
            }
            Err(FileError::NotFound) => {}
            Err(error) => return Err(error),
        }

        idempotency
            .remove_file(&record_name)
            .map_err(FileError::Storage)?;
        if let Some(done_name) = done_markers.get(&stem) {
            idempotency
                .remove_file(done_name)
                .map_err(FileError::Storage)?;
        }
        removed = true;
    }
    if removed {
        sync_directory(idempotency)?;
    }
    Ok(())
}

fn entry_names(directory: &Dir) -> Result<Vec<String>, FileError> {
    let mut names = Vec::new();
    for entry in directory.entries().map_err(FileError::Storage)? {
        names.push(
            entry
                .map_err(FileError::Storage)?
                .file_name()
                .into_string()
                .map_err(|_| FileError::UnsafePath)?,
        );
    }
    Ok(names)
}

fn internal_uuid_name(name: &str, prefix: &str) -> bool {
    let Some(suffix) = name.strip_prefix(prefix) else {
        return false;
    };
    suffix.len() == 32
        && suffix
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn idempotency_entry_stem<'a>(name: &'a str, suffix: &str) -> Option<&'a str> {
    let stem = name.strip_suffix(suffix)?;
    is_sha256_hex(stem).then_some(stem)
}

fn read_object(store: &Store, file_id: &str) -> Result<FileSnapshot, FileError> {
    validate_file_id(file_id)?;
    let object = open_object_directory(&store.objects, file_id)?;
    validate_object_entries(&object)?;
    let metadata: FileObjectMetadata = read_json(&object, METADATA_FILE, MAX_METADATA_BYTES)?;
    if metadata.version != RECORD_VERSION
        || metadata.file.id != file_id
        || !is_sha256_hex(&metadata.sha256)
    {
        return Err(FileError::DataInvalid);
    }
    validate_file_ref(&metadata.file)?;
    let bytes = read_file_bounded(&object, CONTENT_FILE, MAX_FILE_BYTES)?;
    if bytes.len() as u64 != metadata.file.size_bytes || sha256_hex(&bytes) != metadata.sha256 {
        return Err(FileError::DataInvalid);
    }
    Ok(FileSnapshot {
        reference: metadata.file,
        bytes,
        sha256: metadata.sha256,
    })
}

fn create_object(
    store: &Store,
    metadata: &FileObjectMetadata,
    bytes: &[u8],
) -> Result<(), FileError> {
    validate_file_ref(&metadata.file)?;
    if metadata.version != RECORD_VERSION
        || bytes.len() as u64 != metadata.file.size_bytes
        || sha256_hex(bytes) != metadata.sha256
    {
        return Err(FileError::DataInvalid);
    }
    match store.objects.symlink_metadata(&metadata.file.id) {
        Ok(_) => return Err(FileError::DataInvalid),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(FileError::Storage(error)),
    }

    let staging_name = format!(".upload-{}", Uuid::new_v4().simple());
    store
        .objects
        .create_dir(&staging_name)
        .map_err(FileError::Storage)?;
    let result = (|| {
        let staging = store
            .objects
            .open_dir_nofollow(&staging_name)
            .map_err(FileError::Storage)?;
        atomic_create_file(&staging, CONTENT_FILE, bytes)?;
        atomic_create_json(&staging, METADATA_FILE, metadata)?;
        sync_directory(&staging)?;
        drop(staging);
        store
            .objects
            .rename(&staging_name, &store.objects, &metadata.file.id)
            .map_err(FileError::Storage)?;
        sync_directory(&store.objects)
    })();
    if result.is_err() {
        cleanup_staging_directory(&store.objects, &staging_name);
    }
    result
}

fn delete_object(store: &Store, file_id: &str) -> Result<(), FileError> {
    let object = match open_object_directory(&store.objects, file_id) {
        Ok(object) => object,
        Err(FileError::NotFound) => return Ok(()),
        Err(error) => return Err(error),
    };
    validate_object_entries(&object)?;
    let _ = read_object(store, file_id)?;
    drop(object);

    let deleting_name = format!(".deleting-{}", Uuid::new_v4().simple());
    store
        .objects
        .rename(file_id, &store.objects, &deleting_name)
        .map_err(FileError::Storage)?;
    sync_directory(&store.objects)?;
    let deleting = store
        .objects
        .open_dir_nofollow(&deleting_name)
        .map_err(FileError::Storage)?;
    deleting
        .remove_file(CONTENT_FILE)
        .map_err(FileError::Storage)?;
    deleting
        .remove_file(METADATA_FILE)
        .map_err(FileError::Storage)?;
    sync_directory(&deleting)?;
    drop(deleting);
    store
        .objects
        .remove_dir(&deleting_name)
        .map_err(FileError::Storage)?;
    sync_directory(&store.objects)
}

fn open_object_directory(objects: &Dir, file_id: &str) -> Result<Dir, FileError> {
    match objects.symlink_metadata(file_id) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Err(FileError::NotFound),
        Err(error) => return Err(FileError::Storage(error)),
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(FileError::UnsafePath);
        }
        Ok(_) => {}
    }
    objects
        .open_dir_nofollow(file_id)
        .map_err(|error| map_nofollow_error(objects, file_id, error))
}

fn validate_object_entries(object: &Dir) -> Result<(), FileError> {
    let mut names = BTreeSet::new();
    for entry in object.entries().map_err(FileError::Storage)? {
        let entry = entry.map_err(FileError::Storage)?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| FileError::UnsafePath)?;
        if !matches!(name.as_str(), CONTENT_FILE | METADATA_FILE) {
            return Err(FileError::UnsafePath);
        }
        let metadata = object.symlink_metadata(&name).map_err(FileError::Storage)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() || !names.insert(name) {
            return Err(FileError::UnsafePath);
        }
    }
    if names.len() == 2 {
        Ok(())
    } else {
        Err(FileError::DataInvalid)
    }
}

fn ensure_done_marker(directory: &Dir, name: &str) -> Result<(), FileError> {
    match directory.symlink_metadata(name) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            Err(FileError::UnsafePath)
        }
        Ok(metadata) if metadata.len() == 0 => Ok(()),
        Ok(_) => Err(FileError::DataInvalid),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            atomic_create_file(directory, name, b"")
        }
        Err(error) => Err(FileError::Storage(error)),
    }
}

fn path_exists(directory: &Dir, name: &str) -> Result<bool, FileError> {
    match directory.symlink_metadata(name) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            Err(FileError::UnsafePath)
        }
        Ok(metadata) if metadata.len() == 0 => Ok(true),
        Ok(_) => Err(FileError::DataInvalid),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(FileError::Storage(error)),
    }
}

fn read_json<T: for<'de> Deserialize<'de>>(
    directory: &Dir,
    name: &str,
    maximum: u64,
) -> Result<T, FileError> {
    let bytes = read_file_bounded(directory, name, maximum)?;
    serde_json::from_slice(&bytes).map_err(|_| FileError::DataInvalid)
}

fn read_json_optional<T: for<'de> Deserialize<'de>>(
    directory: &Dir,
    name: &str,
    maximum: u64,
) -> Result<Option<T>, FileError> {
    match directory.symlink_metadata(name) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(FileError::Storage(error)),
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            Err(FileError::UnsafePath)
        }
        Ok(_) => read_json(directory, name, maximum).map(Some),
    }
}

fn read_file_bounded(directory: &Dir, name: &str, maximum: u64) -> Result<Vec<u8>, FileError> {
    let file = open_file_nofollow(directory, name)?;
    let metadata = file.metadata().map_err(FileError::Storage)?;
    if !metadata.is_file() {
        return Err(FileError::UnsafePath);
    }
    if metadata.len() > maximum {
        return Err(FileError::DataInvalid);
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(maximum + 1)
        .read_to_end(&mut bytes)
        .map_err(FileError::Storage)?;
    if bytes.len() as u64 > maximum {
        return Err(FileError::DataInvalid);
    }
    Ok(bytes)
}

fn atomic_create_json<T: Serialize>(
    directory: &Dir,
    name: &str,
    value: &T,
) -> Result<(), FileError> {
    let bytes = serde_json::to_vec(value).map_err(|_| FileError::DataInvalid)?;
    if bytes.len() as u64 > MAX_METADATA_BYTES {
        return Err(FileError::DataInvalid);
    }
    atomic_create_file(directory, name, &bytes)
}

fn atomic_create_file(directory: &Dir, name: &str, bytes: &[u8]) -> Result<(), FileError> {
    let temporary_name = format!(".tmp-{}", Uuid::new_v4().simple());
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    options.follow(FollowSymlinks::No);
    let mut temporary = directory
        .open_with(&temporary_name, &options)
        .map_err(FileError::Storage)?;
    let result = (|| {
        temporary.write_all(bytes).map_err(FileError::Storage)?;
        temporary.flush().map_err(FileError::Storage)?;
        temporary.sync_all().map_err(FileError::Storage)?;
        drop(temporary);
        match directory.symlink_metadata(name) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Ok(_) => return Err(FileError::UnsafePath),
            Err(error) => return Err(FileError::Storage(error)),
        }
        directory
            .hard_link(&temporary_name, directory, name)
            .map_err(FileError::Storage)?;
        directory
            .remove_file(&temporary_name)
            .map_err(FileError::Storage)?;
        sync_directory(directory)
    })();
    if result.is_err() {
        let _ = directory.remove_file(&temporary_name);
    }
    result
}

fn open_file_nofollow(directory: &Dir, name: &str) -> Result<CapFile, FileError> {
    let metadata = directory
        .symlink_metadata(name)
        .map_err(FileError::Storage)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(FileError::UnsafePath);
    }
    let mut options = OpenOptions::new();
    options.read(true);
    options.follow(FollowSymlinks::No);
    directory
        .open_with(name, &options)
        .map_err(|error| map_nofollow_error(directory, name, error))
}

fn open_lock_file(directory: &Dir, name: &str) -> Result<CapFile, FileError> {
    match directory.symlink_metadata(name) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err(FileError::UnsafePath);
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(FileError::Storage(error)),
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    options.follow(FollowSymlinks::No);
    directory
        .open_with(name, &options)
        .map_err(|error| map_nofollow_error(directory, name, error))
}

fn open_or_create_directory(parent: &Dir, name: &str) -> Result<Dir, FileError> {
    match parent.open_dir_nofollow(name) {
        Ok(directory) => return Ok(directory),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(map_nofollow_error(parent, name, error)),
    }
    match parent.create_dir(name) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(FileError::Storage(error)),
    }
    parent
        .open_dir_nofollow(name)
        .map_err(|error| map_nofollow_error(parent, name, error))
}

fn map_nofollow_error(directory: &Dir, name: impl AsRef<Path>, error: io::Error) -> FileError {
    match directory.symlink_metadata(name) {
        Ok(metadata) if metadata.file_type().is_symlink() => FileError::UnsafePath,
        Ok(_) => FileError::Storage(error),
        Err(metadata_error) if metadata_error.kind() == io::ErrorKind::NotFound => {
            FileError::Storage(error)
        }
        Err(metadata_error) => FileError::Storage(metadata_error),
    }
}

fn open_ambient_directory_nofollow(path: &Path) -> Result<Dir, FileError> {
    let Some(parent) = path.parent() else {
        return Dir::open_ambient_dir(path, ambient_authority()).map_err(FileError::Storage);
    };
    let Some(name) = path.file_name() else {
        return Dir::open_ambient_dir(path, ambient_authority()).map_err(FileError::Storage);
    };
    let canonical_parent = fs::canonicalize(parent).map_err(FileError::Storage)?;
    let parent =
        Dir::open_ambient_dir(canonical_parent, ambient_authority()).map_err(FileError::Storage)?;
    parent
        .open_dir_nofollow(name)
        .map_err(|error| map_nofollow_error(&parent, name, error))
}

fn ensure_safe_directory(path: &Path) -> Result<(), FileError> {
    let metadata = fs::symlink_metadata(path).map_err(FileError::Storage)?;
    if metadata.file_type().is_symlink()
        || is_windows_reparse_point(&metadata)
        || !metadata.is_dir()
    {
        Err(FileError::UnsafePath)
    } else {
        Ok(())
    }
}

#[cfg(windows)]
fn is_windows_reparse_point(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn is_windows_reparse_point(_metadata: &fs::Metadata) -> bool {
    false
}

#[cfg(unix)]
fn sync_directory(directory: &Dir) -> Result<(), FileError> {
    directory
        .try_clone()
        .map_err(FileError::Storage)?
        .into_std_file()
        .sync_all()
        .map_err(FileError::Storage)
}

#[cfg(not(unix))]
fn sync_directory(_directory: &Dir) -> Result<(), FileError> {
    Ok(())
}

fn cleanup_staging_directory(objects: &Dir, name: &str) {
    let Ok(directory) = objects.open_dir_nofollow(name) else {
        return;
    };
    let _ = directory.remove_file(CONTENT_FILE);
    let _ = directory.remove_file(METADATA_FILE);
    drop(directory);
    let _ = objects.remove_dir(name);
}

#[cfg(test)]
mod tests {
    use std::{sync::Barrier, thread};

    use tempfile::TempDir;

    use super::*;

    fn upload(name: &str, mime_type: &str, bytes: &[u8]) -> FileUpload {
        FileUpload {
            name: name.to_owned(),
            mime_type: mime_type.to_owned(),
            bytes: bytes.to_vec(),
        }
    }

    #[test]
    fn upload_replay_acquire_delete_and_conflict_are_durable() {
        let home = TempDir::new().unwrap();
        let service = FileService::new(home.path());
        let request = upload("skill.zip", "application/zip", b"zip bytes");
        let created = service.upload(&request, "durable-file-key").unwrap();
        let replay = FileService::new(home.path())
            .upload(&request, "durable-file-key")
            .unwrap();
        assert_eq!(created, replay);

        let snapshot = service.acquire_for_skill(&created.id).unwrap();
        assert_eq!(snapshot.reference, created);
        assert_eq!(snapshot.bytes, b"zip bytes");
        assert_eq!(snapshot.sha256, sha256_hex(b"zip bytes"));

        let changed = upload("skill.zip", "application/zip", b"other bytes");
        assert!(matches!(
            service.upload(&changed, "durable-file-key"),
            Err(FileError::IdempotencyConflict)
        ));
        service.delete(&created.id).unwrap();
        assert!(matches!(
            service.upload(&request, "durable-file-key"),
            Err(FileError::IdempotencyResourceGone)
        ));
    }

    #[test]
    fn pending_upload_recovers_the_same_opaque_resource_after_restart() {
        let home = TempDir::new().unwrap();
        let service = FileService::new(home.path());
        let request = upload("skill.zip", "application/zip", b"recoverable bytes");
        let key = "pending-recovery-key";
        let created = service.upload(&request, key).unwrap();

        fs::remove_file(
            home.path()
                .join(".synthchat/files/idempotency")
                .join(idempotency_done_name(key)),
        )
        .unwrap();
        fs::remove_dir_all(
            home.path()
                .join(".synthchat/files/objects")
                .join(&created.id),
        )
        .unwrap();

        let restarted = FileService::new(home.path());
        let recovered = restarted.upload(&request, key).unwrap();
        assert_eq!(recovered, created);
        assert_eq!(
            restarted.acquire_for_skill(&created.id).unwrap().bytes,
            b"recoverable bytes"
        );
    }

    #[test]
    fn retained_quota_counts_complete_snapshots_and_delete_releases_capacity() {
        let home = TempDir::new().unwrap();
        let service = FileService::with_quota(home.path(), 3, 5, 16);
        let first_request = upload("first.txt", "text/plain", b"aa");
        let first = service.upload(&first_request, "quota-first").unwrap();
        let second = service
            .upload(&upload("second.txt", "text/plain", b"bbb"), "quota-second")
            .unwrap();

        assert_eq!(
            service.upload(&first_request, "quota-first").unwrap(),
            first
        );
        assert!(matches!(
            service.upload(
                &upload("too-many-bytes.txt", "text/plain", b"x"),
                "quota-bytes"
            ),
            Err(FileError::QuotaExceeded)
        ));

        service.delete(&first.id).unwrap();
        let replacement = service
            .upload(
                &upload("replacement.txt", "text/plain", b"aa"),
                "quota-replacement",
            )
            .unwrap();
        let zero_length = service
            .upload(&upload("empty.txt", "text/plain", b""), "quota-empty")
            .unwrap();
        assert!(matches!(
            service.upload(&upload("fourth.txt", "text/plain", b""), "quota-objects"),
            Err(FileError::QuotaExceeded)
        ));

        assert_eq!(service.read(&second.id).unwrap().bytes, b"bbb");
        assert_eq!(service.read(&replacement.id).unwrap().bytes, b"aa");
        assert!(service.read(&zero_length.id).unwrap().bytes.is_empty());
    }

    #[test]
    fn pending_upload_reserves_quota_until_the_same_request_recovers_it() {
        let home = TempDir::new().unwrap();
        let service = FileService::with_quota(home.path(), 1, 4, 8);
        let request = upload("pending.txt", "text/plain", b"data");
        let key = "quota-pending";
        let created = service.upload(&request, key).unwrap();
        let store = home.path().join(".synthchat/files");
        fs::remove_file(
            store
                .join(IDEMPOTENCY_DIRECTORY)
                .join(idempotency_done_name(key)),
        )
        .unwrap();
        fs::remove_dir_all(store.join(OBJECTS_DIRECTORY).join(&created.id)).unwrap();

        assert!(matches!(
            service.upload(
                &upload("other.txt", "text/plain", b""),
                "quota-pending-other"
            ),
            Err(FileError::QuotaExceeded)
        ));
        assert_eq!(service.upload(&request, key).unwrap(), created);
    }

    #[test]
    fn expired_idempotency_sidecars_release_the_record_quota_without_objects() {
        let home = TempDir::new().unwrap();
        let service = FileService::with_quota(home.path(), 2, 2, 1);
        let expired_key = "expired-sidecar";
        let expired = service
            .upload(&upload("expired.txt", "text/plain", b"a"), expired_key)
            .unwrap();
        service.delete(&expired.id).unwrap();
        rewrite_record_created_at(
            home.path(),
            expired_key,
            OffsetDateTime::now_utc() - time::Duration::days(8),
        );

        service
            .upload(
                &upload("replacement.txt", "text/plain", b"b"),
                "replacement-sidecar",
            )
            .unwrap();
        let idempotency = home.path().join(".synthchat/files/idempotency");
        assert!(
            !idempotency
                .join(idempotency_record_name(expired_key))
                .exists()
        );
        assert!(
            !idempotency
                .join(idempotency_done_name(expired_key))
                .exists()
        );
    }

    #[test]
    fn idempotency_sidecars_younger_than_twenty_four_hours_preserve_replay() {
        let home = TempDir::new().unwrap();
        let service = FileService::with_quota(home.path(), 1, 1, 1);
        let key = "young-sidecar";
        let request = upload("young.txt", "text/plain", b"a");
        let created = service.upload(&request, key).unwrap();
        service.delete(&created.id).unwrap();
        rewrite_record_created_at(
            home.path(),
            key,
            OffsetDateTime::now_utc() - time::Duration::hours(23),
        );

        assert!(matches!(
            service.upload(&request, key),
            Err(FileError::IdempotencyResourceGone)
        ));
        let idempotency = home.path().join(".synthchat/files/idempotency");
        assert!(idempotency.join(idempotency_record_name(key)).is_file());
        assert!(idempotency.join(idempotency_done_name(key)).is_file());
    }

    #[test]
    fn cross_service_concurrent_uploads_cannot_overbook_quota() {
        let home = TempDir::new().unwrap();
        let first = FileService::with_quota(home.path(), 1, 1, 8);
        let second = FileService::with_quota(home.path(), 1, 1, 8);
        let barrier = Arc::new(Barrier::new(2));

        let first_barrier = barrier.clone();
        let first_thread = thread::spawn(move || {
            first_barrier.wait();
            first.upload(&upload("first.txt", "text/plain", b"a"), "concurrent-first")
        });
        let second_thread = thread::spawn(move || {
            barrier.wait();
            second.upload(
                &upload("second.txt", "text/plain", b"b"),
                "concurrent-second",
            )
        });

        let results = [first_thread.join().unwrap(), second_thread.join().unwrap()];
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result, Err(FileError::QuotaExceeded)))
                .count(),
            1
        );
        let objects = fs::read_dir(home.path().join(".synthchat/files/objects"))
            .unwrap()
            .count();
        assert_eq!(objects, 1);
    }

    #[test]
    fn orphaned_internal_staging_entries_are_cleaned_before_quota_is_measured() {
        let home = TempDir::new().unwrap();
        let service = FileService::with_quota(home.path(), 1, 1, 8);
        let store = service.open_store().unwrap();
        let upload_name = ".upload-00000000000000000000000000000001";
        store.objects.create_dir(upload_name).unwrap();
        let upload_directory = store.objects.open_dir_nofollow(upload_name).unwrap();
        atomic_create_file(
            &upload_directory,
            ".tmp-00000000000000000000000000000002",
            b"partial",
        )
        .unwrap();
        let deleting_name = ".deleting-00000000000000000000000000000003";
        store.objects.create_dir(deleting_name).unwrap();
        atomic_create_file(
            &store.idempotency,
            ".tmp-00000000000000000000000000000004",
            b"partial",
        )
        .unwrap();
        drop(upload_directory);
        drop(store);

        service
            .upload(&upload("new.txt", "text/plain", b"x"), "cleanup-orphans")
            .unwrap();
        let files_root = home.path().join(".synthchat/files");
        assert!(!files_root.join("objects").join(upload_name).exists());
        assert!(!files_root.join("objects").join(deleting_name).exists());
        assert!(
            !files_root
                .join("idempotency")
                .join(".tmp-00000000000000000000000000000004")
                .exists()
        );
    }

    #[test]
    fn names_ids_mime_types_and_sizes_are_validated_at_the_service_boundary() {
        let home = TempDir::new().unwrap();
        let service = FileService::new(home.path());
        assert!(matches!(
            service.upload(&upload("../secret", "text/plain", b"x"), "invalid-name-key"),
            Err(FileError::InvalidRequest)
        ));
        assert!(matches!(
            service.upload(&upload("page.html", "text/html", b"x"), "invalid-mime-key"),
            Err(FileError::UnsupportedMimeType)
        ));
        assert!(matches!(
            service.read("../../outside"),
            Err(FileError::InvalidFileId)
        ));
        let oversized = vec![0_u8; MAX_FILE_BYTES as usize + 1];
        assert!(matches!(
            service.upload(
                &upload("large.bin", "application/octet-stream", &oversized),
                "oversize-file-key"
            ),
            Err(FileError::PayloadTooLarge)
        ));
    }

    fn rewrite_record_created_at(home: &Path, key: &str, created_at: OffsetDateTime) {
        let path = home
            .join(".synthchat/files/idempotency")
            .join(idempotency_record_name(key));
        let mut record: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        record["file"]["createdAt"] =
            serde_json::Value::String(created_at.format(&Rfc3339).expect("valid RFC 3339 time"));
        fs::write(path, serde_json::to_vec(&record).unwrap()).unwrap();
    }
}
