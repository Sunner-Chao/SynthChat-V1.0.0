use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};
use serde::Serialize;
use time::{OffsetDateTime, UtcOffset, format_description::well_known::Rfc3339};

use crate::runs::RunError;

use super::{
    SessionError, SessionService, schema,
    store::{next_timestamp, now_timestamp},
};

const MAX_COMMAND_PREVIEW_CHARS: usize = 2_000;
const MAX_PROCESS_IDENTITY_CHARS: usize = 1_024;
const MAX_COMPLETION_REASON_CHARS: usize = 2_000;
const MAX_TERMINATION_SOURCE_CHARS: usize = 128;
const MAX_WATCH_PATTERNS: usize = 16;
const MAX_WATCH_PATTERN_CHARS: usize = 256;

const PROCESS_SELECT: &str = "SELECT process_id, profile_id, session_id, workspace_id, \
    creator_run_id, call_id, command_preview, command_sha256, pid, process_identity, status, \
    started_at, updated_at, finished_at, exit_code, completion_reason, termination_source, \
    detached, completion_notification_required, completion_notification_delivered \
    FROM terminal_processes";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProcessOwner {
    pub profile_id: String,
    pub session_id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AsyncToolDeliveryKind {
    Completion,
    Watch,
}

impl AsyncToolDeliveryKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Completion => "completion",
            Self::Watch => "watch",
        }
    }
}

