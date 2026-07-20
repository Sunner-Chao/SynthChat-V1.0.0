use std::{
    fs,
    path::{Path, PathBuf},
};

use rusqlite::{OptionalExtension, TransactionBehavior, params};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{SessionError, SessionService, Workspace, schema, store::now_timestamp};
use crate::runs::{RunError, RunStatus};

const MAX_PATH_BYTES: usize = 16 * 1024;
const MAX_IDEMPOTENCY_KEY_BYTES: usize = 128;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RunWorkspaceBinding {
    pub(crate) id: String,
    pub(crate) path: PathBuf,
}

impl SessionService {
    pub fn list_workspaces(&self, profile_id: &str) -> Result<Vec<Workspace>, SessionError> {
        validate_profile_id(profile_id)?;
        let connection = schema::open(&self.ready()?.db_path)?;
        let mut statement = connection
            .prepare(
                "SELECT id, profile_id, canonical_path, display_name, created_at, updated_at \
                 FROM workspaces WHERE profile_id = ?1 ORDER BY created_at, id",
            )
            .map_err(schema::map_sqlite)?;
        let rows = statement
            .query_map([profile_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                ))
            })
            .map_err(schema::map_sqlite)?;
        let mut workspaces = Vec::new();
        for row in rows {
            let (id, profile_id, path, display_name, created_at, updated_at) =
                row.map_err(schema::map_sqlite)?;
            workspaces.push(Workspace {
                id,
                profile_id,
                display_name,
                available: canonical_directory_matches(Path::new(&path)),
                created_at,
                updated_at,
            });
        }
        Ok(workspaces)
    }

    pub fn register_workspace(
        &self,
        profile_id: &str,
        requested_path: &str,
        idempotency_key: &str,
    ) -> Result<Workspace, SessionError> {
        validate_profile_id(profile_id)?;
        validate_idempotency_key(idempotency_key)?;
        let canonical = canonical_directory(requested_path)?;
        let canonical_text = canonical
            .to_str()
            .ok_or(SessionError::InvalidWorkspacePath)?
            .to_owned();
        let path_digest = digest(&canonical_text);
        let fingerprint = digest(&format!("{profile_id}\0{canonical_text}"));
        let api_path = format!("/api/v1/profiles/{profile_id}/workspaces");
        let ready = self.ready()?.clone();
        let _guard = ready
            .write_lock
            .lock()
            .map_err(|_| SessionError::StorageUnavailable)?;
        let mut connection = schema::open(&ready.db_path)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(schema::map_sqlite)?;
        let replay = transaction
            .query_row(
                "SELECT request_fingerprint, resource_id FROM idempotency_records \
                 WHERE method = 'POST' AND canonical_path = ?1 AND idempotency_key = ?2",
                params![api_path, idempotency_key],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(schema::map_sqlite)?;
        if let Some((stored_fingerprint, workspace_id)) = replay {
            if stored_fingerprint != fingerprint {
                return Err(SessionError::IdempotencyConflict);
            }
            let workspace = workspace_by_id(&transaction, &workspace_id)?
                .ok_or(SessionError::IdempotentResourceDeleted)?;
            transaction.commit().map_err(schema::map_sqlite)?;
            return Ok(workspace);
        }

        let existing_id = transaction
            .query_row(
                "SELECT id FROM workspaces WHERE profile_id = ?1 AND path_digest = ?2",
                params![profile_id, path_digest],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(schema::map_sqlite)?;
        let workspace_id =
            existing_id.unwrap_or_else(|| format!("workspace_{}", Uuid::new_v4().simple()));
        if workspace_by_id(&transaction, &workspace_id)?.is_none() {
            let timestamp = now_timestamp()?;
            let display_name = canonical
                .file_name()
                .and_then(|value| value.to_str())
                .filter(|value| !value.is_empty())
                .unwrap_or("Workspace");
            transaction
                .execute(
                    "INSERT INTO workspaces(\
                        id, profile_id, canonical_path, path_digest, display_name, created_at, updated_at\
                     ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?6)",
                    params![
                        workspace_id,
                        profile_id,
                        canonical_text,
                        path_digest,
                        display_name,
                        timestamp
                    ],
                )
                .map_err(schema::map_sqlite)?;
        }
        transaction
            .execute(
                "INSERT INTO idempotency_records(\
                    method, canonical_path, idempotency_key, request_fingerprint, resource_id, response_json, created_at\
                 ) VALUES('POST', ?1, ?2, ?3, ?4, NULL, ?5)",
                params![api_path, idempotency_key, fingerprint, workspace_id, now_timestamp()?],
            )
            .map_err(schema::map_sqlite)?;
        let workspace =
            workspace_by_id(&transaction, &workspace_id)?.ok_or(SessionError::DataInvalid)?;
        transaction.commit().map_err(schema::map_sqlite)?;
        Ok(workspace)
    }

    pub fn delete_workspace(
        &self,
        profile_id: &str,
        workspace_id: &str,
    ) -> Result<(), SessionError> {
        validate_profile_id(profile_id)?;
        validate_workspace_id(workspace_id)?;
        let ready = self.ready()?.clone();
        let _guard = ready
            .write_lock
            .lock()
            .map_err(|_| SessionError::StorageUnavailable)?;
        let mut connection = schema::open(&ready.db_path)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(schema::map_sqlite)?;
        let exists: bool = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM workspaces WHERE id = ?1 AND profile_id = ?2)",
                params![workspace_id, profile_id],
                |row| row.get(0),
            )
            .map_err(schema::map_sqlite)?;
        if !exists {
            return Err(SessionError::NotFound);
        }
        let referenced: bool = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM runs WHERE workspace_id = ?1)",
                [workspace_id],
                |row| row.get(0),
            )
            .map_err(schema::map_sqlite)?;
        if referenced {
            return Err(SessionError::WorkspaceInUse);
        }
        transaction
            .execute("DELETE FROM workspaces WHERE id = ?1", [workspace_id])
            .map_err(schema::map_sqlite)?;
        transaction.commit().map_err(schema::map_sqlite)
    }

    #[cfg(test)]
    pub(crate) fn workspace_path_for_run(&self, run_id: &str) -> Result<Option<PathBuf>, RunError> {
        self.workspace_for_run(run_id)
            .map(|workspace| workspace.map(|workspace| workspace.path))
    }

    pub(crate) fn workspace_for_run(
        &self,
        run_id: &str,
    ) -> Result<Option<RunWorkspaceBinding>, RunError> {
        let connection = schema::open(&self.ready().map_err(map_session_to_run)?.db_path)
            .map_err(map_session_to_run)?;
        let row = connection
            .query_row(
                "SELECT r.status, r.workspace_id, w.canonical_path FROM runs r \
                 LEFT JOIN workspaces w ON w.id = r.workspace_id WHERE r.id = ?1",
                [run_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(schema::map_sqlite)
            .map_err(map_session_to_run)?
            .ok_or(RunError::NotFound)?;
        let _status = RunStatus::try_from(row.0.as_str()).map_err(|_| RunError::DataInvalid)?;
        let Some(workspace_id) = row.1 else {
            return Ok(None);
        };
        let path = row.2.ok_or(RunError::DataInvalid)?;
        let path = PathBuf::from(path);
        if !canonical_directory_matches(&path) {
            return Err(RunError::CapabilityMissing);
        }
        Ok(Some(RunWorkspaceBinding {
            id: workspace_id,
            path,
        }))
    }
}

fn workspace_by_id(
    connection: &rusqlite::Connection,
    workspace_id: &str,
) -> Result<Option<Workspace>, SessionError> {
    connection
        .query_row(
            "SELECT id, profile_id, canonical_path, display_name, created_at, updated_at \
             FROM workspaces WHERE id = ?1",
            [workspace_id],
            |row| {
                let path: String = row.get(2)?;
                Ok(Workspace {
                    id: row.get(0)?,
                    profile_id: row.get(1)?,
                    display_name: row.get(3)?,
                    available: canonical_directory_matches(Path::new(&path)),
                    created_at: row.get(4)?,
                    updated_at: row.get(5)?,
                })
            },
        )
        .optional()
        .map_err(schema::map_sqlite)
}

fn canonical_directory(value: &str) -> Result<PathBuf, SessionError> {
    if value.is_empty() || value.len() > MAX_PATH_BYTES || value.contains('\0') {
        return Err(SessionError::InvalidWorkspacePath);
    }
    let requested = PathBuf::from(value);
    if !requested.is_absolute() {
        return Err(SessionError::InvalidWorkspacePath);
    }
    let canonical = fs::canonicalize(requested).map_err(|_| SessionError::InvalidWorkspacePath)?;
    let metadata = fs::metadata(&canonical).map_err(|_| SessionError::InvalidWorkspacePath)?;
    if !metadata.is_dir() || canonical.to_str().is_none() {
        return Err(SessionError::InvalidWorkspacePath);
    }
    Ok(canonical)
}

fn canonical_directory_matches(path: &Path) -> bool {
    fs::canonicalize(path).ok().is_some_and(|canonical| {
        canonical == path && fs::metadata(&canonical).is_ok_and(|m| m.is_dir())
    })
}

fn validate_profile_id(value: &str) -> Result<(), SessionError> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(SessionError::InvalidRequest);
    }
    Ok(())
}

fn validate_workspace_id(value: &str) -> Result<(), SessionError> {
    if !value.starts_with("workspace_")
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return Err(SessionError::InvalidRequest);
    }
    Ok(())
}

fn validate_idempotency_key(value: &str) -> Result<(), SessionError> {
    if value.is_empty()
        || value.len() > MAX_IDEMPOTENCY_KEY_BYTES
        || !value.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
    {
        return Err(SessionError::InvalidRequest);
    }
    Ok(())
}

fn digest(value: &str) -> String {
    let bytes = Sha256::digest(value.as_bytes());
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn map_session_to_run(error: SessionError) -> RunError {
    match error {
        SessionError::StorageBusy => RunError::StorageBusy,
        SessionError::StorageUnavailable | SessionError::DataInvalid => {
            RunError::StorageUnavailable
        }
        _ => RunError::InvalidRequest,
    }
}
