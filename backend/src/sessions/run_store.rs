use std::collections::HashSet;

use rusqlite::{
    Connection, OptionalExtension, Row, Transaction, TransactionBehavior, params, types::Type,
};
use serde::Serialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use uuid::Uuid;

use crate::{
    profiles::Versioned,
    runs::{
        ActiveRun, ActiveRunList, CancelDisposition, CreateRun, MAX_RUN_EVENTS, PendingAction,
        QueuedRunClaim, Run, RunAccepted, RunDisposition, RunError, RunEventBatch, RunEventRecord,
        RunProblem, RunStatus, event_data,
    },
};

use super::{
    ClarificationContinuationBinding, ClarificationError, ClarificationRequest,
    ClarificationResolution, ClarificationResolutionDisposition, ClarificationResolvedBy,
    ClarificationState, CommitMessage, CompleteRunPlan, Message, MessagePart, MessageRole,
    ProviderContextMessage, ProviderTurnFinish, ProviderTurnPlan, RawToolCallPlan, RuntimeLease,
    RuntimeLeaseState, SessionError, SessionService, StoredClarification, StoredProviderTurn,
    StoredToolApproval, StoredToolInvocation, ToolApprovalDecision, ToolApprovalError,
    ToolApprovalExecutionBinding, ToolApprovalRequest, ToolApprovalResolution,
    ToolApprovalResolutionDisposition, ToolApprovalResolvedBy, ToolApprovalState, ToolCall,
    ToolInvocationOrigin, ToolInvocationStatus, Usage,
    process_store::{
        AsyncToolDeliveryDisposition, AsyncToolDeliveryKind, AsyncToolDeliveryTrigger,
        PendingAsyncToolDelivery, ProcessStatus,
    },
    schema,
    store::{
        close_current_version, current_session_tx, insert_current_version, message_by_id_tx,
        new_revision, next_change, next_timestamp, now_timestamp, searchable_text, truncate_chars,
        validate_message,
    },
};

const MAX_ACTIVE_RUNS: i64 = 16;
const MAX_ACTIVE_RUN_DISCOVERY_ITEMS: usize = MAX_ACTIVE_RUNS as usize;
const MAX_CONTEXT_MESSAGES: i64 = 256;
const MAX_CONTEXT_CHARS: usize = 4_000_000;
const MAX_CLIENT_REQUEST_ID_BYTES: usize = 128;
const MAX_IDEMPOTENCY_KEY_BYTES: usize = 128;
const MAX_RUN_ID_BYTES: usize = 128;
const MAX_EVENT_NAME_BYTES: usize = 64;
const MAX_MESSAGE_TEXT_CHARS: usize = 1_000_000;
const MAX_FILE_IDS: usize = 20;
const MAX_PROVIDER_TURNS: u32 = 64;
const MAX_TOOL_CALLS_PER_TURN: usize = 64;
const MAX_TOOL_CALL_ID_BYTES: usize = 256;
const MAX_TOOL_NAME_BYTES: usize = 256;
const MAX_TOOL_ARGUMENT_BYTES: usize = 64 * 1024;
const MAX_TOOL_RESULT_BYTES: usize = 4 * 1024 * 1024;
const MAX_APPROVAL_ID_BYTES: usize = 128;
const MAX_APPROVAL_REASON_CHARS: usize = 2_000;
const MAX_CLARIFICATION_ID_BYTES: usize = 128;
const MAX_CLARIFICATION_QUESTION_CHARS: usize = 2_000;
const MAX_CLARIFICATION_CHOICES: usize = 4;
const MAX_CLARIFICATION_CHOICE_CHARS: usize = 500;
const MAX_CLARIFICATION_ANSWER_CHARS: usize = 10_000;
const RUNTIME_LEASE_TTL_MILLIS: i64 = 15_000;

struct ToolInvocationTerminal<'a> {
    status: ToolInvocationStatus,
    raw_json: &'a str,
    provider_content: &'a str,
    public_event: Option<(&'a str, serde_json::Value)>,
}