impl TryFrom<&str> for AsyncToolDeliveryKind {
    type Error = ProcessStoreError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "completion" => Ok(Self::Completion),
            "watch" => Ok(Self::Watch),
            _ => Err(ProcessStoreError::DataInvalid),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AsyncToolDeliveryRequest {
    pub(crate) kind: AsyncToolDeliveryKind,
    pub(crate) watch_patterns: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PendingAsyncToolDelivery {
    pub(crate) process: ProcessRecord,
    pub(crate) kind: AsyncToolDeliveryKind,
    pub(crate) watch_patterns: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AsyncToolDeliveryTrigger {
    Completion,
    Watch { matched_pattern_count: u8 },
    WatchMissed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AsyncToolDeliveryDisposition {
    Published,
    AlreadySettled,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CreateProcess {
    pub process_id: String,
    pub workspace_id: String,
    pub creator_run_id: String,
    pub call_id: String,
    pub command_preview: String,
    pub command_sha256: String,
    pub detached: bool,
    pub completion_notification_required: bool,
    pub async_delivery: Option<AsyncToolDeliveryRequest>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ProcessStatus {
    Starting,
    Running,
    Exited,
    Killed,
    Lost,
    FailedStart,
}

impl ProcessStatus {
    pub(crate) fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Exited | Self::Killed | Self::Lost | Self::FailedStart
        )
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Exited => "exited",
            Self::Killed => "killed",
            Self::Lost => "lost",
            Self::FailedStart => "failed_start",
        }
    }
}

impl TryFrom<&str> for ProcessStatus {
    type Error = ProcessStoreError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "starting" => Ok(Self::Starting),
            "running" => Ok(Self::Running),
            "exited" => Ok(Self::Exited),
            "killed" => Ok(Self::Killed),
            "lost" => Ok(Self::Lost),
            "failed_start" => Ok(Self::FailedStart),
            _ => Err(ProcessStoreError::DataInvalid),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ProcessTransition {
    Running {
        pid: u32,
        process_identity: Option<String>,
    },
    Exited {
        exit_code: i32,
        completion_reason: String,
        termination_source: String,
    },
    Killed {
        exit_code: Option<i32>,
        completion_reason: String,
        termination_source: String,
    },
    Lost {
        completion_reason: String,
        termination_source: String,
    },
    FailedStart {
        completion_reason: String,
        termination_source: String,
    },
}

impl ProcessTransition {
    fn target_status(&self) -> ProcessStatus {
        match self {
            Self::Running { .. } => ProcessStatus::Running,
            Self::Exited { .. } => ProcessStatus::Exited,
            Self::Killed { .. } => ProcessStatus::Killed,
            Self::Lost { .. } => ProcessStatus::Lost,
            Self::FailedStart { .. } => ProcessStatus::FailedStart,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProcessRecord {
    pub process_id: String,
    pub profile_id: String,
    pub session_id: String,
    pub workspace_id: String,
    pub creator_run_id: String,
    pub call_id: String,
    pub command_preview: String,
    pub command_sha256: String,
    pub pid: Option<u32>,
    pub process_identity: Option<String>,
    pub status: ProcessStatus,
    pub started_at: String,
    pub updated_at: String,
    pub finished_at: Option<String>,
    pub exit_code: Option<i32>,
    pub completion_reason: Option<String>,
    pub termination_source: Option<String>,
    pub detached: bool,
    pub completion_notification_required: bool,
    pub completion_notification_delivered: bool,
}

#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum ProcessStoreError {
    #[error("invalid process request")]
    InvalidRequest,
    #[error("process not found")]
    NotFound,
    #[error("process state changed from the expected value")]
    TransitionConflict { current_status: ProcessStatus },
    #[error("the active process limit of {limit} was reached")]
    ProcessLimitReached { limit: usize },
    #[error("process storage is busy")]
    StorageBusy,
    #[error("process storage is unavailable")]
    StorageUnavailable,
    #[error("process data is invalid")]
    DataInvalid,
}

impl SessionService {
    #[cfg(test)]
    pub(crate) fn create_process(
        &self,
        owner: &ProcessOwner,
        request: &CreateProcess,
    ) -> Result<ProcessRecord, ProcessStoreError> {
        self.create_process_inner(owner, request, None)
    }

    /// Atomically reserves one slot in the global active-process capacity.
    pub(crate) fn reserve_process(
        &self,
        owner: &ProcessOwner,
        request: &CreateProcess,
        active_process_limit: usize,
    ) -> Result<ProcessRecord, ProcessStoreError> {
        self.create_process_inner(owner, request, Some(active_process_limit))
    }

    fn create_process_inner(
        &self,
        owner: &ProcessOwner,
        request: &CreateProcess,
        active_process_limit: Option<usize>,
    ) -> Result<ProcessRecord, ProcessStoreError> {
        validate_owner(owner)?;
        validate_create_process(request)?;
        let ready = self.ready().map_err(map_session)?.clone();
        let _guard = ready
            .write_lock
            .lock()
            .map_err(|_| ProcessStoreError::StorageUnavailable)?;
        let mut connection = schema::open(&ready.db_path).map_err(map_session)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(schema::map_sqlite)
            .map_err(map_session)?;
        self.require_runtime_lease_tx(&transaction)
            .map_err(map_runtime_lease)?;
        require_process_binding(&transaction, owner, request)?;

        if let Some(existing) = process_by_id(&transaction, &request.process_id)? {
            if existing.profile_id != owner.profile_id || existing.session_id != owner.session_id {
                return Err(ProcessStoreError::NotFound);
            }
            return Err(ProcessStoreError::TransitionConflict {
                current_status: existing.status,
            });
        }
        if let Some(existing) =
            process_by_call(&transaction, &request.creator_run_id, &request.call_id)?
        {
            if existing.profile_id != owner.profile_id || existing.session_id != owner.session_id {
                return Err(ProcessStoreError::NotFound);
            }
            return Err(ProcessStoreError::TransitionConflict {
                current_status: existing.status,
            });
        }

        if let Some(limit) = active_process_limit {
            let active: i64 = transaction
                .query_row(
                    "SELECT COUNT(*) FROM terminal_processes \
                     WHERE status IN ('starting', 'running')",
                    [],
                    |row| row.get(0),
                )
                .map_err(schema::map_sqlite)
                .map_err(map_session)?;
            let active = usize::try_from(active).map_err(|_| ProcessStoreError::DataInvalid)?;
            if active >= limit {
                return Err(ProcessStoreError::ProcessLimitReached { limit });
            }
        }

        let started_at = now_timestamp().map_err(map_session)?;
        transaction
            .execute(
                "INSERT INTO terminal_processes(\
                    process_id, profile_id, session_id, workspace_id, creator_run_id, call_id, \
                    command_preview, command_sha256, pid, process_identity, status, started_at, \
                    updated_at, finished_at, exit_code, completion_reason, termination_source, \
                    detached, completion_notification_required, completion_notification_delivered\
                 ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, NULL, 'starting', ?9, ?9, \
                    NULL, NULL, NULL, NULL, ?10, ?11, 0)",
                params![
                    request.process_id,
                    owner.profile_id,
                    owner.session_id,
                    request.workspace_id,
                    request.creator_run_id,
                    request.call_id,
                    request.command_preview,
                    request.command_sha256,
                    started_at,
                    bool_to_sql(request.detached),
                    bool_to_sql(request.completion_notification_required),
                ],
            )
            .map_err(schema::map_sqlite)
            .map_err(map_session)?;
        if let Some(delivery) = request.async_delivery.as_ref() {
            let watch_patterns_json = serde_json::to_string(&delivery.watch_patterns)
                .map_err(|_| ProcessStoreError::InvalidRequest)?;
            transaction
                .execute(
                    "INSERT INTO async_tool_deliveries(\
                        process_id, delivery_kind, watch_patterns_json, state, settled_at, \
                        matched_pattern_count\
                     ) VALUES(?1, ?2, ?3, 'pending', NULL, NULL)",
                    params![
                        request.process_id,
                        delivery.kind.as_str(),
                        watch_patterns_json,
                    ],
                )
                .map_err(schema::map_sqlite)
                .map_err(map_session)?;
        }
        let record = process_by_owner(&transaction, owner, &request.process_id)?
            .ok_or(ProcessStoreError::DataInvalid)?;
        transaction
            .commit()
            .map_err(schema::map_sqlite)
            .map_err(map_session)?;
        Ok(record)
    }

    pub(crate) fn list_processes(
        &self,
        owner: &ProcessOwner,
    ) -> Result<Vec<ProcessRecord>, ProcessStoreError> {
        validate_owner(owner)?;
        let connection =
            schema::open(&self.ready().map_err(map_session)?.db_path).map_err(map_session)?;
        require_owner(&connection, owner)?;
        query_processes(
            &connection,
            &format!(
                "{PROCESS_SELECT} WHERE profile_id = ?1 AND session_id = ?2 \
                 ORDER BY started_at DESC, process_id DESC"
            ),
            params![owner.profile_id, owner.session_id],
        )
    }

    pub(crate) fn get_process(
        &self,
        owner: &ProcessOwner,
        process_id: &str,
    ) -> Result<ProcessRecord, ProcessStoreError> {
        validate_owner(owner)?;
        validate_process_id(process_id)?;
        let connection =
            schema::open(&self.ready().map_err(map_session)?.db_path).map_err(map_session)?;
        process_by_owner(&connection, owner, process_id)?.ok_or(ProcessStoreError::NotFound)
    }

    pub(crate) fn transition_process(
        &self,
        owner: &ProcessOwner,
        process_id: &str,
        expected_status: ProcessStatus,
        transition: &ProcessTransition,
    ) -> Result<ProcessRecord, ProcessStoreError> {
        validate_owner(owner)?;
        validate_process_id(process_id)?;
        let ready = self.ready().map_err(map_session)?.clone();
        let _guard = ready
            .write_lock
            .lock()
            .map_err(|_| ProcessStoreError::StorageUnavailable)?;
        let mut connection = schema::open(&ready.db_path).map_err(map_session)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(schema::map_sqlite)
            .map_err(map_session)?;
        self.require_runtime_lease_tx(&transaction)
            .map_err(map_runtime_lease)?;
        let current = process_by_owner(&transaction, owner, process_id)?
            .ok_or(ProcessStoreError::NotFound)?;
        if current.status != expected_status || current.status.is_terminal() {
            return Err(ProcessStoreError::TransitionConflict {
                current_status: current.status,
            });
        }
        let update = transition_update(&current, transition)?;
        let updated_at = next_timestamp(&current.updated_at).map_err(map_session)?;
        let finished_at = update.status.is_terminal().then_some(updated_at.as_str());
        let changed = transaction
            .execute(
                "UPDATE terminal_processes SET status = ?1, pid = ?2, process_identity = ?3, \
                    updated_at = ?4, finished_at = ?5, exit_code = ?6, completion_reason = ?7, \
                    termination_source = ?8 \
                 WHERE process_id = ?9 AND profile_id = ?10 AND session_id = ?11 \
                    AND status = ?12 AND updated_at = ?13",
                params![
                    update.status.as_str(),
                    update.pid.map(i64::from),
                    update.process_identity,
                    updated_at,
                    finished_at,
                    update.exit_code,
                    update.completion_reason,
                    update.termination_source,
                    process_id,
                    owner.profile_id,
                    owner.session_id,
                    expected_status.as_str(),
                    current.updated_at,
                ],
            )
            .map_err(schema::map_sqlite)
            .map_err(map_session)?;
        if changed != 1 {
            let latest = process_by_owner(&transaction, owner, process_id)?
                .ok_or(ProcessStoreError::NotFound)?;
            return Err(ProcessStoreError::TransitionConflict {
                current_status: latest.status,
            });
        }
        let record = process_by_owner(&transaction, owner, process_id)?
            .ok_or(ProcessStoreError::DataInvalid)?;
        transaction
            .commit()
            .map_err(schema::map_sqlite)
            .map_err(map_session)?;
        Ok(record)
    }

    pub(crate) fn list_recovery_candidates(&self) -> Result<Vec<ProcessRecord>, ProcessStoreError> {
        let connection =
            schema::open(&self.ready().map_err(map_session)?.db_path).map_err(map_session)?;
        query_processes(
            &connection,
            &format!(
                "{PROCESS_SELECT} WHERE status IN ('starting', 'running') \
                 ORDER BY started_at, process_id"
            ),
            [],
        )
    }

    pub(crate) fn pending_async_tool_deliveries(
        &self,
    ) -> Result<Vec<PendingAsyncToolDelivery>, ProcessStoreError> {
        let connection =
            schema::open(&self.ready().map_err(map_session)?.db_path).map_err(map_session)?;
        let mut statement = connection
            .prepare(
                "SELECT p.*, d.delivery_kind, d.watch_patterns_json \
                 FROM terminal_processes p \
                 JOIN async_tool_deliveries d ON d.process_id = p.process_id \
                 WHERE d.state = 'pending' \
                 ORDER BY p.started_at, p.process_id",
            )
            .map_err(schema::map_sqlite)
            .map_err(map_session)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    raw_process_row(row)?,
                    row.get::<_, String>(20)?,
                    row.get::<_, String>(21)?,
                ))
            })
            .map_err(schema::map_sqlite)
            .map_err(map_session)?;
        let mut deliveries = Vec::new();
        for row in rows {
            let (raw_process, kind, watch_patterns_json) =
                row.map_err(schema::map_sqlite).map_err(map_session)?;
            let kind = AsyncToolDeliveryKind::try_from(kind.as_str())?;
            let watch_patterns = parse_watch_patterns_json(&watch_patterns_json, kind)?;
            deliveries.push(PendingAsyncToolDelivery {
                process: ProcessRecord::try_from(raw_process)?,
                kind,
                watch_patterns,
            });
        }
        Ok(deliveries)
    }

    pub(crate) fn mark_process_lost(
        &self,
        owner: &ProcessOwner,
        process_id: &str,
        expected_status: ProcessStatus,
    ) -> Result<ProcessRecord, ProcessStoreError> {
        self.transition_process(
            owner,
            process_id,
            expected_status,
            &ProcessTransition::Lost {
                completion_reason: "Backend restarted before process identity was verified."
                    .to_owned(),
                termination_source: "backend_restart".to_owned(),
            },
        )
    }

    #[cfg(test)]
    pub(crate) fn mark_completion_notification_delivered(
        &self,
        owner: &ProcessOwner,
        process_id: &str,
    ) -> Result<ProcessRecord, ProcessStoreError> {
        validate_owner(owner)?;
        validate_process_id(process_id)?;
        let ready = self.ready().map_err(map_session)?.clone();
        let _guard = ready
            .write_lock
            .lock()
            .map_err(|_| ProcessStoreError::StorageUnavailable)?;
        let mut connection = schema::open(&ready.db_path).map_err(map_session)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(schema::map_sqlite)
            .map_err(map_session)?;
        self.require_runtime_lease_tx(&transaction)
            .map_err(map_runtime_lease)?;
        let current = process_by_owner(&transaction, owner, process_id)?
            .ok_or(ProcessStoreError::NotFound)?;
        if !current.status.is_terminal() || !current.completion_notification_required {
            return Err(ProcessStoreError::InvalidRequest);
        }
        if current.completion_notification_delivered {
            transaction
                .commit()
                .map_err(schema::map_sqlite)
                .map_err(map_session)?;
            return Ok(current);
        }
        let updated_at = next_timestamp(&current.updated_at).map_err(map_session)?;
        let changed = transaction
            .execute(
                "UPDATE terminal_processes SET completion_notification_delivered = 1, \
                    updated_at = ?1 WHERE process_id = ?2 AND profile_id = ?3 AND session_id = ?4 \
                    AND status = ?5 AND updated_at = ?6 \
                    AND completion_notification_required = 1 \
                    AND completion_notification_delivered = 0",
                params![
                    updated_at,
                    process_id,
                    owner.profile_id,
                    owner.session_id,
                    current.status.as_str(),
                    current.updated_at,
                ],
            )
            .map_err(schema::map_sqlite)
            .map_err(map_session)?;
        if changed != 1 {
            let latest = process_by_owner(&transaction, owner, process_id)?
                .ok_or(ProcessStoreError::NotFound)?;
            if latest.completion_notification_delivered {
                transaction
                    .commit()
                    .map_err(schema::map_sqlite)
                    .map_err(map_session)?;
                return Ok(latest);
            }
            return Err(ProcessStoreError::TransitionConflict {
                current_status: latest.status,
            });
        }
        let record = process_by_owner(&transaction, owner, process_id)?
            .ok_or(ProcessStoreError::DataInvalid)?;
        transaction
            .commit()
            .map_err(schema::map_sqlite)
            .map_err(map_session)?;
        Ok(record)
    }

    pub(crate) fn prune_finished_processes(
        &self,
        finished_before: &str,
    ) -> Result<usize, ProcessStoreError> {
        let cutoff = OffsetDateTime::parse(finished_before, &Rfc3339)
            .map_err(|_| ProcessStoreError::InvalidRequest)?
            .to_offset(UtcOffset::UTC)
            .format(&Rfc3339)
            .map_err(|_| ProcessStoreError::InvalidRequest)?;
        let ready = self.ready().map_err(map_session)?.clone();
        let _guard = ready
            .write_lock
            .lock()
            .map_err(|_| ProcessStoreError::StorageUnavailable)?;
        let mut connection = schema::open(&ready.db_path).map_err(map_session)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(schema::map_sqlite)
            .map_err(map_session)?;
        self.require_runtime_lease_tx(&transaction)
            .map_err(map_runtime_lease)?;
        let deleted = transaction
            .execute(
                "DELETE FROM terminal_processes \
                 WHERE status IN ('exited', 'killed', 'lost', 'failed_start') \
                    AND finished_at IS NOT NULL AND julianday(finished_at) < julianday(?1) \
                    AND (completion_notification_required = 0 \
                        OR completion_notification_delivered = 1 \
                        OR EXISTS(\
                            SELECT 1 FROM async_tool_deliveries d \
                            WHERE d.process_id = terminal_processes.process_id \
                                AND d.state <> 'pending'\
                        ))",
                [cutoff],
            )
            .map_err(schema::map_sqlite)
            .map_err(map_session)?;
        transaction
            .commit()
            .map_err(schema::map_sqlite)
            .map_err(map_session)?;
        Ok(deleted)
    }
}

struct TransitionUpdate {
    status: ProcessStatus,
    pid: Option<u32>,
    process_identity: Option<String>,
    exit_code: Option<i32>,
    completion_reason: Option<String>,
    termination_source: Option<String>,
}

fn transition_update(
    current: &ProcessRecord,
    transition: &ProcessTransition,
) -> Result<TransitionUpdate, ProcessStoreError> {
    let target = transition.target_status();
    let allowed = matches!(
        (current.status, target),
        (ProcessStatus::Starting, ProcessStatus::Running)
            | (ProcessStatus::Starting, ProcessStatus::Lost)
            | (ProcessStatus::Starting, ProcessStatus::FailedStart)
            | (ProcessStatus::Running, ProcessStatus::Exited)
            | (ProcessStatus::Running, ProcessStatus::Killed)
            | (ProcessStatus::Running, ProcessStatus::Lost)
    );
    if !allowed {
        return Err(ProcessStoreError::InvalidRequest);
    }
    match transition {
        ProcessTransition::Running {
            pid,
            process_identity,
        } => {
            if *pid == 0 || process_identity.as_deref().is_none_or(str::is_empty) {
                return Err(ProcessStoreError::InvalidRequest);
            }
            validate_optional_text(process_identity.as_deref(), MAX_PROCESS_IDENTITY_CHARS)?;
            Ok(TransitionUpdate {
                status: target,
                pid: Some(*pid),
                process_identity: process_identity.clone(),
                exit_code: None,
                completion_reason: None,
                termination_source: None,
            })
        }
        ProcessTransition::Exited {
            exit_code,
            completion_reason,
            termination_source,
        } => terminal_update(
            current,
            target,
            Some(*exit_code),
            completion_reason,
            termination_source,
        ),
        ProcessTransition::Killed {
            exit_code,
            completion_reason,
            termination_source,
        } => terminal_update(
            current,
            target,
            *exit_code,
            completion_reason,
            termination_source,
        ),
        ProcessTransition::Lost {
            completion_reason,
            termination_source,
        }
        | ProcessTransition::FailedStart {
            completion_reason,
            termination_source,
        } => terminal_update(current, target, None, completion_reason, termination_source),
    }
}

fn terminal_update(
    current: &ProcessRecord,
    status: ProcessStatus,
    exit_code: Option<i32>,
    completion_reason: &str,
    termination_source: &str,
) -> Result<TransitionUpdate, ProcessStoreError> {
    validate_text(completion_reason, MAX_COMPLETION_REASON_CHARS)?;
    validate_text(termination_source, MAX_TERMINATION_SOURCE_CHARS)?;
    Ok(TransitionUpdate {
        status,
        pid: current.pid,
        process_identity: current.process_identity.clone(),
        exit_code,
        completion_reason: Some(completion_reason.to_owned()),
        termination_source: Some(termination_source.to_owned()),
    })
}

fn validate_owner(owner: &ProcessOwner) -> Result<(), ProcessStoreError> {
    if owner.profile_id.is_empty()
        || owner.profile_id.len() > 64
        || !owner
            .profile_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        || !valid_prefixed_id(&owner.session_id, "session_", 128)
    {
        return Err(ProcessStoreError::NotFound);
    }
    Ok(())
}

fn validate_create_process(request: &CreateProcess) -> Result<(), ProcessStoreError> {
    validate_process_id(&request.process_id)?;
    if !valid_prefixed_id(&request.workspace_id, "workspace_", 128)
        || !valid_prefixed_id(&request.creator_run_id, "run_", 128)
        || request.call_id.is_empty()
        || request.call_id.len() > 256
        || request.call_id.contains('\0')
    {
        return Err(ProcessStoreError::NotFound);
    }
    validate_text(&request.command_preview, MAX_COMMAND_PREVIEW_CHARS)?;
    if request.command_sha256.len() != 64
        || !request
            .command_sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(ProcessStoreError::InvalidRequest);
    }
    if let Some(delivery) = request.async_delivery.as_ref() {
        if !request.detached || !request.completion_notification_required {
            return Err(ProcessStoreError::InvalidRequest);
        }
        validate_async_delivery(delivery)?;
    }
    Ok(())
}

fn validate_async_delivery(delivery: &AsyncToolDeliveryRequest) -> Result<(), ProcessStoreError> {
    match delivery.kind {
        AsyncToolDeliveryKind::Completion if !delivery.watch_patterns.is_empty() => {
            return Err(ProcessStoreError::InvalidRequest);
        }
        AsyncToolDeliveryKind::Watch
            if delivery.watch_patterns.is_empty()
                || delivery.watch_patterns.len() > MAX_WATCH_PATTERNS =>
        {
            return Err(ProcessStoreError::InvalidRequest);
        }
        _ => {}
    }
    if delivery.watch_patterns.iter().any(|pattern| {
        pattern.is_empty()
            || pattern.chars().count() > MAX_WATCH_PATTERN_CHARS
            || pattern.contains('\0')
    }) {
        return Err(ProcessStoreError::InvalidRequest);
    }
    Ok(())
}

fn parse_watch_patterns_json(
    value: &str,
    kind: AsyncToolDeliveryKind,
) -> Result<Vec<String>, ProcessStoreError> {
    let watch_patterns: Vec<String> =
        serde_json::from_str(value).map_err(|_| ProcessStoreError::DataInvalid)?;
    validate_async_delivery(&AsyncToolDeliveryRequest {
        kind,
        watch_patterns: watch_patterns.clone(),
    })
    .map_err(|_| ProcessStoreError::DataInvalid)?;
    Ok(watch_patterns)
}

fn validate_process_id(value: &str) -> Result<(), ProcessStoreError> {
    let valid = value.strip_prefix("process_").is_some_and(|suffix| {
        suffix.len() == 32
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    });
    if valid {
        Ok(())
    } else {
        Err(ProcessStoreError::NotFound)
    }
}

fn valid_prefixed_id(value: &str, prefix: &str, max_bytes: usize) -> bool {
    value.starts_with(prefix)
        && value.len() <= max_bytes
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn validate_optional_text(value: Option<&str>, max_chars: usize) -> Result<(), ProcessStoreError> {
    match value {
        Some(value) => validate_text(value, max_chars),
        None => Ok(()),
    }
}

fn validate_text(value: &str, max_chars: usize) -> Result<(), ProcessStoreError> {
    if value.is_empty() || value.chars().count() > max_chars || value.contains('\0') {
        return Err(ProcessStoreError::InvalidRequest);
    }
    Ok(())
}

fn require_process_binding(
    connection: &Connection,
    owner: &ProcessOwner,
    request: &CreateProcess,
) -> Result<(), ProcessStoreError> {
    let exists: bool = connection
        .query_row(
            "SELECT EXISTS(\
                SELECT 1 FROM sessions s \
                JOIN runs r ON r.session_id = s.id AND r.profile_id = s.profile_id \
                JOIN tool_invocations i ON i.run_id = r.id \
                WHERE s.id = ?1 AND s.profile_id = ?2 AND r.id = ?3 \
                    AND r.workspace_id = ?4 AND i.call_id = ?5 AND i.status = 'running'\
             )",
            params![
                owner.session_id,
                owner.profile_id,
                request.creator_run_id,
                request.workspace_id,
                request.call_id,
            ],
            |row| row.get(0),
        )
        .map_err(schema::map_sqlite)
        .map_err(map_session)?;
    if exists {
        Ok(())
    } else {
        Err(ProcessStoreError::NotFound)
    }
}

fn require_owner(connection: &Connection, owner: &ProcessOwner) -> Result<(), ProcessStoreError> {
    let exists: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sessions WHERE id = ?1 AND profile_id = ?2)",
            params![owner.session_id, owner.profile_id],
            |row| row.get(0),
        )
        .map_err(schema::map_sqlite)
        .map_err(map_session)?;
    if exists {
        Ok(())
    } else {
        Err(ProcessStoreError::NotFound)
    }
}

