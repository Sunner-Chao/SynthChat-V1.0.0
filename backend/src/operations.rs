use std::{
    collections::BTreeSet,
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

#[cfg(test)]
use std::sync::atomic::{AtomicBool, Ordering};

use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt};
use cap_std::{
    ambient_authority,
    fs::{Dir, OpenOptions as CapOpenOptions},
};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use uuid::Uuid;

const MAX_OPERATION_BYTES: u64 = 64 * 1024;
const MAX_IDEMPOTENCY_BYTES: u64 = 16 * 1024;
const MIN_IDEMPOTENCY_KEY_BYTES: usize = 8;
const MAX_IDEMPOTENCY_KEY_BYTES: usize = 128;
const MIN_TERMINAL_RETENTION_SECONDS: i64 = 24 * 60 * 60;

#[cfg(not(test))]
pub(crate) const MAX_PERSISTED_OBJECTS: usize = 4_096;
#[cfg(test)]
pub(crate) const MAX_PERSISTED_OBJECTS: usize = 32;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) enum OperationStatus {
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl OperationStatus {
    pub(crate) fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct OperationProblem {
    #[serde(rename = "type")]
    pub(crate) problem_type: String,
    pub(crate) title: String,
    pub(crate) status: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) instance: Option<String>,
    pub(crate) code: String,
    pub(crate) request_id: String,
    pub(crate) retryable: bool,
}

impl OperationProblem {
    pub(crate) fn new(
        operation_id: &str,
        origin_request_id: &str,
        title: impl Into<String>,
        status: u16,
        code: impl Into<String>,
        detail: impl Into<String>,
        retryable: bool,
    ) -> Self {
        Self {
            problem_type: "about:blank".to_owned(),
            title: title.into(),
            status,
            detail: Some(detail.into()),
            instance: Some(format!("/api/v1/operations/{operation_id}")),
            code: code.into(),
            request_id: origin_request_id.to_owned(),
            retryable,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Operation {
    pub(crate) id: String,
    pub(crate) kind: String,
    pub(crate) status: OperationStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<OperationProblem>,
    pub(crate) created_at: String,
    pub(crate) updated_at: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OperationCreate {
    pub(crate) operation: Operation,
    pub(crate) created: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredOperation {
    operation: Operation,
    fingerprint: String,
    idempotency_scope: String,
    origin_request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    initial_idempotency_key_digest: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RecoverableOperation {
    pub(crate) operation: Operation,
    pub(crate) idempotency_scope: String,
    pub(crate) origin_request_id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct IdempotencyRecord {
    fingerprint: String,
    operation_id: String,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub(crate) enum OperationError {
    #[error("operation ID is invalid")]
    InvalidId,
    #[error("idempotency key is invalid")]
    InvalidIdempotencyKey,
    #[error("operation was not found")]
    NotFound,
    #[error("idempotency key was reused with a different request")]
    IdempotencyConflict,
    #[error("operation state transition conflicts with its current state")]
    TransitionConflict,
    #[error("operation data is invalid")]
    DataInvalid,
    #[error("operation storage is unavailable")]
    StorageUnavailable,
}

#[derive(Clone)]
pub(crate) struct OperationStore {
    root: Option<Arc<Dir>>,
    process_lock: Arc<Mutex<()>>,
    #[cfg(test)]
    fail_next_initial_sidecar_write: Arc<AtomicBool>,
}

struct OperationFs {
    operations: Dir,
    idempotency: Dir,
}

impl OperationStore {
    pub(crate) fn new(hermes_home: impl Into<PathBuf>) -> Self {
        let hermes_home = hermes_home.into();
        Self {
            root: open_operation_root(&hermes_home).ok().map(Arc::new),
            process_lock: Arc::new(Mutex::new(())),
            #[cfg(test)]
            fail_next_initial_sidecar_write: Arc::new(AtomicBool::new(false)),
        }
    }

    pub(crate) fn create_idempotent(
        &self,
        kind: &str,
        fingerprint: &str,
        idempotency_scope: &str,
        idempotency_key: &str,
        origin_request_id: &str,
    ) -> Result<OperationCreate, OperationError> {
        validate_kind(kind)?;
        validate_fingerprint(fingerprint)?;
        validate_idempotency_scope(idempotency_scope)?;
        validate_idempotency_key(idempotency_key)?;
        validate_origin_request_id(origin_request_id)?;
        let key_digest = idempotency_digest(idempotency_scope, idempotency_key);
        self.with_lock(|store| {
            if let Some(operation) = self.replay_or_active_locked(
                store,
                kind,
                fingerprint,
                idempotency_scope,
                &key_digest,
                true,
            )? {
                return Ok(OperationCreate {
                    operation,
                    created: false,
                });
            }

            self.enforce_capacity_locked(store, 2)?;
            let stored = StoredOperation {
                operation: new_operation(kind)?,
                fingerprint: fingerprint.to_owned(),
                idempotency_scope: idempotency_scope.to_owned(),
                origin_request_id: origin_request_id.to_owned(),
                initial_idempotency_key_digest: Some(key_digest.clone()),
            };
            self.write_operation_locked(store, &stored)?;
            let record = IdempotencyRecord {
                fingerprint: fingerprint.to_owned(),
                operation_id: stored.operation.id.clone(),
            };
            #[cfg(test)]
            let sidecar_result = if self
                .fail_next_initial_sidecar_write
                .swap(false, Ordering::SeqCst)
            {
                Err(OperationError::StorageUnavailable)
            } else {
                self.write_idempotency_locked(store, &key_digest, &record)
            };
            #[cfg(not(test))]
            let sidecar_result = self.write_idempotency_locked(store, &key_digest, &record);
            if let Err(error) = sidecar_result {
                tracing::warn!(
                    ?error,
                    operation_id = %stored.operation.id,
                    "initial Operation idempotency index is unavailable; using the durable Operation binding"
                );
            }
            Ok(OperationCreate {
                operation: stored.operation,
                created: true,
            })
        })
    }

    /// Finds an exact idempotency replay or binds a new key to an active request with the same
    /// kind and semantic fingerprint. It never creates a new Operation.
    pub(crate) fn replay_or_active(
        &self,
        kind: &str,
        fingerprint: &str,
        idempotency_scope: &str,
        idempotency_key: &str,
    ) -> Result<Option<Operation>, OperationError> {
        validate_kind(kind)?;
        validate_fingerprint(fingerprint)?;
        validate_idempotency_scope(idempotency_scope)?;
        validate_idempotency_key(idempotency_key)?;
        let key_digest = idempotency_digest(idempotency_scope, idempotency_key);
        self.with_lock(|store| {
            self.replay_or_active_locked(
                store,
                kind,
                fingerprint,
                idempotency_scope,
                &key_digest,
                true,
            )
        })
    }

    pub(crate) fn get(&self, operation_id: &str) -> Result<Operation, OperationError> {
        validate_operation_id(operation_id)?;
        self.with_lock(|store| {
            self.read_locked(store, operation_id)
                .map(|value| value.operation)
        })
    }

    pub(crate) fn probe(&self) -> Result<(), OperationError> {
        self.with_lock(|store| {
            probe_directory(&store.operations)?;
            probe_directory(&store.idempotency)?;
            self.enforce_capacity_locked(store, 0)?;
            let _ = now_rfc3339()?;
            Ok(())
        })
    }

    #[cfg(test)]
    fn fail_next_initial_sidecar_write_for_test(&self) {
        self.fail_next_initial_sidecar_write
            .store(true, Ordering::SeqCst);
    }

    pub(crate) fn mark_running(&self, operation_id: &str) -> Result<Operation, OperationError> {
        self.transition(operation_id, |operation| {
            if operation.status != OperationStatus::Queued {
                return Err(OperationError::TransitionConflict);
            }
            operation.status = OperationStatus::Running;
            operation.error = None;
            Ok(())
        })
    }

    pub(crate) fn complete(&self, operation_id: &str) -> Result<Operation, OperationError> {
        self.transition(operation_id, |operation| {
            if operation.status != OperationStatus::Running {
                return Err(OperationError::TransitionConflict);
            }
            operation.status = OperationStatus::Completed;
            operation.error = None;
            Ok(())
        })
    }

    pub(crate) fn fail(
        &self,
        operation_id: &str,
        problem: OperationProblem,
    ) -> Result<Operation, OperationError> {
        validate_operation_id(operation_id)?;
        self.with_lock(|store| {
            let mut stored = self.read_locked(store, operation_id)?;
            validate_problem(&problem, operation_id, &stored.origin_request_id)?;
            if stored.operation.status.is_terminal() {
                return Err(OperationError::TransitionConflict);
            }
            stored.operation.status = OperationStatus::Failed;
            stored.operation.error = Some(problem);
            stored.operation.updated_at = now_rfc3339()?;
            self.write_operation_locked(store, &stored)?;
            Ok(stored.operation)
        })
    }

    /// Returns a stable snapshot for lifecycle-specific crash reconciliation. No state is changed.
    pub(crate) fn list(&self) -> Result<Vec<RecoverableOperation>, OperationError> {
        self.with_lock(|store| {
            let mut operations = self
                .read_all_locked(store)?
                .into_iter()
                .map(|stored| RecoverableOperation {
                    operation: stored.operation,
                    idempotency_scope: stored.idempotency_scope,
                    origin_request_id: stored.origin_request_id,
                })
                .collect::<Vec<_>>();
            operations.sort_by(|left, right| {
                left.operation
                    .created_at
                    .cmp(&right.operation.created_at)
                    .then(left.operation.id.cmp(&right.operation.id))
            });
            Ok(operations)
        })
    }

    /// Completes a queued or running Operation after durable domain state proves it committed.
    /// Replaying the same reconciliation is idempotent; other terminal states remain immutable.
    pub(crate) fn reconcile_complete(
        &self,
        operation_id: &str,
    ) -> Result<Operation, OperationError> {
        validate_operation_id(operation_id)?;
        self.with_lock(|store| {
            let mut stored = self.read_locked(store, operation_id)?;
            match stored.operation.status {
                OperationStatus::Queued | OperationStatus::Running => {
                    stored.operation.status = OperationStatus::Completed;
                    stored.operation.error = None;
                    stored.operation.updated_at = now_rfc3339()?;
                    self.write_operation_locked(store, &stored)?;
                    Ok(stored.operation)
                }
                OperationStatus::Completed => Ok(stored.operation),
                OperationStatus::Failed | OperationStatus::Cancelled => {
                    Err(OperationError::TransitionConflict)
                }
            }
        })
    }

    /// Fails a queued or running Operation after domain recovery proves it did not commit.
    /// Replaying an identical terminal failure is idempotent.
    pub(crate) fn reconcile_fail(
        &self,
        operation_id: &str,
        problem: OperationProblem,
    ) -> Result<Operation, OperationError> {
        validate_operation_id(operation_id)?;
        self.with_lock(|store| {
            let mut stored = self.read_locked(store, operation_id)?;
            validate_problem(&problem, operation_id, &stored.origin_request_id)?;
            match stored.operation.status {
                OperationStatus::Queued | OperationStatus::Running => {
                    stored.operation.status = OperationStatus::Failed;
                    stored.operation.error = Some(problem.clone());
                    stored.operation.updated_at = now_rfc3339()?;
                    self.write_operation_locked(store, &stored)?;
                    Ok(stored.operation)
                }
                OperationStatus::Failed if stored.operation.error.as_ref() == Some(&problem) => {
                    Ok(stored.operation)
                }
                OperationStatus::Completed
                | OperationStatus::Failed
                | OperationStatus::Cancelled => Err(OperationError::TransitionConflict),
            }
        })
    }

    fn replay_or_active_locked(
        &self,
        store: &OperationFs,
        kind: &str,
        fingerprint: &str,
        idempotency_scope: &str,
        key_digest: &str,
        bind_active: bool,
    ) -> Result<Option<Operation>, OperationError> {
        if let Some(record) = self.read_idempotency_locked(store, key_digest)? {
            if record.fingerprint != fingerprint {
                return Err(OperationError::IdempotencyConflict);
            }
            let stored =
                self.read_locked(store, &record.operation_id)
                    .map_err(|error| match error {
                        OperationError::NotFound => OperationError::DataInvalid,
                        error => error,
                    })?;
            if stored.operation.kind != kind || stored.idempotency_scope != idempotency_scope {
                return Err(OperationError::IdempotencyConflict);
            }
            return Ok(Some(stored.operation));
        }

        let stored_operations = self.read_all_locked(store)?;
        let mut initial_matches = stored_operations
            .iter()
            .filter(|stored| stored.initial_idempotency_key_digest.as_deref() == Some(key_digest));
        if let Some(stored) = initial_matches.next() {
            if initial_matches.next().is_some() {
                return Err(OperationError::DataInvalid);
            }
            if stored.fingerprint != fingerprint
                || stored.operation.kind != kind
                || stored.idempotency_scope != idempotency_scope
            {
                return Err(OperationError::IdempotencyConflict);
            }
            return Ok(Some(stored.operation.clone()));
        }

        let mut active = stored_operations.into_iter().filter(|stored| {
            stored.operation.kind == kind
                && stored.fingerprint == fingerprint
                && stored.idempotency_scope == idempotency_scope
                && !stored.operation.status.is_terminal()
        });
        let Some(stored) = active.next() else {
            return Ok(None);
        };
        if active.next().is_some() {
            return Err(OperationError::DataInvalid);
        }
        if bind_active {
            self.enforce_capacity_locked(store, 1)?;
            self.write_idempotency_locked(
                store,
                key_digest,
                &IdempotencyRecord {
                    fingerprint: fingerprint.to_owned(),
                    operation_id: stored.operation.id.clone(),
                },
            )?;
        }
        Ok(Some(stored.operation))
    }

    fn read_all_locked(&self, store: &OperationFs) -> Result<Vec<StoredOperation>, OperationError> {
        let mut stored = Vec::new();
        for entry in store
            .operations
            .entries()
            .map_err(|_| OperationError::StorageUnavailable)?
        {
            let entry = entry.map_err(|_| OperationError::StorageUnavailable)?;
            let file_name = entry.file_name();
            let Some(file_name) = file_name.to_str() else {
                continue;
            };
            let Some(operation_id) = file_name.strip_suffix(".json") else {
                continue;
            };
            if validate_operation_id(operation_id).is_err() {
                continue;
            }
            stored.push(self.read_locked(store, operation_id)?);
        }
        Ok(stored)
    }

    fn enforce_capacity_locked(
        &self,
        store: &OperationFs,
        additional: usize,
    ) -> Result<(), OperationError> {
        if additional > MAX_PERSISTED_OBJECTS {
            return Err(OperationError::StorageUnavailable);
        }
        let mut count = self.persisted_object_count_locked(store)?;
        if count.saturating_add(additional) <= MAX_PERSISTED_OBJECTS {
            return Ok(());
        }

        let now = OffsetDateTime::now_utc();
        let mut candidates = self
            .read_all_locked(store)?
            .into_iter()
            .filter_map(|stored| {
                let updated = OffsetDateTime::parse(&stored.operation.updated_at, &Rfc3339).ok()?;
                (stored.operation.status.is_terminal()
                    && now
                        .unix_timestamp()
                        .saturating_sub(updated.unix_timestamp())
                        >= MIN_TERMINAL_RETENTION_SECONDS)
                    .then_some((updated, stored.operation.id))
            })
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| left.0.cmp(&right.0).then(left.1.cmp(&right.1)));

        for (_, operation_id) in candidates {
            self.remove_idempotency_for_operation_locked(store, &operation_id)?;
            store
                .operations
                .remove_file(operation_file_name(&operation_id)?)
                .map_err(|_| OperationError::StorageUnavailable)?;
            sync_directory(&store.operations)?;
            count = self.persisted_object_count_locked(store)?;
            if count.saturating_add(additional) <= MAX_PERSISTED_OBJECTS {
                return Ok(());
            }
        }

        Err(OperationError::StorageUnavailable)
    }

    fn persisted_object_count_locked(&self, store: &OperationFs) -> Result<usize, OperationError> {
        let mut operation_objects = 0_usize;
        for entry in store
            .operations
            .entries()
            .map_err(|_| OperationError::StorageUnavailable)?
        {
            let entry = entry.map_err(|_| OperationError::StorageUnavailable)?;
            if entry.file_name() != "idempotency" {
                operation_objects = operation_objects
                    .checked_add(1)
                    .ok_or(OperationError::StorageUnavailable)?;
            }
        }
        let mut idempotency_objects = 0_usize;
        for entry in store
            .idempotency
            .entries()
            .map_err(|_| OperationError::StorageUnavailable)?
        {
            let _ = entry.map_err(|_| OperationError::StorageUnavailable)?;
            idempotency_objects = idempotency_objects
                .checked_add(1)
                .ok_or(OperationError::StorageUnavailable)?;
        }
        operation_objects
            .checked_add(idempotency_objects)
            .ok_or(OperationError::StorageUnavailable)
    }

    fn remove_idempotency_for_operation_locked(
        &self,
        store: &OperationFs,
        operation_id: &str,
    ) -> Result<(), OperationError> {
        let mut removed = false;
        for entry in store
            .idempotency
            .entries()
            .map_err(|_| OperationError::StorageUnavailable)?
        {
            let entry = entry.map_err(|_| OperationError::StorageUnavailable)?;
            let file_name = entry.file_name();
            let Some(file_name) = file_name.to_str() else {
                continue;
            };
            let Some(key_digest) = file_name.strip_suffix(".json") else {
                continue;
            };
            if validate_fingerprint(key_digest).is_err() {
                continue;
            }
            let record = self
                .read_idempotency_locked(store, key_digest)?
                .ok_or(OperationError::DataInvalid)?;
            if record.operation_id == operation_id {
                store
                    .idempotency
                    .remove_file(file_name)
                    .map_err(|_| OperationError::StorageUnavailable)?;
                removed = true;
            }
        }
        if removed {
            sync_directory(&store.idempotency)?;
        }
        Ok(())
    }

    fn transition(
        &self,
        operation_id: &str,
        update: impl FnOnce(&mut Operation) -> Result<(), OperationError>,
    ) -> Result<Operation, OperationError> {
        validate_operation_id(operation_id)?;
        self.with_lock(|store| {
            let mut stored = self.read_locked(store, operation_id)?;
            update(&mut stored.operation)?;
            stored.operation.updated_at = now_rfc3339()?;
            self.write_operation_locked(store, &stored)?;
            Ok(stored.operation)
        })
    }

    fn with_lock<T>(
        &self,
        operation: impl FnOnce(&OperationFs) -> Result<T, OperationError>,
    ) -> Result<T, OperationError> {
        let _process_guard = self
            .process_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let root = self
            .root
            .as_deref()
            .ok_or(OperationError::StorageUnavailable)?;
        let mut options = CapOpenOptions::new();
        options.create(true).truncate(false).read(true).write(true);
        options.follow(FollowSymlinks::No);
        let lock_file = root
            .open_with("operations.lock", &options)
            .map_err(|_| OperationError::StorageUnavailable)?;
        let lock_file = lock_file.into_std();
        lock_file
            .lock_exclusive()
            .map_err(|_| OperationError::StorageUnavailable)?;
        let result = open_operation_store(root).and_then(|store| operation(&store));
        let unlock = FileExt::unlock(&lock_file).map_err(|_| OperationError::StorageUnavailable);
        match (result, unlock) {
            (Ok(value), Ok(())) => Ok(value),
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
        }
    }

    fn read_locked(
        &self,
        store: &OperationFs,
        operation_id: &str,
    ) -> Result<StoredOperation, OperationError> {
        let file_name = operation_file_name(operation_id)?;
        let bytes = read_bounded(&store.operations, &file_name, MAX_OPERATION_BYTES)?;
        let stored: StoredOperation =
            serde_json::from_slice(&bytes).map_err(|_| OperationError::DataInvalid)?;
        if validate_operation(
            &stored.operation,
            Some(operation_id),
            &stored.origin_request_id,
        )
        .is_err()
            || validate_fingerprint(&stored.fingerprint).is_err()
            || validate_idempotency_scope(&stored.idempotency_scope).is_err()
            || stored
                .initial_idempotency_key_digest
                .as_deref()
                .is_some_and(|digest| validate_fingerprint(digest).is_err())
        {
            return Err(OperationError::DataInvalid);
        }
        Ok(stored)
    }

    fn write_operation_locked(
        &self,
        store: &OperationFs,
        operation: &StoredOperation,
    ) -> Result<(), OperationError> {
        validate_operation(&operation.operation, None, &operation.origin_request_id)?;
        validate_fingerprint(&operation.fingerprint)?;
        validate_idempotency_scope(&operation.idempotency_scope)?;
        if let Some(digest) = operation.initial_idempotency_key_digest.as_deref() {
            validate_fingerprint(digest)?;
        }
        let file_name = operation_file_name(&operation.operation.id)?;
        atomic_write_json(
            &store.operations,
            &file_name,
            operation,
            MAX_OPERATION_BYTES,
        )
    }

    fn read_idempotency_locked(
        &self,
        store: &OperationFs,
        key_digest: &str,
    ) -> Result<Option<IdempotencyRecord>, OperationError> {
        validate_fingerprint(key_digest)?;
        let file_name = format!("{key_digest}.json");
        let bytes =
            match read_optional_bounded(&store.idempotency, &file_name, MAX_IDEMPOTENCY_BYTES)? {
                Some(bytes) => bytes,
                None => return Ok(None),
            };
        let record: IdempotencyRecord =
            serde_json::from_slice(&bytes).map_err(|_| OperationError::DataInvalid)?;
        validate_fingerprint(&record.fingerprint)?;
        validate_operation_id(&record.operation_id)?;
        Ok(Some(record))
    }

    fn write_idempotency_locked(
        &self,
        store: &OperationFs,
        key_digest: &str,
        record: &IdempotencyRecord,
    ) -> Result<(), OperationError> {
        validate_fingerprint(key_digest)?;
        atomic_write_json(
            &store.idempotency,
            &format!("{key_digest}.json"),
            record,
            MAX_IDEMPOTENCY_BYTES,
        )
    }
}

fn new_operation(kind: &str) -> Result<Operation, OperationError> {
    let timestamp = now_rfc3339()?;
    Ok(Operation {
        id: format!("op_{}", Uuid::new_v4().simple()),
        kind: kind.to_owned(),
        status: OperationStatus::Queued,
        error: None,
        created_at: timestamp.clone(),
        updated_at: timestamp,
    })
}

fn validate_operation(
    operation: &Operation,
    expected_id: Option<&str>,
    origin_request_id: &str,
) -> Result<(), OperationError> {
    validate_operation_id(&operation.id).map_err(|_| OperationError::DataInvalid)?;
    if expected_id.is_some_and(|expected| expected != operation.id) {
        return Err(OperationError::DataInvalid);
    }
    validate_kind(&operation.kind)?;
    let created = OffsetDateTime::parse(&operation.created_at, &Rfc3339)
        .map_err(|_| OperationError::DataInvalid)?;
    let updated = OffsetDateTime::parse(&operation.updated_at, &Rfc3339)
        .map_err(|_| OperationError::DataInvalid)?;
    if updated < created {
        return Err(OperationError::DataInvalid);
    }
    match operation.status {
        OperationStatus::Failed => {
            let problem = operation
                .error
                .as_ref()
                .ok_or(OperationError::DataInvalid)?;
            validate_problem(problem, &operation.id, origin_request_id)?;
        }
        _ if operation.error.is_some() => return Err(OperationError::DataInvalid),
        _ => {}
    }
    Ok(())
}

fn validate_problem(
    problem: &OperationProblem,
    operation_id: &str,
    origin_request_id: &str,
) -> Result<(), OperationError> {
    if problem.problem_type != "about:blank"
        || !(400..=599).contains(&problem.status)
        || validate_origin_request_id(origin_request_id).is_err()
        || problem.request_id != origin_request_id
        || problem.instance.as_deref()
            != Some(format!("/api/v1/operations/{operation_id}").as_str())
        || !valid_bounded_text(&problem.title, 1, 160)
        || !valid_problem_code(&problem.code)
        || problem
            .detail
            .as_deref()
            .is_some_and(|detail| !valid_bounded_text(detail, 1, 2_048))
    {
        return Err(OperationError::DataInvalid);
    }
    Ok(())
}

pub(crate) fn valid_origin_request_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b':' | b'-'))
}

fn validate_origin_request_id(value: &str) -> Result<(), OperationError> {
    if valid_origin_request_id(value) {
        Ok(())
    } else {
        Err(OperationError::DataInvalid)
    }
}

fn valid_bounded_text(value: &str, minimum: usize, maximum: usize) -> bool {
    let length = value.chars().count();
    (minimum..=maximum).contains(&length) && !value.chars().any(char::is_control)
}

fn valid_problem_code(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.as_bytes()[0].is_ascii_lowercase()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

fn now_rfc3339() -> Result<String, OperationError> {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|_| OperationError::StorageUnavailable)
}

fn validate_operation_id(value: &str) -> Result<(), OperationError> {
    if value.len() == 35
        && value.starts_with("op_")
        && value[3..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(OperationError::InvalidId)
    }
}

fn validate_kind(value: &str) -> Result<(), OperationError> {
    if !value.is_empty()
        && value.len() <= 64
        && value.bytes().all(|byte| byte.is_ascii_alphanumeric())
    {
        Ok(())
    } else {
        Err(OperationError::DataInvalid)
    }
}

fn validate_idempotency_key(value: &str) -> Result<(), OperationError> {
    if (MIN_IDEMPOTENCY_KEY_BYTES..=MAX_IDEMPOTENCY_KEY_BYTES).contains(&value.len())
        && value.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
    {
        Ok(())
    } else {
        Err(OperationError::InvalidIdempotencyKey)
    }
}

fn validate_idempotency_scope(value: &str) -> Result<(), OperationError> {
    let valid = value.len() <= 2_048
        && value.split_once(' ').is_some_and(|(method, path)| {
            !method.is_empty()
                && method.len() <= 16
                && method.bytes().all(|byte| byte.is_ascii_uppercase())
                && path.starts_with("/api/v1/")
                && path.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
        });
    if valid {
        Ok(())
    } else {
        Err(OperationError::DataInvalid)
    }
}

fn idempotency_digest(scope: &str, key: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"synthchat-idempotency-v1\0");
    for value in [scope, key] {
        digest.update((value.len() as u64).to_be_bytes());
        digest.update(value.as_bytes());
    }
    let digest = digest.finalize();
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

fn validate_fingerprint(value: &str) -> Result<(), OperationError> {
    if value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(OperationError::DataInvalid)
    }
}

fn open_operation_root(hermes_home: &Path) -> Result<Dir, OperationError> {
    let home = open_ambient_directory_nofollow(hermes_home)?;
    open_or_create_directory(&home, ".synthchat")
}

fn open_operation_store(root: &Dir) -> Result<OperationFs, OperationError> {
    let operations = open_or_create_directory(root, "operations")?;
    let idempotency = open_or_create_directory(&operations, "idempotency")?;
    recover_directory_transactions(&operations, valid_operation_file_name)?;
    recover_directory_transactions(&idempotency, valid_idempotency_file_name)?;
    Ok(OperationFs {
        operations,
        idempotency,
    })
}

fn operation_file_name(operation_id: &str) -> Result<String, OperationError> {
    validate_operation_id(operation_id)?;
    Ok(format!("{operation_id}.json"))
}

fn valid_operation_file_name(name: &str) -> bool {
    name.strip_suffix(".json")
        .is_some_and(|operation_id| validate_operation_id(operation_id).is_ok())
}

fn valid_idempotency_file_name(name: &str) -> bool {
    name.strip_suffix(".json")
        .is_some_and(|digest| validate_fingerprint(digest).is_ok())
}

fn open_ambient_directory_nofollow(path: &Path) -> Result<Dir, OperationError> {
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()
            .map_err(|_| OperationError::StorageUnavailable)?
            .join(path)
    };
    let Some(parent) = absolute.parent() else {
        return Dir::open_ambient_dir(&absolute, ambient_authority())
            .map_err(|_| OperationError::StorageUnavailable);
    };
    let Some(name) = absolute.file_name() else {
        return Dir::open_ambient_dir(&absolute, ambient_authority())
            .map_err(|_| OperationError::StorageUnavailable);
    };
    let canonical_parent =
        fs::canonicalize(parent).map_err(|_| OperationError::StorageUnavailable)?;
    let parent = Dir::open_ambient_dir(canonical_parent, ambient_authority())
        .map_err(|_| OperationError::StorageUnavailable)?;
    open_or_create_directory(&parent, name)
}

fn open_or_create_directory(parent: &Dir, name: impl AsRef<Path>) -> Result<Dir, OperationError> {
    let name = name.as_ref();
    match parent.open_dir_nofollow(name) {
        Ok(directory) => return Ok(directory),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(_) => return Err(OperationError::StorageUnavailable),
    }
    match parent.create_dir(name) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(_) => return Err(OperationError::StorageUnavailable),
    }
    parent
        .open_dir_nofollow(name)
        .map_err(|_| OperationError::StorageUnavailable)
}

fn read_bounded(directory: &Dir, name: &str, maximum: u64) -> Result<Vec<u8>, OperationError> {
    read_optional_bounded(directory, name, maximum)?.ok_or(OperationError::NotFound)
}

fn read_optional_bounded(
    directory: &Dir,
    name: &str,
    maximum: u64,
) -> Result<Option<Vec<u8>>, OperationError> {
    let metadata = match directory.symlink_metadata(name) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(OperationError::StorageUnavailable),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > maximum {
        return Err(OperationError::DataInvalid);
    }

    let mut options = CapOpenOptions::new();
    options.read(true);
    options.follow(FollowSymlinks::No);
    let file = directory
        .open_with(name, &options)
        .map_err(|_| OperationError::StorageUnavailable)?;
    let metadata = file
        .metadata()
        .map_err(|_| OperationError::StorageUnavailable)?;
    if !metadata.is_file() || metadata.len() > maximum {
        return Err(OperationError::DataInvalid);
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(maximum + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| OperationError::StorageUnavailable)?;
    if bytes.len() as u64 > maximum {
        return Err(OperationError::DataInvalid);
    }
    Ok(Some(bytes))
}

fn probe_directory(directory: &Dir) -> Result<(), OperationError> {
    let name = format!(".operation-probe-{}", Uuid::new_v4().simple());
    let mut options = CapOpenOptions::new();
    options.write(true).create_new(true);
    options.follow(FollowSymlinks::No);
    let mut probe = directory
        .open_with(&name, &options)
        .map_err(|_| OperationError::StorageUnavailable)?;
    let result = probe
        .write_all(b"synthchat-operation-store-probe")
        .and_then(|_| probe.flush())
        .and_then(|_| probe.sync_all())
        .map_err(|_| OperationError::StorageUnavailable);
    drop(probe);
    if result.is_err() {
        let _ = directory.remove_file(&name);
        return result;
    }
    directory
        .remove_file(&name)
        .map_err(|_| OperationError::StorageUnavailable)?;
    sync_directory(directory)
}

fn atomic_write_json(
    directory: &Dir,
    name: &str,
    value: &impl Serialize,
    maximum: u64,
) -> Result<(), OperationError> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|_| OperationError::DataInvalid)?;
    if bytes.len() as u64 > maximum {
        return Err(OperationError::DataInvalid);
    }
    recover_atomic_target(directory, name)?;
    let temporary_name = transaction_name(".operation-write-", name);
    let mut options = CapOpenOptions::new();
    options.write(true).create_new(true);
    options.follow(FollowSymlinks::No);
    let mut temporary = directory
        .open_with(&temporary_name, &options)
        .map_err(|_| OperationError::StorageUnavailable)?;
    let write_result = temporary
        .write_all(&bytes)
        .and_then(|_| temporary.flush())
        .and_then(|_| temporary.sync_all())
        .map_err(|_| OperationError::StorageUnavailable);
    drop(temporary);
    if let Err(error) = write_result {
        let _ = directory.remove_file(&temporary_name);
        return Err(error);
    }

    let commit_result = replace_atomic_target(directory, name, &temporary_name);
    if commit_result.is_err() {
        let _ = remove_regular_file_if_exists(directory, &temporary_name);
    }
    commit_result?;
    let persisted = read_bounded(directory, name, maximum)?;
    if persisted != bytes {
        return Err(OperationError::DataInvalid);
    }
    Ok(())
}

fn transaction_name(prefix: &str, target: &str) -> String {
    format!("{prefix}{target}")
}

fn recover_directory_transactions(
    directory: &Dir,
    valid_target: impl Fn(&str) -> bool,
) -> Result<(), OperationError> {
    let mut targets = BTreeSet::new();
    for entry in directory
        .entries()
        .map_err(|_| OperationError::StorageUnavailable)?
    {
        let name = entry
            .map_err(|_| OperationError::StorageUnavailable)?
            .file_name()
            .into_string()
            .map_err(|_| OperationError::StorageUnavailable)?;
        let target = name
            .strip_prefix(".operation-write-")
            .or_else(|| name.strip_prefix(".operation-backup-"));
        let Some(target) = target else {
            continue;
        };
        if !valid_target(target) {
            return Err(OperationError::StorageUnavailable);
        }
        targets.insert(target.to_owned());
    }
    for target in targets {
        recover_atomic_target(directory, &target)?;
    }
    Ok(())
}

fn recover_atomic_target(directory: &Dir, target: &str) -> Result<(), OperationError> {
    let temporary = transaction_name(".operation-write-", target);
    let backup = transaction_name(".operation-backup-", target);
    let target_exists = regular_file_exists(directory, target)?;
    let backup_exists = regular_file_exists(directory, &backup)?;
    let mut changed = false;

    if target_exists {
        if backup_exists {
            directory
                .remove_file(&backup)
                .map_err(|_| OperationError::StorageUnavailable)?;
            changed = true;
        }
    } else if backup_exists {
        directory
            .rename(&backup, directory, target)
            .map_err(|_| OperationError::StorageUnavailable)?;
        changed = true;
    }
    if remove_regular_file_if_exists(directory, &temporary)? {
        changed = true;
    }
    if changed {
        sync_directory(directory)?;
    }
    Ok(())
}

fn regular_file_exists(directory: &Dir, name: &str) -> Result<bool, OperationError> {
    match directory.symlink_metadata(name) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            Err(OperationError::StorageUnavailable)
        }
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(OperationError::StorageUnavailable),
    }
}