impl SessionService {
    pub(crate) fn acquire_runtime_lease(&self, owner_id: &str) -> Result<RuntimeLease, RunError> {
        validate_runtime_owner_id(owner_id)?;
        self.begin_runtime_lease_acquisition();
        let ready = self.ready().map_err(run_from_session)?.clone();
        let _guard = ready
            .write_lock
            .lock()
            .map_err(|_| RunError::StorageUnavailable)?;
        let mut connection = schema::open(&ready.db_path).map_err(run_from_session)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(schema::map_sqlite)
            .map_err(run_from_session)?;
        let previous: Option<(i64, i64)> = transaction
            .query_row(
                "SELECT epoch, expires_at_unix_ms FROM runtime_leases \
                 WHERE lease_name = 'run-runtime'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(schema::map_sqlite)
            .map_err(run_from_session)?;
        let now_unix_ms = now_unix_millis()?;
        if previous.is_some_and(|(_, expires_at)| expires_at >= now_unix_ms) {
            return Err(RunError::EngineUnavailable);
        }
        let epoch = previous
            .map(|(epoch, _)| epoch)
            .unwrap_or(0)
            .checked_add(1)
            .ok_or(RunError::DataInvalid)?;
        let now = now_timestamp().map_err(run_from_session)?;
        let expires_at = now_unix_ms
            .checked_add(RUNTIME_LEASE_TTL_MILLIS)
            .ok_or(RunError::DataInvalid)?;
        transaction
            .execute(
                "INSERT INTO runtime_leases(lease_name, owner_id, epoch, expires_at_unix_ms, updated_at) \
                 VALUES('run-runtime', ?1, ?2, ?3, ?4) \
                 ON CONFLICT(lease_name) DO UPDATE SET owner_id = excluded.owner_id, \
                    epoch = excluded.epoch, expires_at_unix_ms = excluded.expires_at_unix_ms, \
                    updated_at = excluded.updated_at",
                params![owner_id, epoch, expires_at, now],
            )
            .map_err(schema::map_sqlite)
            .map_err(run_from_session)?;
        transaction
            .commit()
            .map_err(schema::map_sqlite)
            .map_err(run_from_session)?;
        let lease = RuntimeLease {
            owner_id: owner_id.to_owned(),
            epoch: u64::try_from(epoch).map_err(|_| RunError::DataInvalid)?,
        };
        self.hold_runtime_lease(lease.clone());
        Ok(lease)
    }

    pub(crate) fn renew_runtime_lease(&self, lease: &RuntimeLease) -> Result<(), RunError> {
        let result = (|| {
            if self.runtime_lease_state() != RuntimeLeaseState::Held(lease.clone()) {
                return Err(RunError::EngineUnavailable);
            }
            let ready = self.ready().map_err(run_from_session)?.clone();
            let _guard = ready
                .write_lock
                .lock()
                .map_err(|_| RunError::StorageUnavailable)?;
            if self.runtime_lease_state() != RuntimeLeaseState::Held(lease.clone()) {
                return Err(RunError::EngineUnavailable);
            }
            let connection = schema::open(&ready.db_path).map_err(run_from_session)?;
            let now = now_timestamp().map_err(run_from_session)?;
            let now_unix_ms = now_unix_millis()?;
            let expires_at = now_unix_ms
                .checked_add(RUNTIME_LEASE_TTL_MILLIS)
                .ok_or(RunError::DataInvalid)?;
            let changed = connection
                .execute(
                    "UPDATE runtime_leases SET expires_at_unix_ms = ?1, updated_at = ?2 \
                     WHERE lease_name = 'run-runtime' AND owner_id = ?3 AND epoch = ?4 \
                        AND expires_at_unix_ms >= ?5",
                    params![
                        expires_at,
                        now,
                        lease.owner_id,
                        i64::try_from(lease.epoch).map_err(|_| RunError::DataInvalid)?,
                        now_unix_ms,
                    ],
                )
                .map_err(schema::map_sqlite)
                .map_err(run_from_session)?;
            if changed == 1 {
                Ok(())
            } else {
                Err(RunError::EngineUnavailable)
            }
        })();
        if result.is_err() {
            self.fence_runtime_lease(lease);
        }
        result
    }

    pub(crate) fn release_runtime_lease(&self, lease: &RuntimeLease) {
        if let Ok(ready) = self.ready() {
            let _guard = ready
                .write_lock
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Ok(connection) = schema::open(&ready.db_path) {
                let _ = connection.execute(
                    "UPDATE runtime_leases SET expires_at_unix_ms = 0 \
                     WHERE lease_name = 'run-runtime' AND owner_id = ?1 AND epoch = ?2",
                    params![
                        lease.owner_id,
                        i64::try_from(lease.epoch).unwrap_or(i64::MAX)
                    ],
                );
            }
            self.fence_runtime_lease(lease);
        } else {
            self.fence_runtime_lease(lease);
        }
    }

    pub(super) fn require_runtime_lease_tx(
        &self,
        transaction: &Transaction<'_>,
    ) -> Result<(), RunError> {
        let lease = match self.runtime_lease_state() {
            RuntimeLeaseState::Unmanaged => return Ok(()),
            RuntimeLeaseState::Held(lease) => lease,
            RuntimeLeaseState::Fenced => return Err(RunError::EngineUnavailable),
        };
        let validation = (|| {
            transaction
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM runtime_leases \
                     WHERE lease_name = 'run-runtime' AND owner_id = ?1 AND epoch = ?2 \
                        AND expires_at_unix_ms >= ?3)",
                    params![
                        lease.owner_id,
                        i64::try_from(lease.epoch).map_err(|_| RunError::DataInvalid)?,
                        now_unix_millis()?
                    ],
                    |row| row.get::<_, bool>(0),
                )
                .map_err(schema::map_sqlite)
                .map_err(run_from_session)
        })();
        match validation {
            Ok(true) => Ok(()),
            Ok(false) => {
                self.fence_runtime_lease(&lease);
                Err(RunError::EngineUnavailable)
            }
            Err(error) => {
                self.fence_runtime_lease(&lease);
                Err(error)
            }
        }
    }

    pub(crate) fn lookup_run_replay(
        &self,
        session_id: &str,
        request: &CreateRun,
        idempotency_key: &str,
    ) -> Result<Option<RunAccepted>, RunError> {
        validate_create_run(session_id, request, idempotency_key)?;
        let connection = self.run_connection()?;
        let canonical_path = canonical_run_path(session_id);
        let fingerprint = request_fingerprint(request)?;
        lookup_replay(
            &connection,
            session_id,
            &canonical_path,
            idempotency_key,
            &fingerprint,
        )
    }

    pub(crate) fn create_run(
        &self,
        session_id: &str,
        request: &CreateRun,
        idempotency_key: &str,
        model_label: &str,
    ) -> Result<RunAccepted, RunError> {
        validate_create_run(session_id, request, idempotency_key)?;
        if model_label.is_empty() || model_label.chars().count() > 500 {
            return Err(RunError::InvalidRequest);
        }
        let ready = self.ready().map_err(run_from_session)?.clone();
        let _guard = ready
            .write_lock
            .lock()
            .map_err(|_| RunError::StorageUnavailable)?;
        let mut connection = schema::open(&ready.db_path).map_err(run_from_session)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(schema::map_sqlite)
            .map_err(run_from_session)?;
        self.require_runtime_lease_tx(&transaction)?;

        let canonical_path = canonical_run_path(session_id);
        let fingerprint = request_fingerprint(request)?;
        if let Some(replayed) = lookup_replay_tx(
            &transaction,
            session_id,
            &canonical_path,
            idempotency_key,
            &fingerprint,
        )? {
            transaction
                .commit()
                .map_err(schema::map_sqlite)
                .map_err(run_from_session)?;
            return Ok(replayed);
        }

        let current = current_session_tx(&transaction, session_id)
            .map_err(run_from_session)?
            .ok_or(RunError::NotFound)?;
        if current.value.archived {
            return Err(RunError::SessionArchived);
        }
        if let Some(workspace_id) = request.workspace_id.as_deref() {
            validate_workspace_id(workspace_id)?;
            let belongs_to_profile: bool = transaction
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM workspaces WHERE id = ?1 AND profile_id = ?2)",
                    params![workspace_id, current.value.profile_id],
                    |row| row.get(0),
                )
                .map_err(schema::map_sqlite)
                .map_err(run_from_session)?;
            if !belongs_to_profile {
                return Err(RunError::InvalidRequest);
            }
        }
        let active: bool = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM runs WHERE session_id = ?1 AND status IN (\
                    'queued', 'running', 'waitingApproval', 'waitingClarification', 'cancelling'\
                 ))",
                [session_id],
                |row| row.get(0),
            )
            .map_err(schema::map_sqlite)
            .map_err(run_from_session)?;
        let active_count: i64 = transaction
            .query_row(
                "SELECT COUNT(*) FROM runs WHERE profile_id = ?1 AND status IN (\
                    'queued', 'running', 'waitingApproval', 'waitingClarification', 'cancelling'\
                 )",
                [&current.value.profile_id],
                |row| row.get(0),
            )
            .map_err(schema::map_sqlite)
            .map_err(run_from_session)?;
        if active_count >= MAX_ACTIVE_RUNS {
            return Err(RunError::CapacityExceeded);
        }

        let user_commit = CommitMessage {
            role: MessageRole::User,
            parts: vec![MessagePart::Text {
                text: request.message.text.clone(),
            }],
            reasoning: None,
            tool_calls: Vec::new(),
            usage: None,
            model: Some(model_label.to_owned()),
        };
        let (user_message, updated_session) =
            insert_message_tx(&transaction, session_id, &current, &user_commit, None)?;
        let run_id = format!("run_{}", Uuid::new_v4().simple());
        let queue_item_id = active.then(|| format!("queue_{}", Uuid::new_v4().simple()));
        let status = if active {
            RunStatus::Queued
        } else {
            RunStatus::Running
        };
        let created_at = user_message.created_at.clone();
        transaction
            .execute(
                "INSERT INTO runs(\
                    id, session_id, profile_id, status, last_sequence, user_message_id,\
                    message_id, queue_item_id, usage_json, error_json, pending_action_json, workspace_id,\
                    created_at, updated_at, terminal_at\
                 ) VALUES(?1, ?2, ?3, ?4, 0, ?5, NULL, ?6, NULL, NULL, NULL, ?7, ?8, ?8, NULL)",
                params![
                    run_id,
                    session_id,
                    current.value.profile_id,
                    status.as_str(),
                    user_message.id,
                    queue_item_id,
                    request.workspace_id,
                    created_at
                ],
            )
            .map_err(schema::map_sqlite)
            .map_err(run_from_session)?;
        if let Some(queue_item_id) = queue_item_id.as_deref() {
            transaction
                .execute(
                    "INSERT INTO run_queue(\
                        queue_item_id, run_id, session_id, profile_id, request_json, created_at\
                     ) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        queue_item_id,
                        run_id,
                        session_id,
                        current.value.profile_id,
                        serialize_json(request)?,
                        created_at,
                    ],
                )
                .map_err(schema::map_sqlite)
                .map_err(run_from_session)?;
            insert_event_tx(
                &transaction,
                &run_id,
                session_id,
                "run.queued",
                event_data(json!({"queueItemId": queue_item_id}))?,
                &created_at,
            )?;
        } else {
            insert_event_tx(
                &transaction,
                &run_id,
                session_id,
                "run.started",
                event_data(json!({"profileId": current.value.profile_id}))?,
                &created_at,
            )?;
        }
        transaction
            .execute(
                "INSERT INTO idempotency_records(\
                    method, canonical_path, idempotency_key, request_fingerprint, resource_id,\
                    response_json, created_at\
                 ) VALUES('POST', ?1, ?2, ?3, ?4, NULL, ?5)",
                params![
                    canonical_path,
                    idempotency_key,
                    fingerprint,
                    run_id,
                    created_at
                ],
            )
            .map_err(schema::map_sqlite)
            .map_err(run_from_session)?;
        let run = run_by_id_tx(&transaction, &run_id)?.ok_or(RunError::DataInvalid)?;
        transaction
            .commit()
            .map_err(schema::map_sqlite)
            .map_err(run_from_session)?;
        Ok(RunAccepted {
            run,
            disposition: if active {
                RunDisposition::Queued
            } else {
                RunDisposition::Started
            },
            queue_item_id,
            user_message,
            session_revision: updated_session.value.revision,
        })
    }

    pub(crate) fn get_run(&self, run_id: &str) -> Result<Run, RunError> {
        validate_run_id(run_id)?;
        let connection = self.run_connection()?;
        run_by_id(&connection, run_id)?.ok_or(RunError::NotFound)
    }

    pub(crate) fn list_active_runs(
        &self,
        profile_id: &str,
        session_id: Option<&str>,
    ) -> Result<ActiveRunList, RunError> {
        validate_profile_id(profile_id)?;
        if let Some(session_id) = session_id {
            validate_session_id(session_id)?;
        }

        let ready = self.ready().map_err(run_from_session)?.clone();
        let mut connection = schema::open(&ready.db_path).map_err(run_from_session)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Deferred)
            .map_err(schema::map_sqlite)
            .map_err(run_from_session)?;
        let rows = {
            let mut statement = transaction
                .prepare(
                    "SELECT id, queue_item_id, user_message_id, session_id FROM runs \
                     WHERE profile_id = ?1 AND (?2 IS NULL OR session_id = ?2) AND status IN (\
                        'queued', 'running', 'waitingApproval', 'waitingClarification', 'cancelling'\
                     ) ORDER BY created_at ASC, id ASC LIMIT ?3",
                )
                .map_err(schema::map_sqlite)
                .map_err(run_from_session)?;
            let limit = i64::try_from(MAX_ACTIVE_RUN_DISCOVERY_ITEMS + 1)
                .map_err(|_| RunError::DataInvalid)?;
            let mapped = statement
                .query_map(params![profile_id, session_id, limit], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                })
                .map_err(schema::map_sqlite)
                .map_err(run_from_session)?;
            mapped
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| RunError::DataInvalid)?
        };
        if rows.len() > MAX_ACTIVE_RUN_DISCOVERY_ITEMS {
            return Err(RunError::DataInvalid);
        }

        let mut items = Vec::with_capacity(rows.len());
        for (run_id, queue_item_id, user_message_id, stored_session_id) in rows {
            validate_run_id(&run_id).map_err(|_| RunError::DataInvalid)?;
            validate_session_id(&stored_session_id).map_err(|_| RunError::DataInvalid)?;
            if let Some(queue_item_id) = queue_item_id.as_deref() {
                validate_queue_item_id(queue_item_id).map_err(|_| RunError::DataInvalid)?;
            }
            let run = run_by_id_tx(&transaction, &run_id)?.ok_or(RunError::DataInvalid)?;
            if run.profile_id != profile_id
                || run.session_id != stored_session_id
                || run.status.is_terminal()
                || (run.status == RunStatus::Queued) != queue_item_id.is_some()
            {
                return Err(RunError::DataInvalid);
            }
            let session = current_session_tx(&transaction, &stored_session_id)
                .map_err(run_from_session)?
                .ok_or(RunError::DataInvalid)?;
            if session.value.profile_id != profile_id {
                return Err(RunError::DataInvalid);
            }
            let user_message = message_by_id_tx(&transaction, &user_message_id)
                .map_err(run_from_session)?
                .ok_or(RunError::DataInvalid)?;
            if user_message.role != MessageRole::User
                || user_message.session_id != stored_session_id
                || session.value.revision.is_empty()
                || session.value.revision.len() > 126
            {
                return Err(RunError::DataInvalid);
            }
            items.push(ActiveRun {
                run,
                queue_item_id,
                user_message,
                session_revision: session.value.revision,
            });
        }
        transaction
            .commit()
            .map_err(schema::map_sqlite)
            .map_err(run_from_session)?;
        Ok(ActiveRunList { items })
    }

    pub(crate) fn queued_session_ids(&self) -> Result<Vec<String>, RunError> {
        let connection = self.run_connection()?;
        let mut statement = connection
            .prepare(
                "SELECT DISTINCT session_id FROM run_queue \
                 ORDER BY created_at ASC, session_id ASC",
            )
            .map_err(schema::map_sqlite)
            .map_err(run_from_session)?;
        statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(schema::map_sqlite)
            .map_err(run_from_session)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| RunError::DataInvalid)
    }

    pub(crate) fn claim_next_queued_run(
        &self,
        session_id: &str,
    ) -> Result<Option<QueuedRunClaim>, RunError> {
        validate_session_id(session_id)?;
        let ready = self.ready().map_err(run_from_session)?.clone();
        let _guard = ready
            .write_lock
            .lock()
            .map_err(|_| RunError::StorageUnavailable)?;
        let mut connection = schema::open(&ready.db_path).map_err(run_from_session)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(schema::map_sqlite)
            .map_err(run_from_session)?;
        self.require_runtime_lease_tx(&transaction)?;
        let has_executor: bool = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM runs WHERE session_id = ?1 AND status IN (\
                    'running', 'waitingApproval', 'waitingClarification', 'cancelling'\
                 ))",
                [session_id],
                |row| row.get(0),
            )
            .map_err(schema::map_sqlite)
            .map_err(run_from_session)?;
        if has_executor {
            transaction
                .commit()
                .map_err(schema::map_sqlite)
                .map_err(run_from_session)?;
            return Ok(None);
        }
        let queued = transaction
            .query_row(
                "SELECT queue.queue_item_id, queue.run_id, queue.profile_id, queue.request_json \
                 FROM run_queue queue JOIN runs run ON run.id = queue.run_id \
                 WHERE queue.session_id = ?1 AND run.status = 'queued' \
                    AND run.queue_item_id = queue.queue_item_id \
                 ORDER BY queue.created_at ASC, queue.run_id ASC LIMIT 1",
                [session_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()
            .map_err(schema::map_sqlite)
            .map_err(run_from_session)?;
        let Some((queue_item_id, run_id, profile_id, request_json)) = queued else {
            transaction
                .commit()
                .map_err(schema::map_sqlite)
                .map_err(run_from_session)?;
            return Ok(None);
        };
        validate_queue_item_id(&queue_item_id).map_err(|_| RunError::DataInvalid)?;
        let request: CreateRun =
            serde_json::from_str(&request_json).map_err(|_| RunError::DataInvalid)?;
        validate_create_run(session_id, &request, "queued-request")?;
        let occurred_at = now_timestamp().map_err(run_from_session)?;
        let deleted = transaction
            .execute(
                "DELETE FROM run_queue WHERE queue_item_id = ?1 AND run_id = ?2",
                params![queue_item_id, run_id],
            )
            .map_err(schema::map_sqlite)
            .map_err(run_from_session)?;
        let changed = transaction
            .execute(
                "UPDATE runs SET status = 'running', queue_item_id = NULL, updated_at = ?1 \
                 WHERE id = ?2 AND session_id = ?3 AND profile_id = ?4 AND status = 'queued' \
                    AND queue_item_id = ?5",
                params![occurred_at, run_id, session_id, profile_id, queue_item_id],
            )
            .map_err(schema::map_sqlite)
            .map_err(run_from_session)?;
        if deleted != 1 || changed != 1 {
            return Err(RunError::DataInvalid);
        }
        insert_event_tx(
            &transaction,
            &run_id,
            session_id,
            "run.started",
            event_data(json!({"profileId": profile_id}))?,
            &occurred_at,
        )?;
        let run = run_by_id_tx(&transaction, &run_id)?.ok_or(RunError::DataInvalid)?;
        transaction
            .commit()
            .map_err(schema::map_sqlite)
            .map_err(run_from_session)?;
        Ok(Some(QueuedRunClaim { run, request }))
    }

    pub(crate) fn begin_assistant_message(
        &self,
        run_id: &str,
        message_id: &str,
    ) -> Result<Run, RunError> {
        if message_id.is_empty() || message_id.len() > MAX_RUN_ID_BYTES {
            return Err(RunError::InvalidRequest);
        }
        self.mutate_run(run_id, |transaction, run| {
            require_running(run)?;
            if run.message_id.is_some() {
                return Err(RunError::DataInvalid);
            }
            let occurred_at = now_timestamp().map_err(run_from_session)?;
            transaction
                .execute(
                    "UPDATE runs SET message_id = ?1, updated_at = ?2 WHERE id = ?3",
                    params![message_id, occurred_at, run_id],
                )
                .map_err(schema::map_sqlite)
                .map_err(run_from_session)?;
            insert_event_tx(
                transaction,
                run_id,
                &run.session_id,
                "message.started",
                event_data(json!({"messageId": message_id, "role": "assistant"}))?,
                &occurred_at,
            )?;
            Ok(())
        })
    }

    pub(crate) fn append_run_delta(
        &self,
        run_id: &str,
        event_name: &str,
        message_id: &str,
        delta: &str,
    ) -> Result<Run, RunError> {
        if !matches!(event_name, "message.delta" | "reasoning.delta")
            || delta.is_empty()
            || event_name.len() > MAX_EVENT_NAME_BYTES
        {
            return Err(RunError::InvalidRequest);
        }
        self.mutate_run(run_id, |transaction, run| {
            require_running(run)?;
            if run.message_id.as_deref() != Some(message_id) {
                return Err(RunError::DataInvalid);
            }
            let occurred_at = now_timestamp().map_err(run_from_session)?;
            insert_event_tx(
                transaction,
                run_id,
                &run.session_id,
                event_name,
                event_data(json!({"messageId": message_id, "delta": delta}))?,
                &occurred_at,
            )
        })
    }

    pub(crate) fn update_run_usage(&self, run_id: &str, usage: &Usage) -> Result<Run, RunError> {
        validate_usage(usage)?;
        self.mutate_run(run_id, |transaction, run| {
            require_running(run)?;
            if let Some(previous) = &run.usage
                && (usage.prompt_tokens < previous.prompt_tokens
                    || usage.completion_tokens < previous.completion_tokens
                    || usage.total_tokens < previous.total_tokens)
            {
                return Err(RunError::DataInvalid);
            }
            let occurred_at = now_timestamp().map_err(run_from_session)?;
            let usage_json = serialize_json(usage)?;
            transaction
                .execute(
                    "UPDATE runs SET usage_json = ?1, updated_at = ?2 WHERE id = ?3",
                    params![usage_json, occurred_at, run_id],
                )
                .map_err(schema::map_sqlite)
                .map_err(run_from_session)?;
            insert_event_tx(
                transaction,
                run_id,
                &run.session_id,
                "usage.updated",
                event_data(usage)?,
                &occurred_at,
            )
        })
    }

    pub(crate) fn complete_run(
        &self,
        run_id: &str,
        plan: CompleteRunPlan,
    ) -> Result<Run, RunError> {
        validate_usage(&plan.usage)?;
        let ready = self.ready().map_err(run_from_session)?.clone();
        let _guard = ready
            .write_lock
            .lock()
            .map_err(|_| RunError::StorageUnavailable)?;
        let mut connection = schema::open(&ready.db_path).map_err(run_from_session)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(schema::map_sqlite)
            .map_err(run_from_session)?;
        self.require_runtime_lease_tx(&transaction)?;
        let run = run_by_id_tx(&transaction, run_id)?.ok_or(RunError::NotFound)?;
        require_running(&run)?;
        if run.message_id.as_deref() != Some(plan.message_id.as_str()) {
            return Err(RunError::DataInvalid);
        }
        let current = current_session_tx(&transaction, &run.session_id)
            .map_err(run_from_session)?
            .ok_or(RunError::IdempotentResourceDeleted)?;
        let commit = CommitMessage {
            role: MessageRole::Assistant,
            parts: vec![MessagePart::Text { text: plan.text }],
            reasoning: plan.reasoning,
            tool_calls: plan.tool_calls,
            usage: Some(plan.usage.clone()),
            model: Some(plan.model_label),
        };
        let (message, updated_session) = insert_message_tx(
            &transaction,
            &run.session_id,
            &current,
            &commit,
            Some(&plan.message_id),
        )?;
        let occurred_at = message.created_at.clone();
        insert_event_tx(
            &transaction,
            run_id,
            &run.session_id,
            "message.completed",
            event_data(json!({
                "message": message,
                "sessionRevision": updated_session.value.revision,
            }))?,
            &occurred_at,
        )?;
        insert_event_tx(
            &transaction,
            run_id,
            &run.session_id,
            "run.completed",
            event_data(json!({"usage": plan.usage, "messageId": plan.message_id}))?,
            &occurred_at,
        )?;
        let usage_json = serialize_json(&plan.usage)?;
        transaction
            .execute(
                "UPDATE runs SET status = 'completed', usage_json = ?1, error_json = NULL,\
                    pending_action_json = NULL, updated_at = ?2, terminal_at = ?2 WHERE id = ?3",
                params![usage_json, occurred_at, run_id],
            )
            .map_err(schema::map_sqlite)
            .map_err(run_from_session)?;
        let completed = run_by_id_tx(&transaction, run_id)?.ok_or(RunError::DataInvalid)?;
        transaction
            .commit()
            .map_err(schema::map_sqlite)
            .map_err(run_from_session)?;
        Ok(completed)
    }

    pub(crate) fn fail_run(&self, run_id: &str, problem: &RunProblem) -> Result<Run, RunError> {
        self.terminal_run(
            run_id,
            RunStatus::Failed,
            "run.failed",
            json!({"error": problem}),
            Some(problem),
        )
    }

    pub(crate) fn cancel_run_terminal(&self, run_id: &str, reason: &str) -> Result<Run, RunError> {
        self.terminal_run(
            run_id,
            RunStatus::Cancelled,
            "run.cancelled",
            json!({"reason": reason}),
            None,
        )
    }

    pub(crate) fn request_run_cancel(
        &self,
        run_id: &str,
    ) -> Result<(Run, CancelDisposition), RunError> {
        self.mutate_run_with(run_id, |transaction, run| {
            if run.status.is_terminal() {
                return Ok(CancelDisposition::AlreadyTerminal);
            }
            if matches!(run.status, RunStatus::Queued) {
                let queue_item_id: String = transaction
                    .query_row(
                        "SELECT queue_item_id FROM runs WHERE id = ?1",
                        [run_id],
                        |row| row.get(0),
                    )
                    .map_err(schema::map_sqlite)
                    .map_err(run_from_session)?;
                validate_queue_item_id(&queue_item_id).map_err(|_| RunError::DataInvalid)?;
                let updated_at = now_timestamp().map_err(run_from_session)?;
                let deleted = transaction
                    .execute(
                        "DELETE FROM run_queue WHERE run_id = ?1 AND queue_item_id = ?2",
                        params![run_id, queue_item_id],
                    )
                    .map_err(schema::map_sqlite)
                    .map_err(run_from_session)?;
                if deleted != 1 {
                    return Err(RunError::DataInvalid);
                }
                insert_event_tx(
                    transaction,
                    run_id,
                    &run.session_id,
                    "run.cancelled",
                    event_data(json!({"reason": "cancelled while queued"}))?,
                    &updated_at,
                )?;
                let changed = transaction
                    .execute(
                        "UPDATE runs SET status = 'cancelled', queue_item_id = NULL, \
                            pending_action_json = NULL, updated_at = ?1, terminal_at = ?1 \
                         WHERE id = ?2 AND status = 'queued'",
                        params![updated_at, run_id],
                    )
                    .map_err(schema::map_sqlite)
                    .map_err(run_from_session)?;
                if changed != 1 {
                    return Err(RunError::DataInvalid);
                }
                return Ok(CancelDisposition::CancelledQueued);
            }
            let updated_at = now_timestamp().map_err(run_from_session)?;
            if run.status == RunStatus::WaitingApproval {
                let approval = pending_tool_approval_for_run(transaction, run_id)
                    .map_err(run_from_approval)?
                    .ok_or(RunError::DataInvalid)?;
                require_pending_approval_run(run, &approval).map_err(run_from_approval)?;
                resolve_denied_approval_tx(
                    transaction,
                    run,
                    &approval,
                    ToolApprovalResolvedBy::Cancellation,
                    None,
                    RunStatus::Cancelling,
                    &updated_at,
                )
                .map_err(run_from_approval)?;
                return Ok(CancelDisposition::SignalExecutor);
            }
            if run.status == RunStatus::WaitingClarification {
                let clarification = pending_clarification_for_run(transaction, run_id)
                    .map_err(run_from_clarification)?
                    .ok_or(RunError::DataInvalid)?;
                require_pending_clarification_run(run, &clarification)
                    .map_err(run_from_clarification)?;
                resolve_abandoned_clarification_tx(
                    transaction,
                    run,
                    &clarification,
                    ClarificationResolvedBy::Cancellation,
                    RunStatus::Cancelling,
                    &updated_at,
                )
                .map_err(run_from_clarification)?;
                return Ok(CancelDisposition::SignalExecutor);
            }
            transaction
                .execute(
                    "UPDATE runs SET status = 'cancelling', pending_action_json = NULL,\
                        updated_at = ?1 WHERE id = ?2 AND status NOT IN ('completed','cancelled','failed')",
                    params![updated_at, run_id],
                )
                .map_err(schema::map_sqlite)
                .map_err(run_from_session)?;
            Ok(CancelDisposition::SignalExecutor)
        })
    }

    pub(crate) fn run_event_batch(
        &self,
        run_id: &str,
        after_sequence: u64,
    ) -> Result<RunEventBatch, RunError> {
        validate_run_id(run_id)?;
        let connection = self.run_connection()?;
        let run = run_by_id(&connection, run_id)?.ok_or(RunError::NotFound)?;
        if after_sequence > run.last_sequence {
            return Err(RunError::InvalidEventId);
        }
        let earliest: Option<i64> = connection
            .query_row(
                "SELECT MIN(sequence) FROM run_events WHERE run_id = ?1",
                [run_id],
                |row| row.get(0),
            )
            .map_err(schema::map_sqlite)
            .map_err(run_from_session)?;
        let earliest = earliest
            .map(|value| u64::try_from(value).map_err(|_| RunError::DataInvalid))
            .transpose()?;
        if earliest.is_some_and(|value| after_sequence.saturating_add(1) < value) {
            return Err(RunError::EventHistoryExpired);
        }
        let mut statement = connection
            .prepare(
                "SELECT sequence, event_name, envelope_json FROM run_events \
                 WHERE run_id = ?1 AND sequence > ?2 ORDER BY sequence ASC",
            )
            .map_err(schema::map_sqlite)
            .map_err(run_from_session)?;
        let rows = statement
            .query_map(
                params![
                    run_id,
                    i64::try_from(after_sequence).map_err(|_| RunError::InvalidEventId)?
                ],
                event_from_row,
            )
            .map_err(schema::map_sqlite)
            .map_err(run_from_session)?;
        let mut events = Vec::new();
        for row in rows {
            events.push(row.map_err(|_| RunError::DataInvalid)?);
        }
        Ok(RunEventBatch {
            events,
            last_sequence: run.last_sequence,
            terminal: run.status.is_terminal()
                && !has_pending_async_tool_delivery(&connection, run_id)?,
        })
    }

    pub(crate) fn settle_async_tool_delivery(
        &self,
        delivery: &PendingAsyncToolDelivery,
        trigger: AsyncToolDeliveryTrigger,
    ) -> Result<AsyncToolDeliveryDisposition, RunError> {
        let ready = self.ready().map_err(run_from_session)?.clone();
        let _guard = ready
            .write_lock
            .lock()
            .map_err(|_| RunError::StorageUnavailable)?;
        let mut connection = schema::open(&ready.db_path).map_err(run_from_session)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(schema::map_sqlite)
            .map_err(run_from_session)?;
        self.require_runtime_lease_tx(&transaction)?;

        let current: Option<(String, Option<i64>, String, String)> = transaction
            .query_row(
                "SELECT p.status, p.exit_code, d.delivery_kind, d.state \
                 FROM terminal_processes p \
                 JOIN async_tool_deliveries d ON d.process_id = p.process_id \
                 WHERE p.process_id = ?1 AND p.profile_id = ?2 AND p.session_id = ?3 \
                    AND p.creator_run_id = ?4 AND p.call_id = ?5",
                params![
                    delivery.process.process_id,
                    delivery.process.profile_id,
                    delivery.process.session_id,
                    delivery.process.creator_run_id,
                    delivery.process.call_id,
                ],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(schema::map_sqlite)
            .map_err(run_from_session)?;
        let Some((status, exit_code, kind, state)) = current else {
            return Err(RunError::DataInvalid);
        };
        let status = ProcessStatus::try_from(status.as_str()).map_err(|_| RunError::DataInvalid)?;
        let exit_code = exit_code
            .map(|value| i32::try_from(value).map_err(|_| RunError::DataInvalid))
            .transpose()?;
        let kind =
            AsyncToolDeliveryKind::try_from(kind.as_str()).map_err(|_| RunError::DataInvalid)?;
        if state != "pending" {
            transaction
                .commit()
                .map_err(schema::map_sqlite)
                .map_err(run_from_session)?;
            return Ok(AsyncToolDeliveryDisposition::AlreadySettled);
        }
        if kind != delivery.kind {
            return Err(RunError::DataInvalid);
        }
        let (publish, settled_state, matched_pattern_count) = match trigger {
            AsyncToolDeliveryTrigger::Completion
                if kind == AsyncToolDeliveryKind::Completion && status.is_terminal() =>
            {
                (true, "delivered", None)
            }
            AsyncToolDeliveryTrigger::Watch {
                matched_pattern_count,
            } if kind == AsyncToolDeliveryKind::Watch
                && (1..=16).contains(&matched_pattern_count) =>
            {
                (true, "delivered", Some(matched_pattern_count))
            }
            AsyncToolDeliveryTrigger::WatchMissed
                if kind == AsyncToolDeliveryKind::Watch && status.is_terminal() =>
            {
                (false, "dismissed", None)
            }
            _ => return Err(RunError::DataInvalid),
        };
        let run = run_by_id_tx(&transaction, &delivery.process.creator_run_id)?
            .ok_or(RunError::DataInvalid)?;
        if run.profile_id != delivery.process.profile_id
            || run.session_id != delivery.process.session_id
        {
            return Err(RunError::DataInvalid);
        }
        let occurred_at = now_timestamp().map_err(run_from_session)?;
        if publish {
            let mut data = serde_json::Map::from_iter([
                ("callId".to_owned(), json!(delivery.process.call_id)),
                ("processId".to_owned(), json!(delivery.process.process_id)),
                ("delivery".to_owned(), json!(kind.as_str())),
                ("status".to_owned(), json!(status.as_str())),
            ]);
            if let Some(exit_code) = exit_code {
                data.insert("exitCode".to_owned(), json!(exit_code));
            }
            if let Some(matched_pattern_count) = matched_pattern_count {
                data.insert(
                    "matchedPatternCount".to_owned(),
                    json!(matched_pattern_count),
                );
            }
            insert_event_tx(
                &transaction,
                &run.id,
                &run.session_id,
                "tool.delivery",
                event_data(serde_json::Value::Object(data))?,
                &occurred_at,
            )?;
        }
        let changed = transaction
            .execute(
                "UPDATE async_tool_deliveries SET state = ?1, settled_at = ?2, \
                    matched_pattern_count = ?3 \
                 WHERE process_id = ?4 AND state = 'pending'",
                params![
                    settled_state,
                    occurred_at,
                    matched_pattern_count.map(i64::from),
                    delivery.process.process_id,
                ],
            )
            .map_err(schema::map_sqlite)
            .map_err(run_from_session)?;
        if changed != 1 {
            return Err(RunError::DataInvalid);
        }
        if status.is_terminal() {
            let changed = transaction
                .execute(
                    "UPDATE terminal_processes SET completion_notification_delivered = 1, \
                        updated_at = ?1 \
                     WHERE process_id = ?2 AND profile_id = ?3 AND session_id = ?4 \
                        AND creator_run_id = ?5 AND call_id = ?6 \
                        AND completion_notification_required = 1 \
                        AND completion_notification_delivered = 0",
                    params![
                        occurred_at,
                        delivery.process.process_id,
                        delivery.process.profile_id,
                        delivery.process.session_id,
                        delivery.process.creator_run_id,
                        delivery.process.call_id,
                    ],
                )
                .map_err(schema::map_sqlite)
                .map_err(run_from_session)?;
            if changed != 1 {
                return Err(RunError::DataInvalid);
            }
        }
        transaction
            .commit()
            .map_err(schema::map_sqlite)
            .map_err(run_from_session)?;
        Ok(AsyncToolDeliveryDisposition::Published)
    }

    pub(crate) fn record_provider_turn(
        &self,
        run_id: &str,
        plan: &ProviderTurnPlan,
    ) -> Result<StoredProviderTurn, RunError> {
        validate_provider_turn_plan(plan)?;
        let usage_json = serialize_json(&plan.usage)?;
        self.mutate_run_with(run_id, |transaction, run| {
            require_running(run)?;
            if run.message_id.as_deref() != Some(plan.assistant_message_id.as_str()) {
                return Err(RunError::DataInvalid);
            }
            let previous_turn: i64 = transaction
                .query_row(
                    "SELECT COALESCE(MAX(turn_index), 0) FROM run_turns WHERE run_id = ?1",
                    [run_id],
                    |row| row.get(0),
                )
                .map_err(schema::map_sqlite)
                .map_err(run_from_session)?;
            let expected_turn = previous_turn.checked_add(1).ok_or(RunError::DataInvalid)?;
            if i64::from(plan.turn_index) != expected_turn {
                return Err(RunError::DataInvalid);
            }
            if previous_turn > 0 {
                let previous_finish: String = transaction
                    .query_row(
                        "SELECT finish_reason FROM run_turns \
                         WHERE run_id = ?1 AND turn_index = ?2",
                        params![run_id, previous_turn],
                        |row| row.get(0),
                    )
                    .map_err(schema::map_sqlite)
                    .map_err(run_from_session)?;
                if previous_finish != ProviderTurnFinish::ToolCalls.as_str() {
                    return Err(RunError::DataInvalid);
                }
            }
            let unfinished: bool = transaction
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM tool_invocations \
                     WHERE run_id = ?1 AND status IN ('planned', 'running'))",
                    [run_id],
                    |row| row.get(0),
                )
                .map_err(schema::map_sqlite)
                .map_err(run_from_session)?;
            if unfinished {
                return Err(RunError::DataInvalid);
            }
            for call in &plan.tool_calls {
                let duplicate: bool = transaction
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM tool_invocations \
                         WHERE run_id = ?1 AND call_id = ?2)",
                        params![run_id, call.call_id],
                        |row| row.get(0),
                    )
                    .map_err(schema::map_sqlite)
                    .map_err(run_from_session)?;
                if duplicate {
                    return Err(RunError::DataInvalid);
                }
            }

            let occurred_at = now_timestamp().map_err(run_from_session)?;
            transaction
                .execute(
                    "INSERT INTO run_turns(\
                        run_id, turn_index, assistant_message_id, content, reasoning,\
                        finish_reason, usage_json, created_at, updated_at\
                     ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
                    params![
                        run_id,
                        i64::from(plan.turn_index),
                        plan.assistant_message_id,
                        plan.content,
                        plan.reasoning,
                        plan.finish.as_str(),
                        usage_json,
                        occurred_at,
                    ],
                )
                .map_err(schema::map_sqlite)
                .map_err(run_from_session)?;
            for (call_index, call) in plan.tool_calls.iter().enumerate() {
                transaction
                    .execute(
                        "INSERT INTO tool_invocations(\
                            run_id, turn_index, call_index, call_id, tool_name, arguments_json,\
                            status, attempt, checkpoint, result_json, error_json, provider_content,\
                            planned_at, started_at, finished_at, updated_at\
                         ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, 'planned', 0, 0,\
                            NULL, NULL, NULL, ?7, NULL, NULL, ?7)",
                        params![
                            run_id,
                            i64::from(plan.turn_index),
                            i64::try_from(call_index).map_err(|_| RunError::DataInvalid)?,
                            call.call_id,
                            call.tool_name,
                            call.arguments_json,
                            occurred_at,
                        ],
                    )
                    .map_err(schema::map_sqlite)
                    .map_err(run_from_session)?;
            }
            transaction
                .execute(
                    "UPDATE runs SET updated_at = ?1 WHERE id = ?2",
                    params![occurred_at, run_id],
                )
                .map_err(schema::map_sqlite)
                .map_err(run_from_session)?;
            provider_turn_by_index(transaction, run_id, plan.turn_index)?
                .ok_or(RunError::DataInvalid)
        })
        .map(|(_, turn)| turn)
    }

    pub(crate) fn plan_code_rpc_invocation(
        &self,
        run_id: &str,
        parent_call_id: &str,
        rpc_sequence: u32,
        tool_name: &str,
        arguments_json: &str,
    ) -> Result<StoredToolInvocation, RunError> {
        validate_tool_call_key(parent_call_id)?;
        if rpc_sequence == 0
            || rpc_sequence > 100
            || tool_name.is_empty()
            || tool_name.len() > MAX_TOOL_NAME_BYTES
            || tool_name.chars().any(char::is_control)
            || validate_bounded_json(arguments_json, MAX_TOOL_ARGUMENT_BYTES, true).is_err()
        {
            return Err(RunError::DataInvalid);
        }
        self.mutate_run_with(run_id, |transaction, run| {
            require_running(run)?;
            if let Some(existing) = transaction
                .query_row(
                    &tool_invocation_select(
                        "WHERE run_id = ?1 AND parent_call_id = ?2 AND rpc_sequence = ?3",
                    ),
                    params![run_id, parent_call_id, i64::from(rpc_sequence)],
                    tool_invocation_row,
                )
                .optional()
                .map_err(schema::map_sqlite)
                .map_err(run_from_session)?
            {
                if existing.origin == ToolInvocationOrigin::CodeRpc
                    && existing.tool_name == tool_name
                    && existing.arguments_json == arguments_json
                {
                    return Ok(existing);
                }
                return Err(RunError::DataInvalid);
            }
            let parent = tool_invocation_by_id(transaction, run_id, parent_call_id)?
                .ok_or(RunError::DataInvalid)?;
            if parent.origin != ToolInvocationOrigin::Provider
                || parent.tool_name != "execute_code"
                || parent.status != ToolInvocationStatus::Running
            {
                return Err(RunError::DataInvalid);
            }
            let previous_sequence: i64 = transaction
                .query_row(
                    "SELECT COALESCE(MAX(rpc_sequence), 0) FROM tool_invocations \
                     WHERE run_id = ?1 AND parent_call_id = ?2 AND origin = 'codeRpc'",
                    params![run_id, parent_call_id],
                    |row| row.get(0),
                )
                .map_err(schema::map_sqlite)
                .map_err(run_from_session)?;
            if i64::from(rpc_sequence) != previous_sequence + 1 {
                return Err(RunError::DataInvalid);
            }
            let call_index: i64 = transaction
                .query_row(
                    "SELECT COALESCE(MAX(call_index), -1) + 1 FROM tool_invocations \
                     WHERE run_id = ?1 AND turn_index = ?2",
                    params![run_id, i64::from(parent.turn_index)],
                    |row| row.get(0),
                )
                .map_err(schema::map_sqlite)
                .map_err(run_from_session)?;
            let call_id = format!("call_code_rpc_{}", Uuid::new_v4().simple());
            let occurred_at = now_timestamp().map_err(run_from_session)?;
            transaction
                .execute(
                    "INSERT INTO tool_invocations(\
                        run_id, turn_index, call_index, call_id, tool_name, arguments_json,\
                        status, attempt, checkpoint, result_json, error_json, provider_content,\
                        origin, parent_call_id, rpc_sequence, planned_at, started_at, finished_at,\
                        updated_at\
                     ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, 'planned', 0, 0, NULL, NULL, NULL,\
                        'codeRpc', ?7, ?8, ?9, NULL, NULL, ?9)",
                    params![
                        run_id,
                        i64::from(parent.turn_index),
                        call_index,
                        call_id,
                        tool_name,
                        arguments_json,
                        parent_call_id,
                        i64::from(rpc_sequence),
                        occurred_at,
                    ],
                )
                .map_err(schema::map_sqlite)
                .map_err(run_from_session)?;
            tool_invocation_by_id(transaction, run_id, &call_id)?.ok_or(RunError::DataInvalid)
        })
        .map(|(_, invocation)| invocation)
    }

    pub(crate) fn start_tool_invocation(
        &self,
        run_id: &str,
        call_id: &str,
    ) -> Result<StoredToolInvocation, RunError> {
        validate_tool_call_key(call_id)?;
        self.mutate_run_with(run_id, |transaction, run| {
            require_running(run)?;
            let occurred_at = now_timestamp().map_err(run_from_session)?;
            let changed = transaction
                .execute(
                    "UPDATE tool_invocations SET status = 'running', attempt = attempt + 1,\
                        checkpoint = checkpoint + 1, started_at = ?1, updated_at = ?1\
                     WHERE run_id = ?2 AND call_id = ?3 AND status = 'planned'\
                        AND attempt = 0 AND checkpoint = 0",
                    params![occurred_at, run_id, call_id],
                )
                .map_err(schema::map_sqlite)
                .map_err(run_from_session)?;
            if changed != 1 {
                return Err(RunError::DataInvalid);
            }
            transaction
                .execute(
                    "UPDATE runs SET updated_at = ?1 WHERE id = ?2",
                    params![occurred_at, run_id],
                )
                .map_err(schema::map_sqlite)
                .map_err(run_from_session)?;
            tool_invocation_by_id(transaction, run_id, call_id)?.ok_or(RunError::DataInvalid)
        })
        .map(|(_, invocation)| invocation)
    }

    pub(crate) fn start_tool_invocation_with_event(
        &self,
        run_id: &str,
        call_id: &str,
        name: &str,
        input_summary: &str,
    ) -> Result<StoredToolInvocation, RunError> {
        validate_public_tool_fields(call_id, name, input_summary, 2_000)?;
        self.mutate_run_with(run_id, |transaction, run| {
            require_running(run)?;
            let occurred_at = now_timestamp().map_err(run_from_session)?;
            let changed = transaction
                .execute(
                    "UPDATE tool_invocations SET status = 'running', attempt = attempt + 1,\
                        checkpoint = checkpoint + 1, started_at = ?1, updated_at = ?1\
                     WHERE run_id = ?2 AND call_id = ?3 AND status = 'planned'\
                        AND attempt = 0 AND checkpoint = 0",
                    params![occurred_at, run_id, call_id],
                )
                .map_err(schema::map_sqlite)
                .map_err(run_from_session)?;
            if changed != 1 {
                return Err(RunError::DataInvalid);
            }
            insert_event_tx(
                transaction,
                run_id,
                &run.session_id,
                "tool.started",
                event_data(json!({
                    "callId": call_id,
                    "name": name,
                    "inputSummary": input_summary,
                }))?,
                &occurred_at,
            )?;
            transaction
                .execute(
                    "UPDATE runs SET updated_at = ?1 WHERE id = ?2",
                    params![occurred_at, run_id],
                )
                .map_err(schema::map_sqlite)
                .map_err(run_from_session)?;
            tool_invocation_by_id(transaction, run_id, call_id)?.ok_or(RunError::DataInvalid)
        })
        .map(|(_, invocation)| invocation)
    }

    pub(crate) fn request_tool_approval_with_event(
        &self,
        run_id: &str,
        request: &ToolApprovalRequest,
    ) -> Result<StoredToolApproval, ToolApprovalError> {
        let expires_at_unix_ms = validate_tool_approval_request(request)?;
        validate_run_id(run_id).map_err(approval_from_run)?;
        let ready = self.ready().map_err(approval_from_session)?.clone();
        let _guard = ready
            .write_lock
            .lock()
            .map_err(|_| ToolApprovalError::StorageUnavailable)?;
        let mut connection = schema::open(&ready.db_path).map_err(approval_from_session)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(schema::map_sqlite)
            .map_err(approval_from_session)?;
        self.require_runtime_lease_tx(&transaction)
            .map_err(approval_from_run)?;
        let run = run_by_id_tx(&transaction, run_id)
            .map_err(approval_from_run)?
            .ok_or(ToolApprovalError::NotFound)?;

        if let Some(stored) = tool_approval_by_id(&transaction, &request.approval_id)? {
            if approval_request_matches(&stored, run_id, request, expires_at_unix_ms) {
                transaction
                    .commit()
                    .map_err(schema::map_sqlite)
                    .map_err(approval_from_session)?;
                return Ok(stored);
            }
            return Err(ToolApprovalError::RequestConflict);
        }
        if tool_approval_for_call(&transaction, run_id, &request.call_id)?.is_some() {
            return Err(ToolApprovalError::RequestConflict);
        }
        if run.status != RunStatus::Running || run.pending_action.is_some() {
            return Err(ToolApprovalError::NoLongerPending);
        }

        let invocation = tool_invocation_by_id(&transaction, run_id, &request.call_id)
            .map_err(approval_from_run)?
            .ok_or(ToolApprovalError::DataInvalid)?;
        if invocation.status != ToolInvocationStatus::Running
            || invocation.tool_name != request.tool_name
            || invocation.checkpoint == 0
        {
            return Err(ToolApprovalError::DataInvalid);
        }
        let public_input_summary =
            require_latest_tool_started_event(&transaction, run_id, request)?;
        let workspace_id = transaction
            .query_row(
                "SELECT workspace_id FROM runs WHERE id = ?1",
                [run_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .map_err(schema::map_sqlite)
            .map_err(approval_from_session)?;
        let arguments_sha256: [u8; 32] =
            Sha256::digest(invocation.arguments_json.as_bytes()).into();

        let (created_at, now_unix_ms) = approval_now()?;
        if now_unix_ms >= expires_at_unix_ms {
            return Err(ToolApprovalError::Expired);
        }
        let choices_json = serialize_approval_choices(&request.choices)?;
        transaction
            .execute(
                "INSERT INTO run_approvals(\
                    approval_id, run_id, profile_id, session_id, workspace_id, call_id,\
                    invocation_checkpoint, tool_name, arguments_sha256, input_summary,\
                    choices_json, expires_at, expires_at_unix_ms, state, decision, reason,\
                    resolved_by, created_at, resolved_at, execution_claimed_at\
                 ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,\
                    'pending', NULL, NULL, NULL, ?14, NULL, NULL)",
                params![
                    request.approval_id,
                    run_id,
                    run.profile_id,
                    run.session_id,
                    workspace_id,
                    request.call_id,
                    i64::try_from(invocation.checkpoint)
                        .map_err(|_| ToolApprovalError::DataInvalid)?,
                    request.tool_name,
                    arguments_sha256.to_vec(),
                    request.input_summary,
                    choices_json,
                    request.expires_at,
                    expires_at_unix_ms,
                    created_at,
                ],
            )
            .map_err(schema::map_sqlite)
            .map_err(approval_from_session)?;
        let pending_action = PendingAction::Approval {
            approval_id: request.approval_id.clone(),
            call_id: request.call_id.clone(),
            tool_name: request.tool_name.clone(),
            input_summary: request.input_summary.clone(),
            choices: request
                .choices
                .iter()
                .map(|choice| choice.as_str().to_owned())
                .collect(),
            expires_at: request.expires_at.clone(),
        };
        let changed = transaction
            .execute(
                "UPDATE runs SET status = 'waitingApproval', pending_action_json = ?1,\
                    updated_at = ?2 WHERE id = ?3 AND status = 'running'\
                    AND pending_action_json IS NULL",
                params![
                    serialize_json(&pending_action).map_err(approval_from_run)?,
                    created_at,
                    run_id
                ],
            )
            .map_err(schema::map_sqlite)
            .map_err(approval_from_session)?;
        if changed != 1 {
            return Err(ToolApprovalError::NoLongerPending);
        }
        insert_event_tx(
            &transaction,
            run_id,
            &run.session_id,
            "approval.required",
            event_data(json!({
                "approvalId": request.approval_id,
                "callId": request.call_id,
                "toolName": request.tool_name,
                "inputSummary": public_input_summary,
                "choices": request.choices,
                "expiresAt": request.expires_at,
            }))
            .map_err(approval_from_run)?,
            &created_at,
        )
        .map_err(approval_from_run)?;
        let stored = tool_approval_by_id(&transaction, &request.approval_id)?
            .ok_or(ToolApprovalError::DataInvalid)?;
        transaction
            .commit()
            .map_err(schema::map_sqlite)
            .map_err(approval_from_session)?;
        Ok(stored)
    }

    pub(crate) fn load_tool_approval(
        &self,
        run_id: &str,
        approval_id: &str,
    ) -> Result<StoredToolApproval, ToolApprovalError> {
        validate_run_id(run_id).map_err(approval_from_run)?;
        validate_approval_id(approval_id)?;
        let connection = self.run_connection().map_err(approval_from_run)?;
        let approval = tool_approval_by_id(&connection, approval_id)?
            .filter(|approval| approval.run_id == run_id)
            .ok_or(ToolApprovalError::NotFound)?;
        Ok(approval)
    }

    pub(crate) fn resolve_tool_approval(
        &self,
        run_id: &str,
        approval_id: &str,
        decision: ToolApprovalDecision,
        reason: Option<&str>,
    ) -> Result<ToolApprovalResolution, ToolApprovalError> {
        validate_run_id(run_id).map_err(approval_from_run)?;
        validate_approval_id(approval_id)?;
        validate_approval_reason(reason)?;
        let ready = self.ready().map_err(approval_from_session)?.clone();
        let _guard = ready
            .write_lock
            .lock()
            .map_err(|_| ToolApprovalError::StorageUnavailable)?;
        let mut connection = schema::open(&ready.db_path).map_err(approval_from_session)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(schema::map_sqlite)
            .map_err(approval_from_session)?;
        self.require_runtime_lease_tx(&transaction)
            .map_err(approval_from_run)?;
        let run = run_by_id_tx(&transaction, run_id)
            .map_err(approval_from_run)?
            .ok_or(ToolApprovalError::NotFound)?;
        let approval = tool_approval_by_id(&transaction, approval_id)?
            .filter(|approval| approval.run_id == run_id)
            .ok_or(ToolApprovalError::NotFound)?;

        if approval.state == ToolApprovalState::Resolved {
            let result = replay_user_approval(&approval, decision, reason)?;
            transaction
                .commit()
                .map_err(schema::map_sqlite)
                .map_err(approval_from_session)?;
            return Ok(result);
        }
        require_pending_approval_run(&run, &approval)?;

        let (resolved_at, now_unix_ms) = approval_now()?;
        if now_unix_ms >= approval.expires_at_unix_ms {
            resolve_denied_approval_tx(
                &transaction,
                &run,
                &approval,
                ToolApprovalResolvedBy::Expiry,
                None,
                RunStatus::Running,
                &resolved_at,
            )?;
            transaction
                .commit()
                .map_err(schema::map_sqlite)
                .map_err(approval_from_session)?;
            return Err(ToolApprovalError::Expired);
        }
        if !approval.choices.contains(&decision) {
            return Err(ToolApprovalError::ChoiceNotOffered);
        }

        match decision {
            ToolApprovalDecision::Once => {
                resolve_allowed_once_tx(&transaction, &run, &approval, reason, &resolved_at)?
            }
            ToolApprovalDecision::Deny => resolve_denied_approval_tx(
                &transaction,
                &run,
                &approval,
                ToolApprovalResolvedBy::User,
                reason,
                RunStatus::Running,
                &resolved_at,
            )?,
            ToolApprovalDecision::Session | ToolApprovalDecision::Always => {
                return Err(ToolApprovalError::ChoiceNotOffered);
            }
        }
        let resolved = tool_approval_by_id(&transaction, approval_id)?
            .ok_or(ToolApprovalError::DataInvalid)?;
        transaction
            .commit()
            .map_err(schema::map_sqlite)
            .map_err(approval_from_session)?;
        Ok(ToolApprovalResolution {
            approval: resolved,
            disposition: ToolApprovalResolutionDisposition::Accepted,
        })
    }

    pub(crate) fn expire_tool_approval(
        &self,
        run_id: &str,
        approval_id: &str,
    ) -> Result<ToolApprovalResolution, ToolApprovalError> {
        validate_run_id(run_id).map_err(approval_from_run)?;
        validate_approval_id(approval_id)?;
        let ready = self.ready().map_err(approval_from_session)?.clone();
        let _guard = ready
            .write_lock
            .lock()
            .map_err(|_| ToolApprovalError::StorageUnavailable)?;
        let mut connection = schema::open(&ready.db_path).map_err(approval_from_session)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(schema::map_sqlite)
            .map_err(approval_from_session)?;
        self.require_runtime_lease_tx(&transaction)
            .map_err(approval_from_run)?;
        let run = run_by_id_tx(&transaction, run_id)
            .map_err(approval_from_run)?
            .ok_or(ToolApprovalError::NotFound)?;
        let approval = tool_approval_by_id(&transaction, approval_id)?
            .filter(|approval| approval.run_id == run_id)
            .ok_or(ToolApprovalError::NotFound)?;
        if approval.state == ToolApprovalState::Resolved {
            if approval.resolved_by == Some(ToolApprovalResolvedBy::Expiry) {
                transaction
                    .commit()
                    .map_err(schema::map_sqlite)
                    .map_err(approval_from_session)?;
                return Ok(ToolApprovalResolution {
                    approval,
                    disposition: ToolApprovalResolutionDisposition::Replayed,
                });
            }
            return Err(ToolApprovalError::NoLongerPending);
        }
        require_pending_approval_run(&run, &approval)?;
        let (resolved_at, now_unix_ms) = approval_now()?;
        if now_unix_ms < approval.expires_at_unix_ms {
            return Err(ToolApprovalError::NotExpired);
        }
        resolve_denied_approval_tx(
            &transaction,
            &run,
            &approval,
            ToolApprovalResolvedBy::Expiry,
            None,
            RunStatus::Running,
            &resolved_at,
        )?;
        let resolved = tool_approval_by_id(&transaction, approval_id)?
            .ok_or(ToolApprovalError::DataInvalid)?;
        transaction
            .commit()
            .map_err(schema::map_sqlite)
            .map_err(approval_from_session)?;
        Ok(ToolApprovalResolution {
            approval: resolved,
            disposition: ToolApprovalResolutionDisposition::Accepted,
        })
    }

    pub(crate) fn claim_tool_approval(
        &self,
        run_id: &str,
        approval_id: &str,
        binding: &ToolApprovalExecutionBinding,
    ) -> Result<StoredToolApproval, ToolApprovalError> {
        validate_run_id(run_id).map_err(approval_from_run)?;
        validate_approval_id(approval_id)?;
        let ready = self.ready().map_err(approval_from_session)?.clone();
        let _guard = ready
            .write_lock
            .lock()
            .map_err(|_| ToolApprovalError::StorageUnavailable)?;
        let mut connection = schema::open(&ready.db_path).map_err(approval_from_session)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(schema::map_sqlite)
            .map_err(approval_from_session)?;
        self.require_runtime_lease_tx(&transaction)
            .map_err(approval_from_run)?;
        let run = run_by_id_tx(&transaction, run_id)
            .map_err(approval_from_run)?
            .ok_or(ToolApprovalError::NotFound)?;
        let approval = tool_approval_by_id(&transaction, approval_id)?
            .filter(|approval| approval.run_id == run_id)
            .ok_or(ToolApprovalError::NotFound)?;
        if approval.execution_claimed_at.is_some() {
            return Err(ToolApprovalError::ExecutionAlreadyClaimed);
        }
        if approval.state != ToolApprovalState::Resolved
            || approval.decision != Some(ToolApprovalDecision::Once)
            || approval.resolved_by != Some(ToolApprovalResolvedBy::User)
            || run.status != RunStatus::Running
            || run.pending_action.is_some()
        {
            return Err(ToolApprovalError::ExecutionNotAuthorized);
        }
        let invocation = tool_invocation_by_id(&transaction, run_id, &approval.call_id)
            .map_err(approval_from_run)?
            .ok_or(ToolApprovalError::DataInvalid)?;
        let workspace_id = transaction
            .query_row(
                "SELECT workspace_id FROM runs WHERE id = ?1",
                [run_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .map_err(schema::map_sqlite)
            .map_err(approval_from_session)?;
        let arguments_sha256: [u8; 32] =
            Sha256::digest(invocation.arguments_json.as_bytes()).into();
        if invocation.status != ToolInvocationStatus::Running
            || invocation.checkpoint != approval.invocation_checkpoint
            || approval.run_id != binding.run_id
            || approval.run_id != run_id
            || approval.profile_id != binding.profile_id
            || approval.profile_id != run.profile_id
            || approval.session_id != binding.session_id
            || approval.session_id != run.session_id
            || approval.workspace_id != binding.workspace_id
            || approval.workspace_id != workspace_id
            || approval.call_id != binding.call_id
            || approval.tool_name != binding.tool_name
            || approval.invocation_checkpoint != binding.invocation_checkpoint
            || approval.arguments_sha256 != binding.arguments_sha256
            || approval.arguments_sha256 != arguments_sha256
            || invocation.call_id != binding.call_id
            || invocation.tool_name != binding.tool_name
        {
            return Err(ToolApprovalError::ExecutionNotAuthorized);
        }
        let (claimed_at, _) = approval_now()?;
        let changed = transaction
            .execute(
                "UPDATE run_approvals SET execution_claimed_at = ?1 \
                 WHERE approval_id = ?2 AND run_id = ?3 AND state = 'resolved'\
                    AND decision = 'once' AND resolved_by = 'user'\
                    AND execution_claimed_at IS NULL \
                    AND EXISTS(SELECT 1 FROM runs WHERE id = ?3 AND status = 'running'\
                        AND pending_action_json IS NULL) \
                    AND EXISTS(SELECT 1 FROM tool_invocations \
                        WHERE run_id = ?3 AND call_id = run_approvals.call_id \
                        AND status = 'running'\
                        AND checkpoint = run_approvals.invocation_checkpoint)",
                params![claimed_at, approval_id, run_id],
            )
            .map_err(schema::map_sqlite)
            .map_err(approval_from_session)?;
        if changed != 1 {
            return Err(ToolApprovalError::ExecutionNotAuthorized);
        }
        let claimed = tool_approval_by_id(&transaction, approval_id)?
            .ok_or(ToolApprovalError::DataInvalid)?;
        transaction
            .commit()
            .map_err(schema::map_sqlite)
            .map_err(approval_from_session)?;
        Ok(claimed)
    }

    pub(crate) fn request_clarification_with_event(
        &self,
        run_id: &str,
        request: &ClarificationRequest,
    ) -> Result<StoredClarification, ClarificationError> {
        validate_clarification_request(request)?;
        validate_run_id(run_id).map_err(clarification_from_run)?;
        let ready = self.ready().map_err(clarification_from_session)?.clone();
        let _guard = ready
            .write_lock
            .lock()
            .map_err(|_| ClarificationError::StorageUnavailable)?;
        let mut connection = schema::open(&ready.db_path).map_err(clarification_from_session)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(schema::map_sqlite)
            .map_err(clarification_from_session)?;
        self.require_runtime_lease_tx(&transaction)
            .map_err(clarification_from_run)?;
        let run = run_by_id_tx(&transaction, run_id)
            .map_err(clarification_from_run)?
            .ok_or(ClarificationError::NotFound)?;

        if let Some(stored) = clarification_by_id(&transaction, &request.request_id)? {
            if clarification_request_matches(&stored, run_id, request) {
                transaction
                    .commit()
                    .map_err(schema::map_sqlite)
                    .map_err(clarification_from_session)?;
                return Ok(stored);
            }
            return Err(ClarificationError::RequestConflict);
        }
        if clarification_for_call(&transaction, run_id, &request.call_id)?.is_some() {
            return Err(ClarificationError::RequestConflict);
        }
        if run.status != RunStatus::Running || run.pending_action.is_some() {
            return Err(ClarificationError::NoLongerPending);
        }

        let invocation = tool_invocation_by_id(&transaction, run_id, &request.call_id)
            .map_err(clarification_from_run)?
            .ok_or(ClarificationError::DataInvalid)?;
        if invocation.status != ToolInvocationStatus::Running
            || invocation.tool_name != "clarify"
            || invocation.checkpoint == 0
        {
            return Err(ClarificationError::DataInvalid);
        }
        require_latest_clarification_tool_started_event(&transaction, run_id, request)?;
        let arguments_sha256: [u8; 32] =
            Sha256::digest(invocation.arguments_json.as_bytes()).into();
        let created_at = now_timestamp().map_err(clarification_from_session)?;
        let choices_json = serialize_clarification_choices(&request.choices)?;
        transaction
            .execute(
                "INSERT INTO run_clarifications(\
                    request_id, run_id, call_id, invocation_checkpoint, arguments_sha256,\
                    question, choices_json, state, answer, resolved_by, created_at, resolved_at,\
                    continuation_claimed_at\
                 ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, 'pending', NULL, NULL, ?8, NULL, NULL)",
                params![
                    request.request_id,
                    run_id,
                    request.call_id,
                    i64::try_from(invocation.checkpoint)
                        .map_err(|_| ClarificationError::DataInvalid)?,
                    arguments_sha256.to_vec(),
                    request.question,
                    choices_json,
                    created_at,
                ],
            )
            .map_err(schema::map_sqlite)
            .map_err(clarification_from_session)?;
        let pending_action = PendingAction::Clarification {
            request_id: request.request_id.clone(),
            question: request.question.clone(),
            choices: request.choices.clone(),
        };
        let changed = transaction
            .execute(
                "UPDATE runs SET status = 'waitingClarification', pending_action_json = ?1,\
                    updated_at = ?2 WHERE id = ?3 AND status = 'running'\
                    AND pending_action_json IS NULL",
                params![
                    serialize_json(&pending_action).map_err(clarification_from_run)?,
                    created_at,
                    run_id,
                ],
            )
            .map_err(schema::map_sqlite)
            .map_err(clarification_from_session)?;
        if changed != 1 {
            return Err(ClarificationError::NoLongerPending);
        }
        insert_event_tx(
            &transaction,
            run_id,
            &run.session_id,
            "clarification.required",
            event_data(json!({
                "requestId": request.request_id,
                "question": request.question,
                "choices": request.choices,
            }))
            .map_err(clarification_from_run)?,
            &created_at,
        )
        .map_err(clarification_from_run)?;
        let stored = clarification_by_id(&transaction, &request.request_id)?
            .ok_or(ClarificationError::DataInvalid)?;
        transaction
            .commit()
            .map_err(schema::map_sqlite)
            .map_err(clarification_from_session)?;
        Ok(stored)
    }

    pub(crate) fn load_clarification(
        &self,
        run_id: &str,
        request_id: &str,
    ) -> Result<StoredClarification, ClarificationError> {
        validate_run_id(run_id).map_err(clarification_from_run)?;
        validate_clarification_id(request_id)?;
        let connection = self.run_connection().map_err(clarification_from_run)?;
        clarification_by_id(&connection, request_id)?
            .filter(|clarification| clarification.run_id == run_id)
            .ok_or(ClarificationError::NotFound)
    }

    pub(crate) fn resolve_clarification(
        &self,
        run_id: &str,
        request_id: &str,
        answer: &str,
    ) -> Result<ClarificationResolution, ClarificationError> {
        validate_run_id(run_id).map_err(clarification_from_run)?;
        validate_clarification_id(request_id)?;
        validate_clarification_answer(answer)?;
        let ready = self.ready().map_err(clarification_from_session)?.clone();
        let _guard = ready
            .write_lock
            .lock()
            .map_err(|_| ClarificationError::StorageUnavailable)?;
        let mut connection = schema::open(&ready.db_path).map_err(clarification_from_session)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(schema::map_sqlite)
            .map_err(clarification_from_session)?;
        self.require_runtime_lease_tx(&transaction)
            .map_err(clarification_from_run)?;
        let run = run_by_id_tx(&transaction, run_id)
            .map_err(clarification_from_run)?
            .ok_or(ClarificationError::NotFound)?;
        let clarification = clarification_by_id(&transaction, request_id)?
            .filter(|clarification| clarification.run_id == run_id)
            .ok_or(ClarificationError::NotFound)?;

        if clarification.state == ClarificationState::Resolved {
            let resolution = replay_user_clarification(&clarification, answer)?;
            transaction
                .commit()
                .map_err(schema::map_sqlite)
                .map_err(clarification_from_session)?;
            return Ok(resolution);
        }
        require_pending_clarification_run(&run, &clarification)?;
        if !clarification.choices.is_empty()
            && !clarification.choices.iter().any(|choice| choice == answer)
        {
            return Err(ClarificationError::ChoiceNotOffered);
        }

        let resolved_at = now_timestamp().map_err(clarification_from_session)?;
        resolve_clarification_ledger_tx(
            &transaction,
            &run,
            &clarification,
            Some(answer),
            ClarificationResolvedBy::User,
            &resolved_at,
        )?;
        let changed = transaction
            .execute(
                "UPDATE runs SET status = 'running', pending_action_json = NULL, updated_at = ?1 \
                 WHERE id = ?2 AND status = 'waitingClarification'",
                params![resolved_at, run_id],
            )
            .map_err(schema::map_sqlite)
            .map_err(clarification_from_session)?;
        if changed != 1 {
            return Err(ClarificationError::NoLongerPending);
        }
        let resolved = clarification_by_id(&transaction, request_id)?
            .ok_or(ClarificationError::DataInvalid)?;
        transaction
            .commit()
            .map_err(schema::map_sqlite)
            .map_err(clarification_from_session)?;
        Ok(ClarificationResolution {
            clarification: resolved,
            disposition: ClarificationResolutionDisposition::Accepted,
        })
    }

    pub(crate) fn claim_clarification_answer(
        &self,
        run_id: &str,
        request_id: &str,
        binding: &ClarificationContinuationBinding,
    ) -> Result<StoredClarification, ClarificationError> {
        validate_run_id(run_id).map_err(clarification_from_run)?;
        validate_clarification_id(request_id)?;
        let ready = self.ready().map_err(clarification_from_session)?.clone();
        let _guard = ready
            .write_lock
            .lock()
            .map_err(|_| ClarificationError::StorageUnavailable)?;
        let mut connection = schema::open(&ready.db_path).map_err(clarification_from_session)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(schema::map_sqlite)
            .map_err(clarification_from_session)?;
        self.require_runtime_lease_tx(&transaction)
            .map_err(clarification_from_run)?;
        let run = run_by_id_tx(&transaction, run_id)
            .map_err(clarification_from_run)?
            .ok_or(ClarificationError::NotFound)?;
        let clarification = clarification_by_id(&transaction, request_id)?
            .filter(|clarification| clarification.run_id == run_id)
            .ok_or(ClarificationError::NotFound)?;
        if clarification.continuation_claimed_at.is_some() {
            return Err(ClarificationError::ContinuationAlreadyClaimed);
        }
        if clarification.state != ClarificationState::Resolved
            || clarification.resolved_by != Some(ClarificationResolvedBy::User)
            || clarification.answer.is_none()
            || run.status != RunStatus::Running
            || run.pending_action.is_some()
        {
            return Err(ClarificationError::ContinuationNotAuthorized);
        }
        let invocation = tool_invocation_by_id(&transaction, run_id, &clarification.call_id)
            .map_err(clarification_from_run)?
            .ok_or(ClarificationError::DataInvalid)?;
        let arguments_sha256: [u8; 32] =
            Sha256::digest(invocation.arguments_json.as_bytes()).into();
        if binding.run_id != run_id
            || binding.run_id != clarification.run_id
            || binding.call_id != clarification.call_id
            || binding.invocation_checkpoint != clarification.invocation_checkpoint
            || binding.arguments_sha256 != clarification.arguments_sha256
            || invocation.call_id != clarification.call_id
            || invocation.status != ToolInvocationStatus::Running
            || invocation.checkpoint != clarification.invocation_checkpoint
            || arguments_sha256 != clarification.arguments_sha256
        {
            return Err(ClarificationError::ContinuationNotAuthorized);
        }
        let claimed_at = now_timestamp().map_err(clarification_from_session)?;
        let changed = transaction
            .execute(
                "UPDATE run_clarifications SET continuation_claimed_at = ?1 \
                 WHERE request_id = ?2 AND run_id = ?3 AND state = 'resolved' \
                    AND resolved_by = 'user' AND answer IS NOT NULL \
                    AND continuation_claimed_at IS NULL \
                    AND EXISTS(SELECT 1 FROM runs WHERE id = ?3 AND status = 'running' \
                        AND pending_action_json IS NULL) \
                    AND EXISTS(SELECT 1 FROM tool_invocations \
                        WHERE run_id = ?3 AND call_id = run_clarifications.call_id \
                        AND status = 'running' \
                        AND checkpoint = run_clarifications.invocation_checkpoint)",
                params![claimed_at, request_id, run_id],
            )
            .map_err(schema::map_sqlite)
            .map_err(clarification_from_session)?;
        if changed != 1 {
            return Err(ClarificationError::ContinuationNotAuthorized);
        }
        let claimed = clarification_by_id(&transaction, request_id)?
            .ok_or(ClarificationError::DataInvalid)?;
        transaction
            .commit()
            .map_err(schema::map_sqlite)
            .map_err(clarification_from_session)?;
        Ok(claimed)
    }

    pub(crate) fn complete_tool_invocation(
        &self,
        run_id: &str,
        call_id: &str,
        expected_checkpoint: u64,
        raw_result_json: &str,
        provider_content: &str,
    ) -> Result<StoredToolInvocation, RunError> {
        validate_tool_terminal_input(call_id, expected_checkpoint, provider_content)?;
        validate_bounded_json(raw_result_json, MAX_TOOL_RESULT_BYTES, false)?;
        self.finish_tool_invocation(
            run_id,
            call_id,
            expected_checkpoint,
            ToolInvocationTerminal {
                status: ToolInvocationStatus::Completed,
                raw_json: raw_result_json,
                provider_content,
                public_event: None,
            },
        )
    }

    pub(crate) fn complete_tool_invocation_with_event(
        &self,
        run_id: &str,
        call_id: &str,
        expected_checkpoint: u64,
        raw_result_json: &str,
        provider_content: &str,
        result_summary: &str,
    ) -> Result<StoredToolInvocation, RunError> {
        self.complete_tool_invocation_with_event_and_async_delivery(
            run_id,
            call_id,
            expected_checkpoint,
            raw_result_json,
            provider_content,
            result_summary,
            false,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn complete_tool_invocation_with_event_and_async_delivery(
        &self,
        run_id: &str,
        call_id: &str,
        expected_checkpoint: u64,
        raw_result_json: &str,
        provider_content: &str,
        result_summary: &str,
        async_delivery_pending: bool,
    ) -> Result<StoredToolInvocation, RunError> {
        validate_public_tool_fields(call_id, "tool", result_summary, 8_000)?;
        validate_tool_terminal_input(call_id, expected_checkpoint, provider_content)?;
        validate_bounded_json(raw_result_json, MAX_TOOL_RESULT_BYTES, false)?;
        let mut public_event = serde_json::Map::from_iter([
            ("callId".to_owned(), json!(call_id)),
            ("resultSummary".to_owned(), json!(result_summary)),
            ("artifacts".to_owned(), json!([])),
        ]);
        if async_delivery_pending {
            public_event.insert("asyncDeliveryPending".to_owned(), json!(true));
        }
        self.finish_tool_invocation(
            run_id,
            call_id,
            expected_checkpoint,
            ToolInvocationTerminal {
                status: ToolInvocationStatus::Completed,
                raw_json: raw_result_json,
                provider_content,
                public_event: Some(("tool.completed", serde_json::Value::Object(public_event))),
            },
        )
    }

    pub(crate) fn fail_tool_invocation(
        &self,
        run_id: &str,
        call_id: &str,
        expected_checkpoint: u64,
        raw_error_json: &str,
        provider_content: &str,
    ) -> Result<StoredToolInvocation, RunError> {
        validate_tool_terminal_input(call_id, expected_checkpoint, provider_content)?;
        validate_bounded_json(raw_error_json, MAX_TOOL_RESULT_BYTES, false)?;
        self.finish_tool_invocation(
            run_id,
            call_id,
            expected_checkpoint,
            ToolInvocationTerminal {
                status: ToolInvocationStatus::Failed,
                raw_json: raw_error_json,
                provider_content,
                public_event: None,
            },
        )
    }

    pub(crate) fn fail_tool_invocation_with_event(
        &self,
        run_id: &str,
        call_id: &str,
        expected_checkpoint: u64,
        raw_error_json: &str,
        provider_content: &str,
        problem: &RunProblem,
    ) -> Result<StoredToolInvocation, RunError> {
        validate_tool_terminal_input(call_id, expected_checkpoint, provider_content)?;
        validate_bounded_json(raw_error_json, MAX_TOOL_RESULT_BYTES, false)?;
        self.finish_tool_invocation(
            run_id,
            call_id,
            expected_checkpoint,
            ToolInvocationTerminal {
                status: ToolInvocationStatus::Failed,
                raw_json: raw_error_json,
                provider_content,
                public_event: Some(("tool.failed", json!({"callId": call_id, "error": problem}))),
            },
        )
    }

    #[cfg(test)]
    pub(crate) fn unfinished_tool_invocations(
        &self,
        run_id: &str,
    ) -> Result<Vec<StoredToolInvocation>, RunError> {
        validate_run_id(run_id)?;
        let connection = self.run_connection()?;
        if run_by_id(&connection, run_id)?.is_none() {
            return Err(RunError::NotFound);
        }
        unfinished_tool_invocations_for_run(&connection, run_id)
    }

    #[cfg(test)]
    pub(crate) fn provider_turns(&self, run_id: &str) -> Result<Vec<StoredProviderTurn>, RunError> {
        validate_run_id(run_id)?;
        let connection = self.run_connection()?;
        if run_by_id(&connection, run_id)?.is_none() {
            return Err(RunError::NotFound);
        }
        provider_turns_for_run(&connection, run_id)
    }

    pub(crate) fn provider_continuation_context(
        &self,
        run_id: &str,
    ) -> Result<Vec<ProviderContextMessage>, RunError> {
        validate_run_id(run_id)?;
        let connection = self.run_connection()?;
        let run = run_by_id(&connection, run_id)?.ok_or(RunError::NotFound)?;
        let mut context = base_provider_context(&connection, run_id, &run.session_id)?;
        let turns = provider_turns_for_run(&connection, run_id)?;
        if turns
            .iter()
            .flat_map(|turn| turn.tool_calls.iter())
            .any(|call| !call.status.is_terminal())
        {
            return Err(RunError::DataInvalid);
        }
        let mut expected_turn = 1u32;
        let turn_count = turns.len();
        for (turn_position, turn) in turns.into_iter().enumerate() {
            if turn.turn_index != expected_turn {
                return Err(RunError::DataInvalid);
            }
            if turn_position + 1 < turn_count && turn.finish != ProviderTurnFinish::ToolCalls {
                return Err(RunError::DataInvalid);
            }
            expected_turn = expected_turn.checked_add(1).ok_or(RunError::DataInvalid)?;
            let tool_calls = turn
                .tool_calls
                .iter()
                .map(|call| RawToolCallPlan {
                    call_id: call.call_id.clone(),
                    tool_name: call.tool_name.clone(),
                    arguments_json: call.arguments_json.clone(),
                })
                .collect();
            context.push(ProviderContextMessage::Assistant {
                content: turn.content,
                tool_calls,
            });
            for call in turn.tool_calls {
                context.push(ProviderContextMessage::Tool {
                    tool_call_id: call.call_id,
                    content: call.provider_content.ok_or(RunError::DataInvalid)?,
                });
            }
        }
        Ok(context)
    }

    fn finish_tool_invocation(
        &self,
        run_id: &str,
        call_id: &str,
        expected_checkpoint: u64,
        terminal: ToolInvocationTerminal<'_>,
    ) -> Result<StoredToolInvocation, RunError> {
        debug_assert!(matches!(
            terminal.status,
            ToolInvocationStatus::Completed | ToolInvocationStatus::Failed
        ));
        self.mutate_run_with(run_id, |transaction, run| {
            if run.status != RunStatus::Running
                && !(run.status == RunStatus::Cancelling
                    && terminal.status == ToolInvocationStatus::Failed)
            {
                return Err(RunError::DataInvalid);
            }
            let occurred_at = now_timestamp().map_err(run_from_session)?;
            let checkpoint =
                i64::try_from(expected_checkpoint).map_err(|_| RunError::DataInvalid)?;
            let (result_json, error_json) = match terminal.status {
                ToolInvocationStatus::Completed => (Some(terminal.raw_json), None),
                ToolInvocationStatus::Failed => (None, Some(terminal.raw_json)),
                _ => return Err(RunError::DataInvalid),
            };
            let changed = transaction
                .execute(
                    "UPDATE tool_invocations SET status = ?1, checkpoint = checkpoint + 1,\
                        result_json = ?2, error_json = ?3, provider_content = ?4,\
                        finished_at = ?5, updated_at = ?5\
                     WHERE run_id = ?6 AND call_id = ?7 AND status = 'running'\
                        AND checkpoint = ?8",
                    params![
                        terminal.status.as_str(),
                        result_json,
                        error_json,
                        terminal.provider_content,
                        occurred_at,
                        run_id,
                        call_id,
                        checkpoint,
                    ],
                )
                .map_err(schema::map_sqlite)
                .map_err(run_from_session)?;
            if changed != 1 {
                return Err(RunError::DataInvalid);
            }
            if let Some((event_name, data)) = terminal.public_event {
                insert_event_tx(
                    transaction,
                    run_id,
                    &run.session_id,
                    event_name,
                    event_data(data)?,
                    &occurred_at,
                )?;
            }
            transaction
                .execute(
                    "UPDATE runs SET updated_at = ?1 WHERE id = ?2",
                    params![occurred_at, run_id],
                )
                .map_err(schema::map_sqlite)
                .map_err(run_from_session)?;
            tool_invocation_by_id(transaction, run_id, call_id)?.ok_or(RunError::DataInvalid)
        })
        .map(|(_, invocation)| invocation)
    }

    pub(crate) fn recover_interrupted_runs(&self) -> Result<Vec<String>, RunError> {
        let ready = self.ready().map_err(run_from_session)?.clone();
        let _guard = ready
            .write_lock
            .lock()
            .map_err(|_| RunError::StorageUnavailable)?;
        let mut connection = schema::open(&ready.db_path).map_err(run_from_session)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(schema::map_sqlite)
            .map_err(run_from_session)?;
        self.require_runtime_lease_tx(&transaction)?;
        let interrupted = {
            let mut statement = transaction
                .prepare(
                    "SELECT id, session_id, status FROM runs WHERE status IN (\
                        'running', 'waitingApproval', 'waitingClarification', 'cancelling'\
                     ) ORDER BY created_at, id",
                )
                .map_err(schema::map_sqlite)
                .map_err(run_from_session)?;
            let rows = statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })
                .map_err(schema::map_sqlite)
                .map_err(run_from_session)?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|_| RunError::DataInvalid)?
        };
        for (run_id, session_id, status) in &interrupted {
            let occurred_at = now_timestamp().map_err(run_from_session)?;
            let problem = RunProblem::backend_restarted(run_id);
            let interrupted_status = RunStatus::try_from(status.as_str())?;
            if interrupted_status == RunStatus::WaitingApproval {
                let run = run_by_id_tx(&transaction, run_id)?.ok_or(RunError::DataInvalid)?;
                let approval = pending_tool_approval_for_run(&transaction, run_id)
                    .map_err(run_from_approval)?
                    .ok_or(RunError::DataInvalid)?;
                require_pending_approval_run(&run, &approval).map_err(run_from_approval)?;
                let now_unix_ms =
                    approval_timestamp_unix_ms(&occurred_at).map_err(run_from_approval)?;
                let resolved_by = if now_unix_ms >= approval.expires_at_unix_ms {
                    ToolApprovalResolvedBy::Expiry
                } else {
                    ToolApprovalResolvedBy::Cancellation
                };
                resolve_denied_approval_tx(
                    &transaction,
                    &run,
                    &approval,
                    resolved_by,
                    None,
                    RunStatus::Running,
                    &occurred_at,
                )
                .map_err(run_from_approval)?;
            } else if interrupted_status == RunStatus::WaitingClarification {
                let run = run_by_id_tx(&transaction, run_id)?.ok_or(RunError::DataInvalid)?;
                let clarification = pending_clarification_for_run(&transaction, run_id)
                    .map_err(run_from_clarification)?
                    .ok_or(RunError::DataInvalid)?;
                require_pending_clarification_run(&run, &clarification)
                    .map_err(run_from_clarification)?;
                resolve_abandoned_clarification_tx(
                    &transaction,
                    &run,
                    &clarification,
                    ClarificationResolvedBy::Failure,
                    RunStatus::Running,
                    &occurred_at,
                )
                .map_err(run_from_clarification)?;
            }
            insert_event_tx(
                &transaction,
                run_id,
                session_id,
                "run.failed",
                event_data(json!({"error": problem}))?,
                &occurred_at,
            )?;
            transaction
                .execute(
                    "UPDATE runs SET status = 'failed', error_json = ?1, pending_action_json = NULL,\
                        updated_at = ?2, terminal_at = ?2 WHERE id = ?3",
                    params![serialize_json(&problem)?, occurred_at, run_id],
                )
                .map_err(schema::map_sqlite)
                .map_err(run_from_session)?;
        }
        transaction
            .commit()
            .map_err(schema::map_sqlite)
            .map_err(run_from_session)?;
        Ok(interrupted
            .into_iter()
            .map(|(run_id, _, _)| run_id)
            .collect())
    }

    fn terminal_run(
        &self,
        run_id: &str,
        status: RunStatus,
        event_name: &str,
        data: serde_json::Value,
        problem: Option<&RunProblem>,
    ) -> Result<Run, RunError> {
        self.mutate_run(run_id, |transaction, run| {
            if run.status.is_terminal() {
                return Ok(());
            }
            if run.status == RunStatus::WaitingClarification {
                let (resolved_by, next_status) = match status {
                    RunStatus::Failed => (ClarificationResolvedBy::Failure, RunStatus::Running),
                    RunStatus::Cancelled => {
                        (ClarificationResolvedBy::Cancellation, RunStatus::Cancelling)
                    }
                    _ => return Err(RunError::DataInvalid),
                };
                let clarification = pending_clarification_for_run(transaction, run_id)
                    .map_err(run_from_clarification)?
                    .ok_or(RunError::DataInvalid)?;
                require_pending_clarification_run(run, &clarification)
                    .map_err(run_from_clarification)?;
                let resolved_at = now_timestamp().map_err(run_from_session)?;
                resolve_abandoned_clarification_tx(
                    transaction,
                    run,
                    &clarification,
                    resolved_by,
                    next_status,
                    &resolved_at,
                )
                .map_err(run_from_clarification)?;
            }
            let cancellation_won =
                run.status == RunStatus::Cancelling && status != RunStatus::Cancelled;
            let status = if cancellation_won {
                RunStatus::Cancelled
            } else {
                status
            };
            let event_name = if cancellation_won {
                "run.cancelled"
            } else {
                event_name
            };
            let data = if cancellation_won {
                json!({"reason": "cancelled by user"})
            } else {
                data
            };
            let problem = if cancellation_won { None } else { problem };
            let occurred_at = now_timestamp().map_err(run_from_session)?;
            insert_event_tx(
                transaction,
                run_id,
                &run.session_id,
                event_name,
                data,
                &occurred_at,
            )?;
            let error_json = problem.map(serialize_json).transpose()?;
            transaction
                .execute(
                    "UPDATE runs SET status = ?1, error_json = ?2, pending_action_json = NULL,\
                        updated_at = ?3, terminal_at = ?3 WHERE id = ?4",
                    params![status.as_str(), error_json, occurred_at, run_id],
                )
                .map_err(schema::map_sqlite)
                .map_err(run_from_session)?;
            Ok(())
        })
    }

    fn mutate_run(
        &self,
        run_id: &str,
        operation: impl FnOnce(&Transaction<'_>, &Run) -> Result<(), RunError>,
    ) -> Result<Run, RunError> {
        self.mutate_run_with(run_id, |transaction, run| {
            operation(transaction, run)?;
            Ok(())
        })
        .map(|(run, ())| run)
    }

    fn mutate_run_with<T>(
        &self,
        run_id: &str,
        operation: impl FnOnce(&Transaction<'_>, &Run) -> Result<T, RunError>,
    ) -> Result<(Run, T), RunError> {
        validate_run_id(run_id)?;
        let ready = self.ready().map_err(run_from_session)?.clone();
        let _guard = ready
            .write_lock
            .lock()
            .map_err(|_| RunError::StorageUnavailable)?;
        let mut connection = schema::open(&ready.db_path).map_err(run_from_session)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(schema::map_sqlite)
            .map_err(run_from_session)?;
        self.require_runtime_lease_tx(&transaction)?;
        let run = run_by_id_tx(&transaction, run_id)?.ok_or(RunError::NotFound)?;
        let value = operation(&transaction, &run)?;
        let updated = run_by_id_tx(&transaction, run_id)?.ok_or(RunError::DataInvalid)?;
        transaction
            .commit()
            .map_err(schema::map_sqlite)
            .map_err(run_from_session)?;
        Ok((updated, value))
    }

    fn run_connection(&self) -> Result<Connection, RunError> {
        schema::open(&self.ready().map_err(run_from_session)?.db_path).map_err(run_from_session)
    }
}

fn validate_tool_approval_request(request: &ToolApprovalRequest) -> Result<i64, ToolApprovalError> {
    validate_approval_id(&request.approval_id)?;
    validate_tool_call_key(&request.call_id).map_err(approval_from_run)?;
    if request.tool_name.is_empty()
        || request.tool_name.len() > MAX_TOOL_NAME_BYTES
        || request.tool_name.chars().any(char::is_control)
        || request.input_summary.as_ref().is_some_and(|summary| {
            summary.chars().count() > 2_000 || summary.chars().any(|character| character == '\0')
        })
        || request.choices != [ToolApprovalDecision::Once, ToolApprovalDecision::Deny]
    {
        return Err(ToolApprovalError::InvalidRequest);
    }
    approval_timestamp_unix_ms(&request.expires_at)
}

fn validate_approval_id(approval_id: &str) -> Result<(), ToolApprovalError> {
    if approval_id.is_empty()
        || approval_id.len() > MAX_APPROVAL_ID_BYTES
        || approval_id.chars().any(char::is_control)
    {
        Err(ToolApprovalError::InvalidRequest)
    } else {
        Ok(())
    }
}

fn validate_approval_reason(reason: Option<&str>) -> Result<(), ToolApprovalError> {
    if reason.is_some_and(|reason| {
        reason.chars().count() > MAX_APPROVAL_REASON_CHARS
            || reason.chars().any(|character| character == '\0')
    }) {
        Err(ToolApprovalError::InvalidRequest)
    } else {
        Ok(())
    }
}

fn approval_timestamp_unix_ms(value: &str) -> Result<i64, ToolApprovalError> {
    let timestamp =
        OffsetDateTime::parse(value, &Rfc3339).map_err(|_| ToolApprovalError::InvalidRequest)?;
    i64::try_from(timestamp.unix_timestamp_nanos().div_euclid(1_000_000))
        .map_err(|_| ToolApprovalError::InvalidRequest)
}

fn approval_now() -> Result<(String, i64), ToolApprovalError> {
    let now = OffsetDateTime::now_utc();
    let formatted = now
        .format(&Rfc3339)
        .map_err(|_| ToolApprovalError::DataInvalid)?;
    let unix_ms = i64::try_from(now.unix_timestamp_nanos().div_euclid(1_000_000))
        .map_err(|_| ToolApprovalError::DataInvalid)?;
    Ok((formatted, unix_ms))
}

fn serialize_approval_choices(
    choices: &[ToolApprovalDecision],
) -> Result<String, ToolApprovalError> {
    serde_json::to_string(choices).map_err(|_| ToolApprovalError::DataInvalid)
}

fn approval_request_matches(
    stored: &StoredToolApproval,
    run_id: &str,
    request: &ToolApprovalRequest,
    expires_at_unix_ms: i64,
) -> bool {
    stored.run_id == run_id
        && stored.approval_id == request.approval_id
        && stored.call_id == request.call_id
        && stored.tool_name == request.tool_name
        && stored.input_summary == request.input_summary
        && stored.choices == request.choices
        && stored.expires_at == request.expires_at
        && stored.expires_at_unix_ms == expires_at_unix_ms
}

fn require_latest_tool_started_event(
    transaction: &Transaction<'_>,
    run_id: &str,
    request: &ToolApprovalRequest,
) -> Result<Option<String>, ToolApprovalError> {
    let latest = transaction
        .query_row(
            "SELECT event_name, envelope_json FROM run_events \
             WHERE run_id = ?1 ORDER BY sequence DESC LIMIT 1",
            [run_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(schema::map_sqlite)
        .map_err(approval_from_session)?
        .ok_or(ToolApprovalError::DataInvalid)?;
    let envelope: serde_json::Value =
        serde_json::from_str(&latest.1).map_err(|_| ToolApprovalError::DataInvalid)?;
    if latest.0 != "tool.started"
        || envelope
            .pointer("/data/callId")
            .and_then(serde_json::Value::as_str)
            != Some(request.call_id.as_str())
        || envelope
            .pointer("/data/name")
            .and_then(serde_json::Value::as_str)
            != Some(request.tool_name.as_str())
    {
        return Err(ToolApprovalError::DataInvalid);
    }
    match envelope.pointer("/data/inputSummary") {
        Some(serde_json::Value::String(summary)) => Ok(Some(summary.clone())),
        Some(serde_json::Value::Null) | None => Ok(None),
        _ => Err(ToolApprovalError::DataInvalid),
    }
}

fn require_pending_approval_run(
    run: &Run,
    approval: &StoredToolApproval,
) -> Result<(), ToolApprovalError> {
    let expected_choices = approval
        .choices
        .iter()
        .map(|choice| choice.as_str().to_owned())
        .collect::<Vec<_>>();
    if run.status != RunStatus::WaitingApproval
        || approval.state != ToolApprovalState::Pending
        || !matches!(
            run.pending_action.as_ref(),
            Some(PendingAction::Approval {
                approval_id,
                call_id,
                tool_name,
                input_summary,
                choices,
                expires_at,
            }) if approval_id == &approval.approval_id
                && call_id == &approval.call_id
                && tool_name == &approval.tool_name
                && input_summary == &approval.input_summary
                && choices == &expected_choices
                && expires_at == &approval.expires_at
        )
    {
        return Err(ToolApprovalError::NoLongerPending);
    }
    Ok(())
}

fn replay_user_approval(
    approval: &StoredToolApproval,
    decision: ToolApprovalDecision,
    reason: Option<&str>,
) -> Result<ToolApprovalResolution, ToolApprovalError> {
    match approval.resolved_by {
        Some(ToolApprovalResolvedBy::User)
            if approval.decision == Some(decision) && approval.reason.as_deref() == reason =>
        {
            Ok(ToolApprovalResolution {
                approval: approval.clone(),
                disposition: ToolApprovalResolutionDisposition::Replayed,
            })
        }
        Some(ToolApprovalResolvedBy::User) => Err(ToolApprovalError::DecisionConflict),
        Some(ToolApprovalResolvedBy::Expiry) => Err(ToolApprovalError::Expired),
        Some(ToolApprovalResolvedBy::Cancellation) => Err(ToolApprovalError::NoLongerPending),
        None => Err(ToolApprovalError::DataInvalid),
    }
}

fn resolve_allowed_once_tx(
    transaction: &Transaction<'_>,
    run: &Run,
    approval: &StoredToolApproval,
    reason: Option<&str>,
    resolved_at: &str,
) -> Result<(), ToolApprovalError> {
    resolve_approval_ledger_tx(
        transaction,
        run,
        approval,
        ToolApprovalDecision::Once,
        reason,
        ToolApprovalResolvedBy::User,
        resolved_at,
    )?;
    let changed = transaction
        .execute(
            "UPDATE runs SET status = 'running', pending_action_json = NULL, updated_at = ?1 \
             WHERE id = ?2 AND status = 'waitingApproval'",
            params![resolved_at, run.id],
        )
        .map_err(schema::map_sqlite)
        .map_err(approval_from_session)?;
    if changed != 1 {
        return Err(ToolApprovalError::NoLongerPending);
    }
    Ok(())
}

fn resolve_denied_approval_tx(
    transaction: &Transaction<'_>,
    run: &Run,
    approval: &StoredToolApproval,
    resolved_by: ToolApprovalResolvedBy,
    reason: Option<&str>,
    next_status: RunStatus,
    resolved_at: &str,
) -> Result<(), ToolApprovalError> {
    if resolved_by != ToolApprovalResolvedBy::User && reason.is_some() {
        return Err(ToolApprovalError::DataInvalid);
    }
    resolve_approval_ledger_tx(
        transaction,
        run,
        approval,
        ToolApprovalDecision::Deny,
        reason,
        resolved_by,
        resolved_at,
    )?;

    let (code, title, detail, status, provider_content) = match resolved_by {
        ToolApprovalResolvedBy::Cancellation => (
            "tool_execution_cancelled",
            "Tool execution cancelled",
            "The tool was not executed because the run was cancelled.",
            499,
            "Tool execution cancelled before side effects began",
        ),
        ToolApprovalResolvedBy::User | ToolApprovalResolvedBy::Expiry => (
            "tool_execution_denied",
            "Tool execution denied",
            "The tool was not executed because approval was denied.",
            403,
            "Tool execution denied before side effects began",
        ),
    };
    let problem = RunProblem {
        problem_type: format!("urn:synthchat:error:{code}"),
        title: title.to_owned(),
        status,
        detail: Some(detail.to_owned()),
        instance: Some(format!("/api/v1/runs/{}", run.id)),
        code: code.to_owned(),
        request_id: format!("tool:{}", approval.call_id),
        retryable: false,
    };
    let raw_error =
        serialize_json(&json!({"code": code, "retryable": false})).map_err(approval_from_run)?;
    let checkpoint = i64::try_from(approval.invocation_checkpoint)
        .map_err(|_| ToolApprovalError::DataInvalid)?;
    let changed = transaction
        .execute(
            "UPDATE tool_invocations SET status = 'failed', checkpoint = checkpoint + 1,\
                error_json = ?1, provider_content = ?2, finished_at = ?3, updated_at = ?3 \
             WHERE run_id = ?4 AND call_id = ?5 AND status = 'running' AND checkpoint = ?6",
            params![
                raw_error,
                provider_content,
                resolved_at,
                run.id,
                approval.call_id,
                checkpoint,
            ],
        )
        .map_err(schema::map_sqlite)
        .map_err(approval_from_session)?;
    if changed != 1 {
        return Err(ToolApprovalError::DataInvalid);
    }
    insert_event_tx(
        transaction,
        &run.id,
        &run.session_id,
        "tool.failed",
        event_data(json!({"callId": approval.call_id, "error": problem}))
            .map_err(approval_from_run)?,
        resolved_at,
    )
    .map_err(approval_from_run)?;
    let changed = transaction
        .execute(
            "UPDATE runs SET status = ?1, pending_action_json = NULL, updated_at = ?2 \
             WHERE id = ?3 AND status = 'waitingApproval'",
            params![next_status.as_str(), resolved_at, run.id],
        )
        .map_err(schema::map_sqlite)
        .map_err(approval_from_session)?;
    if changed != 1 {
        return Err(ToolApprovalError::NoLongerPending);
    }
    Ok(())
}

fn resolve_approval_ledger_tx(
    transaction: &Transaction<'_>,
    run: &Run,
    approval: &StoredToolApproval,
    decision: ToolApprovalDecision,
    reason: Option<&str>,
    resolved_by: ToolApprovalResolvedBy,
    resolved_at: &str,
) -> Result<(), ToolApprovalError> {
    let changed = transaction
        .execute(
            "UPDATE run_approvals SET state = 'resolved', decision = ?1, reason = ?2,\
                resolved_by = ?3, resolved_at = ?4 \
             WHERE approval_id = ?5 AND run_id = ?6 AND state = 'pending'",
            params![
                decision.as_str(),
                reason,
                resolved_by.as_str(),
                resolved_at,
                approval.approval_id,
                run.id,
            ],
        )
        .map_err(schema::map_sqlite)
        .map_err(approval_from_session)?;
    if changed != 1 {
        return Err(ToolApprovalError::NoLongerPending);
    }
    insert_event_tx(
        transaction,
        &run.id,
        &run.session_id,
        "approval.resolved",
        event_data(json!({
            "approvalId": approval.approval_id,
            "callId": approval.call_id,
            "decision": decision,
            "resolvedBy": resolved_by.as_str(),
        }))
        .map_err(approval_from_run)?,
        resolved_at,
    )
    .map_err(approval_from_run)
}

fn tool_approval_by_id(
    connection: &Connection,
    approval_id: &str,
) -> Result<Option<StoredToolApproval>, ToolApprovalError> {
    connection
        .query_row(
            &tool_approval_select("WHERE approval_id = ?1"),
            [approval_id],
            tool_approval_row,
        )
        .optional()
        .map_err(|_| ToolApprovalError::DataInvalid)
}

fn tool_approval_for_call(
    connection: &Connection,
    run_id: &str,
    call_id: &str,
) -> Result<Option<StoredToolApproval>, ToolApprovalError> {
    connection
        .query_row(
            &tool_approval_select("WHERE run_id = ?1 AND call_id = ?2"),
            params![run_id, call_id],
            tool_approval_row,
        )
        .optional()
        .map_err(|_| ToolApprovalError::DataInvalid)
}

fn pending_tool_approval_for_run(
    connection: &Connection,
    run_id: &str,
) -> Result<Option<StoredToolApproval>, ToolApprovalError> {
    connection
        .query_row(
            &tool_approval_select("WHERE run_id = ?1 AND state = 'pending'"),
            [run_id],
            tool_approval_row,
        )
        .optional()
        .map_err(|_| ToolApprovalError::DataInvalid)
}

fn tool_approval_select(suffix: &str) -> String {
    format!(
        "SELECT approval_id, run_id, profile_id, session_id, workspace_id, call_id,\
            invocation_checkpoint, tool_name, arguments_sha256, input_summary, choices_json,\
            expires_at, expires_at_unix_ms, state, decision, reason, resolved_by, created_at,\
            resolved_at, execution_claimed_at \
         FROM run_approvals {suffix}"
    )
}

fn tool_approval_row(row: &Row<'_>) -> rusqlite::Result<StoredToolApproval> {
    let checkpoint: i64 = row.get(6)?;
    let arguments_sha256 = fixed_sha256(row, 8)?;
    let choices_json: String = row.get(10)?;
    let choices = parse_approval_choices(&choices_json).map_err(sql_json_error)?;
    let expires_at: String = row.get(11)?;
    let expires_at_unix_ms: i64 = row.get(12)?;
    if approval_timestamp_unix_ms(&expires_at).map_err(sql_json_error)? != expires_at_unix_ms {
        return Err(sql_json_error(ToolApprovalError::DataInvalid));
    }
    let state: String = row.get(13)?;
    let decision: Option<String> = row.get(14)?;
    let resolved_by: Option<String> = row.get(16)?;
    Ok(StoredToolApproval {
        approval_id: row.get(0)?,
        run_id: row.get(1)?,
        profile_id: row.get(2)?,
        session_id: row.get(3)?,
        workspace_id: row.get(4)?,
        call_id: row.get(5)?,
        invocation_checkpoint: u64::try_from(checkpoint)
            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(6, checkpoint))?,
        tool_name: row.get(7)?,
        arguments_sha256,
        input_summary: row.get(9)?,
        choices,
        expires_at,
        expires_at_unix_ms,
        state: approval_state_from_str(&state).map_err(sql_json_error)?,
        decision: decision
            .map(|decision| approval_decision_from_str(&decision).map_err(sql_json_error))
            .transpose()?,
        reason: row.get(15)?,
        resolved_by: resolved_by
            .map(|resolved_by| approval_resolved_by_from_str(&resolved_by).map_err(sql_json_error))
            .transpose()?,
        created_at: row.get(17)?,
        resolved_at: row.get(18)?,
        execution_claimed_at: row.get(19)?,
    })
}

fn fixed_sha256(row: &Row<'_>, index: usize) -> rusqlite::Result<[u8; 32]> {
    let value: Vec<u8> = row.get(index)?;
    value.try_into().map_err(|value: Vec<u8>| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            Type::Blob,
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "expected a 32-byte SHA-256 digest, got {} bytes",
                    value.len()
                ),
            )
            .into(),
        )
    })
}

fn parse_approval_choices(value: &str) -> Result<Vec<ToolApprovalDecision>, ToolApprovalError> {
    let choices: Vec<String> =
        serde_json::from_str(value).map_err(|_| ToolApprovalError::DataInvalid)?;
    let choices = choices
        .iter()
        .map(|choice| approval_decision_from_str(choice))
        .collect::<Result<Vec<_>, _>>()?;
    if choices != [ToolApprovalDecision::Once, ToolApprovalDecision::Deny] {
        return Err(ToolApprovalError::DataInvalid);
    }
    Ok(choices)
}

fn approval_decision_from_str(value: &str) -> Result<ToolApprovalDecision, ToolApprovalError> {
    match value {
        "once" => Ok(ToolApprovalDecision::Once),
        "session" => Ok(ToolApprovalDecision::Session),
        "always" => Ok(ToolApprovalDecision::Always),
        "deny" => Ok(ToolApprovalDecision::Deny),
        _ => Err(ToolApprovalError::DataInvalid),
    }
}

fn approval_state_from_str(value: &str) -> Result<ToolApprovalState, ToolApprovalError> {
    match value {
        "pending" => Ok(ToolApprovalState::Pending),
        "resolved" => Ok(ToolApprovalState::Resolved),
        _ => Err(ToolApprovalError::DataInvalid),
    }
}

fn approval_resolved_by_from_str(value: &str) -> Result<ToolApprovalResolvedBy, ToolApprovalError> {
    match value {
        "user" => Ok(ToolApprovalResolvedBy::User),
        "expiry" => Ok(ToolApprovalResolvedBy::Expiry),
        "cancellation" => Ok(ToolApprovalResolvedBy::Cancellation),
        _ => Err(ToolApprovalError::DataInvalid),
    }
}

fn approval_from_session(error: SessionError) -> ToolApprovalError {
    match error {
        SessionError::StorageBusy => ToolApprovalError::StorageBusy,
        SessionError::StorageUnavailable => ToolApprovalError::StorageUnavailable,
        SessionError::NotFound => ToolApprovalError::NotFound,
        SessionError::DataInvalid => ToolApprovalError::DataInvalid,
        _ => ToolApprovalError::InvalidRequest,
    }
}

fn approval_from_run(error: RunError) -> ToolApprovalError {
    match error {
        RunError::InvalidRequest | RunError::InvalidRunId => ToolApprovalError::InvalidRequest,
        RunError::NotFound => ToolApprovalError::NotFound,
        RunError::StorageBusy => ToolApprovalError::StorageBusy,
        RunError::StorageUnavailable => ToolApprovalError::StorageUnavailable,
        _ => ToolApprovalError::DataInvalid,
    }
}

fn run_from_approval(error: ToolApprovalError) -> RunError {
    match error {
        ToolApprovalError::InvalidRequest => RunError::InvalidRequest,
        ToolApprovalError::NotFound => RunError::NotFound,
        ToolApprovalError::StorageBusy => RunError::StorageBusy,
        ToolApprovalError::StorageUnavailable => RunError::StorageUnavailable,
        _ => RunError::DataInvalid,
    }
}

fn validate_clarification_request(
    request: &ClarificationRequest,
) -> Result<(), ClarificationError> {
    validate_clarification_id(&request.request_id)?;
    if validate_tool_call_key(&request.call_id).is_err()
        || request.question.trim().is_empty()
        || !(1..=MAX_CLARIFICATION_QUESTION_CHARS).contains(&request.question.chars().count())
        || request.question.contains('\0')
        || request.choices.len() > MAX_CLARIFICATION_CHOICES
    {
        return Err(ClarificationError::InvalidRequest);
    }
    let mut choices = HashSet::with_capacity(request.choices.len());
    for choice in &request.choices {
        if choice.trim().is_empty()
            || !(1..=MAX_CLARIFICATION_CHOICE_CHARS).contains(&choice.chars().count())
            || choice.contains('\0')
            || !choices.insert(choice.as_str())
        {
            return Err(ClarificationError::InvalidRequest);
        }
    }
    Ok(())
}

fn validate_clarification_id(request_id: &str) -> Result<(), ClarificationError> {
    if request_id.is_empty()
        || request_id.len() > MAX_CLARIFICATION_ID_BYTES
        || request_id.chars().any(char::is_control)
    {
        Err(ClarificationError::InvalidRequest)
    } else {
        Ok(())
    }
}

fn validate_clarification_answer(answer: &str) -> Result<(), ClarificationError> {
    if !(1..=MAX_CLARIFICATION_ANSWER_CHARS).contains(&answer.chars().count())
        || answer.contains('\0')
    {
        Err(ClarificationError::InvalidRequest)
    } else {
        Ok(())
    }
}

fn serialize_clarification_choices(choices: &[String]) -> Result<String, ClarificationError> {
    serde_json::to_string(choices).map_err(|_| ClarificationError::DataInvalid)
}

fn clarification_request_matches(
    stored: &StoredClarification,
    run_id: &str,
    request: &ClarificationRequest,
) -> bool {
    stored.request_id == request.request_id
        && stored.run_id == run_id
        && stored.call_id == request.call_id
        && stored.question == request.question
        && stored.choices == request.choices
}

fn require_latest_clarification_tool_started_event(
    transaction: &Transaction<'_>,
    run_id: &str,
    request: &ClarificationRequest,
) -> Result<(), ClarificationError> {
    let latest = transaction
        .query_row(
            "SELECT event_name, envelope_json FROM run_events \
             WHERE run_id = ?1 ORDER BY sequence DESC LIMIT 1",
            [run_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(schema::map_sqlite)
        .map_err(clarification_from_session)?
        .ok_or(ClarificationError::DataInvalid)?;
    let envelope: serde_json::Value =
        serde_json::from_str(&latest.1).map_err(|_| ClarificationError::DataInvalid)?;
    if latest.0 != "tool.started"
        || envelope
            .pointer("/data/callId")
            .and_then(serde_json::Value::as_str)
            != Some(request.call_id.as_str())
        || envelope
            .pointer("/data/name")
            .and_then(serde_json::Value::as_str)
            != Some("clarify")
    {
        return Err(ClarificationError::DataInvalid);
    }
    Ok(())
}

fn require_pending_clarification_run(
    run: &Run,
    clarification: &StoredClarification,
) -> Result<(), ClarificationError> {
    if run.status != RunStatus::WaitingClarification
        || clarification.state != ClarificationState::Pending
        || !matches!(
            run.pending_action.as_ref(),
            Some(PendingAction::Clarification {
                request_id,
                question,
                choices,
            }) if request_id == &clarification.request_id
                && question == &clarification.question
                && choices == &clarification.choices
        )
    {
        return Err(ClarificationError::NoLongerPending);
    }
    Ok(())
}

fn replay_user_clarification(
    clarification: &StoredClarification,
    answer: &str,
) -> Result<ClarificationResolution, ClarificationError> {
    match clarification.resolved_by {
        Some(ClarificationResolvedBy::User) if clarification.answer.as_deref() == Some(answer) => {
            Ok(ClarificationResolution {
                clarification: clarification.clone(),
                disposition: ClarificationResolutionDisposition::Replayed,
            })
        }
        Some(ClarificationResolvedBy::User) => Err(ClarificationError::AnswerConflict),
        Some(ClarificationResolvedBy::Cancellation | ClarificationResolvedBy::Failure) => {
            Err(ClarificationError::NoLongerPending)
        }
        None => Err(ClarificationError::DataInvalid),
    }
}

fn resolve_abandoned_clarification_tx(
    transaction: &Transaction<'_>,
    run: &Run,
    clarification: &StoredClarification,
    resolved_by: ClarificationResolvedBy,
    next_status: RunStatus,
    resolved_at: &str,
) -> Result<(), ClarificationError> {
    let (code, title, detail, status, provider_content) = match resolved_by {
        ClarificationResolvedBy::Cancellation => (
            "tool_execution_cancelled",
            "Tool execution cancelled",
            "The clarification was not answered because the run was cancelled.",
            499,
            "Clarification cancelled before an answer was received",
        ),
        ClarificationResolvedBy::Failure => (
            "clarification_interrupted",
            "Clarification interrupted",
            "The clarification was abandoned before an answer was received.",
            500,
            "Clarification interrupted before an answer was received",
        ),
        ClarificationResolvedBy::User => return Err(ClarificationError::DataInvalid),
    };
    resolve_clarification_ledger_tx(
        transaction,
        run,
        clarification,
        None,
        resolved_by,
        resolved_at,
    )?;

    let problem = RunProblem {
        problem_type: format!("urn:synthchat:error:{code}"),
        title: title.to_owned(),
        status,
        detail: Some(detail.to_owned()),
        instance: Some(format!("/api/v1/runs/{}", run.id)),
        code: code.to_owned(),
        request_id: format!("tool:{}", clarification.call_id),
        retryable: false,
    };
    let raw_error = serialize_json(&json!({"code": code, "retryable": false}))
        .map_err(clarification_from_run)?;
    let checkpoint = i64::try_from(clarification.invocation_checkpoint)
        .map_err(|_| ClarificationError::DataInvalid)?;
    let changed = transaction
        .execute(
            "UPDATE tool_invocations SET status = 'failed', checkpoint = checkpoint + 1,\
                error_json = ?1, provider_content = ?2, finished_at = ?3, updated_at = ?3 \
             WHERE run_id = ?4 AND call_id = ?5 AND status = 'running' AND checkpoint = ?6",
            params![
                raw_error,
                provider_content,
                resolved_at,
                run.id,
                clarification.call_id,
                checkpoint,
            ],
        )
        .map_err(schema::map_sqlite)
        .map_err(clarification_from_session)?;
    if changed != 1 {
        return Err(ClarificationError::DataInvalid);
    }
    insert_event_tx(
        transaction,
        &run.id,
        &run.session_id,
        "tool.failed",
        event_data(json!({"callId": clarification.call_id, "error": problem}))
            .map_err(clarification_from_run)?,
        resolved_at,
    )
    .map_err(clarification_from_run)?;
    let changed = transaction
        .execute(
            "UPDATE runs SET status = ?1, pending_action_json = NULL, updated_at = ?2 \
             WHERE id = ?3 AND status = 'waitingClarification'",
            params![next_status.as_str(), resolved_at, run.id],
        )
        .map_err(schema::map_sqlite)
        .map_err(clarification_from_session)?;
    if changed != 1 {
        return Err(ClarificationError::NoLongerPending);
    }
    Ok(())
}

fn resolve_clarification_ledger_tx(
    transaction: &Transaction<'_>,
    run: &Run,
    clarification: &StoredClarification,
    answer: Option<&str>,
    resolved_by: ClarificationResolvedBy,
    resolved_at: &str,
) -> Result<(), ClarificationError> {
    if (resolved_by == ClarificationResolvedBy::User) != answer.is_some() {
        return Err(ClarificationError::DataInvalid);
    }
    let changed = transaction
        .execute(
            "UPDATE run_clarifications SET state = 'resolved', answer = ?1,\
                resolved_by = ?2, resolved_at = ?3 \
             WHERE request_id = ?4 AND run_id = ?5 AND state = 'pending'",
            params![
                answer,
                resolved_by.as_str(),
                resolved_at,
                clarification.request_id,
                run.id,
            ],
        )
        .map_err(schema::map_sqlite)
        .map_err(clarification_from_session)?;
    if changed != 1 {
        return Err(ClarificationError::NoLongerPending);
    }
    insert_event_tx(
        transaction,
        &run.id,
        &run.session_id,
        "clarification.resolved",
        event_data(json!({
            "requestId": clarification.request_id,
            "resolvedBy": resolved_by.as_str(),
        }))
        .map_err(clarification_from_run)?,
        resolved_at,
    )
    .map_err(clarification_from_run)
}

fn clarification_by_id(
    connection: &Connection,
    request_id: &str,
) -> Result<Option<StoredClarification>, ClarificationError> {
    connection
        .query_row(
            &clarification_select("WHERE request_id = ?1"),
            [request_id],
            clarification_row,
        )
        .optional()
        .map_err(|_| ClarificationError::DataInvalid)
}

fn clarification_for_call(
    connection: &Connection,
    run_id: &str,
    call_id: &str,
) -> Result<Option<StoredClarification>, ClarificationError> {
    connection
        .query_row(
            &clarification_select("WHERE run_id = ?1 AND call_id = ?2"),
            params![run_id, call_id],
            clarification_row,
        )
        .optional()
        .map_err(|_| ClarificationError::DataInvalid)
}

fn pending_clarification_for_run(
    connection: &Connection,
    run_id: &str,
) -> Result<Option<StoredClarification>, ClarificationError> {
    connection
        .query_row(
            &clarification_select("WHERE run_id = ?1 AND state = 'pending'"),
            [run_id],
            clarification_row,
        )
        .optional()
        .map_err(|_| ClarificationError::DataInvalid)
}

fn clarification_select(suffix: &str) -> String {
    format!(
        "SELECT request_id, run_id, call_id, invocation_checkpoint, arguments_sha256,\
            question, choices_json, state, answer, resolved_by, created_at, resolved_at,\
            continuation_claimed_at \
         FROM run_clarifications {suffix}"
    )
}

fn clarification_row(row: &Row<'_>) -> rusqlite::Result<StoredClarification> {
    let checkpoint: i64 = row.get(3)?;
    let arguments_sha256 = fixed_sha256(row, 4)?;
    let choices_json: String = row.get(6)?;
    let choices = parse_clarification_choices(&choices_json).map_err(sql_json_error)?;
    let state: String = row.get(7)?;
    let state = clarification_state_from_str(&state).map_err(sql_json_error)?;
    let answer: Option<String> = row.get(8)?;
    let resolved_by: Option<String> = row.get(9)?;
    let resolved_by = resolved_by
        .map(|value| clarification_resolved_by_from_str(&value).map_err(sql_json_error))
        .transpose()?;
    if let Some(answer) = answer.as_deref() {
        validate_clarification_answer(answer).map_err(sql_json_error)?;
        if resolved_by != Some(ClarificationResolvedBy::User)
            || (!choices.is_empty() && !choices.iter().any(|choice| choice == answer))
        {
            return Err(sql_json_error(ClarificationError::DataInvalid));
        }
    }
    Ok(StoredClarification {
        request_id: row.get(0)?,
        run_id: row.get(1)?,
        call_id: row.get(2)?,
        invocation_checkpoint: u64::try_from(checkpoint)
            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(3, checkpoint))?,
        arguments_sha256,
        question: row.get(5)?,
        choices,
        state,
        answer,
        resolved_by,
        created_at: row.get(10)?,
        resolved_at: row.get(11)?,
        continuation_claimed_at: row.get(12)?,
    })
}

fn parse_clarification_choices(value: &str) -> Result<Vec<String>, ClarificationError> {
    let choices: Vec<String> =
        serde_json::from_str(value).map_err(|_| ClarificationError::DataInvalid)?;
    if choices.len() > MAX_CLARIFICATION_CHOICES {
        return Err(ClarificationError::DataInvalid);
    }
    let mut seen = HashSet::with_capacity(choices.len());
    if choices.iter().any(|choice| {
        choice.trim().is_empty()
            || !(1..=MAX_CLARIFICATION_CHOICE_CHARS).contains(&choice.chars().count())
            || choice.contains('\0')
            || !seen.insert(choice.as_str())
    }) {
        return Err(ClarificationError::DataInvalid);
    }
    Ok(choices)
}

fn clarification_state_from_str(value: &str) -> Result<ClarificationState, ClarificationError> {
    match value {
        "pending" => Ok(ClarificationState::Pending),
        "resolved" => Ok(ClarificationState::Resolved),
        _ => Err(ClarificationError::DataInvalid),
    }
}

fn clarification_resolved_by_from_str(
    value: &str,
) -> Result<ClarificationResolvedBy, ClarificationError> {
    match value {
        "user" => Ok(ClarificationResolvedBy::User),
        "cancellation" => Ok(ClarificationResolvedBy::Cancellation),
        "failure" => Ok(ClarificationResolvedBy::Failure),
        _ => Err(ClarificationError::DataInvalid),
    }
}

fn clarification_from_session(error: SessionError) -> ClarificationError {
    match error {
        SessionError::StorageBusy => ClarificationError::StorageBusy,
        SessionError::StorageUnavailable => ClarificationError::StorageUnavailable,
        SessionError::NotFound => ClarificationError::NotFound,
        SessionError::DataInvalid => ClarificationError::DataInvalid,
        _ => ClarificationError::InvalidRequest,
    }
}

fn clarification_from_run(error: RunError) -> ClarificationError {
    match error {
        RunError::InvalidRequest | RunError::InvalidRunId => ClarificationError::InvalidRequest,
        RunError::NotFound => ClarificationError::NotFound,
        RunError::StorageBusy => ClarificationError::StorageBusy,
        RunError::StorageUnavailable => ClarificationError::StorageUnavailable,
        _ => ClarificationError::DataInvalid,
    }
}

fn run_from_clarification(error: ClarificationError) -> RunError {
    match error {
        ClarificationError::InvalidRequest => RunError::InvalidRequest,
        ClarificationError::NotFound => RunError::NotFound,
        ClarificationError::StorageBusy => RunError::StorageBusy,
        ClarificationError::StorageUnavailable => RunError::StorageUnavailable,
        _ => RunError::DataInvalid,
    }
}

fn validate_provider_turn_plan(plan: &ProviderTurnPlan) -> Result<(), RunError> {
    validate_usage(&plan.usage)?;
    if plan.turn_index == 0
        || plan.turn_index > MAX_PROVIDER_TURNS
        || plan.assistant_message_id.is_empty()
        || plan.assistant_message_id.len() > MAX_RUN_ID_BYTES
        || plan.assistant_message_id.chars().any(char::is_control)
        || plan
            .content
            .as_ref()
            .is_some_and(|content| content.chars().count() > MAX_MESSAGE_TEXT_CHARS)
        || plan
            .reasoning
            .as_ref()
            .is_some_and(|reasoning| reasoning.chars().count() > MAX_MESSAGE_TEXT_CHARS)
        || plan.tool_calls.len() > MAX_TOOL_CALLS_PER_TURN
        || (plan.finish == ProviderTurnFinish::ToolCalls) == plan.tool_calls.is_empty()
    {
        return Err(RunError::DataInvalid);
    }
    let mut call_ids = HashSet::new();
    for call in &plan.tool_calls {
        if validate_tool_call_key(&call.call_id).is_err()
            || call.tool_name.is_empty()
            || call.tool_name.len() > MAX_TOOL_NAME_BYTES
            || call.tool_name.chars().any(char::is_control)
            || validate_bounded_json(&call.arguments_json, MAX_TOOL_ARGUMENT_BYTES, true).is_err()
            || !call_ids.insert(call.call_id.as_str())
        {
            return Err(RunError::DataInvalid);
        }
    }
    Ok(())
}

fn validate_tool_call_key(call_id: &str) -> Result<(), RunError> {
    if call_id.is_empty()
        || call_id.len() > MAX_TOOL_CALL_ID_BYTES
        || call_id.chars().any(char::is_control)
    {
        Err(RunError::DataInvalid)
    } else {
        Ok(())
    }
}

fn validate_public_tool_fields(
    call_id: &str,
    name: &str,
    summary: &str,
    maximum_summary_chars: usize,
) -> Result<(), RunError> {
    validate_tool_call_key(call_id)?;
    if name.is_empty()
        || name.len() > MAX_TOOL_NAME_BYTES
        || name.chars().any(char::is_control)
        || summary.chars().count() > maximum_summary_chars
        || summary.chars().any(|character| character == '\0')
    {
        return Err(RunError::InvalidRequest);
    }
    Ok(())
}

fn validate_tool_terminal_input(
    call_id: &str,
    expected_checkpoint: u64,
    provider_content: &str,
) -> Result<(), RunError> {
    validate_tool_call_key(call_id)?;
    if expected_checkpoint == 0 || provider_content.len() > MAX_TOOL_RESULT_BYTES {
        Err(RunError::DataInvalid)
    } else {
        Ok(())
    }
}

fn validate_bounded_json(
    value: &str,
    max_bytes: usize,
    require_object: bool,
) -> Result<(), RunError> {
    if value.len() > max_bytes {
        return Err(RunError::DataInvalid);
    }
    let parsed: serde_json::Value =
        serde_json::from_str(value).map_err(|_| RunError::DataInvalid)?;
    if require_object && !parsed.is_object() {
        return Err(RunError::DataInvalid);
    }
    Ok(())
}

fn provider_turn_by_index(
    connection: &Connection,
    run_id: &str,
    turn_index: u32,
) -> Result<Option<StoredProviderTurn>, RunError> {
    let mut turn = connection
        .query_row(
            "SELECT run_id, turn_index, assistant_message_id, content, reasoning,\
                finish_reason, usage_json, created_at FROM run_turns \
             WHERE run_id = ?1 AND turn_index = ?2",
            params![run_id, i64::from(turn_index)],
            provider_turn_row,
        )
        .optional()
        .map_err(|_| RunError::DataInvalid)?;
    if let Some(turn) = &mut turn {
        turn.tool_calls = tool_invocations_for_turn(connection, run_id, turn_index)?;
        validate_stored_turn(turn)?;
    }
    Ok(turn)
}

fn provider_turns_for_run(
    connection: &Connection,
    run_id: &str,
) -> Result<Vec<StoredProviderTurn>, RunError> {
    let mut statement = connection
        .prepare(
            "SELECT run_id, turn_index, assistant_message_id, content, reasoning,\
                finish_reason, usage_json, created_at FROM run_turns \
             WHERE run_id = ?1 ORDER BY turn_index ASC",
        )
        .map_err(schema::map_sqlite)
        .map_err(run_from_session)?;
    let rows = statement
        .query_map([run_id], provider_turn_row)
        .map_err(schema::map_sqlite)
        .map_err(run_from_session)?;
    let mut turns = rows
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| RunError::DataInvalid)?;
    drop(statement);
    for turn in &mut turns {
        turn.tool_calls = tool_invocations_for_turn(connection, run_id, turn.turn_index)?;
        validate_stored_turn(turn)?;
    }
    Ok(turns)
}

fn validate_stored_turn(turn: &StoredProviderTurn) -> Result<(), RunError> {
    validate_usage(&turn.usage)?;
    if (turn.finish == ProviderTurnFinish::ToolCalls) == turn.tool_calls.is_empty() {
        Err(RunError::DataInvalid)
    } else {
        Ok(())
    }
}

fn provider_turn_row(row: &Row<'_>) -> rusqlite::Result<StoredProviderTurn> {
    let turn_index: i64 = row.get(1)?;
    let finish_reason: String = row.get(5)?;
    let usage_json: String = row.get(6)?;
    Ok(StoredProviderTurn {
        run_id: row.get(0)?,
        turn_index: u32::try_from(turn_index)
            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(1, turn_index))?,
        assistant_message_id: row.get(2)?,
        content: row.get(3)?,
        reasoning: row.get(4)?,
        finish: provider_finish_from_str(&finish_reason).map_err(sql_json_error)?,
        usage: serde_json::from_str(&usage_json).map_err(sql_json_error)?,
        created_at: row.get(7)?,
        tool_calls: Vec::new(),
    })
}