fn process_by_id(
    connection: &Connection,
    process_id: &str,
) -> Result<Option<ProcessRecord>, ProcessStoreError> {
    query_process(
        connection,
        &format!("{PROCESS_SELECT} WHERE process_id = ?1"),
        [process_id],
    )
}

fn process_by_owner(
    connection: &Connection,
    owner: &ProcessOwner,
    process_id: &str,
) -> Result<Option<ProcessRecord>, ProcessStoreError> {
    query_process(
        connection,
        &format!("{PROCESS_SELECT} WHERE process_id = ?1 AND profile_id = ?2 AND session_id = ?3"),
        params![process_id, owner.profile_id, owner.session_id],
    )
}

fn process_by_call(
    connection: &Connection,
    run_id: &str,
    call_id: &str,
) -> Result<Option<ProcessRecord>, ProcessStoreError> {
    query_process(
        connection,
        &format!("{PROCESS_SELECT} WHERE creator_run_id = ?1 AND call_id = ?2"),
        params![run_id, call_id],
    )
}

fn query_process<P: rusqlite::Params>(
    connection: &Connection,
    sql: &str,
    params: P,
) -> Result<Option<ProcessRecord>, ProcessStoreError> {
    connection
        .query_row(sql, params, raw_process_row)
        .optional()
        .map_err(schema::map_sqlite)
        .map_err(map_session)?
        .map(ProcessRecord::try_from)
        .transpose()
}

