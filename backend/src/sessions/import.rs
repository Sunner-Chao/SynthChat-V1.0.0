use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fmt::Write as _,
};

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use uuid::Uuid;

use crate::compat::hermes_v21::{
    HERMES_AGENT_COMMIT, HERMES_V21_ADAPTER_ID, HERMES_V21_SCHEMA_VERSION, HermesV21Message,
    HermesV21ModelUsage, HermesV21Session, HermesV21Snapshot, ImportedToolCall, WarningCode,
};

use super::{
    MessagePart, MessageRole, SessionError, SessionService, ToolCall, ToolCallStatus, schema,
};

const IMPORT_PATH_SUFFIX: &str = "/session-imports/hermes-v21";
const MAX_CONFLICTS: usize = 64;
const MAX_TITLE_CHARS: usize = 500;
const MAX_MODEL_CHARS: usize = 500;
const MAX_MESSAGE_TEXT_CHARS: usize = 1_000_000;
const MAX_PREVIEW_CHARS: usize = 500;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HermesV21ImportRequest {
    pub expected_snapshot_fingerprint: String,
    pub allow_attachment_omission: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum HermesImportDisposition {
    Imported,
    Unchanged,
    Replayed,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HermesImportWarningSummary {
    pub code: String,
    pub count: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HermesV21ImportPreview {
    pub state: String,
    pub adapter_id: String,
    pub reference_commit: String,
    pub schema_version: Option<i64>,
    pub snapshot_fingerprint: Option<String>,
    pub session_count: Option<usize>,
    pub message_count: Option<usize>,
    pub model_usage_row_count: Option<usize>,
    pub attachment_count: Option<usize>,
    pub rewound_message_count: Option<usize>,
    pub warnings: Vec<HermesImportWarningSummary>,
    pub warnings_dropped: usize,
}

impl HermesV21ImportPreview {
    pub fn absent() -> Self {
        Self {
            state: "absent".to_owned(),
            adapter_id: HERMES_V21_ADAPTER_ID.to_owned(),
            reference_commit: HERMES_AGENT_COMMIT.to_owned(),
            schema_version: None,
            snapshot_fingerprint: None,
            session_count: None,
            message_count: None,
            model_usage_row_count: None,
            attachment_count: None,
            rewound_message_count: None,
            warnings: Vec::new(),
            warnings_dropped: 0,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HermesV21ImportResult {
    pub import_id: String,
    pub profile_id: String,
    pub disposition: HermesImportDisposition,
    pub adapter_id: String,
    pub reference_commit: String,
    pub source_schema_version: i64,
    pub snapshot_fingerprint: String,
    pub imported_session_count: usize,
    pub imported_message_count: usize,
    pub imported_model_usage_row_count: usize,
    pub omitted_attachment_count: usize,
    pub warnings: Vec<HermesImportWarningSummary>,
    pub warnings_dropped: usize,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum HermesImportConflictCode {
    SourceRemoved,
    SourceChanged,
    SourceExtended,
    TargetDeleted,
    TargetModified,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HermesImportConflict {
    pub code: HermesImportConflictCode,
    pub source_key_digest: String,
    pub target_session_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HermesImportConflictReport {
    pub conflict_count: usize,
    pub conflicts: Vec<HermesImportConflict>,
    pub conflicts_dropped: usize,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum HermesImportError {
    #[error("invalid Hermes import request")]
    InvalidRequest,
    #[error("Hermes import idempotency conflict")]
    IdempotencyConflict,
    #[error("Hermes import source changed after preview")]
    SourceChanged,
    #[error("Hermes import attachments require an explicit omission policy")]
    AttachmentsRequirePolicy,
    #[error("Hermes import source data is invalid")]
    SourceInvalid,
    #[error("Hermes import conflicts with existing target data")]
    Conflict(HermesImportConflictReport),
    #[error("session storage is busy")]
    StorageBusy,
    #[error("session storage is unavailable")]
    StorageUnavailable,
}

#[derive(Clone, Debug)]
struct SessionMapping {
    target_session_id: String,
}

#[derive(Clone, Debug)]
struct NewSession<'a> {
    source: &'a HermesV21Session,
    messages: Vec<&'a HermesV21Message>,
    model_usage: Vec<&'a HermesV21ModelUsage>,
    target_session_id: String,
}

impl SessionService {
    pub fn preview_from_snapshot(
        &self,
        profile_id: &str,
        snapshot: &HermesV21Snapshot,
    ) -> Result<HermesV21ImportPreview, HermesImportError> {
        validate_profile_id(profile_id)?;
        validate_snapshot_identity(snapshot)?;
        Ok(HermesV21ImportPreview {
            state: "ready".to_owned(),
            adapter_id: snapshot.provenance.adapter_id.clone(),
            reference_commit: snapshot.provenance.upstream_commit.clone(),
            schema_version: Some(snapshot.provenance.schema_version),
            snapshot_fingerprint: Some(snapshot.provenance.logical_fingerprint.clone()),
            session_count: Some(snapshot.sessions.len()),
            message_count: Some(snapshot.messages.len()),
            model_usage_row_count: Some(snapshot.model_usage.len()),
            attachment_count: Some(attachment_count(snapshot)),
            rewound_message_count: Some(snapshot.statistics.rewound_message_count),
            warnings: warning_summaries(snapshot, false),
            warnings_dropped: snapshot.warnings_dropped,
        })
    }

    pub fn lookup_hermes_v21_replay(
        &self,
        profile_id: &str,
        idempotency_key: &str,
        request: &HermesV21ImportRequest,
    ) -> Result<Option<HermesV21ImportResult>, HermesImportError> {
        validate_import_request(profile_id, idempotency_key, request)?;
        let ready = self.ready().map_err(map_session_error)?.clone();
        let _guard = ready
            .write_lock
            .lock()
            .map_err(|_| HermesImportError::StorageUnavailable)?;
        let connection = schema::open(&ready.db_path).map_err(map_session_error)?;
        lookup_replay(&connection, profile_id, idempotency_key, request)
    }

    pub fn import_hermes_v21_snapshot(
        &self,
        profile_id: &str,
        snapshot: &HermesV21Snapshot,
        request: &HermesV21ImportRequest,
        idempotency_key: &str,
    ) -> Result<HermesV21ImportResult, HermesImportError> {
        validate_import_request(profile_id, idempotency_key, request)?;
        validate_snapshot_identity(snapshot)?;
        if snapshot.provenance.logical_fingerprint != request.expected_snapshot_fingerprint {
            return Err(HermesImportError::SourceChanged);
        }
        if attachment_count(snapshot) > 0 && !request.allow_attachment_omission {
            return Err(HermesImportError::AttachmentsRequirePolicy);
        }

        let ready = self.ready().map_err(map_session_error)?.clone();
        let _guard = ready
            .write_lock
            .lock()
            .map_err(|_| HermesImportError::StorageUnavailable)?;
        let mut connection = schema::open(&ready.db_path).map_err(map_session_error)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_sqlite)?;

        if let Some(replay) = lookup_replay(&transaction, profile_id, idempotency_key, request)? {
            transaction.commit().map_err(map_sqlite)?;
            return Ok(replay);
        }

        let result = apply_import_tx(&transaction, profile_id, snapshot, request, idempotency_key)?;
        transaction.commit().map_err(map_sqlite)?;
        Ok(result)
    }
}

fn lookup_replay(
    connection: &Connection,
    profile_id: &str,
    idempotency_key: &str,
    request: &HermesV21ImportRequest,
) -> Result<Option<HermesV21ImportResult>, HermesImportError> {
    let canonical_path = canonical_path(profile_id);
    let request_fingerprint = request_fingerprint(profile_id, request);
    let stored = connection
        .query_row(
            "SELECT request_fingerprint, response_json FROM idempotency_records \
             WHERE method = 'POST' AND canonical_path = ?1 AND idempotency_key = ?2",
            params![canonical_path, idempotency_key],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
        )
        .optional()
        .map_err(map_sqlite)?;
    let Some((stored_fingerprint, response_json)) = stored else {
        return Ok(None);
    };
    if stored_fingerprint != request_fingerprint {
        return Err(HermesImportError::IdempotencyConflict);
    }
    let mut result = response_json
        .as_deref()
        .ok_or(HermesImportError::StorageUnavailable)
        .and_then(|json| {
            serde_json::from_str::<HermesV21ImportResult>(json)
                .map_err(|_| HermesImportError::StorageUnavailable)
        })?;
    result.disposition = HermesImportDisposition::Replayed;
    Ok(Some(result))
}

fn validate_import_request(
    profile_id: &str,
    idempotency_key: &str,
    request: &HermesV21ImportRequest,
) -> Result<(), HermesImportError> {
    validate_profile_id(profile_id)?;
    if idempotency_key.len() < 8
        || idempotency_key.len() > 128
        || !idempotency_key
            .bytes()
            .all(|byte| (0x21..=0x7e).contains(&byte))
        || !is_digest(&request.expected_snapshot_fingerprint)
    {
        return Err(HermesImportError::InvalidRequest);
    }
    Ok(())
}

fn validate_profile_id(value: &str) -> Result<(), HermesImportError> {
    let bytes = value.as_bytes();
    if value == "default"
        || (!bytes.is_empty()
            && bytes.len() <= 64
            && matches!(bytes[0], b'a'..=b'z' | b'0'..=b'9' | b'_')
            && bytes
                .iter()
                .all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-')))
    {
        Ok(())
    } else {
        Err(HermesImportError::InvalidRequest)
    }
}

fn validate_snapshot_identity(snapshot: &HermesV21Snapshot) -> Result<(), HermesImportError> {
    if snapshot.provenance.adapter_id != HERMES_V21_ADAPTER_ID
        || snapshot.provenance.upstream_commit != HERMES_AGENT_COMMIT
        || snapshot.provenance.schema_version != HERMES_V21_SCHEMA_VERSION
        || !is_digest(&snapshot.provenance.logical_fingerprint)
    {
        return Err(HermesImportError::SourceInvalid);
    }
    Ok(())
}

fn is_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn attachment_count(snapshot: &HermesV21Snapshot) -> usize {
    snapshot
        .messages
        .iter()
        .map(|message| message.content.pending_attachments.len())
        .sum()
}

fn warning_summaries(
    snapshot: &HermesV21Snapshot,
    include_attachment_omission: bool,
) -> Vec<HermesImportWarningSummary> {
    let mut counts = BTreeMap::<String, usize>::new();
    for warning in &snapshot.warnings {
        *counts
            .entry(warning_code(warning.code).to_owned())
            .or_default() += 1;
    }
    if include_attachment_omission {
        let count = attachment_count(snapshot);
        if count > 0 {
            counts.insert("attachment_omitted".to_owned(), count);
        }
    }
    counts
        .into_iter()
        .map(|(code, count)| HermesImportWarningSummary { code, count })
        .collect()
}

fn warning_code(code: WarningCode) -> &'static str {
    match code {
        WarningCode::ActiveNullTreatedAsActive => "active_null_treated_as_active",
        WarningCode::StructuredContentTooLarge => "structured_content_too_large",
        WarningCode::StructuredContentInvalidJson => "structured_content_invalid_json",
        WarningCode::StructuredContentUnsupportedShape => "structured_content_unsupported_shape",
        WarningCode::StructuredContentPartIgnored => "structured_content_part_ignored",
        WarningCode::AttachmentReferenceMissing => "attachment_reference_missing",
        WarningCode::ReasoningDetailsTooLarge => "reasoning_details_too_large",
        WarningCode::ReasoningDetailsInvalidJson => "reasoning_details_invalid_json",
        WarningCode::ReasoningIgnoredForRole => "reasoning_ignored_for_role",
        WarningCode::ToolCallsTooLarge => "tool_calls_too_large",
        WarningCode::ToolCallsInvalidJson => "tool_calls_invalid_json",
        WarningCode::ToolCallsNotArray => "tool_calls_not_array",
        WarningCode::ToolCallEntryIgnored => "tool_call_entry_ignored",
        WarningCode::ToolCallArgumentsInvalidJson => "tool_call_arguments_invalid_json",
    }
}

fn canonical_path(profile_id: &str) -> String {
    format!("/api/v1/profiles/{profile_id}{IMPORT_PATH_SUFFIX}")
}

fn request_fingerprint(profile_id: &str, request: &HermesV21ImportRequest) -> String {
    let mut digest = Sha256::new();
    digest.update(b"synthchat-hermes-v21-import-request-v1\0");
    digest.update(profile_id.as_bytes());
    digest.update([0]);
    digest.update(request.expected_snapshot_fingerprint.as_bytes());
    digest.update([u8::from(request.allow_attachment_omission)]);
    hex(&digest.finalize())
}

fn map_session_error(error: SessionError) -> HermesImportError {
    match error {
        SessionError::StorageBusy => HermesImportError::StorageBusy,
        _ => HermesImportError::StorageUnavailable,
    }
}

fn map_sqlite(error: rusqlite::Error) -> HermesImportError {
    match &error {
        rusqlite::Error::SqliteFailure(failure, _)
            if matches!(
                failure.code,
                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
            ) =>
        {
            HermesImportError::StorageBusy
        }
        _ => HermesImportError::StorageUnavailable,
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(output, "{byte:02x}");
    }
    output
}

fn apply_import_tx(
    transaction: &Transaction<'_>,
    profile_id: &str,
    snapshot: &HermesV21Snapshot,
    request: &HermesV21ImportRequest,
    idempotency_key: &str,
) -> Result<HermesV21ImportResult, HermesImportError> {
    validate_source_rows(snapshot)?;

    let adapter_id = snapshot.provenance.adapter_id.as_str();
    let session_by_upstream = snapshot
        .sessions
        .iter()
        .map(|session| (session.upstream_id.as_str(), session))
        .collect::<HashMap<_, _>>();
    if session_by_upstream.len() != snapshot.sessions.len() {
        return Err(HermesImportError::SourceInvalid);
    }
    let session_key_by_upstream = snapshot
        .sessions
        .iter()
        .map(|session| {
            (
                session.upstream_id.as_str(),
                session.provenance.source_key_digest.as_str(),
            )
        })
        .collect::<HashMap<_, _>>();

    let mut conflicts = ConflictSink::default();
    let mut mapped_sessions = HashMap::<String, SessionMapping>::new();
    let source_session_keys = snapshot
        .sessions
        .iter()
        .map(|session| session.provenance.source_key_digest.clone())
        .collect::<HashSet<_>>();
    if source_session_keys.len() != snapshot.sessions.len() {
        return Err(HermesImportError::SourceInvalid);
    }

    for session in &snapshot.sessions {
        let source_key = &session.provenance.source_key_digest;
        let mapping = transaction
            .query_row(
                "SELECT source_row_digest, target_session_id, target_revision \
                 FROM hermes_import_session_map \
                 WHERE profile_id = ?1 AND adapter_id = ?2 AND source_key_digest = ?3",
                params![profile_id, adapter_id, source_key],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(map_sqlite)?;
        if let Some((source_row_digest, target_session_id, target_revision)) = mapping {
            if source_row_digest != session.provenance.source_row_digest {
                conflicts.push(
                    HermesImportConflictCode::SourceChanged,
                    source_key,
                    target_session_id,
                );
                continue;
            }
            let Some(target_session_id) = target_session_id else {
                conflicts.push(HermesImportConflictCode::TargetDeleted, source_key, None);
                continue;
            };
            let current_revision = transaction
                .query_row(
                    "SELECT revision FROM sessions WHERE id = ?1",
                    [&target_session_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(map_sqlite)?;
            match current_revision {
                None => conflicts.push(
                    HermesImportConflictCode::TargetDeleted,
                    source_key,
                    Some(target_session_id),
                ),
                Some(current_revision) if current_revision != target_revision => conflicts.push(
                    HermesImportConflictCode::TargetModified,
                    source_key,
                    Some(target_session_id),
                ),
                Some(_) => {
                    mapped_sessions
                        .insert(source_key.clone(), SessionMapping { target_session_id });
                }
            }
        } else {
            let target_id = deterministic_id("session", profile_id, adapter_id, source_key);
            if transaction
                .query_row(
                    "SELECT 1 FROM sessions WHERE id = ?1",
                    [&target_id],
                    |row| row.get::<_, i64>(0),
                )
                .optional()
                .map_err(map_sqlite)?
                .is_some()
            {
                conflicts.push(
                    HermesImportConflictCode::TargetModified,
                    source_key,
                    Some(target_id),
                );
            }
        }
    }
    collect_removed_mappings(
        transaction,
        "hermes_import_session_map",
        profile_id,
        adapter_id,
        &source_session_keys,
        &mut conflicts,
    )?;

    let source_message_keys = snapshot
        .messages
        .iter()
        .map(|message| message.provenance.source_key_digest.clone())
        .collect::<HashSet<_>>();
    if source_message_keys.len() != snapshot.messages.len() {
        return Err(HermesImportError::SourceInvalid);
    }
    for message in &snapshot.messages {
        let source_session_key = session_key_by_upstream
            .get(message.session_upstream_id.as_str())
            .ok_or(HermesImportError::SourceInvalid)?;
        let source_key = &message.provenance.source_key_digest;
        let mapping = transaction
            .query_row(
                "SELECT source_row_digest, target_message_id, target_session_id \
                 FROM hermes_import_message_map \
                 WHERE profile_id = ?1 AND adapter_id = ?2 AND source_key_digest = ?3",
                params![profile_id, adapter_id, source_key],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(map_sqlite)?;
        match mapping {
            Some((source_row_digest, target_message_id, target_session_id)) => {
                if source_row_digest != message.provenance.source_row_digest {
                    conflicts.push(
                        HermesImportConflictCode::SourceChanged,
                        source_key,
                        target_session_id,
                    );
                    continue;
                }
                let Some(target_message_id) = target_message_id else {
                    conflicts.push(
                        HermesImportConflictCode::TargetDeleted,
                        source_key,
                        target_session_id,
                    );
                    continue;
                };
                if transaction
                    .query_row(
                        "SELECT 1 FROM messages WHERE id = ?1",
                        [&target_message_id],
                        |row| row.get::<_, i64>(0),
                    )
                    .optional()
                    .map_err(map_sqlite)?
                    .is_none()
                {
                    conflicts.push(
                        HermesImportConflictCode::TargetDeleted,
                        source_key,
                        target_session_id,
                    );
                }
            }
            None if mapped_sessions.contains_key(*source_session_key) => conflicts.push(
                HermesImportConflictCode::SourceExtended,
                source_key,
                mapped_sessions
                    .get(*source_session_key)
                    .map(|mapping| mapping.target_session_id.clone()),
            ),
            None => {
                let target_id = deterministic_id("message", profile_id, adapter_id, source_key);
                if transaction
                    .query_row(
                        "SELECT 1 FROM messages WHERE id = ?1",
                        [&target_id],
                        |row| row.get::<_, i64>(0),
                    )
                    .optional()
                    .map_err(map_sqlite)?
                    .is_some()
                {
                    conflicts.push(HermesImportConflictCode::TargetModified, source_key, None);
                }
            }
        }
    }
    collect_removed_mappings(
        transaction,
        "hermes_import_message_map",
        profile_id,
        adapter_id,
        &source_message_keys,
        &mut conflicts,
    )?;

    let source_usage_keys = snapshot
        .model_usage
        .iter()
        .map(|usage| usage.provenance.source_key_digest.clone())
        .collect::<HashSet<_>>();
    if source_usage_keys.len() != snapshot.model_usage.len() {
        return Err(HermesImportError::SourceInvalid);
    }
    for usage in &snapshot.model_usage {
        let source_session_key = session_key_by_upstream
            .get(usage.session_upstream_id.as_str())
            .ok_or(HermesImportError::SourceInvalid)?;
        let source_key = &usage.provenance.source_key_digest;
        let mapping = transaction
            .query_row(
                "SELECT source_row_digest, target_session_id \
                 FROM hermes_import_model_usage \
                 WHERE profile_id = ?1 AND adapter_id = ?2 AND source_key_digest = ?3",
                params![profile_id, adapter_id, source_key],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
            )
            .optional()
            .map_err(map_sqlite)?;
        match mapping {
            Some((source_row_digest, target_session_id)) => {
                if source_row_digest != usage.provenance.source_row_digest {
                    conflicts.push(
                        HermesImportConflictCode::SourceChanged,
                        source_key,
                        target_session_id,
                    );
                } else if target_session_id.is_none() {
                    conflicts.push(HermesImportConflictCode::TargetDeleted, source_key, None);
                }
            }
            None if mapped_sessions.contains_key(*source_session_key) => conflicts.push(
                HermesImportConflictCode::SourceExtended,
                source_key,
                mapped_sessions
                    .get(*source_session_key)
                    .map(|mapping| mapping.target_session_id.clone()),
            ),
            None => {}
        }
    }
    collect_removed_mappings(
        transaction,
        "hermes_import_model_usage",
        profile_id,
        adapter_id,
        &source_usage_keys,
        &mut conflicts,
    )?;

    if let Some(report) = conflicts.finish() {
        return Err(HermesImportError::Conflict(report));
    }

    let existing_batch_id = transaction
        .query_row(
            "SELECT id FROM hermes_import_batches \
             WHERE profile_id = ?1 AND adapter_id = ?2 AND snapshot_fingerprint = ?3",
            params![
                profile_id,
                adapter_id,
                snapshot.provenance.logical_fingerprint
            ],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(map_sqlite)?;
    if let Some(import_id) = existing_batch_id {
        let result = HermesV21ImportResult {
            import_id: import_id.clone(),
            profile_id: profile_id.to_owned(),
            disposition: HermesImportDisposition::Unchanged,
            adapter_id: adapter_id.to_owned(),
            reference_commit: snapshot.provenance.upstream_commit.clone(),
            source_schema_version: snapshot.provenance.schema_version,
            snapshot_fingerprint: snapshot.provenance.logical_fingerprint.clone(),
            imported_session_count: 0,
            imported_message_count: 0,
            imported_model_usage_row_count: 0,
            omitted_attachment_count: attachment_count(snapshot),
            warnings: warning_summaries(snapshot, request.allow_attachment_omission),
            warnings_dropped: snapshot.warnings_dropped,
        };
        insert_idempotency(
            transaction,
            profile_id,
            idempotency_key,
            request,
            &import_id,
            &result,
        )?;
        return Ok(result);
    }

    let mut new_sessions = Vec::new();
    for session in &snapshot.sessions {
        if mapped_sessions.contains_key(&session.provenance.source_key_digest) {
            continue;
        }
        let mut messages = snapshot
            .messages
            .iter()
            .filter(|message| message.session_upstream_id == session.upstream_id)
            .collect::<Vec<_>>();
        messages.sort_by(|left, right| {
            left.timestamp
                .total_cmp(&right.timestamp)
                .then_with(|| left.upstream_id.cmp(&right.upstream_id))
        });
        let model_usage = snapshot
            .model_usage
            .iter()
            .filter(|usage| usage.session_upstream_id == session.upstream_id)
            .collect::<Vec<_>>();
        new_sessions.push(NewSession {
            source: session,
            messages,
            model_usage,
            target_session_id: deterministic_id(
                "session",
                profile_id,
                adapter_id,
                &session.provenance.source_key_digest,
            ),
        });
    }

    let imported_message_count = new_sessions
        .iter()
        .map(|session| session.messages.len())
        .sum::<usize>();
    let imported_model_usage_row_count = new_sessions
        .iter()
        .map(|session| session.model_usage.len())
        .sum::<usize>();
    let omitted_attachment_count = new_sessions
        .iter()
        .flat_map(|session| session.messages.iter())
        .map(|message| message.content.pending_attachments.len())
        .sum::<usize>();
    let import_id = format!("import_hv21_{}", Uuid::new_v4().simple());
    let created_at = now_timestamp()?;
    let warnings = warning_summaries(snapshot, request.allow_attachment_omission);
    let result = HermesV21ImportResult {
        import_id: import_id.clone(),
        profile_id: profile_id.to_owned(),
        disposition: HermesImportDisposition::Imported,
        adapter_id: adapter_id.to_owned(),
        reference_commit: snapshot.provenance.upstream_commit.clone(),
        source_schema_version: snapshot.provenance.schema_version,
        snapshot_fingerprint: snapshot.provenance.logical_fingerprint.clone(),
        imported_session_count: new_sessions.len(),
        imported_message_count,
        imported_model_usage_row_count,
        omitted_attachment_count,
        warnings: warnings.clone(),
        warnings_dropped: snapshot.warnings_dropped,
    };
    let result_json =
        serde_json::to_string(&result).map_err(|_| HermesImportError::StorageUnavailable)?;
    transaction
        .execute(
            "INSERT INTO hermes_import_batches(\
               id, profile_id, adapter_id, snapshot_fingerprint, reference_commit,\
               source_schema_version, source_session_count, source_message_count,\
               source_model_usage_count, imported_session_count, imported_message_count,\
               imported_model_usage_count, omitted_attachment_count, warnings_dropped,\
               result_json, created_at\
             ) VALUES(\
               ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16\
             )",
            params![
                import_id,
                profile_id,
                adapter_id,
                snapshot.provenance.logical_fingerprint,
                snapshot.provenance.upstream_commit,
                snapshot.provenance.schema_version,
                to_i64(snapshot.sessions.len())?,
                to_i64(snapshot.messages.len())?,
                to_i64(snapshot.model_usage.len())?,
                to_i64(new_sessions.len())?,
                to_i64(imported_message_count)?,
                to_i64(imported_model_usage_row_count)?,
                to_i64(omitted_attachment_count)?,
                to_i64(snapshot.warnings_dropped)?,
                result_json,
                created_at,
            ],
        )
        .map_err(map_sqlite)?;
    for warning in &warnings {
        transaction
            .execute(
                "INSERT INTO hermes_import_batch_warnings(batch_id, code, warning_count) \
                 VALUES(?1, ?2, ?3)",
                params![import_id, warning.code, to_i64(warning.count)?],
            )
            .map_err(map_sqlite)?;
    }

    let change = next_change(transaction)?;
    for session in &new_sessions {
        insert_session(
            transaction,
            profile_id,
            adapter_id,
            &import_id,
            change,
            session,
            &session_key_by_upstream,
        )?;
    }
    insert_idempotency(
        transaction,
        profile_id,
        idempotency_key,
        request,
        &import_id,
        &result,
    )?;
    Ok(result)
}

#[derive(Default)]
struct ConflictSink {
    count: usize,
    conflicts: Vec<HermesImportConflict>,
}

impl ConflictSink {
    fn push(
        &mut self,
        code: HermesImportConflictCode,
        source_key_digest: &str,
        target_session_id: Option<String>,
    ) {
        self.count = self.count.saturating_add(1);
        if self.conflicts.len() < MAX_CONFLICTS {
            self.conflicts.push(HermesImportConflict {
                code,
                source_key_digest: source_key_digest.to_owned(),
                target_session_id,
            });
        }
    }

    fn finish(self) -> Option<HermesImportConflictReport> {
        (self.count > 0).then(|| HermesImportConflictReport {
            conflict_count: self.count,
            conflicts_dropped: self.count.saturating_sub(self.conflicts.len()),
            conflicts: self.conflicts,
        })
    }
}

fn collect_removed_mappings(
    transaction: &Transaction<'_>,
    table: &str,
    profile_id: &str,
    adapter_id: &str,
    source_keys: &HashSet<String>,
    conflicts: &mut ConflictSink,
) -> Result<(), HermesImportError> {
    let sql = format!(
        "SELECT source_key_digest, target_session_id FROM {table} \
         WHERE profile_id = ?1 AND adapter_id = ?2"
    );
    let mut statement = transaction.prepare(&sql).map_err(map_sqlite)?;
    let rows = statement
        .query_map(params![profile_id, adapter_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })
        .map_err(map_sqlite)?;
    for row in rows {
        let (source_key, target_session_id) = row.map_err(map_sqlite)?;
        if !source_keys.contains(&source_key) {
            conflicts.push(
                HermesImportConflictCode::SourceRemoved,
                &source_key,
                target_session_id,
            );
        }
    }
    Ok(())
}

fn validate_source_rows(snapshot: &HermesV21Snapshot) -> Result<(), HermesImportError> {
    for session in &snapshot.sessions {
        validate_record_digests(
            &session.provenance.source_key_digest,
            &session.provenance.source_row_digest,
        )?;
        if session.upstream_id.is_empty()
            || session.upstream_id.len() > 4096
            || !bounded_optional(session.title.as_deref(), MAX_MESSAGE_TEXT_CHARS)
            || !bounded_optional(session.model.as_deref(), 4096)
            || session.source.len() > 4096
            || session.aggregate_usage.message_count < 0
            || session.aggregate_usage.tool_call_count < 0
            || session.aggregate_usage.api_call_count < 0
            || session.aggregate_usage.input_tokens < 0
            || session.aggregate_usage.output_tokens < 0
            || session.aggregate_usage.cache_read_tokens < 0
            || session.aggregate_usage.cache_write_tokens < 0
            || session.aggregate_usage.reasoning_tokens < 0
            || session
                .aggregate_usage
                .estimated_cost_usd
                .is_some_and(|cost| cost < 0.0)
            || session
                .aggregate_usage
                .actual_cost_usd
                .is_some_and(|cost| cost < 0.0)
        {
            return Err(HermesImportError::SourceInvalid);
        }
    }
    for message in &snapshot.messages {
        validate_record_digests(
            &message.provenance.source_key_digest,
            &message.provenance.source_row_digest,
        )?;
        if message.session_upstream_id.is_empty()
            || message.session_upstream_id.len() > 4096
            || !matches!(
                message.role.as_str(),
                "user" | "assistant" | "system" | "tool"
            )
            || !bounded_optional(message.content.text.as_deref(), MAX_MESSAGE_TEXT_CHARS)
            || !bounded_optional(message.reasoning.as_deref(), MAX_MESSAGE_TEXT_CHARS)
            || !bounded_optional(message.tool_call_id.as_deref(), 4096)
            || !bounded_optional(message.tool_name.as_deref(), 4096)
            || !bounded_optional(message.finish_reason.as_deref(), 4096)
            || message.token_count.is_some_and(|count| count < 0)
            || message.tool_calls.len() > 10_000
            || message.tool_calls.iter().any(|tool| {
                tool.name.is_empty()
                    || tool.name.chars().count() > 4096
                    || !bounded_optional(tool.call_id.as_deref(), 4096)
            })
        {
            return Err(HermesImportError::SourceInvalid);
        }
    }
    for usage in &snapshot.model_usage {
        validate_record_digests(
            &usage.provenance.source_key_digest,
            &usage.provenance.source_row_digest,
        )?;
        if usage.session_upstream_id.is_empty()
            || usage.session_upstream_id.len() > 4096
            || usage.model.is_empty()
            || usage.model.chars().count() > 4096
            || usage.billing_provider.chars().count() > 4096
            || usage.billing_mode.chars().count() > 4096
            || !bounded_optional(usage.cost_status.as_deref(), 4096)
            || !bounded_optional(usage.cost_source.as_deref(), 4096)
            || usage.api_call_count < 0
            || usage.input_tokens < 0
            || usage.output_tokens < 0
            || usage.cache_read_tokens < 0
            || usage.cache_write_tokens < 0
            || usage.reasoning_tokens < 0
            || usage.estimated_cost_usd < 0.0
            || usage.actual_cost_usd < 0.0
        {
            return Err(HermesImportError::SourceInvalid);
        }
    }
    Ok(())
}

fn validate_record_digests(key: &str, row: &str) -> Result<(), HermesImportError> {
    if is_digest(key) && is_digest(row) {
        Ok(())
    } else {
        Err(HermesImportError::SourceInvalid)
    }
}

fn bounded_optional(value: Option<&str>, maximum: usize) -> bool {
    value.is_none_or(|value| value.chars().count() <= maximum)
}

fn insert_session(
    transaction: &Transaction<'_>,
    profile_id: &str,
    adapter_id: &str,
    import_id: &str,
    change: i64,
    planned: &NewSession<'_>,
    session_key_by_upstream: &HashMap<&str, &str>,
) -> Result<(), HermesImportError> {
    let source = planned.source;
    let title = clean_text(source.title.as_deref().unwrap_or(""), MAX_TITLE_CHARS);
    let title = if title.is_empty() {
        "Imported Hermes conversation".to_owned()
    } else {
        title
    };
    let model = clean_text(source.model.as_deref().unwrap_or(""), MAX_MODEL_CHARS);
    let created_at = source_timestamp(source.started_at)?;
    let updated_timestamp = planned
        .messages
        .iter()
        .map(|message| message.timestamp)
        .chain(source.ended_at)
        .fold(source.started_at, f64::max);
    let updated_at = source_timestamp(updated_timestamp)?;
    let preview = planned
        .messages
        .iter()
        .rev()
        .filter_map(|message| message.content.text.as_deref())
        .find(|text| !text.is_empty())
        .map(|text| clean_text(text, MAX_PREVIEW_CHARS))
        .unwrap_or_default();
    let revision = new_revision();
    let next_sequence = planned
        .messages
        .len()
        .checked_add(1)
        .ok_or(HermesImportError::SourceInvalid)?;
    transaction
        .execute(
            "INSERT INTO sessions(\
               id, profile_id, title, preview, source, model, message_count, archived,\
               revision, created_at, updated_at, next_message_sequence, current_change\
             ) VALUES(?1, ?2, ?3, ?4, 'hermes-agent:v21', ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                planned.target_session_id,
                profile_id,
                title,
                preview,
                model,
                to_i64(planned.messages.len())?,
                bool_to_i64(source.archived),
                revision,
                created_at,
                updated_at,
                to_i64(next_sequence)?,
                change,
            ],
        )
        .map_err(map_sqlite)?;
    transaction
        .execute(
            "INSERT INTO session_versions(\
               session_id, valid_from_change, valid_to_change, profile_id, title, preview,\
               source, model, message_count, archived, revision, created_at, updated_at\
             ) VALUES(?1, ?2, NULL, ?3, ?4, ?5, 'hermes-agent:v21', ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                planned.target_session_id,
                change,
                profile_id,
                title,
                preview,
                model,
                to_i64(planned.messages.len())?,
                bool_to_i64(source.archived),
                revision,
                created_at,
                updated_at,
            ],
        )
        .map_err(map_sqlite)?;
    let parent_source_key_digest = source.parent_upstream_id.as_deref().map(|parent| {
        session_key_by_upstream
            .get(parent)
            .copied()
            .map(str::to_owned)
            .unwrap_or_else(|| digest_bytes(parent.as_bytes()))
    });
    transaction
        .execute(
            "INSERT INTO hermes_import_session_map(\
               profile_id, adapter_id, source_key_digest, source_row_digest,\
               parent_source_key_digest, target_session_id, target_revision, batch_id\
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                profile_id,
                adapter_id,
                source.provenance.source_key_digest,
                source.provenance.source_row_digest,
                parent_source_key_digest,
                planned.target_session_id,
                revision,
                import_id,
            ],
        )
        .map_err(map_sqlite)?;

    let tool_result_ids = planned
        .messages
        .iter()
        .filter(|message| message.role == "tool")
        .filter_map(|message| message.tool_call_id.as_deref())
        .collect::<HashSet<_>>();
    for (index, message) in planned.messages.iter().enumerate() {
        insert_message(
            transaction,
            profile_id,
            adapter_id,
            import_id,
            change,
            planned,
            message,
            index + 1,
            &tool_result_ids,
        )?;
    }
    for usage in &planned.model_usage {
        insert_model_usage(
            transaction,
            profile_id,
            adapter_id,
            import_id,
            &planned.target_session_id,
            usage,
        )?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn insert_message(
    transaction: &Transaction<'_>,
    profile_id: &str,
    adapter_id: &str,
    import_id: &str,
    change: i64,
    planned: &NewSession<'_>,
    message: &HermesV21Message,
    sequence: usize,
    tool_result_ids: &HashSet<&str>,
) -> Result<(), HermesImportError> {
    let role = message_role(&message.role)?;
    let mut parts = Vec::new();
    if let Some(text) = &message.content.text {
        parts.push(MessagePart::Text { text: text.clone() });
    } else if !message.content.pending_attachments.is_empty() {
        parts.push(MessagePart::Text {
            text: String::new(),
        });
    }
    let tool_calls = mapped_tool_calls(
        profile_id,
        adapter_id,
        &planned.target_session_id,
        message,
        tool_result_ids,
    );
    let parts_json = serde_json::to_string(&parts).map_err(|_| HermesImportError::SourceInvalid)?;
    let tool_calls_json =
        serde_json::to_string(&tool_calls).map_err(|_| HermesImportError::SourceInvalid)?;
    let searchable_text = parts
        .iter()
        .filter_map(|part| match part {
            MessagePart::Text { text } => Some(text.as_str()),
            MessagePart::File { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    let target_message_id = deterministic_id(
        "message",
        profile_id,
        adapter_id,
        &message.provenance.source_key_digest,
    );
    transaction
        .execute(
            "INSERT INTO messages(\
               id, session_id, sequence, role, parts_json, reasoning, tool_calls_json,\
               searchable_text, created_at, committed_change, context_eligible\
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                target_message_id,
                planned.target_session_id,
                to_i64(sequence)?,
                role.as_str(),
                parts_json,
                message.reasoning,
                tool_calls_json,
                searchable_text,
                source_timestamp(message.timestamp)?,
                change,
                bool_to_i64(message.active),
            ],
        )
        .map_err(map_sqlite)?;
    transaction
        .execute(
            "INSERT INTO hermes_import_message_map(\
               profile_id, adapter_id, source_key_digest, source_row_digest,\
               source_session_key_digest, target_message_id, target_session_id, active,\
               compacted, token_count, finish_reason, batch_id\
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                profile_id,
                adapter_id,
                message.provenance.source_key_digest,
                message.provenance.source_row_digest,
                planned.source.provenance.source_key_digest,
                target_message_id,
                planned.target_session_id,
                bool_to_i64(message.active),
                bool_to_i64(message.compacted),
                message.token_count,
                message
                    .finish_reason
                    .as_deref()
                    .map(|value| clean_text(value, 500)),
                import_id,
            ],
        )
        .map_err(map_sqlite)?;
    Ok(())
}

fn insert_model_usage(
    transaction: &Transaction<'_>,
    profile_id: &str,
    adapter_id: &str,
    import_id: &str,
    target_session_id: &str,
    usage: &HermesV21ModelUsage,
) -> Result<(), HermesImportError> {
    transaction
        .execute(
            "INSERT INTO hermes_import_model_usage(\
               profile_id, adapter_id, source_key_digest, source_row_digest,\
               target_session_id, route_fingerprint, model, billing_provider, billing_mode,\
               billing_base_url_present, api_call_count, input_tokens, output_tokens,\
               cache_read_tokens, cache_write_tokens, reasoning_tokens, estimated_cost_usd,\
               actual_cost_usd, cost_status, cost_source, first_seen, last_seen, batch_id\
             ) VALUES(\
               ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,\
               ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23\
             )",
            params![
                profile_id,
                adapter_id,
                usage.provenance.source_key_digest,
                usage.provenance.source_row_digest,
                target_session_id,
                usage.route_fingerprint,
                clean_text(&usage.model, 4096),
                clean_text(&usage.billing_provider, 4096),
                clean_text(&usage.billing_mode, 4096),
                bool_to_i64(usage.billing_base_url_present),
                usage.api_call_count,
                usage.input_tokens,
                usage.output_tokens,
                usage.cache_read_tokens,
                usage.cache_write_tokens,
                usage.reasoning_tokens,
                usage.estimated_cost_usd,
                usage.actual_cost_usd,
                usage
                    .cost_status
                    .as_deref()
                    .map(|value| clean_text(value, 4096)),
                usage
                    .cost_source
                    .as_deref()
                    .map(|value| clean_text(value, 4096)),
                usage.first_seen,
                usage.last_seen,
                import_id,
            ],
        )
        .map_err(map_sqlite)?;
    Ok(())
}

fn mapped_tool_calls(
    profile_id: &str,
    adapter_id: &str,
    target_session_id: &str,
    message: &HermesV21Message,
    tool_result_ids: &HashSet<&str>,
) -> Vec<ToolCall> {
    let mut calls = message
        .tool_calls
        .iter()
        .enumerate()
        .map(|(index, call)| {
            mapped_tool_call(
                profile_id,
                adapter_id,
                target_session_id,
                &message.provenance.source_key_digest,
                index,
                call,
                tool_result_ids,
            )
        })
        .collect::<Vec<_>>();
    if message.role == "tool"
        && let Some(raw_call_id) = message.tool_call_id.as_deref()
    {
        let call_id = deterministic_call_id(profile_id, adapter_id, target_session_id, raw_call_id);
        if !calls.iter().any(|call| call.call_id == call_id) {
            calls.push(ToolCall {
                call_id,
                name: clean_tool_name(message.tool_name.as_deref().unwrap_or("unknown-tool")),
                status: ToolCallStatus::Completed,
                input_summary: None,
                result_summary: message
                    .content
                    .text
                    .as_deref()
                    .filter(|text| !text.is_empty())
                    .map(|text| clean_text(text, MAX_PREVIEW_CHARS)),
                artifacts: Vec::new(),
            });
        }
    }
    calls
}

#[allow(clippy::too_many_arguments)]
fn mapped_tool_call(
    profile_id: &str,
    adapter_id: &str,
    target_session_id: &str,
    message_source_key: &str,
    index: usize,
    call: &ImportedToolCall,
    tool_result_ids: &HashSet<&str>,
) -> ToolCall {
    let call_key = call
        .call_id
        .as_deref()
        .map(str::to_owned)
        .unwrap_or_else(|| format!("{message_source_key}:{index}"));
    let status = if call
        .call_id
        .as_deref()
        .is_some_and(|call_id| tool_result_ids.contains(call_id))
    {
        ToolCallStatus::Completed
    } else {
        ToolCallStatus::Unknown
    };
    ToolCall {
        call_id: deterministic_call_id(profile_id, adapter_id, target_session_id, &call_key),
        name: clean_tool_name(&call.name),
        status,
        input_summary: None,
        result_summary: None,
        artifacts: Vec::new(),
    }
}

fn deterministic_call_id(
    profile_id: &str,
    adapter_id: &str,
    target_session_id: &str,
    source_call_id: &str,
) -> String {
    deterministic_id(
        "call",
        profile_id,
        adapter_id,
        &digest_bytes(format!("{target_session_id}\0{source_call_id}").as_bytes()),
    )
}

fn message_role(role: &str) -> Result<MessageRole, HermesImportError> {
    match role {
        "user" => Ok(MessageRole::User),
        "assistant" => Ok(MessageRole::Assistant),
        "system" => Ok(MessageRole::System),
        "tool" => Ok(MessageRole::Tool),
        _ => Err(HermesImportError::SourceInvalid),
    }
}

fn clean_tool_name(value: &str) -> String {
    let value = clean_text(value, 256);
    if value.is_empty() {
        "unknown-tool".to_owned()
    } else {
        value
    }
}

fn clean_text(value: &str, maximum: usize) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_control() && !matches!(character, '\n' | '\t') {
                ' '
            } else {
                character
            }
        })
        .take(maximum)
        .collect::<String>()
        .trim()
        .to_owned()
}

fn insert_idempotency(
    transaction: &Transaction<'_>,
    profile_id: &str,
    idempotency_key: &str,
    request: &HermesV21ImportRequest,
    import_id: &str,
    result: &HermesV21ImportResult,
) -> Result<(), HermesImportError> {
    let response_json =
        serde_json::to_string(result).map_err(|_| HermesImportError::StorageUnavailable)?;
    transaction
        .execute(
            "INSERT INTO idempotency_records(\
               method, canonical_path, idempotency_key, request_fingerprint, resource_id,\
               response_json, created_at\
             ) VALUES('POST', ?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                canonical_path(profile_id),
                idempotency_key,
                request_fingerprint(profile_id, request),
                import_id,
                response_json,
                now_timestamp()?,
            ],
        )
        .map_err(map_sqlite)?;
    Ok(())
}

fn deterministic_id(prefix: &str, profile_id: &str, adapter_id: &str, source_key: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"synthchat-hermes-v21-target-id-v1\0");
    for value in [prefix, profile_id, adapter_id, source_key] {
        digest.update((value.len() as u64).to_be_bytes());
        digest.update(value.as_bytes());
    }
    let encoded = hex(&digest.finalize());
    format!("{prefix}_hv21_{}", &encoded[..32])
}

fn digest_bytes(value: &[u8]) -> String {
    hex(&Sha256::digest(value))
}

fn next_change(transaction: &Transaction<'_>) -> Result<i64, HermesImportError> {
    transaction
        .query_row(
            "UPDATE app_meta SET integer_value = integer_value + 1 \
             WHERE key = 'session_change_sequence' RETURNING integer_value",
            [],
            |row| row.get(0),
        )
        .map_err(map_sqlite)
}

fn source_timestamp(value: f64) -> Result<String, HermesImportError> {
    if !value.is_finite() {
        return Err(HermesImportError::SourceInvalid);
    }
    let nanoseconds = (value * 1_000_000_000.0).round();
    let timestamp = OffsetDateTime::from_unix_timestamp_nanos(nanoseconds as i128)
        .map_err(|_| HermesImportError::SourceInvalid)?;
    timestamp
        .format(&Rfc3339)
        .map_err(|_| HermesImportError::SourceInvalid)
}

fn now_timestamp() -> Result<String, HermesImportError> {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|_| HermesImportError::StorageUnavailable)
}

fn new_revision() -> String {
    format!("session_rev_{}", Uuid::new_v4().simple())
}

fn to_i64(value: usize) -> Result<i64, HermesImportError> {
    i64::try_from(value).map_err(|_| HermesImportError::SourceInvalid)
}

fn bool_to_i64(value: bool) -> i64 {
    if value { 1 } else { 0 }
}