fn provider_finish_from_str(value: &str) -> Result<ProviderTurnFinish, RunError> {
    match value {
        "stop" => Ok(ProviderTurnFinish::Stop),
        "toolCalls" => Ok(ProviderTurnFinish::ToolCalls),
        "length" => Ok(ProviderTurnFinish::Length),
        "contentFilter" => Ok(ProviderTurnFinish::ContentFilter),
        _ => Err(RunError::DataInvalid),
    }
}

fn tool_invocation_by_id(
    connection: &Connection,
    run_id: &str,
    call_id: &str,
) -> Result<Option<StoredToolInvocation>, RunError> {
    connection
        .query_row(
            &tool_invocation_select(
                "WHERE run_id = ?1 AND call_id = ?2 ORDER BY turn_index, call_index",
            ),
            params![run_id, call_id],
            tool_invocation_row,
        )
        .optional()
        .map_err(|_| RunError::DataInvalid)
}

fn tool_invocations_for_turn(
    connection: &Connection,
    run_id: &str,
    turn_index: u32,
) -> Result<Vec<StoredToolInvocation>, RunError> {
    let mut statement = connection
        .prepare(&tool_invocation_select(
            "WHERE run_id = ?1 AND turn_index = ?2 AND origin = 'provider' \
             ORDER BY call_index ASC",
        ))
        .map_err(schema::map_sqlite)
        .map_err(run_from_session)?;
    let rows = statement
        .query_map(params![run_id, i64::from(turn_index)], tool_invocation_row)
        .map_err(schema::map_sqlite)
        .map_err(run_from_session)?;
    let invocations = rows
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| RunError::DataInvalid)?;
    for (expected, invocation) in invocations.iter().enumerate() {
        if usize::try_from(invocation.call_index).ok() != Some(expected) {
            return Err(RunError::DataInvalid);
        }
    }
    Ok(invocations)
}