fn query_processes<P: rusqlite::Params>(
    connection: &Connection,
    sql: &str,
    params: P,
) -> Result<Vec<ProcessRecord>, ProcessStoreError> {
    let mut statement = connection
        .prepare(sql)
        .map_err(schema::map_sqlite)
        .map_err(map_session)?;
    let rows = statement
        .query_map(params, raw_process_row)
        .map_err(schema::map_sqlite)
        .map_err(map_session)?;
    let mut records = Vec::new();
    for row in rows {
        records.push(ProcessRecord::try_from(
            row.map_err(schema::map_sqlite).map_err(map_session)?,
        )?);
    }
    Ok(records)
}

struct RawProcessRecord {
    process_id: String,
    profile_id: String,
    session_id: String,
    workspace_id: String,
    creator_run_id: String,
    call_id: String,
    command_preview: String,
    command_sha256: String,
    pid: Option<i64>,
    process_identity: Option<String>,
    status: String,
    started_at: String,
    updated_at: String,
    finished_at: Option<String>,
    exit_code: Option<i64>,
    completion_reason: Option<String>,
    termination_source: Option<String>,
    detached: i64,
    completion_notification_required: i64,
    completion_notification_delivered: i64,
}

fn raw_process_row(row: &Row<'_>) -> rusqlite::Result<RawProcessRecord> {
    Ok(RawProcessRecord {
        process_id: row.get(0)?,
        profile_id: row.get(1)?,
        session_id: row.get(2)?,
        workspace_id: row.get(3)?,
        creator_run_id: row.get(4)?,
        call_id: row.get(5)?,
        command_preview: row.get(6)?,
        command_sha256: row.get(7)?,
        pid: row.get(8)?,
        process_identity: row.get(9)?,
        status: row.get(10)?,
        started_at: row.get(11)?,
        updated_at: row.get(12)?,
        finished_at: row.get(13)?,
        exit_code: row.get(14)?,
        completion_reason: row.get(15)?,
        termination_source: row.get(16)?,
        detached: row.get(17)?,
        completion_notification_required: row.get(18)?,
        completion_notification_delivered: row.get(19)?,
    })
}