fn remove_regular_file_if_exists(directory: &Dir, name: &str) -> Result<bool, OperationError> {
    if !regular_file_exists(directory, name)? {
        return Ok(false);
    }
    directory
        .remove_file(name)
        .map_err(|_| OperationError::StorageUnavailable)?;
    Ok(true)
}

#[cfg(not(windows))]
fn replace_atomic_target(
    directory: &Dir,
    target: &str,
    temporary: &str,
) -> Result<(), OperationError> {
    directory
        .rename(temporary, directory, target)
        .map_err(|_| OperationError::StorageUnavailable)?;
    sync_directory(directory)
}

#[cfg(windows)]
fn replace_atomic_target(
    directory: &Dir,
    target: &str,
    temporary: &str,
) -> Result<(), OperationError> {
    let backup = transaction_name(".operation-backup-", target);
    if regular_file_exists(directory, target)? {
        directory
            .rename(target, directory, &backup)
            .map_err(|_| OperationError::StorageUnavailable)?;
        if directory.rename(temporary, directory, target).is_err() {
            let _ = directory.rename(&backup, directory, target);
            return Err(OperationError::StorageUnavailable);
        }
        sync_directory(directory)?;
        directory
            .remove_file(&backup)
            .map_err(|_| OperationError::StorageUnavailable)?;
    } else {
        directory
            .rename(temporary, directory, target)
            .map_err(|_| OperationError::StorageUnavailable)?;
    }
    sync_directory(directory)
}