#[cfg(test)]
fn unfinished_tool_invocations_for_run(
    connection: &Connection,
    run_id: &str,
) -> Result<Vec<StoredToolInvocation>, RunError> {
    let mut statement = connection
        .prepare(&tool_invocation_select(
            "WHERE run_id = ?1 AND status IN ('planned', 'running')\
             ORDER BY turn_index ASC, call_index ASC",
        ))
        .map_err(schema::map_sqlite)
        .map_err(run_from_session)?;
    let rows = statement
        .query_map([run_id], tool_invocation_row)
        .map_err(schema::map_sqlite)
        .map_err(run_from_session)?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|_| RunError::DataInvalid)
}

fn tool_invocation_select(suffix: &str) -> String {
    format!(
        "SELECT run_id, turn_index, call_index, call_id, tool_name, arguments_json,\
            status, attempt, checkpoint, result_json, error_json, provider_content,\
            planned_at, started_at, finished_at, updated_at, origin, parent_call_id,\
            rpc_sequence FROM tool_invocations {suffix}"
    )
}

fn tool_invocation_row(row: &Row<'_>) -> rusqlite::Result<StoredToolInvocation> {
    let turn_index: i64 = row.get(1)?;
    let call_index: i64 = row.get(2)?;
    let arguments_json: String = row.get(5)?;
    let status: String = row.get(6)?;
    let attempt: i64 = row.get(7)?;
    let checkpoint: i64 = row.get(8)?;
    let result_json: Option<String> = row.get(9)?;
    let error_json: Option<String> = row.get(10)?;
    let origin: String = row.get(16)?;
    let rpc_sequence: Option<i64> = row.get(18)?;
    Ok(StoredToolInvocation {
        run_id: row.get(0)?,
        turn_index: u32::try_from(turn_index)
            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(1, turn_index))?,
        call_index: u32::try_from(call_index)
            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(2, call_index))?,
        call_id: row.get(3)?,
        tool_name: row.get(4)?,
        arguments_json: {
            validate_bounded_json(&arguments_json, MAX_TOOL_ARGUMENT_BYTES, true)
                .map_err(sql_json_error)?;
            arguments_json
        },
        status: tool_status_from_str(&status).map_err(sql_json_error)?,
        attempt: u32::try_from(attempt)
            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(7, attempt))?,
        checkpoint: u64::try_from(checkpoint)
            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(8, checkpoint))?,
        result_json: result_json
            .map(|value| {
                validate_bounded_json(&value, MAX_TOOL_RESULT_BYTES, false)
                    .map_err(sql_json_error)?;
                Ok::<String, rusqlite::Error>(value)
            })
            .transpose()?,
        error_json: error_json
            .map(|value| {
                validate_bounded_json(&value, MAX_TOOL_RESULT_BYTES, false)
                    .map_err(sql_json_error)?;
                Ok::<String, rusqlite::Error>(value)
            })
            .transpose()?,
        provider_content: row.get(11)?,
        origin: tool_origin_from_str(&origin).map_err(sql_json_error)?,
        parent_call_id: row.get(17)?,
        rpc_sequence: rpc_sequence
            .map(|value| {
                u32::try_from(value)
                    .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(18, value))
            })
            .transpose()?,
        planned_at: row.get(12)?,
        started_at: row.get(13)?,
        finished_at: row.get(14)?,
        updated_at: row.get(15)?,
    })
}