impl TryFrom<RawProcessRecord> for ProcessRecord {
    type Error = ProcessStoreError;

    fn try_from(raw: RawProcessRecord) -> Result<Self, Self::Error> {
        let status = ProcessStatus::try_from(raw.status.as_str())?;
        let pid = raw
            .pid
            .map(|pid| u32::try_from(pid).map_err(|_| ProcessStoreError::DataInvalid))
            .transpose()?;
        let exit_code = raw
            .exit_code
            .map(|code| i32::try_from(code).map_err(|_| ProcessStoreError::DataInvalid))
            .transpose()?;
        let detached = sql_bool(raw.detached)?;
        let completion_notification_required = sql_bool(raw.completion_notification_required)?;
        let completion_notification_delivered = sql_bool(raw.completion_notification_delivered)?;
        validate_stored_timestamps(&raw.started_at, &raw.updated_at, raw.finished_at.as_deref())?;
        Ok(Self {
            process_id: raw.process_id,
            profile_id: raw.profile_id,
            session_id: raw.session_id,
            workspace_id: raw.workspace_id,
            creator_run_id: raw.creator_run_id,
            call_id: raw.call_id,
            command_preview: raw.command_preview,
            command_sha256: raw.command_sha256,
            pid,
            process_identity: raw.process_identity,
            status,
            started_at: raw.started_at,
            updated_at: raw.updated_at,
            finished_at: raw.finished_at,
            exit_code,
            completion_reason: raw.completion_reason,
            termination_source: raw.termination_source,
            detached,
            completion_notification_required,
            completion_notification_delivered,
        })
    }
}

fn validate_stored_timestamps(
    started_at: &str,
    updated_at: &str,
    finished_at: Option<&str>,
) -> Result<(), ProcessStoreError> {
    let started =
        OffsetDateTime::parse(started_at, &Rfc3339).map_err(|_| ProcessStoreError::DataInvalid)?;
    let updated =
        OffsetDateTime::parse(updated_at, &Rfc3339).map_err(|_| ProcessStoreError::DataInvalid)?;
    if updated < started {
        return Err(ProcessStoreError::DataInvalid);
    }
    if let Some(finished_at) = finished_at {
        let finished = OffsetDateTime::parse(finished_at, &Rfc3339)
            .map_err(|_| ProcessStoreError::DataInvalid)?;
        if finished < started || finished > updated {
            return Err(ProcessStoreError::DataInvalid);
        }
    }
    Ok(())
}

fn bool_to_sql(value: bool) -> i64 {
    i64::from(value)
}

fn sql_bool(value: i64) -> Result<bool, ProcessStoreError> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(ProcessStoreError::DataInvalid),
    }
}