fn sync_directory(directory: &Dir) -> Result<(), OperationError> {
    #[cfg(unix)]
    {
        directory
            .try_clone()
            .and_then(|directory| directory.into_std_file().sync_all())
            .map_err(|_| OperationError::StorageUnavailable)
    }
    #[cfg(not(unix))]
    {
        let _ = directory;
        Ok(())
    }
}

#[cfg(test)]
fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    const INSTALL_SCOPE: &str = "POST /api/v1/profiles/default/skills/install";
    const UNINSTALL_SCOPE: &str =
        "DELETE /api/v1/profiles/default/skills/skill_0123456789abcdef0123456789abcdef";

    fn fingerprint(value: &str) -> String {
        sha256_hex(value.as_bytes())
    }

    #[test]
    fn operation_transitions_and_idempotent_replay_are_durable() {
        let home = tempfile::tempdir().unwrap();
        let store = OperationStore::new(home.path());
        let created = store
            .create_idempotent(
                "skillInstall",
                &fingerprint("request-a"),
                INSTALL_SCOPE,
                "idem-key-a",
                "request-first",
            )
            .unwrap();
        assert!(created.created);
        let first = created.operation;
        let replay = store
            .create_idempotent(
                "skillInstall",
                &fingerprint("request-a"),
                INSTALL_SCOPE,
                "idem-key-a",
                "request-replay",
            )
            .unwrap();
        assert!(!replay.created);
        assert_eq!(replay.operation.id, first.id);
        assert_eq!(
            store.mark_running(&first.id).unwrap().status,
            OperationStatus::Running
        );
        assert_eq!(
            store.complete(&first.id).unwrap().status,
            OperationStatus::Completed
        );
        let operation_path = home
            .path()
            .join(".synthchat/operations")
            .join(format!("{}.json", first.id));
        let completed_bytes = fs::read(&operation_path).unwrap();
        store.reconcile_complete(&first.id).unwrap();
        assert_eq!(fs::read(&operation_path).unwrap(), completed_bytes);

        let reopened = OperationStore::new(home.path());
        assert_eq!(
            reopened.get(&first.id).unwrap().status,
            OperationStatus::Completed
        );
        assert_eq!(
            reopened.create_idempotent(
                "skillInstall",
                &fingerprint("request-b"),
                INSTALL_SCOPE,
                "idem-key-a",
                "request-conflict",
            ),
            Err(OperationError::IdempotencyConflict),
        );
    }

    #[test]
    fn idempotency_keys_are_scoped_to_method_and_canonical_path() {
        let home = tempfile::tempdir().unwrap();
        let store = OperationStore::new(home.path());
        let shared_key = "shared-across-paths";
        let first = store
            .create_idempotent(
                "skillInstall",
                &fingerprint("profile-a-body"),
                "POST /api/v1/profiles/profile-a/skills/install",
                shared_key,
                "request-profile-a",
            )
            .unwrap();
        let second = store
            .create_idempotent(
                "skillInstall",
                &fingerprint("profile-b-body"),
                "POST /api/v1/profiles/profile-b/skills/install",
                shared_key,
                "request-profile-b",
            )
            .unwrap();
        assert_ne!(first.operation.id, second.operation.id);
        assert_eq!(
            store.create_idempotent(
                "skillInstall",
                &fingerprint("different-profile-a-body"),
                "POST /api/v1/profiles/profile-a/skills/install",
                shared_key,
                "request-profile-a-conflict",
            ),
            Err(OperationError::IdempotencyConflict)
        );
    }

    #[test]
    fn durable_initial_key_binding_survives_a_missing_sidecar() {
        let home = tempfile::tempdir().unwrap();
        let store = OperationStore::new(home.path());
        store.fail_next_initial_sidecar_write_for_test();
        let request = fingerprint("sidecar-failure-request");
        let key = "sidecar-failure-key";
        let created = store
            .create_idempotent(
                "skillInstall",
                &request,
                INSTALL_SCOPE,
                key,
                "request-sidecar-failure",
            )
            .unwrap();
        assert!(created.created);
        let operation_id = created.operation.id;
        let sidecar = home
            .path()
            .join(".synthchat/operations/idempotency")
            .join(format!("{}.json", idempotency_digest(INSTALL_SCOPE, key)));
        assert!(!sidecar.exists());

        store.mark_running(&operation_id).unwrap();
        store.complete(&operation_id).unwrap();
        let reopened = OperationStore::new(home.path());
        let replay = reopened
            .create_idempotent(
                "skillInstall",
                &request,
                INSTALL_SCOPE,
                key,
                "request-sidecar-replay",
            )
            .unwrap();
        assert!(!replay.created);
        assert_eq!(replay.operation.id, operation_id);
        assert_eq!(replay.operation.status, OperationStatus::Completed);
        assert_eq!(
            reopened.create_idempotent(
                "skillInstall",
                &fingerprint("different-after-sidecar-failure"),
                INSTALL_SCOPE,
                key,
                "request-sidecar-conflict",
            ),
            Err(OperationError::IdempotencyConflict)
        );
    }

    #[test]
    fn lifecycle_can_list_and_reconcile_interrupted_operations() {
        let home = tempfile::tempdir().unwrap();
        let store = OperationStore::new(home.path());
        let operation = store
            .create_idempotent(
                "skillUninstall",
                &fingerprint("remove-a"),
                UNINSTALL_SCOPE,
                "remove-key-a",
                "request-remove",
            )
            .unwrap()
            .operation;
        store.mark_running(&operation.id).unwrap();
        let listed = store.list().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].operation.status, OperationStatus::Running);
        assert_eq!(listed[0].origin_request_id, "request-remove");
        let problem = OperationProblem::new(
            &operation.id,
            "request-remove",
            "Operation interrupted",
            503,
            "operation_interrupted",
            "The backend restarted before this operation reached a durable terminal state.",
            true,
        );
        let recovered = store
            .reconcile_fail(&operation.id, problem.clone())
            .unwrap();
        assert_eq!(recovered.status, OperationStatus::Failed);
        assert_eq!(
            recovered.error.as_ref().map(|error| error.code.as_str()),
            Some("operation_interrupted"),
        );
        assert_eq!(
            recovered
                .error
                .as_ref()
                .map(|error| error.request_id.as_str()),
            Some("request-remove")
        );
        assert_ne!(
            recovered
                .error
                .as_ref()
                .map(|error| error.request_id.as_str()),
            Some(operation.id.as_str())
        );
        let path = home
            .path()
            .join(".synthchat/operations")
            .join(format!("{}.json", operation.id));
        let terminal_bytes = fs::read(&path).unwrap();
        let terminal_updated_at = recovered.updated_at.clone();
        assert_eq!(
            store.reconcile_fail(&operation.id, problem).unwrap(),
            recovered
        );
        assert_eq!(fs::read(path).unwrap(), terminal_bytes);
        assert_eq!(
            store.get(&operation.id).unwrap().updated_at,
            terminal_updated_at
        );
        assert_eq!(
            store.reconcile_complete(&operation.id),
            Err(OperationError::TransitionConflict)
        );
    }

    #[test]
    fn active_requests_dedupe_across_distinct_idempotency_keys() {
        let home = tempfile::tempdir().unwrap();
        let store = OperationStore::new(home.path());
        let request = fingerprint("same-active-request");
        let first = store
            .create_idempotent(
                "skillInstall",
                &request,
                INSTALL_SCOPE,
                "active-key-one",
                "request-active-one",
            )
            .unwrap();
        let lookup = store
            .replay_or_active("skillInstall", &request, INSTALL_SCOPE, "active-key-two")
            .unwrap()
            .unwrap();
        assert_eq!(lookup.id, first.operation.id);
        let replay = store
            .create_idempotent(
                "skillInstall",
                &request,
                INSTALL_SCOPE,
                "active-key-two",
                "request-active-two",
            )
            .unwrap();
        assert!(!replay.created);
        assert_eq!(replay.operation.id, first.operation.id);
        assert_eq!(store.list().unwrap().len(), 1);

        store.mark_running(&first.operation.id).unwrap();
        store.complete(&first.operation.id).unwrap();
        let next = store
            .create_idempotent(
                "skillInstall",
                &request,
                INSTALL_SCOPE,
                "active-key-three",
                "request-active-three",
            )
            .unwrap();
        assert!(next.created);
        assert_ne!(next.operation.id, first.operation.id);
    }

    #[test]
    fn capacity_keeps_recent_terminals_and_collects_only_after_twenty_four_hours() {
        let home = tempfile::tempdir().unwrap();
        let store = OperationStore::new(home.path());
        let mut terminal_ids = Vec::new();
        for index in 0..(MAX_PERSISTED_OBJECTS / 2) {
            let operation = store
                .create_idempotent(
                    "skillInstall",
                    &fingerprint(&format!("quota-{index}")),
                    INSTALL_SCOPE,
                    &format!("quota-key-{index:04}"),
                    &format!("request-quota-{index:04}"),
                )
                .unwrap()
                .operation;
            store.mark_running(&operation.id).unwrap();
            store.complete(&operation.id).unwrap();
            terminal_ids.push(operation.id);
        }
        assert_eq!(
            store.create_idempotent(
                "skillInstall",
                &fingerprint("quota-overflow"),
                INSTALL_SCOPE,
                "quota-key-overflow",
                "request-quota-overflow",
            ),
            Err(OperationError::StorageUnavailable)
        );

        let old = OffsetDateTime::now_utc() - time::Duration::hours(25);
        let old_timestamp = old.format(&Rfc3339).unwrap();
        store
            .with_lock(|operation_fs| {
                let mut stored = store.read_locked(operation_fs, &terminal_ids[0])?;
                stored.operation.created_at = old_timestamp.clone();
                stored.operation.updated_at = old_timestamp;
                store.write_operation_locked(operation_fs, &stored)
            })
            .unwrap();
        let admitted = store
            .create_idempotent(
                "skillInstall",
                &fingerprint("quota-after-gc"),
                INSTALL_SCOPE,
                "quota-key-after-gc",
                "request-quota-after-gc",
            )
            .unwrap();
        assert!(admitted.created);
        assert_eq!(store.get(&terminal_ids[0]), Err(OperationError::NotFound));
    }

    #[test]
    fn operation_paths_and_state_transitions_fail_closed() {
        let home = tempfile::tempdir().unwrap();
        let store = OperationStore::new(home.path());
        assert_eq!(store.get("../operation"), Err(OperationError::InvalidId));
        let operation = store
            .create_idempotent(
                "skillInstall",
                &fingerprint("a"),
                INSTALL_SCOPE,
                "state-key-a",
                "request-state",
            )
            .unwrap()
            .operation;
        assert_eq!(
            store.complete(&operation.id),
            Err(OperationError::TransitionConflict),
        );
        store.mark_running(&operation.id).unwrap();
        store.complete(&operation.id).unwrap();
        assert_eq!(
            store.mark_running(&operation.id),
            Err(OperationError::TransitionConflict),
        );
    }

    #[test]
    fn invalid_idempotency_keys_and_missing_replay_targets_fail_closed() {
        let home = tempfile::tempdir().unwrap();
        let store = OperationStore::new(home.path());
        let too_long = "x".repeat(129);
        for key in ["short", "contains space", "01234567\n", too_long.as_str()] {
            assert_eq!(
                store.create_idempotent(
                    "skillInstall",
                    &fingerprint("a"),
                    INSTALL_SCOPE,
                    key,
                    "request-invalid-key",
                ),
                Err(OperationError::InvalidIdempotencyKey),
            );
        }

        let created = store
            .create_idempotent(
                "skillInstall",
                &fingerprint("a"),
                INSTALL_SCOPE,
                "missing-target-key",
                "request-missing-target",
            )
            .unwrap();
        let path = home
            .path()
            .join(".synthchat")
            .join("operations")
            .join(format!("{}.json", created.operation.id));
        fs::remove_file(path).unwrap();
        assert_eq!(
            store.create_idempotent(
                "skillInstall",
                &fingerprint("a"),
                INSTALL_SCOPE,
                "missing-target-key",
                "request-missing-replay",
            ),
            Err(OperationError::DataInvalid),
        );
    }

    #[test]
    fn operation_store_probe_checks_both_writable_directories() {
        let home = tempfile::tempdir().unwrap();
        let store = OperationStore::new(home.path());
        store.probe().unwrap();
        assert!(home.path().join(".synthchat/operations").is_dir());
        assert!(
            home.path()
                .join(".synthchat/operations/idempotency")
                .is_dir()
        );
    }

    #[test]
    fn root_handle_does_not_follow_a_visible_parent_replacement() {
        let home = tempfile::tempdir().unwrap();
        let store = OperationStore::new(home.path());
        store.probe().unwrap();
        let visible_root = home.path().join(".synthchat");
        let held_root = home.path().join(".synthchat-held");
        if let Err(error) = fs::rename(&visible_root, &held_root) {
            #[cfg(windows)]
            if error.raw_os_error() == Some(32)
                || matches!(
                    error.kind(),
                    io::ErrorKind::PermissionDenied | io::ErrorKind::Other
                )
            {
                return;
            }
            panic!("failed to replace the visible Operation parent: {error}");
        }
        fs::create_dir(&visible_root).unwrap();
        fs::write(visible_root.join("sentinel"), b"replacement").unwrap();

        let operation = store
            .create_idempotent(
                "skillInstall",
                &fingerprint("held-root"),
                INSTALL_SCOPE,
                "held-root-key",
                "request-held-root",
            )
            .unwrap()
            .operation;

        assert!(
            held_root
                .join("operations")
                .join(format!("{}.json", operation.id))
                .is_file()
        );
        assert_eq!(
            fs::read(visible_root.join("sentinel")).unwrap(),
            b"replacement"
        );
        assert!(!visible_root.join("operations").exists());
    }

    #[cfg(unix)]
    #[test]
    fn unix_operations_symlink_is_rejected_without_touching_its_target() {
        use std::os::unix::fs::symlink;

        let home = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        let store = OperationStore::new(home.path());
        store.probe().unwrap();
        let operations = home.path().join(".synthchat/operations");
        fs::rename(&operations, home.path().join("operations-held")).unwrap();
        fs::write(external.path().join("sentinel"), b"external").unwrap();
        symlink(external.path(), &operations).unwrap();

        assert_eq!(store.probe(), Err(OperationError::StorageUnavailable));
        assert_eq!(
            fs::read(external.path().join("sentinel")).unwrap(),
            b"external"
        );
        assert_eq!(fs::read_dir(external.path()).unwrap().count(), 1);
    }

    #[cfg(windows)]
    #[test]
    fn windows_operations_reparse_is_rejected_without_touching_its_target() {
        use std::os::windows::fs::symlink_dir;

        let home = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        let store = OperationStore::new(home.path());
        store.probe().unwrap();
        let operations = home.path().join(".synthchat/operations");
        fs::rename(&operations, home.path().join("operations-held")).unwrap();
        fs::write(external.path().join("sentinel"), b"external").unwrap();
        if let Err(error) = symlink_dir(external.path(), &operations) {
            if matches!(
                error.kind(),
                io::ErrorKind::PermissionDenied | io::ErrorKind::Unsupported
            ) || error.raw_os_error() == Some(1314)
            {
                return;
            }
            panic!("failed to create a Windows directory reparse point: {error}");
        }

        assert_eq!(store.probe(), Err(OperationError::StorageUnavailable));
        assert_eq!(
            fs::read(external.path().join("sentinel")).unwrap(),
            b"external"
        );
        assert_eq!(fs::read_dir(external.path()).unwrap().count(), 1);
    }

    #[test]
    fn failed_operation_requires_a_matching_redacted_problem() {
        let home = tempfile::tempdir().unwrap();
        let store = OperationStore::new(home.path());
        let operation = store
            .create_idempotent(
                "skillInstall",
                &fingerprint("a"),
                INSTALL_SCOPE,
                "problem-key-a",
                "request-problem",
            )
            .unwrap()
            .operation;
        store.mark_running(&operation.id).unwrap();
        let wrong = OperationProblem::new(
            "op_0123456789abcdef0123456789abcdef",
            "request-problem",
            "Failure",
            503,
            "operation_failed",
            "The operation failed.",
            true,
        );
        assert_eq!(
            store.fail(&operation.id, wrong),
            Err(OperationError::DataInvalid),
        );
        assert_eq!(
            store.get(&operation.id).unwrap().status,
            OperationStatus::Running
        );
    }
}