fn tool_origin_from_str(value: &str) -> Result<ToolInvocationOrigin, RunError> {
    match value {
        "provider" => Ok(ToolInvocationOrigin::Provider),
        "codeRpc" => Ok(ToolInvocationOrigin::CodeRpc),
        _ => Err(RunError::DataInvalid),
    }
}

fn tool_status_from_str(value: &str) -> Result<ToolInvocationStatus, RunError> {
    match value {
        "planned" => Ok(ToolInvocationStatus::Planned),
        "running" => Ok(ToolInvocationStatus::Running),
        "completed" => Ok(ToolInvocationStatus::Completed),
        "failed" => Ok(ToolInvocationStatus::Failed),
        _ => Err(RunError::DataInvalid),
    }
}

fn base_provider_context(
    connection: &Connection,
    run_id: &str,
    session_id: &str,
) -> Result<Vec<ProviderContextMessage>, RunError> {
    let mut statement = connection
        .prepare(
            "WITH current_run AS (\
                SELECT user.sequence AS cutoff_sequence \
                FROM runs run JOIN messages user ON user.id = run.user_message_id \
                WHERE run.id = ?1 AND run.session_id = ?2\
             ), logical_messages AS (\
                SELECT message.sequence, message.role, message.parts_json, \
                    COALESCE(owner_user.sequence, message.sequence) AS logical_sequence, \
                    CASE WHEN owner_user.sequence IS NULL THEN 0 ELSE 1 END AS logical_part \
                FROM messages message \
                LEFT JOIN runs assistant_run ON assistant_run.message_id = message.id \
                LEFT JOIN messages owner_user ON owner_user.id = assistant_run.user_message_id \
                CROSS JOIN current_run \
                WHERE message.session_id = ?2 AND message.context_eligible = 1 \
                    AND (message.sequence <= current_run.cutoff_sequence \
                        OR owner_user.sequence <= current_run.cutoff_sequence)\
             ) \
             SELECT role, parts_json FROM (\
                SELECT sequence, role, parts_json, logical_sequence, logical_part \
                FROM logical_messages \
                ORDER BY logical_sequence DESC, logical_part DESC, sequence DESC LIMIT ?3\
             ) ORDER BY logical_sequence ASC, logical_part ASC, sequence ASC",
        )
        .map_err(schema::map_sqlite)
        .map_err(run_from_session)?;
    let rows = statement
        .query_map(params![run_id, session_id, MAX_CONTEXT_MESSAGES], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(schema::map_sqlite)
        .map_err(run_from_session)?;
    let mut messages = Vec::new();
    for row in rows {
        let (role, parts_json) = row.map_err(|_| RunError::DataInvalid)?;
        if role == "tool" {
            continue;
        }
        let parts: Vec<MessagePart> =
            serde_json::from_str(&parts_json).map_err(|_| RunError::DataInvalid)?;
        let content = searchable_text(&parts);
        if content.is_empty() {
            continue;
        }
        let message = match role.as_str() {
            "system" => ProviderContextMessage::System { content },
            "user" => ProviderContextMessage::User { content },
            "assistant" => ProviderContextMessage::Assistant {
                content: Some(content),
                tool_calls: Vec::new(),
            },
            _ => return Err(RunError::DataInvalid),
        };
        messages.push(message);
    }
    let mut kept = Vec::new();
    let mut total = 0usize;
    for message in messages.into_iter().rev() {
        let length = match &message {
            ProviderContextMessage::System { content }
            | ProviderContextMessage::User { content } => content.chars().count(),
            ProviderContextMessage::Assistant { content, .. } => {
                content.as_deref().unwrap_or_default().chars().count()
            }
            ProviderContextMessage::Tool { content, .. } => content.chars().count(),
        };
        if !kept.is_empty() && total.saturating_add(length) > MAX_CONTEXT_CHARS {
            break;
        }
        total = total.saturating_add(length);
        kept.push(message);
    }
    kept.reverse();
    Ok(kept)
}

fn insert_message_tx(
    transaction: &Transaction<'_>,
    session_id: &str,
    current: &super::store::StoredSession,
    request: &CommitMessage,
    fixed_message_id: Option<&str>,
) -> Result<(Message, Versioned<super::Session>), RunError> {
    validate_message(request).map_err(run_from_session)?;
    let parts_json = serialize_json(&request.parts)?;
    let tool_calls_json = serialize_json(&request.tool_calls)?;
    let searchable = searchable_text(&request.parts);
    let change = next_change(transaction).map_err(run_from_session)?;
    let message_id = fixed_message_id
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| format!("message_{}", Uuid::new_v4().simple()));
    let created_at = next_timestamp(&current.value.updated_at).map_err(run_from_session)?;
    let sequence = current.next_message_sequence;
    transaction
        .execute(
            "INSERT INTO messages(\
                id, session_id, sequence, role, parts_json, reasoning, tool_calls_json,\
                searchable_text, created_at, committed_change, context_eligible\
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 1)",
            params![
                message_id,
                session_id,
                i64::try_from(sequence).map_err(|_| RunError::DataInvalid)?,
                request.role.as_str(),
                parts_json,
                request.reasoning,
                tool_calls_json,
                searchable,
                created_at,
                change,
            ],
        )
        .map_err(schema::map_sqlite)
        .map_err(run_from_session)?;
    if let Some(usage) = &request.usage {
        validate_usage(usage)?;
        transaction
            .execute(
                "INSERT INTO message_usage(\
                    message_id, prompt_tokens, completion_tokens, total_tokens, cost\
                 ) VALUES(?1, ?2, ?3, ?4, ?5)",
                params![
                    message_id,
                    i64::try_from(usage.prompt_tokens).map_err(|_| RunError::DataInvalid)?,
                    i64::try_from(usage.completion_tokens).map_err(|_| RunError::DataInvalid)?,
                    i64::try_from(usage.total_tokens).map_err(|_| RunError::DataInvalid)?,
                    usage.cost
                ],
            )
            .map_err(schema::map_sqlite)
            .map_err(run_from_session)?;
    }
    let preview = if searchable.is_empty() {
        current.value.preview.clone()
    } else {
        truncate_chars(&searchable, 500)
    };
    let model = request.model.as_deref().unwrap_or(&current.value.model);
    let revision = new_revision();
    close_current_version(transaction, session_id, change).map_err(run_from_session)?;
    transaction
        .execute(
            "UPDATE sessions SET preview = ?1, model = ?2, message_count = message_count + 1,\
                revision = ?3, updated_at = ?4, next_message_sequence = next_message_sequence + 1,\
                current_change = ?5 WHERE id = ?6",
            params![preview, model, revision, created_at, change, session_id],
        )
        .map_err(schema::map_sqlite)
        .map_err(run_from_session)?;
    insert_current_version(transaction, session_id, change).map_err(run_from_session)?;
    let message = message_by_id_tx(transaction, &message_id)
        .map_err(run_from_session)?
        .ok_or(RunError::DataInvalid)?;
    let updated = current_session_tx(transaction, session_id)
        .map_err(run_from_session)?
        .ok_or(RunError::DataInvalid)?;
    Ok((
        message,
        Versioned {
            etag: format!("\"{}\"", updated.value.revision),
            value: updated.value,
        },
    ))
}