fn map_session(error: SessionError) -> ProcessStoreError {
    match error {
        SessionError::StorageBusy => ProcessStoreError::StorageBusy,
        SessionError::StorageUnavailable => ProcessStoreError::StorageUnavailable,
        SessionError::DataInvalid => ProcessStoreError::DataInvalid,
        _ => ProcessStoreError::InvalidRequest,
    }
}

fn map_runtime_lease(error: RunError) -> ProcessStoreError {
    match error {
        RunError::StorageBusy => ProcessStoreError::StorageBusy,
        RunError::DataInvalid => ProcessStoreError::DataInvalid,
        RunError::EngineUnavailable | RunError::StorageUnavailable => {
            ProcessStoreError::StorageUnavailable
        }
        _ => ProcessStoreError::StorageUnavailable,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};

    use sha2::{Digest, Sha256};
    use tempfile::TempDir;
    use uuid::Uuid;

    use crate::runs::{ChatInput, CreateRun};

    use super::*;
    use crate::sessions::{
        CreateSession, ProviderTurnFinish, ProviderTurnPlan, RawToolCallPlan, Usage,
    };

    const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    struct Fixture {
        home: TempDir,
        service: SessionService,
    }

    struct Binding {
        owner: ProcessOwner,
        workspace_id: String,
        run_id: String,
        call_id: String,
    }

    impl Fixture {
        fn new() -> Self {
            let home = tempfile::tempdir().unwrap();
            let service = SessionService::new(home.path(), TOKEN);
            Self { home, service }
        }

        fn binding(&self, profile_id: &str, key: &str) -> Binding {
            let session = self
                .service
                .create_session(
                    &CreateSession {
                        profile_id: profile_id.to_owned(),
                        persona_id: None,
                        title: Some(format!("process {key}")),
                    },
                    &format!("session-process-{key}"),
                )
                .unwrap();
            let workspace_path = self.home.path().join(format!("workspace-{key}"));
            std::fs::create_dir_all(&workspace_path).unwrap();
            let workspace = self
                .service
                .register_workspace(
                    profile_id,
                    workspace_path.to_str().unwrap(),
                    &format!("workspace-process-{key}"),
                )
                .unwrap();
            let accepted = self
                .service
                .create_run(
                    &session.value.id,
                    &CreateRun {
                        persona_id: None,
                        client_request_id: format!("client-process-{key}"),
                        message: ChatInput {
                            text: "run a process".to_owned(),
                            file_ids: Vec::new(),
                        },
                        model_override: None,
                        reasoning_effort: None,
                        workspace_id: Some(workspace.id.clone()),
                    },
                    &format!("run-process-{key}"),
                    "test/model",
                )
                .unwrap();
            let message_id = format!("message_process_{key}");
            let call_id = format!("call_process_{key}");
            self.service
                .begin_assistant_message(&accepted.run.id, &message_id)
                .unwrap();
            self.service
                .record_provider_turn(
                    &accepted.run.id,
                    &ProviderTurnPlan {
                        turn_index: 1,
                        assistant_message_id: message_id,
                        content: None,
                        reasoning: None,
                        finish: ProviderTurnFinish::ToolCalls,
                        usage: Usage {
                            prompt_tokens: 1,
                            completion_tokens: 1,
                            total_tokens: 2,
                            cost: None,
                        },
                        tool_calls: vec![RawToolCallPlan {
                            call_id: call_id.clone(),
                            tool_name: "terminal".to_owned(),
                            arguments_json: r#"{"command":"cargo test"}"#.to_owned(),
                        }],
                    },
                )
                .unwrap();
            self.service
                .start_tool_invocation_with_event(
                    &accepted.run.id,
                    &call_id,
                    "terminal",
                    "Run approved command",
                )
                .unwrap();
            Binding {
                owner: ProcessOwner {
                    profile_id: profile_id.to_owned(),
                    session_id: session.value.id,
                },
                workspace_id: workspace.id,
                run_id: accepted.run.id,
                call_id,
            }
        }
    }

    fn create_request(binding: &Binding, suffix: u32, detached: bool) -> CreateProcess {
        CreateProcess {
            process_id: format!("process_{suffix:032x}"),
            workspace_id: binding.workspace_id.clone(),
            creator_run_id: binding.run_id.clone(),
            call_id: binding.call_id.clone(),
            command_preview: "cargo test --workspace".to_owned(),
            command_sha256: format!("{:x}", Sha256::digest(b"cargo test --workspace")),
            detached,
            completion_notification_required: detached,
            async_delivery: None,
        }
    }

    #[test]
    fn migrates_v6_to_v7_without_rewriting_sessions() {
        let fixture = Fixture::new();
        let session = fixture
            .service
            .create_session(
                &CreateSession {
                    profile_id: "default".to_owned(),
                    persona_id: None,
                    title: Some("before process migration".to_owned()),
                },
                "process-migration-session",
            )
            .unwrap();
        let db_path = fixture.home.path().join(".synthchat/sessions-v1.db");
        let connection = schema::open(&db_path).unwrap();
        connection
            .execute_batch("DROP TABLE terminal_processes; PRAGMA user_version = 6;")
            .unwrap();
        drop(connection);

        let migrated = SessionService::new(fixture.home.path(), TOKEN);
        assert!(migrated.is_available());
        assert_eq!(
            migrated.schema_version(),
            Some(crate::sessions::SESSION_SCHEMA_VERSION)
        );
        assert_eq!(
            migrated.get_session(&session.value.id).unwrap().value.title,
            "before process migration"
        );
        let connection = schema::open(&db_path).unwrap();
        let version: u32 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        let columns: i64 = connection
            .query_row(
                "SELECT count(*) FROM pragma_table_info('terminal_processes')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version, crate::sessions::SESSION_SCHEMA_VERSION);
        assert_eq!(columns, 20);
    }

    #[test]
    fn process_transition_compare_and_swap_has_one_winner() {
        let fixture = Fixture::new();
        let binding = fixture.binding("default", "cas");
        let request = create_request(&binding, 1, false);
        fixture
            .service
            .create_process(&binding.owner, &request)
            .unwrap();

        let barrier = Arc::new(Barrier::new(3));
        let first_service = SessionService::new(fixture.home.path(), TOKEN);
        let second_service = SessionService::new(fixture.home.path(), TOKEN);
        let first_owner = binding.owner.clone();
        let second_owner = binding.owner.clone();
        let first_id = request.process_id.clone();
        let second_id = request.process_id.clone();
        let first_barrier = barrier.clone();
        let second_barrier = barrier.clone();
        let first = std::thread::spawn(move || {
            first_barrier.wait();
            first_service.transition_process(
                &first_owner,
                &first_id,
                ProcessStatus::Starting,
                &ProcessTransition::Running {
                    pid: 41,
                    process_identity: Some("birth:41".to_owned()),
                },
            )
        });
        let second = std::thread::spawn(move || {
            second_barrier.wait();
            second_service.transition_process(
                &second_owner,
                &second_id,
                ProcessStatus::Starting,
                &ProcessTransition::FailedStart {
                    completion_reason: "spawn failed".to_owned(),
                    termination_source: "spawn".to_owned(),
                },
            )
        });
        barrier.wait();
        let results = [first.join().unwrap(), second.join().unwrap()];
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(
                    result,
                    Err(ProcessStoreError::TransitionConflict { .. })
                ))
                .count(),
            1
        );

        let current = fixture
            .service
            .get_process(&binding.owner, &request.process_id)
            .unwrap();
        if current.status == ProcessStatus::Running {
            fixture
                .service
                .transition_process(
                    &binding.owner,
                    &request.process_id,
                    ProcessStatus::Running,
                    &ProcessTransition::Exited {
                        exit_code: 0,
                        completion_reason: "completed".to_owned(),
                        termination_source: "process".to_owned(),
                    },
                )
                .unwrap();
        }
        let terminal = fixture
            .service
            .get_process(&binding.owner, &request.process_id)
            .unwrap();
        assert!(terminal.status.is_terminal());
        assert!(matches!(
            fixture.service.transition_process(
                &binding.owner,
                &request.process_id,
                terminal.status,
                &ProcessTransition::Running {
                    pid: 42,
                    process_identity: None,
                },
            ),
            Err(ProcessStoreError::TransitionConflict { .. })
        ));
    }

    #[test]
    fn concurrent_reservations_across_sessions_cannot_exceed_global_limit() {
        let fixture = Fixture::new();
        let first_binding = fixture.binding("default", "capacity-a");
        let second_binding = fixture.binding("default", "capacity-b");
        assert_ne!(
            first_binding.owner.session_id,
            second_binding.owner.session_id
        );

        let first_request = create_request(&first_binding, 9, true);
        let second_request = create_request(&second_binding, 10, true);
        let first_service = SessionService::new(fixture.home.path(), TOKEN);
        let second_service = SessionService::new(fixture.home.path(), TOKEN);
        let barrier = Arc::new(Barrier::new(3));

        let first_barrier = barrier.clone();
        let first = std::thread::spawn(move || {
            first_barrier.wait();
            first_service.reserve_process(&first_binding.owner, &first_request, 1)
        });
        let second_barrier = barrier.clone();
        let second = std::thread::spawn(move || {
            second_barrier.wait();
            second_service.reserve_process(&second_binding.owner, &second_request, 1)
        });

        barrier.wait();
        let results = [first.join().unwrap(), second.join().unwrap()];
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(
                    result,
                    Err(ProcessStoreError::ProcessLimitReached { limit: 1 })
                ))
                .count(),
            1
        );

        let connection =
            schema::open(&fixture.home.path().join(".synthchat/sessions-v1.db")).unwrap();
        let active: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM terminal_processes \
                 WHERE status IN ('starting', 'running')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(active, 1);
    }

    #[test]
    fn owner_scope_hides_processes_and_invalid_ids() {
        let fixture = Fixture::new();
        let first = fixture.binding("default", "owner-a");
        let second = fixture.binding("default", "owner-b");
        let request = create_request(&first, 2, false);
        fixture
            .service
            .create_process(&first.owner, &request)
            .unwrap();

        assert_eq!(
            fixture.service.list_processes(&first.owner).unwrap().len(),
            1
        );
        assert!(
            fixture
                .service
                .list_processes(&second.owner)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            fixture
                .service
                .get_process(&second.owner, &request.process_id)
                .unwrap_err(),
            ProcessStoreError::NotFound
        );
        let wrong_profile = ProcessOwner {
            profile_id: "other".to_owned(),
            session_id: first.owner.session_id.clone(),
        };
        assert_eq!(
            fixture.service.list_processes(&wrong_profile).unwrap_err(),
            ProcessStoreError::NotFound
        );
        assert_eq!(
            fixture
                .service
                .get_process(&wrong_profile, &request.process_id)
                .unwrap_err(),
            ProcessStoreError::NotFound
        );
        assert_eq!(
            fixture
                .service
                .get_process(&first.owner, "process_NOT_VALID")
                .unwrap_err(),
            ProcessStoreError::NotFound
        );
        assert_eq!(
            fixture
                .service
                .create_process(&second.owner, &request)
                .unwrap_err(),
            ProcessStoreError::NotFound
        );
    }

    #[test]
    fn restart_candidates_are_explicitly_marked_lost_with_detached_preserved() {
        let fixture = Fixture::new();
        let attached = fixture.binding("default", "restart-attached");
        let detached = fixture.binding("default", "restart-detached");
        let completed = fixture.binding("default", "restart-completed");
        let attached_request = create_request(&attached, 3, false);
        let detached_request = create_request(&detached, 4, true);
        let completed_request = create_request(&completed, 5, false);
        fixture
            .service
            .create_process(&attached.owner, &attached_request)
            .unwrap();
        fixture
            .service
            .create_process(&detached.owner, &detached_request)
            .unwrap();
        fixture
            .service
            .transition_process(
                &detached.owner,
                &detached_request.process_id,
                ProcessStatus::Starting,
                &ProcessTransition::Running {
                    pid: 77,
                    process_identity: Some("birth:77".to_owned()),
                },
            )
            .unwrap();
        fixture
            .service
            .create_process(&completed.owner, &completed_request)
            .unwrap();
        fixture
            .service
            .transition_process(
                &completed.owner,
                &completed_request.process_id,
                ProcessStatus::Starting,
                &ProcessTransition::Running {
                    pid: 88,
                    process_identity: Some("birth:88".to_owned()),
                },
            )
            .unwrap();
        fixture
            .service
            .transition_process(
                &completed.owner,
                &completed_request.process_id,
                ProcessStatus::Running,
                &ProcessTransition::Exited {
                    exit_code: 0,
                    completion_reason: "completed".to_owned(),
                    termination_source: "process".to_owned(),
                },
            )
            .unwrap();

        let restarted = SessionService::new(fixture.home.path(), TOKEN);
        let candidates = restarted.list_recovery_candidates().unwrap();
        assert_eq!(candidates.len(), 2);
        assert!(candidates.iter().any(|record| {
            record.process_id == attached_request.process_id
                && record.status == ProcessStatus::Starting
                && record.pid.is_none()
                && !record.detached
        }));
        assert!(candidates.iter().any(|record| {
            record.process_id == detached_request.process_id
                && record.status == ProcessStatus::Running
                && record.pid == Some(77)
                && record.process_identity.as_deref() == Some("birth:77")
                && record.detached
                && record.completion_notification_required
                && !record.completion_notification_delivered
        }));

        for candidate in candidates {
            let owner = ProcessOwner {
                profile_id: candidate.profile_id.clone(),
                session_id: candidate.session_id.clone(),
            };
            let lost = restarted
                .mark_process_lost(&owner, &candidate.process_id, candidate.status)
                .unwrap();
            assert_eq!(lost.status, ProcessStatus::Lost);
            assert_eq!(lost.detached, candidate.detached);
            assert_eq!(lost.termination_source.as_deref(), Some("backend_restart"));
            assert!(lost.finished_at.is_some());
        }
        assert!(restarted.list_recovery_candidates().unwrap().is_empty());
        assert_eq!(
            restarted
                .get_process(&completed.owner, &completed_request.process_id)
                .unwrap()
                .status,
            ProcessStatus::Exited
        );
    }

    #[test]
    fn released_runtime_lease_fences_all_process_ledger_writes() {
        let fixture = Fixture::new();
        let binding = fixture.binding("default", "released-lease");
        let mut existing_request = create_request(&binding, 11, true);
        existing_request.async_delivery = Some(AsyncToolDeliveryRequest {
            kind: AsyncToolDeliveryKind::Completion,
            watch_patterns: Vec::new(),
        });
        fixture
            .service
            .create_process(&binding.owner, &existing_request)
            .unwrap();

        let lease = fixture
            .service
            .acquire_runtime_lease("runtime_11111111111111111111111111111111")
            .unwrap();
        fixture.service.release_runtime_lease(&lease);

        let mut late_async_request = create_request(&binding, 12, true);
        late_async_request.async_delivery = Some(AsyncToolDeliveryRequest {
            kind: AsyncToolDeliveryKind::Completion,
            watch_patterns: Vec::new(),
        });
        assert_eq!(
            fixture
                .service
                .create_process(&binding.owner, &late_async_request)
                .unwrap_err(),
            ProcessStoreError::StorageUnavailable
        );
        assert_eq!(
            fixture
                .service
                .reserve_process(&binding.owner, &late_async_request, 16)
                .unwrap_err(),
            ProcessStoreError::StorageUnavailable
        );
        assert_eq!(
            fixture
                .service
                .transition_process(
                    &binding.owner,
                    &existing_request.process_id,
                    ProcessStatus::Starting,
                    &ProcessTransition::Running {
                        pid: 101,
                        process_identity: Some("birth:101".to_owned()),
                    },
                )
                .unwrap_err(),
            ProcessStoreError::StorageUnavailable
        );

        let deliveries = fixture.service.pending_async_tool_deliveries().unwrap();
        assert_eq!(deliveries.len(), 1);
        assert_eq!(
            deliveries[0].process.process_id,
            existing_request.process_id
        );
        assert!(matches!(
            fixture
                .service
                .settle_async_tool_delivery(&deliveries[0], AsyncToolDeliveryTrigger::Completion,),
            Err(RunError::EngineUnavailable)
        ));
        assert_eq!(
            fixture
                .service
                .mark_completion_notification_delivered(
                    &binding.owner,
                    &existing_request.process_id,
                )
                .unwrap_err(),
            ProcessStoreError::StorageUnavailable
        );
        let cutoff = (OffsetDateTime::now_utc() + time::Duration::minutes(1))
            .format(&Rfc3339)
            .unwrap();
        assert_eq!(
            fixture
                .service
                .prune_finished_processes(&cutoff)
                .unwrap_err(),
            ProcessStoreError::StorageUnavailable
        );

        assert_eq!(
            fixture
                .service
                .get_process(&binding.owner, &existing_request.process_id)
                .unwrap()
                .status,
            ProcessStatus::Starting
        );
        assert_eq!(
            fixture
                .service
                .pending_async_tool_deliveries()
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn sqlite_trigger_rejects_terminal_status_reversal() {
        let fixture = Fixture::new();
        let binding = fixture.binding("default", "trigger");
        let request = create_request(&binding, 6, false);
        fixture
            .service
            .create_process(&binding.owner, &request)
            .unwrap();
        fixture
            .service
            .mark_process_lost(&binding.owner, &request.process_id, ProcessStatus::Starting)
            .unwrap();
        let connection =
            schema::open(&fixture.home.path().join(".synthchat/sessions-v1.db")).unwrap();
        assert!(
            connection
                .execute(
                    "UPDATE terminal_processes SET status = 'running', pid = 1, finished_at = NULL, \
                        completion_reason = NULL, termination_source = NULL WHERE process_id = ?1",
                    [&request.process_id],
                )
                .is_err()
        );
    }

    #[test]
    fn pruning_keeps_active_and_undelivered_notification_records() {
        let fixture = Fixture::new();
        let notified = fixture.binding("default", "prune-notified");
        let active = fixture.binding("default", "prune-active");
        let notified_request = create_request(&notified, 7, true);
        let active_request = create_request(&active, 8, false);
        fixture
            .service
            .create_process(&notified.owner, &notified_request)
            .unwrap();
        fixture
            .service
            .transition_process(
                &notified.owner,
                &notified_request.process_id,
                ProcessStatus::Starting,
                &ProcessTransition::Running {
                    pid: 99,
                    process_identity: Some("birth:99".to_owned()),
                },
            )
            .unwrap();
        fixture
            .service
            .transition_process(
                &notified.owner,
                &notified_request.process_id,
                ProcessStatus::Running,
                &ProcessTransition::Killed {
                    exit_code: Some(137),
                    completion_reason: "terminated by user".to_owned(),
                    termination_source: "user".to_owned(),
                },
            )
            .unwrap();
        fixture
            .service
            .create_process(&active.owner, &active_request)
            .unwrap();
        let cutoff = (OffsetDateTime::now_utc() + time::Duration::minutes(1))
            .format(&Rfc3339)
            .unwrap();

        assert_eq!(
            fixture.service.prune_finished_processes(&cutoff).unwrap(),
            0
        );
        let delivered = fixture
            .service
            .mark_completion_notification_delivered(&notified.owner, &notified_request.process_id)
            .unwrap();
        assert!(delivered.completion_notification_delivered);
        assert_eq!(
            fixture
                .service
                .mark_completion_notification_delivered(
                    &notified.owner,
                    &notified_request.process_id,
                )
                .unwrap(),
            delivered
        );
        assert_eq!(
            fixture.service.prune_finished_processes(&cutoff).unwrap(),
            1
        );
        assert_eq!(
            fixture
                .service
                .get_process(&notified.owner, &notified_request.process_id)
                .unwrap_err(),
            ProcessStoreError::NotFound
        );
        assert_eq!(
            fixture
                .service
                .get_process(&active.owner, &active_request.process_id)
                .unwrap()
                .status,
            ProcessStatus::Starting
        );
        assert_eq!(
            fixture
                .service
                .prune_finished_processes("not-a-timestamp")
                .unwrap_err(),
            ProcessStoreError::InvalidRequest
        );
    }

    #[test]
    fn generated_process_ids_are_fixed_lowercase_hex() {
        let id = format!("process_{}", Uuid::new_v4().simple());
        assert!(validate_process_id(&id).is_ok());
    }
}