fn lookup_replay(
    connection: &Connection,
    session_id: &str,
    canonical_path: &str,
    idempotency_key: &str,
    fingerprint: &str,
) -> Result<Option<RunAccepted>, RunError> {
    let record = connection
        .query_row(
            "SELECT request_fingerprint, resource_id FROM idempotency_records \
             WHERE method = 'POST' AND canonical_path = ?1 AND idempotency_key = ?2",
            params![canonical_path, idempotency_key],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(schema::map_sqlite)
        .map_err(run_from_session)?;
    replay_from_record(connection, session_id, record, fingerprint)
}

fn lookup_replay_tx(
    transaction: &Transaction<'_>,
    session_id: &str,
    canonical_path: &str,
    idempotency_key: &str,
    fingerprint: &str,
) -> Result<Option<RunAccepted>, RunError> {
    let record = transaction
        .query_row(
            "SELECT request_fingerprint, resource_id FROM idempotency_records \
             WHERE method = 'POST' AND canonical_path = ?1 AND idempotency_key = ?2",
            params![canonical_path, idempotency_key],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(schema::map_sqlite)
        .map_err(run_from_session)?;
    let Some((stored_fingerprint, run_id)) = record else {
        return Ok(None);
    };
    if stored_fingerprint != fingerprint {
        return Err(RunError::IdempotencyConflict);
    }
    let run = run_by_id_tx(transaction, &run_id)?.ok_or(RunError::IdempotentResourceDeleted)?;
    let queue_item_id = queue_item_id_tx(transaction, &run_id, run.status)?;
    let session = current_session_tx(transaction, session_id)
        .map_err(run_from_session)?
        .ok_or(RunError::IdempotentResourceDeleted)?;
    let user_message = transaction
        .query_row(
            "SELECT m.id, m.session_id, m.sequence, m.role, m.parts_json, m.reasoning,\
                m.tool_calls_json, m.created_at, u.prompt_tokens, u.completion_tokens,\
                u.total_tokens, u.cost FROM messages m \
             LEFT JOIN message_usage u ON u.message_id = m.id \
             WHERE m.id = (SELECT user_message_id FROM runs WHERE id = ?1)",
            [run_id],
            message_row,
        )
        .optional()
        .map_err(|_| RunError::DataInvalid)?
        .ok_or(RunError::IdempotentResourceDeleted)?;
    Ok(Some(RunAccepted {
        run,
        disposition: RunDisposition::Replayed,
        queue_item_id,
        user_message,
        session_revision: session.value.revision,
    }))
}

fn replay_from_record(
    connection: &Connection,
    session_id: &str,
    record: Option<(String, String)>,
    fingerprint: &str,
) -> Result<Option<RunAccepted>, RunError> {
    let Some((stored_fingerprint, run_id)) = record else {
        return Ok(None);
    };
    if stored_fingerprint != fingerprint {
        return Err(RunError::IdempotencyConflict);
    }
    let run = run_by_id(connection, &run_id)?.ok_or(RunError::IdempotentResourceDeleted)?;
    let queue_item_id = queue_item_id(connection, &run_id, run.status)?;
    let session_revision: Option<String> = connection
        .query_row(
            "SELECT revision FROM sessions WHERE id = ?1",
            [session_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(schema::map_sqlite)
        .map_err(run_from_session)?;
    let session_revision = session_revision.ok_or(RunError::IdempotentResourceDeleted)?;
    let user_message = connection
        .query_row(
            "SELECT m.id, m.session_id, m.sequence, m.role, m.parts_json, m.reasoning,\
                m.tool_calls_json, m.created_at, u.prompt_tokens, u.completion_tokens,\
                u.total_tokens, u.cost FROM messages m \
             LEFT JOIN message_usage u ON u.message_id = m.id \
             WHERE m.id = (SELECT user_message_id FROM runs WHERE id = ?1)",
            [run_id],
            message_row,
        )
        .optional()
        .map_err(|_| RunError::DataInvalid)?
        .ok_or(RunError::IdempotentResourceDeleted)?;
    Ok(Some(RunAccepted {
        run,
        disposition: RunDisposition::Replayed,
        queue_item_id,
        user_message,
        session_revision,
    }))
}

fn insert_event_tx(
    transaction: &Transaction<'_>,
    run_id: &str,
    session_id: &str,
    event_name: &str,
    data: serde_json::Value,
    occurred_at: &str,
) -> Result<(), RunError> {
    let last_sequence: i64 = transaction
        .query_row(
            "SELECT last_sequence FROM runs WHERE id = ?1",
            [run_id],
            |row| row.get(0),
        )
        .map_err(schema::map_sqlite)
        .map_err(run_from_session)?;
    let sequence = last_sequence.checked_add(1).ok_or(RunError::DataInvalid)?;
    let envelope = json!({
        "schemaVersion": 1,
        "sequence": sequence,
        "runId": run_id,
        "sessionId": session_id,
        "occurredAt": occurred_at,
        "data": data,
    });
    let envelope_json = serialize_json(&envelope)?;
    transaction
        .execute(
            "INSERT INTO run_events(run_id, sequence, event_name, occurred_at, envelope_json) \
             VALUES(?1, ?2, ?3, ?4, ?5)",
            params![run_id, sequence, event_name, occurred_at, envelope_json],
        )
        .map_err(schema::map_sqlite)
        .map_err(run_from_session)?;
    transaction
        .execute(
            "UPDATE runs SET last_sequence = ?1, updated_at = ?2 WHERE id = ?3",
            params![sequence, occurred_at, run_id],
        )
        .map_err(schema::map_sqlite)
        .map_err(run_from_session)?;
    let keep = i64::try_from(MAX_RUN_EVENTS).map_err(|_| RunError::DataInvalid)?;
    if sequence > keep {
        transaction
            .execute(
                "DELETE FROM run_events WHERE run_id = ?1 AND sequence <= ?2",
                params![run_id, sequence - keep],
            )
            .map_err(schema::map_sqlite)
            .map_err(run_from_session)?;
    }
    Ok(())
}

fn has_pending_async_tool_delivery(
    connection: &Connection,
    run_id: &str,
) -> Result<bool, RunError> {
    connection
        .query_row(
            "SELECT EXISTS(\
                SELECT 1 FROM terminal_processes p \
                JOIN async_tool_deliveries d ON d.process_id = p.process_id \
                WHERE p.creator_run_id = ?1 AND d.state = 'pending'\
             )",
            [run_id],
            |row| row.get(0),
        )
        .map_err(schema::map_sqlite)
        .map_err(run_from_session)
}

fn queue_item_id(
    connection: &Connection,
    run_id: &str,
    status: RunStatus,
) -> Result<Option<String>, RunError> {
    let value: Option<String> = connection
        .query_row(
            "SELECT queue_item_id FROM runs WHERE id = ?1",
            [run_id],
            |row| row.get(0),
        )
        .map_err(schema::map_sqlite)
        .map_err(run_from_session)?;
    validate_queue_item_status(value, status)
}

fn queue_item_id_tx(
    transaction: &Transaction<'_>,
    run_id: &str,
    status: RunStatus,
) -> Result<Option<String>, RunError> {
    let value: Option<String> = transaction
        .query_row(
            "SELECT queue_item_id FROM runs WHERE id = ?1",
            [run_id],
            |row| row.get(0),
        )
        .map_err(schema::map_sqlite)
        .map_err(run_from_session)?;
    validate_queue_item_status(value, status)
}

fn validate_queue_item_status(
    value: Option<String>,
    status: RunStatus,
) -> Result<Option<String>, RunError> {
    if let Some(value) = value.as_deref() {
        validate_queue_item_id(value).map_err(|_| RunError::DataInvalid)?;
    }
    if (status == RunStatus::Queued) != value.is_some() {
        return Err(RunError::DataInvalid);
    }
    Ok(value)
}

fn run_by_id(connection: &Connection, run_id: &str) -> Result<Option<Run>, RunError> {
    connection
        .query_row(run_sql(), [run_id], run_from_row)
        .optional()
        .map_err(|_| RunError::DataInvalid)
}

fn run_by_id_tx(transaction: &Transaction<'_>, run_id: &str) -> Result<Option<Run>, RunError> {
    transaction
        .query_row(run_sql(), [run_id], run_from_row)
        .optional()
        .map_err(|_| RunError::DataInvalid)
}

fn run_sql() -> &'static str {
    "SELECT id, session_id, profile_id, status, last_sequence, message_id, usage_json,\
        error_json, pending_action_json, created_at, updated_at FROM runs WHERE id = ?1"
}

fn run_from_row(row: &Row<'_>) -> rusqlite::Result<Run> {
    let status: String = row.get(3)?;
    let last_sequence: i64 = row.get(4)?;
    let usage_json: Option<String> = row.get(6)?;
    let error_json: Option<String> = row.get(7)?;
    let pending_action_json: Option<String> = row.get(8)?;
    Ok(Run {
        id: row.get(0)?,
        session_id: row.get(1)?,
        profile_id: row.get(2)?,
        status: RunStatus::try_from(status.as_str()).map_err(sql_json_error)?,
        last_sequence: u64::try_from(last_sequence)
            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(4, last_sequence))?,
        message_id: row.get(5)?,
        usage: parse_optional_json(usage_json, 6)?,
        error: parse_optional_json(error_json, 7)?,
        pending_action: parse_optional_json(pending_action_json, 8)?,
        created_at: row.get(9)?,
        updated_at: row.get(10)?,
    })
}

fn event_from_row(row: &Row<'_>) -> rusqlite::Result<RunEventRecord> {
    let sequence: i64 = row.get(0)?;
    Ok(RunEventRecord {
        sequence: u64::try_from(sequence)
            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, sequence))?,
        event_name: row.get(1)?,
        envelope_json: row.get(2)?,
    })
}

fn message_row(row: &Row<'_>) -> rusqlite::Result<Message> {
    let sequence: i64 = row.get(2)?;
    let role: String = row.get(3)?;
    let parts_json: String = row.get(4)?;
    let tool_calls_json: String = row.get(6)?;
    let prompt_tokens: Option<i64> = row.get(8)?;
    let usage = prompt_tokens
        .map(|prompt_tokens| -> rusqlite::Result<Usage> {
            let completion_tokens: i64 = row.get(9)?;
            let total_tokens: i64 = row.get(10)?;
            Ok(Usage {
                prompt_tokens: u64::try_from(prompt_tokens)
                    .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(8, prompt_tokens))?,
                completion_tokens: u64::try_from(completion_tokens)
                    .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(9, completion_tokens))?,
                total_tokens: u64::try_from(total_tokens)
                    .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(10, total_tokens))?,
                cost: row.get(11)?,
            })
        })
        .transpose()?;
    Ok(Message {
        id: row.get(0)?,
        session_id: row.get(1)?,
        sequence: u64::try_from(sequence)
            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(2, sequence))?,
        role: match role.as_str() {
            "user" => MessageRole::User,
            "assistant" => MessageRole::Assistant,
            "system" => MessageRole::System,
            "tool" => MessageRole::Tool,
            _ => return Err(sql_json_error(RunError::DataInvalid)),
        },
        parts: serde_json::from_str(&parts_json).map_err(sql_json_error)?,
        reasoning: row.get(5)?,
        tool_calls: serde_json::from_str::<Vec<ToolCall>>(&tool_calls_json)
            .map_err(sql_json_error)?,
        usage,
        created_at: row.get(7)?,
    })
}

fn parse_optional_json<T: serde::de::DeserializeOwned>(
    value: Option<String>,
    column: usize,
) -> rusqlite::Result<Option<T>> {
    value
        .map(|value| {
            serde_json::from_str(&value).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    column,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })
        })
        .transpose()
}

fn serialize_json(value: &impl Serialize) -> Result<String, RunError> {
    serde_json::to_string(value).map_err(|_| RunError::DataInvalid)
}

fn request_fingerprint(request: &CreateRun) -> Result<String, RunError> {
    let bytes = serde_json::to_vec(request).map_err(|_| RunError::InvalidRequest)?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn canonical_run_path(session_id: &str) -> String {
    format!("/api/v1/sessions/{session_id}/runs")
}

fn validate_create_run(
    session_id: &str,
    request: &CreateRun,
    idempotency_key: &str,
) -> Result<(), RunError> {
    validate_session_id(session_id)?;
    if request.client_request_id.is_empty()
        || request.client_request_id.len() > MAX_CLIENT_REQUEST_ID_BYTES
        || request.client_request_id.chars().any(char::is_control)
        || idempotency_key.len() < 8
        || idempotency_key.len() > MAX_IDEMPOTENCY_KEY_BYTES
        || !idempotency_key
            .bytes()
            .all(|byte| (0x21..=0x7e).contains(&byte))
        || request.message.text.chars().count() > MAX_MESSAGE_TEXT_CHARS
        || request.message.file_ids.len() > MAX_FILE_IDS
        || request.persona_id.as_deref().is_some_and(|id| {
            !id.strip_prefix("persona_").is_some_and(|suffix| {
                suffix.len() == 32
                    && suffix
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            })
        })
    {
        return Err(RunError::InvalidRequest);
    }
    let mut file_ids = HashSet::new();
    if request
        .message
        .file_ids
        .iter()
        .any(|id| id.is_empty() || id.len() > 256 || !file_ids.insert(id))
    {
        return Err(RunError::InvalidRequest);
    }
    Ok(())
}

fn validate_session_id(value: &str) -> Result<(), RunError> {
    if value.starts_with("session_")
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        Ok(())
    } else {
        Err(RunError::InvalidRequest)
    }
}

fn validate_profile_id(value: &str) -> Result<(), RunError> {
    let mut bytes = value.bytes();
    let Some(first) = bytes.next() else {
        return Err(RunError::InvalidRequest);
    };
    if value.len() <= 64
        && (first.is_ascii_lowercase() || first.is_ascii_digit() || first == b'_')
        && bytes.all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
        })
    {
        Ok(())
    } else {
        Err(RunError::InvalidRequest)
    }
}

fn validate_queue_item_id(value: &str) -> Result<(), RunError> {
    if value.strip_prefix("queue_").is_some_and(|suffix| {
        suffix.len() == 32
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    }) {
        Ok(())
    } else {
        Err(RunError::InvalidRequest)
    }
}

fn validate_runtime_owner_id(value: &str) -> Result<(), RunError> {
    if value.strip_prefix("runtime_").is_some_and(|suffix| {
        suffix.len() == 32
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    }) {
        Ok(())
    } else {
        Err(RunError::InvalidRequest)
    }
}

fn now_unix_millis() -> Result<i64, RunError> {
    i64::try_from(OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000)
        .map_err(|_| RunError::DataInvalid)
}

fn validate_run_id(value: &str) -> Result<(), RunError> {
    if value.starts_with("run_")
        && value.len() <= MAX_RUN_ID_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        Ok(())
    } else {
        Err(RunError::InvalidRunId)
    }
}

fn validate_workspace_id(value: &str) -> Result<(), RunError> {
    if !value.starts_with("workspace_")
        || value.len() > MAX_RUN_ID_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return Err(RunError::InvalidRequest);
    }
    Ok(())
}

fn validate_usage(usage: &Usage) -> Result<(), RunError> {
    if usage.total_tokens < usage.prompt_tokens.saturating_add(usage.completion_tokens)
        || usage
            .cost
            .is_some_and(|cost| !cost.is_finite() || cost < 0.0)
    {
        Err(RunError::DataInvalid)
    } else {
        Ok(())
    }
}

fn require_running(run: &Run) -> Result<(), RunError> {
    if run.status == RunStatus::Running {
        Ok(())
    } else if run.status == RunStatus::Cancelling {
        Err(RunError::EngineUnavailable)
    } else {
        Err(RunError::DataInvalid)
    }
}

fn run_from_session(error: SessionError) -> RunError {
    match error {
        SessionError::StorageBusy => RunError::StorageBusy,
        SessionError::StorageUnavailable => RunError::StorageUnavailable,
        SessionError::Archived => RunError::SessionArchived,
        SessionError::Busy => RunError::SessionBusy,
        SessionError::NotFound => RunError::NotFound,
        SessionError::IdempotencyConflict => RunError::IdempotencyConflict,
        SessionError::IdempotentResourceDeleted => RunError::IdempotentResourceDeleted,
        SessionError::DataInvalid => RunError::DataInvalid,
        _ => RunError::InvalidRequest,
    }
}

fn sql_json_error(error: impl std::error::Error + Send + Sync + 'static) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
}
